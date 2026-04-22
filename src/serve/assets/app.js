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
  const $toggleCluster = document.getElementById("view-cluster");
  const $hierarchyControls = document.getElementById("hierarchy-controls");
  const $directionGroup = document.getElementById("hierarchy-direction");
  const $depthGroup = document.getElementById("hierarchy-depth");
  const $clusterControls = document.getElementById("cluster-controls");
  const $colorGroup = document.getElementById("cluster-color");

  // --- View registry ---
  const VIEWS = {
    "2d": () => window.CqsCallgraph2D,
    "3d": () => window.CqsCallgraph3D,
    hierarchy: () => window.CqsHierarchy3D,
    cluster: () => window.CqsCluster3D,
  };

  let currentView = null;
  let currentViewName = null;
  let currentGraphData = null;

  // Default cap on initial render. Spec gates the corpus at 16k nodes;
  // for first paint we limit to top-N by caller count to keep the
  // browser responsive. User can broaden via URL ?max=NNN.
  const url = new URL(window.location.href);
  const MAX_NODES = parseInt(url.searchParams.get("max") || "1500", 10);
  function pickInitialView() {
    const v = url.searchParams.get("view");
    if (v === "3d" || v === "hierarchy" || v === "cluster") return v;
    return "2d";
  }
  const INITIAL_VIEW = pickInitialView();

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
    if ($toggle2D) $toggle2D.classList.toggle("active", viewName === "2d");
    if ($toggle3D) $toggle3D.classList.toggle("active", viewName === "3d");
    if ($toggleCluster)
      $toggleCluster.classList.toggle("active", viewName === "cluster");
  }

  function showClusterControls(show) {
    if (!$clusterControls) return;
    $clusterControls.style.display = show ? "inline-flex" : "none";
  }

  function showHierarchyControls(show) {
    if (!$hierarchyControls) return;
    $hierarchyControls.style.display = show ? "inline-flex" : "none";
  }

  function syncUrlView(viewName) {
    const u = new URL(window.location.href);
    // Always strip view-specific params when switching views — re-set
    // below if the new view owns one of them.
    const stripIfNotInView = (key, owners) => {
      if (!owners.includes(viewName)) u.searchParams.delete(key);
    };
    stripIfNotInView("root", ["hierarchy"]);
    stripIfNotInView("direction", ["hierarchy"]);
    stripIfNotInView("depth", ["hierarchy"]);
    stripIfNotInView("color", ["cluster"]);

    if (viewName === "2d") {
      u.searchParams.delete("view");
    } else {
      u.searchParams.set("view", viewName);
    }
    window.history.replaceState({}, "", u.toString());
  }

  function syncHierarchyControls() {
    if (!$directionGroup || !$depthGroup) return;
    const u = new URL(window.location.href);
    const dir = u.searchParams.get("direction") || "callees";
    const depth = u.searchParams.get("depth") || "5";
    $directionGroup.querySelectorAll("button").forEach((btn) => {
      btn.classList.toggle("active", btn.getAttribute("data-direction") === dir);
    });
    $depthGroup.querySelectorAll("button").forEach((btn) => {
      btn.classList.toggle("active", btn.getAttribute("data-depth") === depth);
    });
  }

  function setHierarchyParam(key, value) {
    const u = new URL(window.location.href);
    u.searchParams.set("view", "hierarchy");
    u.searchParams.set(key, value);
    window.history.replaceState({}, "", u.toString());
    syncHierarchyControls();
    // Force re-fetch of hierarchy data on re-activate.
    currentGraphData = null;
    activateView("hierarchy");
  }

  function syncClusterControls() {
    if (!$colorGroup) return;
    const u = new URL(window.location.href);
    const color = u.searchParams.get("color") || "type";
    $colorGroup.querySelectorAll("button").forEach((btn) => {
      btn.classList.toggle("active", btn.getAttribute("data-color") === color);
    });
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
    showHierarchyControls(viewName === "hierarchy");
    showClusterControls(viewName === "cluster");
    if (viewName === "hierarchy") syncHierarchyControls();
    if (viewName === "cluster") syncClusterControls();
    syncUrlView(viewName);

    setStatus(`booting ${viewName}…`);
    try {
      await currentView.init($cy, { callbacks });
    } catch (e) {
      setStatus(`init error: ${e.message}`);
      console.error("view init failed:", e);
      return;
    }

    // Two data-loading paths:
    //   1. Views with their own loadData() (hierarchy, cluster) — router
    //      calls it with the URL context and renders the result.
    //   2. Default views (2D/3D callgraph) — share /api/graph payload.
    let dataForRender;
    if (typeof currentView.loadData === "function") {
      setStatus(`loading data for ${viewName}…`);
      try {
        dataForRender = await currentView.loadData({
          url: new URL(window.location.href),
          maxNodes: MAX_NODES,
        });
      } catch (e) {
        setStatus(`loadData error: ${e.message}`);
        console.error("view loadData failed:", e);
        return;
      }
      if (!dataForRender) {
        setStatus(`${viewName}: no data`);
        return;
      }
    } else {
      if (!currentGraphData) {
        currentGraphData = await loadGraph();
        if (!currentGraphData) return;
      }
      dataForRender = currentGraphData;
    }

    setStatus(
      `rendering ${dataForRender.nodes.length.toLocaleString()} nodes` +
        (dataForRender.edges
          ? ` / ${dataForRender.edges.length.toLocaleString()} edges`
          : "") +
        "…",
    );
    try {
      await currentView.render(dataForRender);
      setStatus(
        `${dataForRender.nodes.length.toLocaleString()} nodes · ${viewName} · click any`,
      );
    } catch (e) {
      setStatus(`render error: ${e.message}`);
      console.error("view render failed:", e);
    }
  }

  function setClusterColor(color) {
    const u = new URL(window.location.href);
    u.searchParams.set("view", "cluster");
    u.searchParams.set("color", color);
    window.history.replaceState({}, "", u.toString());
    syncClusterControls();
    if (currentView && typeof currentView.setColorMode === "function") {
      currentView.setColorMode(color);
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
        <div class="sidebar-actions">
          <button type="button" class="btn-link" data-action="hierarchy-callees" data-id="${escapeHtml(c.id)}">view callees as hierarchy ↓</button>
          <button type="button" class="btn-link" data-action="hierarchy-callers" data-id="${escapeHtml(c.id)}">view callers as hierarchy ↑</button>
        </div>
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
      $sidebar.querySelectorAll("button[data-action^='hierarchy-']").forEach((btn) => {
        btn.addEventListener("click", () => {
          const action = btn.getAttribute("data-action");
          const targetId = btn.getAttribute("data-id");
          const direction = action === "hierarchy-callers" ? "callers" : "callees";
          openHierarchy(targetId, direction);
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

  function openHierarchy(rootId, direction) {
    const u = new URL(window.location.href);
    u.searchParams.set("view", "hierarchy");
    u.searchParams.set("root", rootId);
    u.searchParams.set("direction", direction || "callees");
    if (!u.searchParams.get("depth")) {
      u.searchParams.set("depth", "5");
    }
    window.history.replaceState({}, "", u.toString());
    currentGraphData = null; // hierarchy uses its own loadData
    activateView("hierarchy");
  }

  // Wire toggle buttons + search input
  if ($toggle2D) {
    $toggle2D.addEventListener("click", () => activateView("2d"));
  }
  if ($toggle3D) {
    $toggle3D.addEventListener("click", () => activateView("3d"));
  }
  if ($toggleCluster) {
    $toggleCluster.addEventListener("click", () => activateView("cluster"));
  }
  // Hierarchy controls (depth + direction). Buttons carry data-attrs that
  // map to URL params; clicking re-activates the view to refetch.
  if ($directionGroup) {
    $directionGroup.querySelectorAll("button").forEach((btn) => {
      btn.addEventListener("click", () => {
        const v = btn.getAttribute("data-direction");
        if (v) setHierarchyParam("direction", v);
      });
    });
  }
  if ($depthGroup) {
    $depthGroup.querySelectorAll("button").forEach((btn) => {
      btn.addEventListener("click", () => {
        const v = btn.getAttribute("data-depth");
        if (v) setHierarchyParam("depth", v);
      });
    });
  }
  // Cluster colour mode (type/language). Same pattern.
  if ($colorGroup) {
    $colorGroup.querySelectorAll("button").forEach((btn) => {
      btn.addEventListener("click", () => {
        const v = btn.getAttribute("data-color");
        if (v) setClusterColor(v);
      });
    });
  }
  $search.addEventListener("input", onSearchInput);

  // Boot
  loadStats();
  activateView(INITIAL_VIEW);
})();
