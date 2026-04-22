// cqs serve — frontend router.
//
// Parses the URL `?view=` parameter, picks the right view module
// (registered as a global on `window`), wires shared callbacks
// (sidebar populate, status bar updates), then hands off to the
// module's init() + render(). View modules live in `views/{name}.js`
// and conform to the small interface documented in
// `docs/plans/2026-04-22-cqs-serve-3d-progressive.md` step 1.

(function () {
  "use strict";

  // --- DOM handles ---
  const $stats = document.getElementById("stats-badge");
  const $search = document.getElementById("search");
  const $sidebar = document.getElementById("sidebar");
  const $status = document.getElementById("status");
  const $cy = document.getElementById("cy");
  const $toggle2D = document.getElementById("view-2d");
  const $toggle3D = document.getElementById("view-3d");

  // --- View registry ---
  const VIEWS = {
    "2d": () => window.CqsCallgraph2D,
    "3d": () => window.CqsCallgraph3D,
  };

  let currentView = null;
  let currentViewName = null;
  let currentGraphData = null;

  // Default cap on initial render. Spec gates the corpus at 16k nodes;
  // for first paint we limit to top-N by caller count to keep the
  // browser responsive. User can broaden via URL ?max=NNN.
  const url = new URL(window.location.href);
  const MAX_NODES = parseInt(url.searchParams.get("max") || "1500", 10);
  const INITIAL_VIEW =
    url.searchParams.get("view") === "3d" ? "3d" : "2d";

  function setStatus(s) {
    $status.textContent = s || "";
  }

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#039;");
  }

  function setActiveToggle(viewName) {
    if (!$toggle2D || !$toggle3D) return;
    $toggle2D.classList.toggle("active", viewName === "2d");
    $toggle3D.classList.toggle("active", viewName === "3d");
  }

  function syncUrlView(viewName) {
    const u = new URL(window.location.href);
    if (viewName === "2d") {
      u.searchParams.delete("view");
    } else {
      u.searchParams.set("view", viewName);
    }
    window.history.replaceState({}, "", u.toString());
  }

  // --- Shared callbacks the view modules dispatch into ---
  const callbacks = {
    onNodeClick: (chunkId) => loadChunkDetail(chunkId),
    onNodeHover: (text) => setStatus(text),
  };

  async function loadStats() {
    try {
      const resp = await fetch("/api/stats");
      if (!resp.ok) {
        $stats.textContent = `stats: HTTP ${resp.status}`;
        return;
      }
      const s = await resp.json();
      $stats.textContent =
        `${s.total_chunks.toLocaleString()} chunks · ` +
        `${s.call_edges.toLocaleString()} edges · ` +
        `${s.total_files.toLocaleString()} files`;
    } catch (e) {
      $stats.textContent = `stats: ${e.message}`;
    }
  }

  async function loadGraph() {
    setStatus(`loading ≤${MAX_NODES.toLocaleString()} nodes…`);
    try {
      const resp = await fetch(`/api/graph?max_nodes=${MAX_NODES}`);
      if (!resp.ok) {
        setStatus(`graph HTTP ${resp.status}`);
        return null;
      }
      const data = await resp.json();
      currentGraphData = data;
      return data;
    } catch (e) {
      setStatus(`graph error: ${e.message}`);
      return null;
    }
  }

  async function activateView(viewName) {
    const factory = VIEWS[viewName];
    if (!factory) {
      console.error("Unknown view:", viewName);
      return;
    }
    const newView = factory();
    if (!newView) {
      console.error("View module not loaded:", viewName);
      setStatus(`view "${viewName}" not loaded`);
      return;
    }

    // Tear down the previous view (if any).
    if (currentView && typeof currentView.dispose === "function") {
      try {
        currentView.dispose();
      } catch (e) {
        console.warn("Previous view dispose threw:", e);
      }
    }

    currentView = newView;
    currentViewName = viewName;
    setActiveToggle(viewName);
    syncUrlView(viewName);

    setStatus(`booting ${viewName}…`);
    try {
      await currentView.init($cy, { callbacks });
    } catch (e) {
      setStatus(`init error: ${e.message}`);
      console.error("view init failed:", e);
      return;
    }

    if (!currentGraphData) {
      currentGraphData = await loadGraph();
      if (!currentGraphData) return;
    }

    setStatus(
      `rendering ${currentGraphData.nodes.length.toLocaleString()} nodes / ` +
        `${currentGraphData.edges.length.toLocaleString()} edges…`,
    );
    try {
      await currentView.render(currentGraphData);
      setStatus(
        `${currentGraphData.nodes.length.toLocaleString()} nodes · ${viewName} · click any`,
      );
    } catch (e) {
      setStatus(`render error: ${e.message}`);
      console.error("view render failed:", e);
    }
  }

  async function loadChunkDetail(id) {
    $sidebar.innerHTML = `<p class="hint">loading…</p>`;
    try {
      const resp = await fetch(`/api/chunk/${encodeURIComponent(id)}`);
      if (!resp.ok) {
        $sidebar.innerHTML = `<p class="error">HTTP ${resp.status}</p>`;
        return;
      }
      const c = await resp.json();
      const callerLis = c.callers
        .map(
          (r) =>
            `<li data-id="${escapeHtml(r.id)}"><strong>${escapeHtml(r.name)}</strong><small>${escapeHtml(r.file)}:${r.line_start}</small></li>`,
        )
        .join("");
      const calleeLis = c.callees
        .map(
          (r) =>
            `<li data-id="${escapeHtml(r.id)}"><strong>${escapeHtml(r.name)}</strong><small>${escapeHtml(r.file)}:${r.line_start}</small></li>`,
        )
        .join("");
      const testLis = c.tests
        .map(
          (r) =>
            `<li data-id="${escapeHtml(r.id)}"><strong>${escapeHtml(r.name)}</strong><small>${escapeHtml(r.file)}:${r.line_start}</small></li>`,
        )
        .join("");
      $sidebar.innerHTML = `
        <h3>${escapeHtml(c.name)}</h3>
        <div class="meta">
          ${escapeHtml(c.type)} · ${escapeHtml(c.language)} ·
          <code>${escapeHtml(c.file)}:${c.line_start}</code>
        </div>
        ${c.signature ? `<div><code>${escapeHtml(c.signature)}</code></div>` : ""}
        ${c.doc ? `<h4>doc</h4><pre>${escapeHtml(c.doc)}</pre>` : ""}
        <h4>source preview</h4>
        <pre>${escapeHtml(c.content_preview)}</pre>
        ${c.callers.length ? `<h4>callers (${c.callers.length})</h4><ul>${callerLis}</ul>` : ""}
        ${c.callees.length ? `<h4>callees (${c.callees.length})</h4><ul>${calleeLis}</ul>` : ""}
        ${c.tests.length ? `<h4>tests (${c.tests.length})</h4><ul>${testLis}</ul>` : ""}
      `;
      $sidebar.querySelectorAll("li[data-id]").forEach((li) => {
        li.addEventListener("click", () => {
          const targetId = li.getAttribute("data-id");
          if (currentView && typeof currentView.onNodeFocus === "function") {
            currentView.onNodeFocus(targetId);
          }
          loadChunkDetail(targetId);
        });
      });
    } catch (e) {
      $sidebar.innerHTML = `<p class="error">error: ${escapeHtml(e.message)}</p>`;
    }
  }

  let searchTimer = null;
  function onSearchInput() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(runSearch, 200);
  }

  async function runSearch() {
    const q = $search.value.trim();
    if (!currentView) return;
    if (q.length < 2) {
      if (typeof currentView.onSearchHighlight === "function") {
        currentView.onSearchHighlight([]);
      }
      return;
    }
    try {
      const resp = await fetch(
        `/api/search?q=${encodeURIComponent(q)}&limit=50`,
      );
      const data = await resp.json();
      const ids = data.matches.map((m) => m.id);
      let inView = 0;
      if (typeof currentView.onSearchHighlight === "function") {
        inView = currentView.onSearchHighlight(ids);
      }
      setStatus(`${data.matches.length} matches; ${inView} in view`);
    } catch (e) {
      setStatus(`search error: ${e.message}`);
    }
  }

  // Wire toggle buttons + search input
  if ($toggle2D) {
    $toggle2D.addEventListener("click", () => activateView("2d"));
  }
  if ($toggle3D) {
    $toggle3D.addEventListener("click", () => activateView("3d"));
  }
  $search.addEventListener("input", onSearchInput);

  // Boot
  loadStats();
  activateView(INITIAL_VIEW);
})();
