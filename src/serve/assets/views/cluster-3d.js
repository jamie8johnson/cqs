// Embedding cluster view — fixed-position 3D layout.
//
// Each chunk lives at:
//     x = umap_x × XY_SCALE
//     y = caller_count × Y_SCALE   (high-degree functions float above)
//     z = umap_y × XY_SCALE
//
// The result: semantic neighbours sit close in the X/Z plane, "important"
// functions stand up vertically. Edges aren't rendered — at this density
// they obscure the structure.
//
// Conforms to the renderer interface from step 1, plus the optional
// `loadData(context)` hook from step 2 (this view fetches /api/embed/2d
// rather than reusing /api/graph).

(function (window) {
  "use strict";

  // Local XSS guard: this IIFE can't reach app.js's escapeHtml, so mirror it here
  // for any server-derived string interpolated into innerHTML.
  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#039;");
  }

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

  const LANGUAGE_COLORS = {
    rust: "#ce422b",
    python: "#3776ab",
    typescript: "#3178c6",
    javascript: "#f0db4f",
    go: "#00add8",
    java: "#b07219",
    cpp: "#00599c",
    c: "#5c6bc0",
    csharp: "#178600",
    ruby: "#cc342d",
    php: "#777bb4",
    swift: "#fa7343",
    kotlin: "#a97bff",
    scala: "#dc322f",
    elixir: "#6e4a7e",
  };

  const XY_SCALE = 60; // tightens or spreads the UMAP layout
  const Y_SCALE = 12; // amplification on caller_count → vertical lift

  function colorByType(kind) {
    return TYPE_COLORS[kind] || "#999";
  }

  function colorByLanguage(lang) {
    return LANGUAGE_COLORS[lang] || "#888";
  }

  function nodeRadius(callers) {
    return Math.max(1.5, Math.min(8, 1.5 + Math.sqrt(callers) * 0.6));
  }

  window.CqsCluster3D = {
    graph: null,
    container: null,
    cb: null,
    nodeIds: new Set(),
    highlighted: new Set(),
    selected: null,
    nodeIndex: new Map(),
    colorMode: "type", // "type" or "language"

    async init(container, options) {
      this.container = container;
      this.cb = options.callbacks || {};
      container.innerHTML =
        '<div style="margin:24px;color:#666">loading 3D renderer…</div>';

      if (typeof window.cqsEnsureThreeBundle === "function") {
        try {
          await window.cqsEnsureThreeBundle();
        } catch (e) {
          container.innerHTML = `<div class="error" style="margin:24px">3D bundle failed to load: ${escapeHtml(e.message)}</div>`;
          throw e;
        }
      }

      container.innerHTML = "";
      if (typeof ForceGraph3D === "undefined") {
        container.innerHTML =
          '<div class="error" style="margin:24px">3D renderer not loaded — check that ' +
          "<code>/static/vendor/three.min.js</code> and " +
          "<code>/static/vendor/3d-force-graph.min.js</code> served correctly.</div>";
        throw new Error("ForceGraph3D global not present");
      }
    },

    /// Router calls this before render() because cluster has its own data
    /// source (UMAP coords, not the call-graph payload).
    async loadData(context) {
      const url = context.url;
      const maxNodes = context.maxNodes || 1500;
      const params = new URLSearchParams({ max_nodes: String(maxNodes) });
      this.colorMode = url.searchParams.get("color") === "language" ? "language" : "type";

      try {
        const resp = await fetch(`/api/embed/2d?${params}`);
        if (!resp.ok) {
          const body = await resp.text();
          this.container.innerHTML = `<div class="error" style="margin:24px">cluster HTTP ${resp.status}: ${escapeHtml(body.slice(0, 200))}</div>`;
          return null;
        }
        const data = await resp.json();
        if (data.nodes.length === 0) {
          this.container.innerHTML =
            `<div class="error" style="margin:24px">No UMAP coordinates in this index ` +
            `(${data.skipped.toLocaleString()} chunks have no projection). ` +
            `Run <code>cqs index --umap</code> from the project root, then refresh.</div>`;
          return null;
        }
        return data;
      } catch (e) {
        this.container.innerHTML = `<div class="error" style="margin:24px">cluster fetch error: ${escapeHtml(e.message)}</div>`;
        return null;
      }
    },

    async render(data) {
      this.nodeIds = new Set(data.nodes.map((n) => n.id));
      const nodes = data.nodes.map((n) => ({
        id: n.id,
        name: n.name,
        kind: n.type,
        language: n.language,
        file: n.file,
        line: n.line_start,
        callers: n.n_callers,
        callees: n.n_callees,
        dead: n.dead,
        // fx/fy/fz freeze position so the layout matches the embedding
        // structure exactly — no force perturbation.
        fx: n.umap_x * XY_SCALE,
        fy: n.n_callers * Y_SCALE,
        fz: n.umap_y * XY_SCALE,
      }));
      this.nodeIndex = new Map(nodes.map((n) => [n.id, n]));

      this.graph = ForceGraph3D()(this.container)
        .graphData({ nodes, links: [] })
        .backgroundColor("#0d1117")
        .nodeLabel(
          (n) =>
            `${n.name} · ${n.kind} · ${n.language} · ${n.callers} callers`,
        )
        .nodeColor((n) => {
          if (this.selected === n.id) return "#fc0";
          if (this.highlighted.has(n.id)) return "#ff8c00";
          if (n.dead) return "#c33";
          return this.colorMode === "language"
            ? colorByLanguage(n.language)
            : colorByType(n.kind);
        })
        .nodeVal((n) => Math.pow(nodeRadius(n.callers), 2))
        .nodeOpacity(0.85)
        // No edges — explicitly empty links makes the cluster shape
        // legible. Edges add too much visual noise at this density.
        .cooldownTime(0) // positions are fixed; no physics needed
        .warmupTicks(0)
        .onNodeClick((node) => {
          this.selected = node.id;
          this.graph.refresh();
          if (this.cb.onNodeClick) this.cb.onNodeClick(node.id);
        })
        .onNodeHover((node) => {
          if (node && this.cb.onNodeHover) {
            this.cb.onNodeHover(
              `${node.name} · ${node.kind} · ${node.language} · ${node.callers} callers`,
            );
          } else if (!node && this.cb.onNodeHover) {
            this.cb.onNodeHover("");
          }
        });

      // Camera default: pull back along +Z so the X/Z plane is visible
      // and the Y "spires" stand out in profile.
      const span = nodes.reduce((m, n) => Math.max(m, Math.abs(n.fx) + Math.abs(n.fz)), 0);
      window.requestAnimationFrame(() => {
        if (!this.graph) return;
        try {
          this.graph.cameraPosition(
            { x: 0, y: span * 0.6 + 200, z: span * 1.6 + 400 },
            { x: 0, y: 0, z: 0 },
            0,
          );
        } catch (e) {
          console.warn("cluster: cameraPosition failed", e);
        }
      });
    },

    setColorMode(mode) {
      if (mode !== "type" && mode !== "language") return;
      this.colorMode = mode;
      if (this.graph) this.graph.refresh();
    },

    onSearchHighlight(matchedIds) {
      if (!this.graph) return 0;
      this.highlighted = new Set();
      let inView = 0;
      let firstMatch = null;
      for (const id of matchedIds) {
        if (this.nodeIds.has(id)) {
          this.highlighted.add(id);
          inView += 1;
          if (!firstMatch) firstMatch = this.nodeIndex.get(id);
        }
      }
      this.graph.refresh();
      if (firstMatch) {
        const distance = 100;
        const distRatio =
          1 +
          distance /
            Math.hypot(
              firstMatch.fx || 0,
              firstMatch.fy || 0,
              firstMatch.fz || 0,
            );
        this.graph.cameraPosition(
          {
            x: (firstMatch.fx || 0) * distRatio,
            y: (firstMatch.fy || 0) * distRatio,
            z: (firstMatch.fz || 0) * distRatio,
          },
          firstMatch,
          1000,
        );
      }
      return inView;
    },

    onNodeFocus(chunkId) {
      if (!this.graph || !this.nodeIds.has(chunkId)) return false;
      const node = this.nodeIndex.get(chunkId);
      if (!node) return false;
      this.selected = chunkId;
      this.graph.refresh();
      const distance = 80;
      const distRatio =
        1 +
        distance / Math.hypot(node.fx || 0, node.fy || 0, node.fz || 0);
      this.graph.cameraPosition(
        {
          x: (node.fx || 0) * distRatio,
          y: (node.fy || 0) * distRatio,
          z: (node.fz || 0) * distRatio,
        },
        node,
        800,
      );
      return true;
    },

    dispose() {
      if (this.graph) {
        try {
          if (typeof this.graph._destructor === "function") {
            this.graph._destructor();
          }
        } catch (e) {
          console.warn("CqsCluster3D.dispose: _destructor threw", e);
        }
        this.graph = null;
      }
      this.nodeIds = new Set();
      this.highlighted = new Set();
      this.selected = null;
      this.nodeIndex = new Map();
      if (this.container) {
        this.container.innerHTML = "";
      }
      this.container = null;
      this.cb = null;
    },
  };
})(window);
