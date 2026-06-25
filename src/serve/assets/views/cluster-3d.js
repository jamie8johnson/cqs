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
    dimmed: "rgba(120,130,150,0.16)", // dimmed base layer
  };

  // Number of top entries per leg used to drive the step-through highlight.
  const MECH_TOP_K = 20;

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
          // Mechanism mode overrides the base palette: dimmed base layer with
          // the active step's role chunks lit. Selection still wins so a
          // clicked chunk stays findable.
          if (this.mechActive) {
            if (this.selected === n.id) return "#fff";
            const role = this.mechRole.get(n.id);
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
          // In mech mode, inflate role chunks so the lit set reads against the
          // dimmed base; base nodes shrink so the highlight carries.
          if (this.mechActive) {
            const base = Math.pow(nodeRadius(n.callers), 2);
            return this.mechRole.has(n.id) ? base * 3.5 : base * 0.5;
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

    // Hover/label text. In mechanism mode, append the per-leg roles so the
    // tooltip carries the mechanism even before the panel is read.
    nodeLabelText(n) {
      const base = `${n.name} · ${n.kind} · ${n.language} · ${n.callers} callers`;
      if (!this.mechActive) return base;
      const role = this.mechRole.get(n.id);
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
      if (this.container) {
        this.container.innerHTML = "";
      }
      this.container = null;
      this.cb = null;
    },
  };
})(window);
