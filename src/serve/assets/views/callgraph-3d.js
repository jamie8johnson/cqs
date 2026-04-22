// 3D call graph view — 3d-force-graph (built on Three.js).
//
// Conforms to the renderer abstraction interface: { init, render,
// onSearchHighlight, onNodeFocus, dispose }. Reuses the same
// /api/graph payload as the 2D view; just renders it differently.

(function (window) {
  "use strict";

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

  function nodeRadius(callers) {
    return Math.max(1.5, Math.min(8, 1.5 + Math.sqrt(callers) * 0.6));
  }

  // Track highlighted IDs as a Set so render-time color lookup is O(1).
  window.CqsCallgraph3D = {
    graph: null,
    container: null,
    cb: null,
    nodeIds: new Set(),
    highlighted: new Set(),
    selected: null,
    nodeIndex: new Map(), // id -> node object reference (for click + focus)

    async init(container, options) {
      this.container = container;
      this.cb = options.callbacks || {};
      container.innerHTML = "";

      if (typeof ForceGraph3D === "undefined") {
        container.innerHTML =
          '<div class="error" style="margin:24px">3D renderer not loaded — check that ' +
          '<code>/static/vendor/three.min.js</code> and ' +
          '<code>/static/vendor/3d-force-graph.min.js</code> served correctly.</div>';
        throw new Error("ForceGraph3D global not present");
      }
    },

    async render(graphData) {
      this.nodeIds = new Set(graphData.nodes.map((n) => n.id));
      const data = {
        nodes: graphData.nodes.map((n) => ({
          id: n.id,
          name: n.name,
          kind: n.type,
          file: n.file,
          line: n.line_start,
          callers: n.n_callers,
          callees: n.n_callees,
          dead: n.dead,
        })),
        links: graphData.edges.map((e) => ({
          source: e.source,
          target: e.target,
        })),
      };
      this.nodeIndex = new Map(data.nodes.map((n) => [n.id, n]));

      this.graph = ForceGraph3D()(this.container)
        .graphData(data)
        .backgroundColor("#0d1117")
        .nodeLabel((n) => `${n.name} · ${n.kind} · ${n.callers} callers`)
        .nodeColor((n) => {
          if (this.selected === n.id) return "#fc0";
          if (this.highlighted.has(n.id)) return "#ff8c00";
          if (n.dead) return "#c33";
          return nodeColor(n.kind);
        })
        .nodeVal((n) => Math.pow(nodeRadius(n.callers), 2))
        .nodeOpacity(0.85)
        .linkColor(() => "rgba(180,180,200,0.25)")
        .linkOpacity(0.4)
        .linkDirectionalArrowLength(2)
        .linkDirectionalArrowRelPos(1)
        .linkDirectionalArrowColor(() => "rgba(180,180,200,0.45)")
        .cooldownTime(8000) // physics convergence cap (ms) — spec target
        .warmupTicks(10)
        .onNodeClick((node) => {
          this.selected = node.id;
          this.graph.refresh();
          if (this.cb.onNodeClick) this.cb.onNodeClick(node.id);
        })
        .onNodeHover((node) => {
          if (node && this.cb.onNodeHover) {
            this.cb.onNodeHover(
              `${node.name} · ${node.kind} · ${node.callers} callers`,
            );
          } else if (!node && this.cb.onNodeHover) {
            this.cb.onNodeHover("");
          }
        });
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
        // Center camera on the first match.
        const distance = 80;
        const distRatio =
          1 +
          distance /
            Math.hypot(
              firstMatch.x || 0,
              firstMatch.y || 0,
              firstMatch.z || 0,
            );
        this.graph.cameraPosition(
          {
            x: (firstMatch.x || 0) * distRatio,
            y: (firstMatch.y || 0) * distRatio,
            z: (firstMatch.z || 0) * distRatio,
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
      const distance = 60;
      const distRatio =
        1 +
        distance / Math.hypot(node.x || 0, node.y || 0, node.z || 0);
      this.graph.cameraPosition(
        {
          x: (node.x || 0) * distRatio,
          y: (node.y || 0) * distRatio,
          z: (node.z || 0) * distRatio,
        },
        node,
        800,
      );
      return true;
    },

    dispose() {
      if (this.graph) {
        try {
          // 3d-force-graph exposes ._destructor() in some versions; guard.
          if (typeof this.graph._destructor === "function") {
            this.graph._destructor();
          }
        } catch (e) {
          console.warn("CqsCallgraph3D.dispose: _destructor threw", e);
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
