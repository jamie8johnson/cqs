// cqs serve — v1 frontend.
//
// Boots Cytoscape.js with the dagre layout extension, fetches the call
// graph from /api/graph, renders nodes + edges. Click → fetch
// /api/chunk/:id, populate sidebar. Search box → /api/search,
// highlight matching nodes.

(function () {
  "use strict";

  // Default cap on initial render. Spec gates the corpus at 16k nodes;
  // for first paint we limit to the top-N by caller count to keep the
  // browser responsive. User can broaden via URL ?max=NNN.
  const url = new URL(window.location.href);
  const MAX_NODES = parseInt(url.searchParams.get("max") || "1500", 10);

  const $stats = document.getElementById("stats-badge");
  const $search = document.getElementById("search");
  const $sidebar = document.getElementById("sidebar");
  const $status = document.getElementById("status");

  // Register dagre with Cytoscape (the extension auto-registers on load
  // when both globals are present).
  if (typeof cytoscape !== "undefined" && typeof cytoscapeDagre !== "undefined") {
    cytoscape.use(cytoscapeDagre);
  }

  let cy = null;
  let allNodeIds = new Set();

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

  // Color by chunk type. Keep palette small; future per-language tints
  // can layer on top.
  const TYPE_COLORS = {
    function: "#4a86e8",
    method: "#3d78d8",
    impl: "#2e5fb0",
    struct: "#43aa6b",
    enum: "#5fbf85",
    trait: "#3d8b5d",
    interface: "#3d8b5d",
    class: "#43aa6b",
    test: "#e6a91c",
    constant: "#888",
    macro: "#a05ec5",
    typealias: "#888",
  };

  function nodeColor(kind) {
    return TYPE_COLORS[kind] || "#999";
  }

  // sqrt-scaled node size: many callers → bigger node.
  function nodeSize(callers) {
    return Math.max(8, Math.min(48, 8 + Math.sqrt(callers) * 4));
  }

  async function loadStats() {
    try {
      const resp = await fetch("/api/stats");
      const s = await resp.json();
      $stats.textContent = `${s.total_chunks.toLocaleString()} chunks · ${s.call_edges.toLocaleString()} edges · ${s.total_files.toLocaleString()} files`;
    } catch (e) {
      $stats.textContent = `stats error: ${e.message}`;
    }
  }

  async function loadGraph() {
    setStatus(`loading ≤${MAX_NODES.toLocaleString()} nodes…`);
    try {
      const resp = await fetch(`/api/graph?max_nodes=${MAX_NODES}`);
      if (!resp.ok) {
        setStatus(`graph HTTP ${resp.status}`);
        return;
      }
      const data = await resp.json();
      setStatus(`rendering ${data.nodes.length.toLocaleString()} nodes / ${data.edges.length.toLocaleString()} edges…`);
      await renderGraph(data);
      setStatus(`${data.nodes.length.toLocaleString()} nodes · click any`);
    } catch (e) {
      setStatus(`graph error: ${e.message}`);
    }
  }

  async function renderGraph(data) {
    allNodeIds = new Set(data.nodes.map(n => n.id));

    const elements = [];
    for (const n of data.nodes) {
      elements.push({
        group: "nodes",
        data: {
          id: n.id,
          label: n.name,
          kind: n.type,
          file: n.file,
          line: n.line_start,
          callers: n.n_callers,
          callees: n.n_callees,
          dead: n.dead,
        },
      });
    }
    for (const e of data.edges) {
      elements.push({
        group: "edges",
        data: { source: e.source, target: e.target, kind: e.kind },
      });
    }

    cy = cytoscape({
      container: document.getElementById("cy"),
      elements,
      style: [
        {
          selector: "node",
          style: {
            "background-color": (e) => nodeColor(e.data("kind")),
            "label": "data(label)",
            "font-size": 9,
            "color": "#222",
            "text-valign": "bottom",
            "text-halign": "center",
            "text-margin-y": 2,
            "width": (e) => nodeSize(e.data("callers")),
            "height": (e) => nodeSize(e.data("callers")),
            "border-width": 1,
            "border-color": "#fff",
            "min-zoomed-font-size": 6,
          },
        },
        {
          selector: "node[?dead]",
          style: {
            "border-color": "#c33",
            "border-width": 2,
            "opacity": 0.6,
          },
        },
        {
          selector: "node.highlight",
          style: {
            "border-color": "#fc0",
            "border-width": 3,
            "z-index": 999,
          },
        },
        {
          selector: "node.selected",
          style: {
            "border-color": "#000",
            "border-width": 3,
            "z-index": 999,
          },
        },
        {
          selector: "edge",
          style: {
            "width": 0.6,
            "line-color": "#bbb",
            "curve-style": "bezier",
            "target-arrow-shape": "triangle",
            "target-arrow-color": "#bbb",
            "arrow-scale": 0.6,
            "opacity": 0.5,
          },
        },
      ],
      layout: { name: "dagre", rankDir: "LR", nodeSep: 30, rankSep: 80, animate: false },
      hideEdgesOnViewport: true,
      textureOnViewport: true,
      minZoom: 0.05,
      maxZoom: 4,
    });

    cy.on("tap", "node", (evt) => {
      const id = evt.target.id();
      cy.elements().removeClass("selected");
      evt.target.addClass("selected");
      loadChunkDetail(id);
    });
    cy.on("mouseover", "node", (evt) => {
      const n = evt.target.data();
      const tip = `${n.label} · ${n.kind} · ${n.callers} callers`;
      setStatus(tip);
    });
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
      const callerLis = c.callers.map(r => `<li data-id="${escapeHtml(r.id)}"><strong>${escapeHtml(r.name)}</strong><small>${escapeHtml(r.file)}:${r.line_start}</small></li>`).join("");
      const calleeLis = c.callees.map(r => `<li data-id="${escapeHtml(r.id)}"><strong>${escapeHtml(r.name)}</strong><small>${escapeHtml(r.file)}:${r.line_start}</small></li>`).join("");
      const testLis = c.tests.map(r => `<li data-id="${escapeHtml(r.id)}"><strong>${escapeHtml(r.name)}</strong><small>${escapeHtml(r.file)}:${r.line_start}</small></li>`).join("");
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
      // Wire follow-the-link clicks in caller/callee/tests lists.
      $sidebar.querySelectorAll("li[data-id]").forEach(li => {
        li.addEventListener("click", () => {
          const targetId = li.getAttribute("data-id");
          if (cy && allNodeIds.has(targetId)) {
            const node = cy.getElementById(targetId);
            cy.elements().removeClass("selected");
            node.addClass("selected");
            cy.center(node);
            loadChunkDetail(targetId);
          } else {
            // Node not in current rendered set; just load detail.
            loadChunkDetail(targetId);
          }
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
    if (!cy) return;
    cy.nodes().removeClass("highlight");
    if (q.length < 2) return;
    try {
      const resp = await fetch(`/api/search?q=${encodeURIComponent(q)}&limit=50`);
      const data = await resp.json();
      let firstMatch = null;
      for (const m of data.matches) {
        if (allNodeIds.has(m.id)) {
          const node = cy.getElementById(m.id);
          node.addClass("highlight");
          if (!firstMatch) firstMatch = node;
        }
      }
      if (firstMatch) cy.center(firstMatch);
      setStatus(`${data.matches.length} matches; ${data.matches.filter(m => allNodeIds.has(m.id)).length} in view`);
    } catch (e) {
      setStatus(`search error: ${e.message}`);
    }
  }

  $search.addEventListener("input", onSearchInput);
  loadStats();
  loadGraph();
})();
