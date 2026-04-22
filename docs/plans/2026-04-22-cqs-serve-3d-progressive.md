# cqs serve — 3D rendering, progressive 4-step rollout

**Status:** Proposed
**Author:** opus 4.7 + jjohnson
**Date:** 2026-04-22
**Builds on:** `docs/plans/2026-04-21-cqs-serve-v1.md` (parent v1 spec) and the merged v2 implementation (PRs #1074 + #1075).

## Problem

The v2 2D Cytoscape view ships and works. At the 1500-node default cap on cqs's own corpus, the user can pan, search, click-to-sidebar, and follow caller/callee links. But the 2D rendering hits two ceilings:

1. **Visual disambiguation.** Dense graphs (cqs has chunks with 700+ callers) render as spaghetti even with dagre LR layout. Adjacent unrelated nodes overlap. The eye can't separate "deep clusters" from "shallow chains."
2. **Information dimensions.** A node has many useful attributes — caller count, depth in some hierarchy, embedding similarity to other nodes, last-modified time, test coverage, project membership. 2D can render two of these visually (X/Y from layout) plus color/size/alpha. We're already using up the visual budget on dagre layout (X/Y carry no semantic meaning) and per-type color.

3D adds a third spatial axis we can use for actual semantic information — the third axis BECOMES the meaning of the view. "Z = call depth" makes a 3D view that answers "how deep does this call chain go?" in a glance. "Z = caller count" makes top-of-graph load-bearing functions visually float above leaves.

Counter: 3D adds navigation cost (rotate + tilt + pan + zoom vs just pan + zoom), is bad on touch, and depth perception in screen-space is approximate. A naive "render everything in 3D because we can" would be worse UX than the 2D view we just shipped.

This spec proposes 3D as a **progressive opt-in family of views**, each with a specific reason for the third axis.

## Goal

Ship 3D rendering in cqs serve via four independently-shippable steps, each with explicit go/no-go decision criteria. Each step builds on the previous one. After step 4, the UI has 4-5 distinct views: the existing 2D call graph + 3 specialized 3D views (call hierarchy, embedding cluster, time/coverage variations). Default view stays 2D.

**Success criteria per step:**

- **Step 1:** A toggle in the header switches between 2D Cytoscape and 3D force-directed renderings of the same `/api/graph` payload, with no page reload. <2 days build cost.
- **Step 2:** A new view (`?view=hierarchy&root=...`) renders a 3D BFS subgraph with Z = call depth from a selected root. <2 days on top of step 1.
- **Step 3:** A new view (`?view=cluster`) renders embedding-cluster 3D (X/Y = UMAP, Z = caller count). Requires `cqs index --umap` index-time work. <4 days.
- **Step 4+:** Additional specialized 3D views (test coverage, time, cross-project) — each ~1 day after the renderer abstraction is in.

## Non-goals

- **Replacing 2D as default.** 2D Cytoscape stays the entry view. 3D is opt-in.
- **VR / AR / WebXR.** 3D in this spec means "3D rendered in a 2D browser viewport." No headset modes.
- **Editing in 3D.** Read-only, same as v1/v2.
- **Mobile/touch optimization.** 3D works in mobile browsers via touch-rotate but the UX is poor; not a target for this spec.
- **Live physics.** Force-directed simulation runs once on initial render and cools. No continuous re-simulation as the user drags nodes.

## Architecture overview

The current v2 frontend has Cytoscape boot logic baked directly into `app.js`. Step 1's first job is to refactor that into a renderer-abstraction pattern:

```
src/serve/assets/
├── app.js           — bootstrap: load stats, parse URL ?view=, dispatch to view module
├── views/
│   ├── callgraph-2d.js   — current Cytoscape rendering
│   ├── callgraph-3d.js   — 3d-force-graph rendering (step 1)
│   ├── hierarchy-3d.js   — Z = BFS depth (step 2)
│   ├── cluster-3d.js     — Z = caller count, X/Y = UMAP (step 3)
│   ├── coverage-3d.js    — Z = test coverage (step 4)
│   └── time-3d.js        — Z = last-modified mtime (step 4)
└── vendor/
    ├── three.min.js                  — Three.js (~600 KB)
    ├── 3d-force-graph.min.js         — vasturiano/3d-force-graph (~120 KB)
    ├── cytoscape.min.js              — existing v2
    ├── dagre.min.js                  — existing v2
    └── cytoscape-dagre.min.js        — existing v2
```

Each view module exports a single object conforming to:

```javascript
export const view = {
  // Initialize the renderer in the given container DOM element.
  // Returns a Promise that resolves when the renderer is ready to receive data.
  async init(container, options) { ... },

  // Render the given graph data. Called once after init.
  async render(graphData) { ... },

  // Optional: respond to UI events (search highlight, sidebar click, etc.)
  onSearchHighlight(matchedIds) { ... },
  onNodeFocus(chunkId) { ... },

  // Tear down — unbind listeners, dispose WebGL contexts, free memory.
  dispose() { ... },
}
```

`app.js` becomes a router: parse `?view=`, fetch the right `/api/...` endpoint, hand the payload to the view module's `render()`. Toggle buttons in the header dispatch to `dispose()` then `init()` on the new view.

This abstraction is the load-bearing piece. Once it lands, every subsequent view is a self-contained file; multi-week refactors are avoided.

## Step 1: Renderer abstraction + 3D force-directed view

**Scope:**
- Refactor `app.js` to `views/callgraph-2d.js` (current logic, no functional change) + new `app.js` router
- Add `views/callgraph-3d.js` using `3d-force-graph`
- Embed `three.min.js` + `3d-force-graph.min.js` vendor bundles (~720 KB total binary growth)
- Header gets a `[2D] [3D]` toggle (default 2D)
- URL state syncs: `?view=2d` (default) / `?view=3d`
- Both views render the same `/api/graph?max_nodes=...` payload

**3D rendering specifics:**
- Sphere nodes, color = chunk_type (same palette as 2D)
- Sphere radius: `sqrt(n_callers) × scale` (same sqrt scaling as 2D)
- Edge lines, default opacity 0.3 (denser graph, edges blend)
- Force-directed layout from 3d-force-graph's default physics
- Cooldown: stop physics after ~6 seconds (default of library is 15s — too long)
- Click → ray-cast hit test → load chunk detail (same `/api/chunk/:id` as 2D)
- Hover → ray-cast → tooltip with name + type + caller count
- Search highlight: nodes pulse via emissive color boost

**Files added/modified:**

```
src/serve/assets/app.js                       MODIFIED — becomes router
src/serve/assets/views/callgraph-2d.js        NEW (lifted from current app.js)
src/serve/assets/views/callgraph-3d.js        NEW (~250 LOC)
src/serve/assets/vendor/three.min.js          NEW (~600 KB)
src/serve/assets/vendor/3d-force-graph.min.js NEW (~120 KB)
src/serve/assets/vendor/LICENSES.md           UPDATED — Three.js MIT, 3d-force-graph MIT
src/serve/assets.rs                           MODIFIED — embed + route the new vendor files
src/serve/assets/index.html                   MODIFIED — add `<script>` tags + view toggle markup
src/serve/assets/app.css                      MODIFIED — toggle button styles
```

**Decision gate after step 1:**
- Open the toggle in your browser. Switch between 2D and 3D on the cqs corpus (1500-node default cap).
- 3D physics convergence target: <8 seconds wall.
- 3D pan/zoom/rotate target: smooth at 30 FPS+ (3d-force-graph's WebGL renderer).
- If 3D feels actively worse than 2D for general "what does this codebase look like" navigation: don't proceed past step 1. Toggle stays as a fallback for users who want to try it. No specialized views land.
- If 3D feels useful for some queries but not others: proceed to step 2.

## Step 2: Hierarchy view (Z = call depth from selected root)

**Scope:**
- New endpoint: `GET /api/hierarchy/:chunk_id?direction={callers|callees}&depth=N`
  - Performs BFS up (callers) or down (callees) from the given chunk
  - Returns nodes with an extra `bfs_depth: u32` field
  - Edges restricted to those within the BFS frontier
  - Default `depth=5`, max `depth=10`
- New view: `views/hierarchy-3d.js`
  - Same 3d-force-graph rendering BUT Z position is locked: `z = bfs_depth × spacing`
  - X/Y from force-directed within each layer (so siblings spread out)
  - Camera defaults to a "side view" (X horizontal, Z vertical = depth axis)
- New URL: `?view=hierarchy&root=<chunk_id>&direction=callers&depth=5`
- New header control: when in hierarchy view, show "depth: [3] [5] [7] [10]" buttons + "direction: ↑callers ↓callees"
- Sidebar adds a "view as hierarchy" button when a node is selected, swapping `?view=hierarchy&root=<id>`

**Backend specifics:**

```rust
// src/serve/data.rs additions
pub(crate) struct HierarchyNode {
    #[serde(flatten)]
    pub base: Node,
    pub bfs_depth: u32,
}

pub(crate) struct HierarchyResponse {
    pub root: String,
    pub direction: String,  // "callers" or "callees"
    pub max_depth: u32,
    pub nodes: Vec<HierarchyNode>,
    pub edges: Vec<Edge>,
}

pub(crate) fn build_hierarchy(
    store: &Store<ReadOnly>,
    root_id: &str,
    direction: HierarchyDirection,
    max_depth: u32,
) -> Result<HierarchyResponse, StoreError> { ... }
```

Implementation: load `Store::get_call_graph()` (already cached, returns `Arc<CallGraph>`), do BFS in-process from the root chunk's name, resolve names back to chunk IDs (same overload-disambiguation pattern as `build_graph`).

**Files added/modified:**

```
src/serve/data.rs                       MODIFIED — add HierarchyNode/Response + build_hierarchy
src/serve/handlers.rs                   MODIFIED — add hierarchy handler
src/serve/mod.rs                        MODIFIED — wire /api/hierarchy/:id route
src/serve/assets/views/hierarchy-3d.js  NEW (~200 LOC, reuses 3d-force-graph init from step 1)
src/serve/assets/index.html             MODIFIED — depth/direction controls
src/serve/assets/app.js                 MODIFIED — route ?view=hierarchy
src/serve/assets/app.css                MODIFIED — depth/direction button styles
```

**Decision gate after step 2:**
- Open `?view=hierarchy&root=<chunk_id>` for a few real chunks. Does the depth axis answer a real question ("what does this function transitively call?")? Or is it gimmicky?
- If gimmicky: ship step 2 anyway (cheap to keep), but don't proceed to step 3's UMAP work.
- If genuinely useful: proceed.

## Step 3: Embedding cluster view (X/Y = UMAP, Z = caller count)

**Scope:**
- Index-time addition: new `cqs index --umap` flag
  - Runs UMAP on existing BGE chunk embeddings to produce 2D coordinates
  - Stores in new SQLite columns `chunks.umap_x REAL` + `chunks.umap_y REAL` (schema bump v22)
  - One-time cost per reindex (~30s for 16k chunks on CPU)
  - Skipped by default; `--umap` is opt-in until v3 ships and then becomes default
- New endpoint: `GET /api/embed/2d?max_nodes=N`
  - Returns nodes with `umap_x` + `umap_y` populated (skips nodes without coords)
  - Edges optional (the cluster view typically renders no edges to reduce clutter)
- New view: `views/cluster-3d.js`
  - 3d-force-graph in "fixed positions" mode — layout disabled
  - Node positions: `(x, y, z) = (umap_x × scale, caller_count × z_scale, umap_y × scale)`
  - Caller-count Z makes "important" functions float visibly above the embedding plane
  - Color by chunk_type or language (toggle)
  - Click → sidebar (same as other views)
- New URL: `?view=cluster`

**Backend specifics:**

```rust
// New crate dep:
// umap = "0.x"  // or call out to Python via subprocess; Rust-native is preferred
// Plausible options: linfa-clustering or a hand-rolled UMAP (~500 LOC)
```

UMAP isn't trivial — the most-used Rust crate is in `linfa-clustering` (works but slow on 16k×1024). Faster alternative: shell out to `umap-learn` Python (already deployed via vLLM stack). Simplest for v1 of step 3: shell out to `umap-learn` if `--umap` flag set; cache result in SQLite columns. Document the Python dep.

```sql
-- v22 schema migration
ALTER TABLE chunks ADD COLUMN umap_x REAL;
ALTER TABLE chunks ADD COLUMN umap_y REAL;
```

**Files added/modified:**

```
src/cli/commands/index/build.rs        MODIFIED — add --umap flag, post-embedding UMAP step
src/store/schema.sql                   MODIFIED — v22 migration adds umap_x/umap_y columns
src/store/migrations.rs                MODIFIED — register v22
evals/scripts/run_umap.py              NEW — UMAP script (Python via subprocess)
src/serve/data.rs                      MODIFIED — add build_cluster, EmbedResponse
src/serve/handlers.rs                  MODIFIED — add /api/embed/2d handler
src/serve/mod.rs                       MODIFIED — wire route
src/serve/assets/views/cluster-3d.js   NEW (~150 LOC)
src/serve/assets/app.js                MODIFIED — route ?view=cluster
docs/plans/2026-04-21-cqs-serve-v1.md  MODIFIED — mark embedding-cluster view as v3 done
```

**Decision gate after step 3:**
- Open `?view=cluster` after running `cqs index --umap`. Do similar functions actually cluster together visibly? (They should; UMAP on BGE embeddings of code does cluster by semantics.)
- Are the caller-count "spires" (high-Z nodes) the actually-important code paths in your project? Or noise?
- If clusters are uninteresting or spires are arbitrary: drop the view but keep the UMAP coords (other views may use them).
- If revealing: step 4 becomes attractive.

## Step 4: Variations (test coverage, time, cross-project)

**Scope:** each of these is ~1 day on top of steps 1-3:

### Test-coverage view (`?view=coverage`)
- Z-axis: test coverage % per chunk (proxy: count of tests-that-cover divided by some normalization)
- High-Z = well-tested (anchored, green)
- Low-Z = untested (floating, red)
- Reuses force-directed X/Y for clustering
- Surfaces the "what's our untested risk" question visually

### Time view (`?view=time`)
- Z-axis: last-modified mtime (or git-blame age) per chunk
- Recent activity rises; old code sinks
- Color overlay: change frequency from git log (heat map)
- Surfaces "what's actively churning" vs "stable substrate"

### Cross-project view (`?view=cross`)
- Z-axis: project (each `[[reference]]` index gets its own layer)
- X/Y from force-directed within each layer
- Bridge edges between layers visually highlighted
- Reuses `Store::CrossProjectContext` data

### Each view requires:
- A new `views/{name}-3d.js` (~150 LOC)
- A new endpoint OR an existing endpoint with a `?z_axis=` parameter
- A header button or URL to dispatch
- Documentation in the spec's "future work" section as it lands

**Decision gate after step 4:**
- Each view is independent. Skip any that don't fit.
- Aim: at least 2 of the 3 (test coverage + time, cross-project deferred to a separate PR if multi-project setup is rare).

## Decision gates summary

| After | Question | Yes path | No path |
|---|---|---|---|
| Step 1 | Does the 2D↔3D toggle feel useful for any queries? | Proceed to step 2 | Stop. Toggle stays as a fallback. |
| Step 2 | Does Z = call depth answer a real question? | Proceed to step 3 | Ship step 2, skip step 3+. |
| Step 3 | Do UMAP clusters reveal real structure? | Proceed to step 4 | Ship step 3, skip step 4. |
| Step 4 | Does each new view earn its keep? | Per-view decision | Drop the ones that don't. |

Each step ships independently as its own PR. Each is reversible by reverting that PR.

## Performance plan

- **Vendor bundle size budget:** ~720 KB added in step 1 (Three.js + 3d-force-graph). Steps 2-4 reuse those bundles, no further binary growth.
- **3D render target:** 30 FPS+ pan/zoom/rotate at 1500 nodes (3d-force-graph default cap mirrors 2D).
- **Physics convergence:** ≤8s on initial render at 1500 nodes (force-graph cooldown tuned).
- **Hierarchy depth cap:** 10 layers max — beyond that the BFS becomes the cost driver, not rendering.
- **Cluster view node cap:** UMAP coords are pre-computed, no physics → can render 16k nodes directly. Test on full corpus.
- **Server-side caching:** `/api/hierarchy/:id` results cached per-chunk-per-direction-per-depth in-memory for the daemon's lifetime (LRU, 256 entries).

## Open questions

1. **Renderer abstraction interface — `init/render/dispose` only, or richer?** Step 1 starts minimal. If steps 2-4 need cross-view state (e.g., "remember last selected node when switching views"), expand the interface then.
2. **Should the toggle be in the URL or just header state?** URL means shareable links — `cqs serve` + URL shared via Slack works. Default to URL state. Header just reflects URL.
3. **3d-force-graph vs custom Three.js.** Library is well-maintained but locks us into its physics + interaction model. For step 4's specialized layouts (Z = mtime, etc.) we may need raw Three.js anyway. Plan: use 3d-force-graph for step 1 + 2 (force-directed with optional Z lock), drop to raw Three.js if step 3+ requires more layout control.
4. **UMAP in Rust vs Python subprocess.** Python is faster to ship (umap-learn already in the vLLM env) but adds a Python runtime dep to anyone running `cqs index --umap`. Rust-native (linfa-clustering) is slower (~3 min on 16k vs Python's ~30s) but pure Rust. Spec'd as Python first; revisit if cqs grows external users who want pure-Rust install.
5. **Schema bump (v22) for UMAP coords.** Migration is additive (two new REAL columns); no risk to existing indexes. Roll into step 3's PR.
6. **What if 3d-force-graph chokes at 16k nodes?** Library claims 60FPS for ~10k nodes. We're testing at 1500 default. Step 1's cap stays. If users push higher via `?max=NNNN`, document the soft cap as 5k for 3D vs 16k for 2D-cluster (which has no physics).
7. **Mobile/touch UX.** 3D rotation via touch is awkward but works. Don't optimize for it; users on mobile probably want 2D anyway. Detect via `'ontouchstart' in window` and default to 2D.
8. **Sidebar interaction during 3D.** Same `/api/chunk/:id` payload, same sidebar HTML. The link-follow behavior in callers/callees lists needs to dispatch to the active renderer's `onNodeFocus(id)` — the renderer abstraction handles this.

## Future work

- **Time-of-day animation** — slider in header that scrubs git history; nodes appear/disappear based on `created_at` / `deleted_at` mtime. "How did this codebase grow?"
- **Search-result trace** — given a query, animate a path through 3D space showing top-K matches in order. "Why this result, why now."
- **Diff overlay** — render two graphs (HEAD vs HEAD~N) overlaid in 3D with delta visualization. "What changed since last week?"
- **VR/AR/WebXR** — explicit non-goal here, but the renderer abstraction doesn't preclude a future `views/callgraph-vr.js` if WebXR matures.
- **Plot exports** — "snapshot to PNG" for sharing screenshots. Trivial via Three.js render-to-canvas.

---

## Concrete action plan

If approved:

1. **Step 1 PR:** `feat(serve): renderer abstraction + 3D force-directed view`. ~2 days. Lands as opt-in via `?view=3d`.
2. **Decision gate** — open in browser, judge.
3. **Step 2 PR:** `feat(serve): hierarchy 3D view (Z = call depth)`. ~2 days. Lands as `?view=hierarchy&root=...`.
4. **Decision gate** — judge.
5. **Step 3 PR:** `feat(serve+index): UMAP cluster view (Z = caller count)`. ~3-4 days. Lands as `?view=cluster` + `cqs index --umap` flag.
6. **Decision gate** — judge.
7. **Step 4 PRs:** one per variation (`coverage`, `time`, `cross`). ~1 day each. Each independent.

Each PR is reviewed + merged before the next is opened. If you want to parallelize, step 1's renderer abstraction MUST land first; steps 2-4 can run in parallel after that.

Total wall time for the full path: ~10-12 days of focused work spread over however many sessions, with go/no-go gates between each.
