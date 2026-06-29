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

  // Mechanism-mode palette. Each step lights a role; the win framing is
  // rank-elevation (fused rank better than dense-alone rank), NOT a per-chunk
  // good/bad color and NOT gold-relevance (that is a later stage). Roles:
  //   dense   — chunk the dense (cosine) leg ranked (HNSW leg)
  //   splade  — chunk SPLADE found that dense ranked low/absent (the story)
  //   fused   — chunk fusion elevated over its dense-alone rank
  //   base    — every other node, dimmed
  const MECH_COLORS = {
    dense: "#4a86e8", // blue    — dense leg
    splade: "#e0529c", // magenta — SPLADE-only / dense-low pulls
    fused: "#f5a623", // amber   — fusion elevated over dense-alone
    gold: "#ffd34d", // bright   — the eval-gold chunk (Stage-2b tour)
    dimmed: "rgba(120,130,150,0.16)", // dimmed base layer
  };

  // Number of top entries per leg used to drive the step-through highlight.
  const MECH_TOP_K = 20;

  // The R@K window for the honest "where hybrid wins" signal. A win/loss is a
  // top-K presence FLIP of the gold between the dense-alone leg and the fused
  // leg: rescued = gold absent from dense top-K but present in fused top-K;
  // hurt = present in dense top-K but absent from fused top-K. K=5 matches the
  // ground-truth reference the tour is validated against.
  const TOUR_K = 5;

  // Per-category anchors the tour prefers when a matching query is present, so
  // the stratified walk reliably surfaces the canonical showcases — including
  // the honest NEGATIVE (conceptual_search, where fusion hurts the gold). Match
  // is case-insensitive prefix; any category without a pin falls back to its
  // first gold-resolved query.
  const TOUR_SHOWCASE = {
    type_filtered: "method implementations on the store struct",
    conceptual_search: "reciprocal rank fusion",
  };

  function colorByType(kind) {
    return TYPE_COLORS[kind] || "#999";
  }

  function colorByLanguage(lang) {
    return LANGUAGE_COLORS[lang] || "#888";
  }

  function nodeRadius(callers) {
    return Math.max(1.5, Math.min(8, 1.5 + Math.sqrt(callers) * 0.6));
  }

  function fmtScore(x) {
    if (x === null || x === undefined || Number.isNaN(x)) return "—";
    return Number(x).toFixed(3);
  }

  function fmtRank(r) {
    return r && r > 0 ? `#${r}` : "absent";
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

    // ── Mechanism mode (query-anchored step-through) ──────────────────────
    // `mechActive` gates the dimmed-base + per-step highlight rendering.
    // `mechLegs` holds the last `/api/search_legs` payload's `.legs`
    //   ({dense, sparse, fused} arrays, or null when fusion did not run).
    // `mechResults` holds the payload's `.results` chunk metadata (path/name).
    // `mechStep` is 0 (dense) / 1 (+SPLADE) / 2 (fused).
    // `mechRole` maps chunk_id → role string ("dense"|"splade"|"fused") for the
    //   ACTIVE step only; recomputed on every step change.
    // `mechPanel` is the overlay DOM node carrying the per-leg numbers.
    mechActive: false,
    mechLegs: null,
    mechResults: [],
    mechQuery: "",
    mechStep: 0,
    mechRole: new Map(),
    mechPanel: null,

    // ── Stage-2b eval-gold tour ───────────────────────────────────────────
    // The "where hybrid wins" stratified walk + R@K-delta panel. Shares the
    // dimmed-base renderer with mechanism mode but is a SEPARATE mode (the two
    // are mutually exclusive; entering one clears the other).
    // `tourActive` gates the tour's dimmed-base highlight + text-win panel.
    // `tourQueries` is the full `/api/eval_gold` set (every gold for the sweep).
    // `tourAnchors` is the stratified one-per-category walk (the step-through).
    // `tourStep` indexes `tourAnchors`.
    // `tourRole` maps chunk_id → role ("gold"|"dense"|"fused") for the active
    //   anchor only; recomputed per step.
    // `tourLegsCache` memoizes `/api/search_legs` by query string so stepping
    //   back and the R@K sweep don't refetch.
    // `tourPanelMode` is "walk" (per-anchor text win) or "rk" (the per-category
    //   R@K-delta table).
    // `rkRows` holds the per-category aggregates; `rkGen` cancels a stale sweep.
    tourActive: false,
    tourQueries: [],
    tourAnchors: [],
    tourStep: 0,
    tourRole: new Map(),
    tourLegsCache: new Map(),
    tourPanelMode: "walk",
    rkRows: null,
    rkGen: 0,
    rkSweeping: false,

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
        .nodeLabel((n) => this.nodeLabelText(n))
        .nodeColor((n) => {
          // Mechanism mode and the eval-gold tour both override the base
          // palette: dimmed base layer with the active step's role chunks lit.
          // Selection still wins so a clicked chunk stays findable.
          if (this.mechActive || this.tourActive) {
            if (this.selected === n.id) return "#fff";
            const role = this.dimRole().get(n.id);
            if (role) return MECH_COLORS[role] || MECH_COLORS.dense;
            return MECH_COLORS.dimmed;
          }
          if (this.selected === n.id) return "#fc0";
          if (this.highlighted.has(n.id)) return "#ff8c00";
          if (n.dead) return "#c33";
          return this.colorMode === "language"
            ? colorByLanguage(n.language)
            : colorByType(n.kind);
        })
        .nodeVal((n) => {
          // In a dimmed-base mode, inflate role chunks so the lit set reads
          // against the dimmed base; base nodes shrink so the highlight carries.
          if (this.mechActive || this.tourActive) {
            const base = Math.pow(nodeRadius(n.callers), 2);
            return this.dimRole().has(n.id) ? base * 3.5 : base * 0.5;
          }
          return Math.pow(nodeRadius(n.callers), 2);
        })
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
            this.cb.onNodeHover(this.nodeLabelText(node));
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

    // The role map the dimmed-base renderer reads — mechanism mode and the
    // eval-gold tour each own one, and they're mutually exclusive.
    dimRole() {
      return this.tourActive ? this.tourRole : this.mechRole;
    },

    // Hover/label text. In a dimmed-base mode, append the active role so the
    // tooltip carries the mechanism even before the panel is read.
    nodeLabelText(n) {
      const base = `${n.name} · ${n.kind} · ${n.language} · ${n.callers} callers`;
      if (!this.mechActive && !this.tourActive) return base;
      const role = this.dimRole().get(n.id);
      const roleTag = role ? ` · [${role}]` : "";
      return base + roleTag;
    },

    // ── Mechanism mode entry point ────────────────────────────────────────
    // Fetches the three pre-fusion legs for `query` and enters the dimmed-base
    // step-through. Honesty: dense scores are RAW cosine in [-1,1], only the
    // sparse leg is min-max'd; a dense `present_in_pool=false` chunk has an
    // INJECTED 0.0 cosine (absent, not zero-similar). Handles the daemon-down
    // 503 and the "fusion did not run" (legs:null) cases without crashing.
    async runMechanism(query) {
      if (!this.graph) return;
      // Mechanism mode and the eval-gold tour are mutually exclusive dimmed-base
      // modes — leave the tour before entering the query-anchored step-through.
      if (this.tourActive) this.clearTour();
      const q = (query || "").trim();
      if (q.length < 2) {
        this.clearMechanism();
        return;
      }
      this.ensureMechPanel();
      this.mechSetPanelHtml(
        `<div class="mech-status">querying legs for <code>${escapeHtml(q)}</code>…</div>`,
      );
      let resp;
      try {
        resp = await fetch(
          `/api/search_legs?q=${encodeURIComponent(q)}&k=${MECH_TOP_K}`,
        );
      } catch (e) {
        this.mechSetPanelHtml(
          `<div class="mech-status error">mechanism fetch error: ${escapeHtml(e.message)}</div>`,
        );
        return;
      }
      if (resp.status === 503) {
        let msg = "mechanism mode requires the retrieval daemon (cqs watch --serve)";
        try {
          const body = await resp.text();
          if (body) msg = body.slice(0, 240);
        } catch (_e) {
          /* keep default */
        }
        this.mechSetPanelHtml(
          `<div class="mech-status error">mechanism unavailable (503): ${escapeHtml(msg)}</div>`,
        );
        return;
      }
      if (!resp.ok) {
        let body = "";
        try {
          body = await resp.text();
        } catch (_e) {
          /* ignore */
        }
        this.mechSetPanelHtml(
          `<div class="mech-status error">legs HTTP ${resp.status}: ${escapeHtml(body.slice(0, 200))}</div>`,
        );
        return;
      }
      let body;
      try {
        body = await resp.json();
      } catch (e) {
        this.mechSetPanelHtml(
          `<div class="mech-status error">legs parse error: ${escapeHtml(e.message)}</div>`,
        );
        return;
      }

      // The serve handler forwards the daemon's dispatch envelope verbatim,
      // which wraps the SearchLegsOutput under a `data` key (sibling of an
      // optional `_meta`). Peel it; tolerate a bare/unwrapped shape too so the
      // join is robust to either envelope.
      const data = body && body.data ? body.data : body;

      this.mechQuery = data.query || q;
      this.mechResults = Array.isArray(data.results) ? data.results : [];

      if (!data.legs) {
        // Fusion did not run for this query (no SPLADE index, or an
        // FTS-by-name short-circuit before embedding). The three-leg view is
        // only defined when fusion ran — say so, don't fake it.
        this.mechLegs = null;
        this.mechActive = false;
        this.mechRole = new Map();
        this.graph.refresh();
        this.mechSetPanelHtml(
          `<div class="mech-status">no SPLADE fusion ran for <code>${escapeHtml(this.mechQuery)}</code> ` +
            `(query short-circuited or no SPLADE index on this surface). ` +
            `The three-leg mechanism view is only defined when fusion runs.</div>`,
        );
        return;
      }

      this.mechLegs = {
        dense: Array.isArray(data.legs.dense) ? data.legs.dense : [],
        sparse: Array.isArray(data.legs.sparse) ? data.legs.sparse : [],
        fused: Array.isArray(data.legs.fused) ? data.legs.fused : [],
      };
      this.mechActive = true;
      this.mechSetStep(0);
    },

    // Leave mechanism mode: restore the base palette + clear the panel.
    clearMechanism() {
      this.mechActive = false;
      this.mechLegs = null;
      this.mechResults = [];
      this.mechQuery = "";
      this.mechStep = 0;
      this.mechRole = new Map();
      if (this.graph) this.graph.refresh();
      if (this.mechPanel) {
        this.mechPanel.style.display = "none";
        this.mechPanel.innerHTML = "";
      }
    },

    // Move to step `s` (0=dense, 1=+SPLADE, 2=fused), recompute roles, repaint.
    mechSetStep(s) {
      if (!this.mechActive || !this.mechLegs) return;
      this.mechStep = Math.max(0, Math.min(2, s));
      this.mechRole = this.computeMechRoles(this.mechStep);
      if (this.graph) this.graph.refresh();
      this.renderMechPanel();
    },

    mechNextStep() {
      this.mechSetStep(this.mechStep + 1);
    },

    mechPrevStep() {
      this.mechSetStep(this.mechStep - 1);
    },

    // Compute the chunk_id → role map for the active step. Roles are CUMULATIVE
    // across steps (a dense chunk stays "dense" in step 1 unless SPLADE claims
    // it as a low-dense pull). The "story" sets:
    //   step 0: dense top-k present_in_pool      → role "dense"
    //   step 1: + sparse top-k; chunks SPLADE found that dense ranked low/absent
    //           → role "splade"; the rest of dense stays "dense"
    //   step 2: fused ranking; chunks whose fused rank beats their dense-alone
    //           rank (or were dense-absent) → role "fused"; the rest of the
    //           fused set stays "dense"
    computeMechRoles(step) {
      const roles = new Map();
      const legs = this.mechLegs;
      if (!legs) return roles;

      // Dense rank lookup (0 = absent). Used to judge "dense ranked low".
      const denseRank = new Map();
      for (const e of legs.dense) denseRank.set(e.chunk_id, e.rank || 0);

      // Step 0 + base for later steps: dense top-k that the dense leg returned.
      const denseTop = legs.dense
        .filter((e) => e.present_in_pool && e.rank > 0)
        .slice(0, MECH_TOP_K);
      for (const e of denseTop) roles.set(e.chunk_id, "dense");

      if (step >= 1) {
        // SPLADE top-k. A chunk SPLADE ranks but dense ranked low (worse than
        // the dense top-k window) or never retrieved is the "SPLADE found what
        // dense missed" story → role "splade". Otherwise it overlaps dense.
        const sparseTop = legs.sparse
          .filter((e) => e.present_in_pool && e.rank > 0)
          .slice(0, MECH_TOP_K);
        for (const e of sparseTop) {
          const dr = denseRank.get(e.chunk_id) || 0;
          const denseLowOrAbsent = dr === 0 || dr > MECH_TOP_K;
          if (denseLowOrAbsent) {
            roles.set(e.chunk_id, "splade");
          } else if (!roles.has(e.chunk_id)) {
            roles.set(e.chunk_id, "dense");
          }
        }
      }

      if (step >= 2) {
        // Fused ranking. A chunk the fusion ELEVATES over its dense-alone rank
        // (better fused rank than dense rank, or dense-absent) is the
        // "fusion pulled this up" set → role "fused". The win framing here is
        // strictly rank-elevation — NOT gold relevance, NOT a per-chunk +/−.
        for (const e of legs.fused) {
          const fr = e.rank || 0;
          if (fr === 0) continue;
          const dr = denseRank.get(e.chunk_id) || 0;
          const elevated = dr === 0 || fr < dr;
          if (elevated) {
            roles.set(e.chunk_id, "fused");
          } else if (!roles.has(e.chunk_id)) {
            roles.set(e.chunk_id, "dense");
          }
        }
      }

      return roles;
    },

    // ── Side panel (per-leg numbers, honest labels) ───────────────────────
    ensureMechPanel() {
      if (this.mechPanel || !this.container) return;
      const panel = document.createElement("div");
      panel.className = "mech-panel";
      panel.style.display = "none";
      // Click a row to focus that chunk in the cluster (joins by chunk_id).
      panel.addEventListener("click", (ev) => {
        const row = ev.target.closest("[data-chunk-id]");
        if (!row) return;
        const id = row.getAttribute("data-chunk-id");
        if (id) {
          this.onNodeFocus(id);
          if (this.cb && this.cb.onNodeClick) this.cb.onNodeClick(id);
        }
      });
      this.container.appendChild(panel);
      this.mechPanel = panel;
    },

    mechSetPanelHtml(html) {
      this.ensureMechPanel();
      if (!this.mechPanel) return;
      this.mechPanel.style.display = "block";
      this.mechPanel.innerHTML = html;
    },

    // Render the per-leg numbers for the chunks the active step lights, joined
    // by chunk_id. Numbers ride on text (the mechanism is read here, not
    // encoded in color). Honest labels per leg.
    renderMechPanel() {
      this.ensureMechPanel();
      if (!this.mechPanel || !this.mechLegs) return;

      const denseById = new Map(this.mechLegs.dense.map((e) => [e.chunk_id, e]));
      const sparseById = new Map(this.mechLegs.sparse.map((e) => [e.chunk_id, e]));
      const fusedById = new Map(this.mechLegs.fused.map((e) => [e.chunk_id, e]));
      const metaById = new Map(this.mechResults.map((r) => [r.chunk_id, r]));

      const stepNames = ["dense top-k", "+ SPLADE top-k", "α-fused result"];
      const stepName = stepNames[this.mechStep] || "";

      // The chunks in focus = those the active step assigns a (non-dimmed) role,
      // ordered by the leg most relevant to the step.
      let focusIds;
      if (this.mechStep === 0) {
        focusIds = this.mechLegs.dense
          .filter((e) => this.mechRole.get(e.chunk_id) === "dense")
          .map((e) => e.chunk_id);
      } else if (this.mechStep === 1) {
        // SPLADE pulls first (the story), then the dense overlap.
        const splade = this.mechLegs.sparse
          .filter((e) => this.mechRole.get(e.chunk_id) === "splade")
          .map((e) => e.chunk_id);
        const dense = this.mechLegs.dense
          .filter((e) => this.mechRole.get(e.chunk_id) === "dense")
          .map((e) => e.chunk_id);
        focusIds = splade.concat(dense.filter((id) => !splade.includes(id)));
      } else {
        focusIds = this.mechLegs.fused
          .filter((e) => this.mechRole.has(e.chunk_id))
          .map((e) => e.chunk_id);
      }
      // De-dup preserving order.
      focusIds = Array.from(new Set(focusIds));

      const rolesCount = { dense: 0, splade: 0, fused: 0 };
      for (const r of this.mechRole.values()) {
        if (rolesCount[r] !== undefined) rolesCount[r] += 1;
      }

      const rows = focusIds
        .map((id) => {
          const d = denseById.get(id);
          const s = sparseById.get(id);
          const f = fusedById.get(id);
          const meta = metaById.get(id);
          const role = this.mechRole.get(id) || "—";
          const node = this.nodeIndex.get(id);
          const inView = this.nodeIds.has(id);
          const name = meta
            ? meta.name
            : node
              ? node.name
              : id.slice(0, 12);
          const loc = meta
            ? `${meta.file}:${meta.line_start}`
            : node
              ? `${node.file}:${node.line}`
              : "";
          const dotColor = MECH_COLORS[role] || "#888";
          const offView = inView
            ? ""
            : ` <span class="mech-offview" title="not in the loaded cluster (raise ?max=N)">off-view</span>`;
          const denseAbsent = d && !d.present_in_pool;
          const sparseAbsent = s && !s.present_in_pool;
          return (
            `<div class="mech-row" data-chunk-id="${escapeHtml(id)}">` +
            `<div class="mech-row-head">` +
            `<span class="mech-dot" style="background:${dotColor}"></span>` +
            `<span class="mech-name">${escapeHtml(name)}</span>${offView}` +
            `</div>` +
            `<div class="mech-loc"><code>${escapeHtml(loc)}</code></div>` +
            `<div class="mech-legs">` +
            `<span class="mech-leg" title="raw cosine in [-1,1], NOT normalized${denseAbsent ? "; injected 0.0 (dense-absent)" : ""}">` +
            `dense ${fmtRank(d ? d.rank : 0)} · cos ${fmtScore(d ? d.raw_cosine : null)}${denseAbsent ? " (inj)" : ""}</span>` +
            `<span class="mech-leg" title="min-max normalized to [0,1] per-query (the fused value); raw dot is stable across queries${sparseAbsent ? "; absent from sparse pool" : ""}">` +
            `splade ${fmtRank(s ? s.rank : 0)} · mm ${fmtScore(s ? s.minmax_score : null)} · dot ${fmtScore(s ? s.raw_splade_dot : null)}</span>` +
            `<span class="mech-leg" title="fused = α·dense_raw_cosine + (1−α)·sparse_minmax">` +
            `fused ${fmtRank(f ? f.rank : 0)} · ${fmtScore(f ? f.fused_score : null)}</span>` +
            `</div>` +
            `</div>`
          );
        })
        .join("");

      const html =
        `<div class="mech-panel-head">` +
        `<div class="mech-title">mechanism · <code>${escapeHtml(this.mechQuery)}</code></div>` +
        `<div class="mech-steptabs">` +
        `<button type="button" class="mech-btn" data-mech="prev"${this.mechStep === 0 ? " disabled" : ""}>‹ prev</button>` +
        `<span class="mech-stepname">step ${this.mechStep + 1}/3 · ${escapeHtml(stepName)}</span>` +
        `<button type="button" class="mech-btn" data-mech="next"${this.mechStep === 2 ? " disabled" : ""}>next ›</button>` +
        `<button type="button" class="mech-btn mech-clear" data-mech="clear">clear</button>` +
        `</div>` +
        `<div class="mech-legend">` +
        `<span><span class="mech-dot" style="background:${MECH_COLORS.dense}"></span>dense</span>` +
        `<span><span class="mech-dot" style="background:${MECH_COLORS.splade}"></span>SPLADE-pull</span>` +
        `<span><span class="mech-dot" style="background:${MECH_COLORS.fused}"></span>fused-elevated</span>` +
        `</div>` +
        `<div class="mech-note">Plane = dense-cosine UMAP (stochastic, locally faithful, ` +
        `globally distorted — NOT a metric, NOT a divergence map). SPLADE structure is ` +
        `non-positional. "Elevated" = fused rank beats dense-alone rank (rank-elevation, ` +
        `not gold relevance).</div>` +
        `</div>` +
        `<div class="mech-rows">${rows || '<div class="mech-status">no chunks in focus for this step</div>'}</div>`;

      this.mechPanel.style.display = "block";
      this.mechPanel.innerHTML = html;

      // Wire the step buttons (innerHTML replaced them, so re-bind).
      this.mechPanel.querySelectorAll("[data-mech]").forEach((btn) => {
        btn.addEventListener("click", (ev) => {
          ev.stopPropagation();
          const action = btn.getAttribute("data-mech");
          if (action === "next") this.mechNextStep();
          else if (action === "prev") this.mechPrevStep();
          else if (action === "clear") this.clearMechanism();
        });
      });
    },

    // ── Stage-2b eval-gold tour ───────────────────────────────────────────
    // The honest "where hybrid wins" walk. For each gold query it reads the
    // gold chunk's rank in the FUSED leg vs the DENSE-alone leg (per-query,
    // @K=TOUR_K, attributable to fusion — NOT a per-chunk +/- color). The
    // default walk is stratified (one anchor per category, ALL categories
    // including the zero/negative ones), and the R@K-delta panel aggregates the
    // net fused-vs-dense flips per category over the full gold set.

    // Entry point: load the eval-gold set, build the stratified anchors, and
    // show anchor 1. Degrades cleanly on 503 (no eval set / daemon down) and on
    // an empty set — no crash.
    async runGoldTour() {
      if (!this.graph) return;
      // Mutually exclusive with mechanism mode.
      if (this.mechActive) this.clearMechanism();
      this.ensureMechPanel();
      this.mechSetPanelHtml(
        `<div class="mech-status">loading eval-gold set…</div>`,
      );

      let resp;
      try {
        resp = await fetch("/api/eval_gold");
      } catch (e) {
        this.mechSetPanelHtml(
          `<div class="mech-status error">eval-gold fetch error: ${escapeHtml(e.message)}</div>`,
        );
        return;
      }
      if (resp.status === 503) {
        let msg = "no eval-gold set at the served root";
        try {
          const b = await resp.text();
          if (b) {
            try {
              msg = JSON.parse(b).detail || msg;
            } catch (_e) {
              msg = b.slice(0, 240);
            }
          }
        } catch (_e) {
          /* keep default */
        }
        this.mechSetPanelHtml(
          `<div class="mech-status error">gold tour unavailable (503): ${escapeHtml(msg)}</div>`,
        );
        return;
      }
      if (!resp.ok) {
        let b = "";
        try {
          b = await resp.text();
        } catch (_e) {
          /* ignore */
        }
        this.mechSetPanelHtml(
          `<div class="mech-status error">eval-gold HTTP ${resp.status}: ${escapeHtml(b.slice(0, 200))}</div>`,
        );
        return;
      }
      let data;
      try {
        data = await resp.json();
      } catch (e) {
        this.mechSetPanelHtml(
          `<div class="mech-status error">eval-gold parse error: ${escapeHtml(e.message)}</div>`,
        );
        return;
      }

      this.tourQueries = Array.isArray(data.queries) ? data.queries : [];
      this.tourCategories = Array.isArray(data.categories)
        ? data.categories
        : [];
      this.tourResolved = data.resolved || 0;
      this.tourTotal = data.total || this.tourQueries.length;
      if (!this.tourQueries.length) {
        this.mechSetPanelHtml(
          `<div class="mech-status">eval-gold set is empty (no usable golds at the served root).</div>`,
        );
        return;
      }

      this.tourAnchors = this.buildTourAnchors();
      this.tourLegsCache = new Map();
      this.rkRows = null;
      this.rkProgress = null;
      this.rkSweeping = false;
      this.rkGen += 1; // strand any sweep left running from a prior tour session
      this.tourActive = true;
      this.tourPanelMode = "walk";
      this.tourStep = 0;
      await this.tourSetStep(0);
    },

    // One anchor per category, in the payload's canonical order, preferring a
    // showcase query when present so the walk reliably surfaces the canonical
    // wins AND the honest negative (conceptual_search). Falls back to the first
    // gold-resolved query in the category, then the first query.
    buildTourAnchors() {
      const byCat = new Map();
      for (const q of this.tourQueries) {
        if (!byCat.has(q.category)) byCat.set(q.category, []);
        byCat.get(q.category).push(q);
      }
      const order =
        this.tourCategories && this.tourCategories.length
          ? this.tourCategories
          : Array.from(byCat.keys());
      const anchors = [];
      for (const cat of order) {
        const list = byCat.get(cat);
        if (!list || !list.length) continue;
        const showcase = TOUR_SHOWCASE[cat];
        let pick = null;
        if (showcase) {
          pick = list.find((q) =>
            (q.query || "").toLowerCase().startsWith(showcase),
          );
        }
        if (!pick)
          pick = list.find((q) => (q.gold_chunk_ids || []).length > 0);
        if (!pick) pick = list[0];
        anchors.push(pick);
      }
      return anchors;
    },

    // Fetch the three legs for one query. Returns a tagged result the panel
    // renders without throwing: ok / no-fusion / daemon-down / http-error /
    // parse-error / error. Shares the daemon envelope-peel with mechanism mode.
    async fetchSearchLegs(query) {
      const q = (query || "").trim();
      if (q.length < 2) return { state: "empty" };
      let resp;
      try {
        resp = await fetch(
          `/api/search_legs?q=${encodeURIComponent(q)}&k=${TOUR_K}`,
        );
      } catch (e) {
        return { state: "error", message: e.message };
      }
      if (resp.status === 503) {
        let msg = "retrieval daemon required (cqs watch --serve)";
        try {
          const b = await resp.text();
          if (b) msg = b.slice(0, 240);
        } catch (_e) {
          /* keep default */
        }
        return { state: "daemon-down", message: msg };
      }
      if (!resp.ok) {
        let b = "";
        try {
          b = await resp.text();
        } catch (_e) {
          /* ignore */
        }
        return {
          state: "http-error",
          message: `HTTP ${resp.status}: ${b.slice(0, 160)}`,
        };
      }
      let body;
      try {
        body = await resp.json();
      } catch (e) {
        return { state: "parse-error", message: e.message };
      }
      const d = body && body.data ? body.data : body;
      const results = Array.isArray(d.results) ? d.results : [];
      if (!d.legs) return { state: "no-fusion", query: d.query || q, results };
      const legs = {
        dense: Array.isArray(d.legs.dense) ? d.legs.dense : [],
        sparse: Array.isArray(d.legs.sparse) ? d.legs.sparse : [],
        fused: Array.isArray(d.legs.fused) ? d.legs.fused : [],
      };
      return { state: "ok", query: d.query || q, legs, results };
    },

    // Memoizing legs fetch — the walk (step back/forth) and the R@K sweep share
    // it so a query is never sent twice. Only terminal states cache; a network
    // blip stays retryable.
    async cachedLegs(query) {
      if (this.tourLegsCache.has(query)) return this.tourLegsCache.get(query);
      const fetched = await this.fetchSearchLegs(query);
      if (fetched.state === "ok" || fetched.state === "no-fusion") {
        this.tourLegsCache.set(query, fetched);
      }
      return fetched;
    },

    // Move the walk to anchor `s`: dim the base, fetch its legs, recompute the
    // gold roles, pan to the gold, render the text win. Guards against a step
    // change while awaiting (the late response is dropped).
    async tourSetStep(s) {
      if (!this.tourActive || !this.tourAnchors.length) return;
      this.tourStep = Math.max(0, Math.min(this.tourAnchors.length - 1, s));
      this.tourPanelMode = "walk";
      const anchor = this.tourAnchors[this.tourStep];

      this.tourRole = this.computeTourRoles(null, anchor.gold_chunk_ids);
      if (this.graph) this.graph.refresh();
      this.renderTourWalk({ state: "loading" }, anchor);

      const fetched = await this.cachedLegs(anchor.query);
      if (!this.tourActive || this.tourAnchors[this.tourStep] !== anchor) return;

      if (fetched.state === "ok") {
        this.tourRole = this.computeTourRoles(
          fetched.legs,
          anchor.gold_chunk_ids,
        );
        if (this.graph) this.graph.refresh();
        this.focusGold(anchor.gold_chunk_ids);
      } else {
        this.tourRole = this.computeTourRoles(null, anchor.gold_chunk_ids);
        if (this.graph) this.graph.refresh();
      }
      this.renderTourWalk(fetched, anchor);
    },

    // Pan the camera to the first resolved gold that's loaded in the cluster.
    focusGold(goldIds) {
      for (const id of goldIds || []) {
        if (this.nodeIds.has(id)) {
          this.onNodeFocus(id);
          return;
        }
      }
    },

    // The honest per-query metric: the gold's rank in the dense-alone leg vs the
    // fused leg, classified by a top-TOUR_K presence FLIP.
    //   rescued = gold below TOUR_K in dense, inside top-TOUR_K after fusion
    //   hurt    = gold inside top-TOUR_K in dense, below TOUR_K after fusion
    //   neutral = no flip across the TOUR_K boundary
    //   unresolved = the gold (origin,name) didn't resolve in the served index
    // `rank == 0` means absent from the leg; the best (lowest) rank across the
    // resolved ids wins when an (origin,name) maps to more than one chunk.
    computeGoldDelta(legs, goldIds) {
      const idSet = new Set(goldIds || []);
      if (idSet.size === 0) {
        return { denseRank: 0, fusedRank: 0, classification: "unresolved" };
      }
      let denseRank = 0;
      for (const e of legs.dense) {
        if (idSet.has(e.chunk_id) && e.present_in_pool && e.rank > 0) {
          if (denseRank === 0 || e.rank < denseRank) denseRank = e.rank;
        }
      }
      let fusedRank = 0;
      for (const e of legs.fused) {
        if (idSet.has(e.chunk_id) && e.rank > 0) {
          if (fusedRank === 0 || e.rank < fusedRank) fusedRank = e.rank;
        }
      }
      const denseTop = denseRank >= 1 && denseRank <= TOUR_K;
      const fusedTop = fusedRank >= 1 && fusedRank <= TOUR_K;
      let classification;
      if (!denseTop && fusedTop) classification = "rescued";
      else if (denseTop && !fusedTop) classification = "hurt";
      else classification = "neutral";
      return { denseRank, fusedRank, classification };
    },

    // Role map for the dimmed-base highlight on one anchor: the gold (always
    // lit, top priority), the dense top neighborhood, and the fused-elevated
    // neighborhood. Pure highlight context — the win itself rides the text.
    computeTourRoles(legs, goldIds) {
      const roles = new Map();
      if (legs) {
        const denseTop = legs.dense
          .filter((e) => e.present_in_pool && e.rank > 0)
          .slice(0, MECH_TOP_K);
        for (const e of denseTop) roles.set(e.chunk_id, "dense");

        const denseRankById = new Map();
        for (const e of legs.dense) denseRankById.set(e.chunk_id, e.rank || 0);
        const fusedTop = legs.fused
          .filter((e) => e.rank > 0)
          .slice(0, MECH_TOP_K);
        for (const e of fusedTop) {
          const dr = denseRankById.get(e.chunk_id) || 0;
          const elevated = dr === 0 || e.rank < dr;
          if (elevated) roles.set(e.chunk_id, "fused");
          else if (!roles.has(e.chunk_id)) roles.set(e.chunk_id, "dense");
        }
      }
      for (const id of goldIds || []) roles.set(id, "gold");
      return roles;
    },

    // Switch the panel to the R@K-delta table; kick off the sweep on first view.
    showRkPanel() {
      if (!this.tourActive) return;
      this.tourPanelMode = "rk";
      if (this.rkRows === null && !this.rkSweeping) {
        this.runRkSweep();
      } else {
        this.renderRkPanel();
      }
    },

    // Sweep the full gold set through the daemon, aggregating the net
    // fused-vs-dense flips per category @K=TOUR_K. Sequential (one daemon
    // round-trip at a time) and incremental (re-renders as it goes). A
    // generation counter cancels a stale sweep when the user clears or restarts.
    // Aborts cleanly the moment the daemon is unreachable.
    async runRkSweep() {
      const gen = ++this.rkGen;
      this.rkSweeping = true;
      const rows = new Map();
      const ensureRow = (c) => {
        if (!rows.has(c)) {
          rows.set(c, {
            rescued: 0,
            hurt: 0,
            neutral: 0,
            unresolved: 0,
            noFusion: 0,
            error: 0,
            total: 0,
          });
        }
        return rows.get(c);
      };
      for (const c of this.tourCategories) ensureRow(c);
      this.rkRows = rows;
      this.rkProgress = {
        done: 0,
        total: this.tourQueries.length,
        daemonDown: false,
      };
      this.renderRkPanel();

      for (const q of this.tourQueries) {
        if (gen !== this.rkGen || !this.tourActive) return; // cancelled
        const row = ensureRow(q.category || "—");
        row.total += 1;
        const ids = q.gold_chunk_ids || [];
        if (ids.length === 0) {
          row.unresolved += 1;
        } else {
          const fetched = await this.cachedLegs(q.query);
          if (gen !== this.rkGen || !this.tourActive) return;
          if (fetched.state === "ok") {
            const d = this.computeGoldDelta(fetched.legs, ids);
            if (d.classification === "rescued") row.rescued += 1;
            else if (d.classification === "hurt") row.hurt += 1;
            else row.neutral += 1;
          } else if (fetched.state === "no-fusion") {
            row.noFusion += 1;
          } else if (fetched.state === "daemon-down") {
            this.rkProgress.daemonDown = true;
            this.rkSweeping = false;
            this.renderRkPanel();
            return; // no daemon — stop, keep partial counts
          } else {
            row.error += 1;
          }
        }
        this.rkProgress.done += 1;
        if (
          this.rkProgress.done % 5 === 0 ||
          this.rkProgress.done === this.rkProgress.total
        ) {
          this.renderRkPanel();
        }
      }
      this.rkSweeping = false;
      this.renderRkPanel();
    },

    // Re-bind the tour panel buttons after an innerHTML replace.
    wireTourButtons() {
      if (!this.mechPanel) return;
      this.mechPanel.querySelectorAll("[data-tour]").forEach((btn) => {
        btn.addEventListener("click", (ev) => {
          ev.stopPropagation();
          const action = btn.getAttribute("data-tour");
          if (action === "next") this.tourSetStep(this.tourStep + 1);
          else if (action === "prev") this.tourSetStep(this.tourStep - 1);
          else if (action === "clear") this.clearTour();
          else if (action === "rk") this.showRkPanel();
          else if (action === "walk") this.tourSetStep(this.tourStep);
        });
      });
    },

    // The per-anchor "walk" panel: the gold identity + the text win
    // (dense rank → fused rank, rescued/hurt/neutral), step nav, and the link to
    // the R@K-delta panel. The win is TEXT, never a per-chunk color.
    renderTourWalk(fetched, anchor) {
      this.ensureMechPanel();
      if (!this.mechPanel) return;
      const n = this.tourAnchors.length;
      const stepLabel = `anchor ${this.tourStep + 1}/${n}`;
      const cat = anchor.category || "—";
      const split = anchor.split || "";
      const ids = anchor.gold_chunk_ids || [];
      const goldName = anchor.gold_name || "?";
      const goldLoc = anchor.gold_origin || "?";

      const CLASS_LABEL = {
        rescued: { text: "rescued", sign: "+", color: MECH_COLORS.fused },
        hurt: { text: "hurt", sign: "−", color: MECH_COLORS.splade },
        neutral: { text: "no change @K", sign: "·", color: "#8591ab" },
        unresolved: { text: "gold not in index", sign: "?", color: "#8591ab" },
      };

      let winHtml;
      if (fetched.state === "loading") {
        winHtml = `<div class="mech-status">querying legs for this gold…</div>`;
      } else if (fetched.state === "daemon-down") {
        winHtml = `<div class="mech-status error">retrieval daemon required for legs (cqs watch --serve): ${escapeHtml(fetched.message || "")}</div>`;
      } else if (fetched.state === "no-fusion") {
        winHtml = `<div class="mech-status">no SPLADE fusion ran for this query — the dense↔fused win is only defined when fusion runs.</div>`;
      } else if (fetched.state !== "ok") {
        winHtml = `<div class="mech-status error">legs unavailable (${escapeHtml(fetched.state)}): ${escapeHtml(fetched.message || "")}</div>`;
      } else {
        const d = this.computeGoldDelta(fetched.legs, ids);
        const meta = CLASS_LABEL[d.classification] || CLASS_LABEL.neutral;
        winHtml =
          `<div class="tour-win" style="border-color:${meta.color}">` +
          `<span class="tour-win-num">gold: dense ${fmtRank(d.denseRank)} → fused ${fmtRank(d.fusedRank)}</span>` +
          `<span class="tour-win-tag" style="color:${meta.color}">${meta.sign} ${escapeHtml(meta.text)}</span>` +
          `</div>`;
      }

      const resolvedTag = ids.length
        ? `<span class="tour-ok">${ids.length} chunk id${ids.length > 1 ? "s" : ""}</span>`
        : `<span class="tour-dead" title="gold (origin,name) not in the served index">dead gold</span>`;

      const html =
        `<div class="mech-panel-head">` +
        `<div class="mech-title">gold tour · <span class="tour-cat">${escapeHtml(cat)}</span>${split ? ` <span class="tour-split">${escapeHtml(split)}</span>` : ""}</div>` +
        `<div class="mech-steptabs">` +
        `<button type="button" class="mech-btn" data-tour="prev"${this.tourStep === 0 ? " disabled" : ""}>‹ prev</button>` +
        `<span class="mech-stepname">${escapeHtml(stepLabel)}</span>` +
        `<button type="button" class="mech-btn" data-tour="next"${this.tourStep === n - 1 ? " disabled" : ""}>next ›</button>` +
        `<button type="button" class="mech-btn mech-clear" data-tour="clear">clear</button>` +
        `</div>` +
        `<div class="tour-query"><code>${escapeHtml(anchor.query)}</code></div>` +
        `<div class="tour-gold">gold: <strong>${escapeHtml(goldName)}</strong> <span class="mech-loc"><code>${escapeHtml(goldLoc)}</code></span> ${resolvedTag}</div>` +
        winHtml +
        `<div class="mech-legend">` +
        `<span><span class="mech-dot" style="background:${MECH_COLORS.gold}"></span>gold</span>` +
        `<span><span class="mech-dot" style="background:${MECH_COLORS.dense}"></span>dense top</span>` +
        `<span><span class="mech-dot" style="background:${MECH_COLORS.fused}"></span>fused-elevated</span>` +
        `</div>` +
        `<div class="mech-steptabs"><button type="button" class="mech-btn" data-tour="rk">R@K-delta panel ▸</button></div>` +
        `<div class="mech-note">Win = the gold's rank in the FUSED leg vs the DENSE-alone leg (per-query, @K=${TOUR_K}, attributable to fusion — not a per-chunk color). Plane = dense-cosine UMAP (locally faithful, globally distorted; NOT a metric). The walk steps through ALL categories, including where hybrid does NOT help.</div>` +
        `</div>`;

      this.mechPanel.style.display = "block";
      this.mechPanel.innerHTML = html;
      this.wireTourButtons();
    },

    // The R@K-delta panel: per-category net (rescued − hurt) @K=TOUR_K over the
    // FULL gold set — the honest "where hybrid earns its keep", explicitly
    // including the ~0 and negative categories (conceptual_search is the
    // negative one; it's in the table on purpose).
    renderRkPanel() {
      this.ensureMechPanel();
      if (!this.mechPanel) return;
      const order =
        this.tourCategories && this.tourCategories.length
          ? this.tourCategories
          : this.rkRows
            ? Array.from(this.rkRows.keys())
            : [];

      let totRes = 0;
      let totHurt = 0;
      let totUnres = 0;
      let totNoFus = 0;
      const bodyRows = order
        .map((cat) => {
          const r = (this.rkRows && this.rkRows.get(cat)) || {
            rescued: 0,
            hurt: 0,
            unresolved: 0,
            noFusion: 0,
          };
          totRes += r.rescued;
          totHurt += r.hurt;
          totUnres += r.unresolved;
          totNoFus += r.noFusion;
          const net = r.rescued - r.hurt;
          const netColor =
            net > 0
              ? MECH_COLORS.fused
              : net < 0
                ? MECH_COLORS.splade
                : "#8591ab";
          return (
            `<tr>` +
            `<td class="rk-cat">${escapeHtml(cat)}</td>` +
            `<td class="rk-num">${r.rescued}</td>` +
            `<td class="rk-num">${r.hurt}</td>` +
            `<td class="rk-net" style="color:${netColor}">${net > 0 ? "+" : ""}${net}</td>` +
            `</tr>`
          );
        })
        .join("");
      const totNet = totRes - totHurt;
      const totColor =
        totNet > 0
          ? MECH_COLORS.fused
          : totNet < 0
            ? MECH_COLORS.splade
            : "#8591ab";

      const prog = this.rkProgress || {
        done: 0,
        total: this.tourQueries.length,
        daemonDown: false,
      };
      let progLine = "";
      if (this.rkSweeping) {
        progLine = `<div class="mech-status">sweeping ${prog.done}/${prog.total} golds through the daemon…</div>`;
      } else if (prog.daemonDown) {
        progLine = `<div class="mech-status error">aborted: retrieval daemon required (cqs watch --serve). Partial counts below.</div>`;
      }

      const excl =
        totUnres || totNoFus
          ? ` (${totUnres} dead gold${totUnres === 1 ? "" : "s"}, ${totNoFus} no-fusion excluded)`
          : "";

      const html =
        `<div class="mech-panel-head">` +
        `<div class="mech-title">R@K-delta · net fused vs dense-alone @K=${TOUR_K}</div>` +
        `<div class="mech-steptabs">` +
        `<button type="button" class="mech-btn" data-tour="walk">‹ back to walk</button>` +
        `<button type="button" class="mech-btn mech-clear" data-tour="clear">clear</button>` +
        `</div>` +
        progLine +
        `<table class="rk-table"><thead><tr><th>category</th><th>resc</th><th>hurt</th><th>net</th></tr></thead><tbody>` +
        bodyRows +
        `<tr class="rk-total"><td>TOTAL</td><td class="rk-num">${totRes}</td><td class="rk-num">${totHurt}</td><td class="rk-net" style="color:${totColor}">${totNet > 0 ? "+" : ""}${totNet}</td></tr>` +
        `</tbody></table>` +
        `<div class="mech-note">net = rescued − hurt @K=${TOUR_K}. Rescue: gold below rank ${TOUR_K} in the dense-alone leg, inside top-${TOUR_K} after fusion. Hurt: the reverse. conceptual_search is the honest negative — fusion tends to hurt its gold; it's in the table on purpose.${excl}</div>` +
        `</div>`;

      this.mechPanel.style.display = "block";
      this.mechPanel.innerHTML = html;
      this.wireTourButtons();
    },

    // Leave the tour: restore the base palette, clear the panel, cancel any
    // in-flight R@K sweep (the generation bump strands it).
    clearTour() {
      this.tourActive = false;
      this.tourQueries = [];
      this.tourAnchors = [];
      this.tourStep = 0;
      this.tourRole = new Map();
      this.tourLegsCache = new Map();
      this.tourPanelMode = "walk";
      this.rkRows = null;
      this.rkProgress = null;
      this.rkSweeping = false;
      this.rkGen += 1;
      if (this.graph) this.graph.refresh();
      if (this.mechPanel) {
        this.mechPanel.style.display = "none";
        this.mechPanel.innerHTML = "";
      }
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
      // Mechanism state. The panel is a child of `container`, cleared below.
      this.mechActive = false;
      this.mechLegs = null;
      this.mechResults = [];
      this.mechQuery = "";
      this.mechStep = 0;
      this.mechRole = new Map();
      this.mechPanel = null;
      // Stage-2b tour state. The generation bump strands any in-flight R@K sweep.
      this.tourActive = false;
      this.tourQueries = [];
      this.tourAnchors = [];
      this.tourStep = 0;
      this.tourRole = new Map();
      this.tourLegsCache = new Map();
      this.tourPanelMode = "walk";
      this.rkRows = null;
      this.rkProgress = null;
      this.rkSweeping = false;
      this.rkGen += 1;
      if (this.container) {
        this.container.innerHTML = "";
      }
      this.container = null;
      this.cb = null;
    },
  };
})(window);
