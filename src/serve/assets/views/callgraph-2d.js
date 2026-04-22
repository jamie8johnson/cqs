// 2D call graph view — Cytoscape.js with selectable layout.
//
// Default layout: `cose` (built-in force-directed). dagre is available
// behind `?layout=dagre` for users who want strict hierarchical reading,
// at the cost of much slower layout time on large graphs.
//
// Conforms to the renderer abstraction interface: { init, render,
// onSearchHighlight, onNodeFocus, dispose }. The main `app.js` router
// instantiates one of these per active view; switching views calls
// dispose() on the old one and init() on the new one.

(function (window) {
  "use strict";

  // Color by chunk type. Same palette as 3D for visual consistency
  // when toggling between views.
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

  function nodeSize(callers) {
    return Math.max(8, Math.min(48, 8 + Math.sqrt(callers) * 4));
  }

  // Cytoscape layout factory. `cose` (built-in force-directed) is the
  // default — runs in 1-2s on ~300 nodes and animates into place.
  // `dagre` is the historical hierarchical layout — much slower (tens
  // of seconds on >500 nodes) but produces strict left-to-right reading
  // order; available via ?layout=dagre.
  function layoutConfig(name) {
    if (name === "dagre") {
      return {
        name: "dagre",
        rankDir: "LR",
        nodeSep: 30,
        rankSep: 80,
        animate: false,
      };
    }
    // Default: cose. Tuned to give a usable layout fast on the ~300-node
    // default cap. nodeOverlap + idealEdgeLength keep clusters legible
    // without taking forever to settle.
    return {
      name: "cose",
      animate: false,
      nodeOverlap: 20,
      idealEdgeLength: 80,
      gravity: 0.25,
      numIter: 1000,
      randomize: true,
      fit: true,
      padding: 30,
    };
  }

  // View module — closure-scoped state lives here so dispose() can
  // tear it all down without leaking globals.
  window.CqsCallgraph2D = {
    cy: null,
    nodeIds: new Set(),
    container: null,
    cb: null,

    async init(container, options) {
      this.container = container;
      this.cb = options.callbacks || {};
      this.layoutName =
        new URL(window.location.href).searchParams.get("layout") || "cose";
      // Cytoscape needs a non-empty container; ensure it.
      container.innerHTML = "";
    },

    async render(graphData) {
      this.nodeIds = new Set(graphData.nodes.map((n) => n.id));

      const elements = [];
      for (const n of graphData.nodes) {
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
      for (const e of graphData.edges) {
        elements.push({
          group: "edges",
          data: { source: e.source, target: e.target, kind: e.kind },
        });
      }

      this.cy = cytoscape({
        container: this.container,
        elements,
        style: [
          {
            selector: "node",
            style: {
              "background-color": (e) => nodeColor(e.data("kind")),
              label: "data(label)",
              "font-size": 9,
              color: "#222",
              "text-valign": "bottom",
              "text-halign": "center",
              "text-margin-y": 2,
              width: (e) => nodeSize(e.data("callers")),
              height: (e) => nodeSize(e.data("callers")),
              "border-width": 1,
              "border-color": "#fff",
              "min-zoomed-font-size": 6,
            },
          },
          {
            selector: "node[?dead]",
            style: { "border-color": "#c33", "border-width": 2, opacity: 0.6 },
          },
          {
            selector: "node.highlight",
            style: { "border-color": "#fc0", "border-width": 3, "z-index": 999 },
          },
          {
            selector: "node.selected",
            style: { "border-color": "#000", "border-width": 3, "z-index": 999 },
          },
          {
            selector: "edge",
            style: {
              width: 0.6,
              "line-color": "#bbb",
              "curve-style": "bezier",
              "target-arrow-shape": "triangle",
              "target-arrow-color": "#bbb",
              "arrow-scale": 0.6,
              opacity: 0.5,
            },
          },
        ],
        layout: layoutConfig(this.layoutName),
        hideEdgesOnViewport: true,
        textureOnViewport: true,
        minZoom: 0.05,
        maxZoom: 4,
      });

      this.cy.on("tap", "node", (evt) => {
        const id = evt.target.id();
        this.cy.elements().removeClass("selected");
        evt.target.addClass("selected");
        if (this.cb.onNodeClick) this.cb.onNodeClick(id);
      });
      this.cy.on("mouseover", "node", (evt) => {
        const n = evt.target.data();
        const tip = `${n.label} · ${n.kind} · ${n.callers} callers`;
        if (this.cb.onNodeHover) this.cb.onNodeHover(tip);
      });
    },

    onSearchHighlight(matchedIds) {
      if (!this.cy) return 0;
      this.cy.nodes().removeClass("highlight");
      let inView = 0;
      let firstMatch = null;
      for (const id of matchedIds) {
        if (this.nodeIds.has(id)) {
          const node = this.cy.getElementById(id);
          node.addClass("highlight");
          if (!firstMatch) firstMatch = node;
          inView += 1;
        }
      }
      if (firstMatch) this.cy.center(firstMatch);
      return inView;
    },

    onNodeFocus(chunkId) {
      if (!this.cy || !this.nodeIds.has(chunkId)) return false;
      const node = this.cy.getElementById(chunkId);
      this.cy.elements().removeClass("selected");
      node.addClass("selected");
      this.cy.center(node);
      return true;
    },

    dispose() {
      if (this.cy) {
        try {
          this.cy.destroy();
        } catch (e) {
          console.warn("CqsCallgraph2D.dispose: cy.destroy() threw", e);
        }
        this.cy = null;
      }
      this.nodeIds = new Set();
      if (this.container) {
        this.container.innerHTML = "";
      }
      this.container = null;
      this.cb = null;
    },
  };
})(window);
