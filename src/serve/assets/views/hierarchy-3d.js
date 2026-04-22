// Hierarchy 3D view — 3d-force-graph with Y axis locked to BFS depth.
//
// Conforms to the renderer abstraction interface from step 1, plus an
// optional `loadData(context)` hook the router calls instead of feeding
// shared `/api/graph` data. Hierarchy fetches its own subgraph from
// `/api/hierarchy/{root_id}` and arranges it as a tree.
//
// Conventions:
//   - direction=callees → root at top, callees grow downward (fy negative)
//   - direction=callers → root at bottom, callers grow upward  (fy positive)
//   - X position from force layout (siblings spread)
//   - Z stays free so dense layers can fan out into space

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

  const Y_SPACING = 80;

  function nodeColor(kind) {
    return TYPE_COLORS[kind] || "#999";
  }

  function nodeRadius(callers) {
    return Math.max(1.5, Math.min(8, 1.5 + Math.sqrt(callers) * 0.6));
  }

  window.CqsHierarchy3D = {
    graph: null,
    container: null,
    cb: null,
    nodeIds: new Set(),
    highlighted: new Set(),
    selected: null,
    nodeIndex: new Map(),
    rootId: null,
    direction: "callees",
    depth: 5,

    async init(container, options) {
      this.container = container;
      this.cb = options.callbacks || {};
      container.innerHTML = "";

      if (typeof ForceGraph3D === "undefined") {
        container.innerHTML =
          '<div class="error" style="margin:24px">3D renderer not loaded — check that ' +
          "<code>/static/vendor/three.min.js</code> and " +
          "<code>/static/vendor/3d-force-graph.min.js</code> served correctly.</div>";
        throw new Error("ForceGraph3D global not present");
      }
    },

    /// Router calls this before render() when the view module declares it.
    /// Reads ?root, ?direction, ?depth from the URL and fetches the
    /// hierarchy subgraph. Returns the response payload (with a
    /// bfs_depth on each node) or null on missing root / HTTP failure.
    async loadData(context) {
      const url = context.url;
      const root = url.searchParams.get("root");
      if (!root) {
        this.container.innerHTML =
          '<div class="error" style="margin:24px">hierarchy view requires <code>?root=&lt;chunk_id&gt;</code> in the URL. ' +
          'Click a node in the 2D or 3D view, then use "view as hierarchy" in the sidebar.</div>';
        return null;
      }
      this.rootId = root;
      this.direction = url.searchParams.get("direction") || "callees";
      this.depth = parseInt(url.searchParams.get("depth") || "5", 10);
      if (!Number.isFinite(this.depth) || this.depth < 1) this.depth = 5;
      if (this.depth > 10) this.depth = 10;

      const params = new URLSearchParams({
        direction: this.direction,
        depth: String(this.depth),
      });
      const apiUrl = `/api/hierarchy/${encodeURIComponent(root)}?${params}`;
      try {
        const resp = await fetch(apiUrl);
        if (!resp.ok) {
          const body = await resp.text();
          this.container.innerHTML = `<div class="error" style="margin:24px">hierarchy HTTP ${resp.status}: ${body.slice(0, 200)}</div>`;
          return null;
        }
        return await resp.json();
      } catch (e) {
        this.container.innerHTML = `<div class="error" style="margin:24px">hierarchy fetch error: ${e.message}</div>`;
        return null;
      }
    },

    async render(data) {
      this.nodeIds = new Set(data.nodes.map((n) => n.id));
      // For callers (BFS up the graph), depth grows upward on screen.
      // For callees (BFS down), depth grows downward. Either way the
      // root sits at fy = 0 so the camera centres on it naturally.
      const sign = this.direction === "callers" ? 1 : -1;
      const nodes = data.nodes.map((n) => ({
        id: n.id,
        name: n.name,
        kind: n.type,
        file: n.file,
        line: n.line_start,
        callers: n.n_callers,
        callees: n.n_callees,
        dead: n.dead,
        depth: n.bfs_depth,
        // fy locks Y to the BFS depth — 3d-force-graph honors fx/fy/fz
        // and skips force-directed updates on those axes.
        fy: sign * n.bfs_depth * Y_SPACING,
      }));
      this.nodeIndex = new Map(nodes.map((n) => [n.id, n]));

      this.graph = ForceGraph3D()(this.container)
        .graphData({
          nodes,
          links: data.edges.map((e) => ({
            source: e.source,
            target: e.target,
          })),
        })
        .backgroundColor("#0d1117")
        .nodeLabel(
          (n) =>
            `${n.name} · depth ${n.depth} · ${n.kind} · ${n.callers} callers`,
        )
        .nodeColor((n) => {
          if (this.selected === n.id) return "#fc0";
          if (this.highlighted.has(n.id)) return "#ff8c00";
          if (n.id === this.rootId) return "#fff";
          if (n.dead) return "#c33";
          return nodeColor(n.kind);
        })
        .nodeVal((n) => Math.pow(nodeRadius(n.callers), 2))
        .nodeOpacity(0.9)
        .linkColor(() => "rgba(180,180,200,0.35)")
        .linkOpacity(0.55)
        .linkDirectionalArrowLength(2.5)
        .linkDirectionalArrowRelPos(1)
        .linkDirectionalArrowColor(() => "rgba(200,200,220,0.55)")
        .cooldownTime(8000)
        .warmupTicks(20)
        .onNodeClick((node) => {
          this.selected = node.id;
          this.graph.refresh();
          if (this.cb.onNodeClick) this.cb.onNodeClick(node.id);
        })
        .onNodeHover((node) => {
          if (node && this.cb.onNodeHover) {
            this.cb.onNodeHover(
              `${node.name} · depth ${node.depth} · ${node.kind}`,
            );
          } else if (!node && this.cb.onNodeHover) {
            this.cb.onNodeHover("");
          }
        });

      // Side-on camera: position the camera off to the +X side so the
      // depth axis (Y) reads vertically and the tree is unmistakable.
      // Defer one frame so the graph has bounds to centre on.
      const maxDepth = nodes.reduce(
        (m, n) => Math.max(m, n.depth || 0),
        0,
      );
      const span = Math.max(maxDepth, 1) * Y_SPACING;
      window.requestAnimationFrame(() => {
        if (!this.graph) return;
        try {
          this.graph.cameraPosition(
            { x: span * 2.2 + 200, y: (sign * span) / 2, z: 200 },
            { x: 0, y: (sign * span) / 2, z: 0 },
            0,
          );
        } catch (e) {
          console.warn("hierarchy: cameraPosition failed", e);
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
        const distance = 100;
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
      const distance = 80;
      const distRatio =
        1 +
        distance /
          Math.hypot(node.x || 0, node.y || 0, node.z || 0);
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
          if (typeof this.graph._destructor === "function") {
            this.graph._destructor();
          }
        } catch (e) {
          console.warn("CqsHierarchy3D.dispose: _destructor threw", e);
        }
        this.graph = null;
      }
      this.nodeIds = new Set();
      this.highlighted = new Set();
      this.selected = null;
      this.nodeIndex = new Map();
      this.rootId = null;
      if (this.container) {
        this.container.innerHTML = "";
      }
      this.container = null;
      this.cb = null;
    },
  };
})(window);
