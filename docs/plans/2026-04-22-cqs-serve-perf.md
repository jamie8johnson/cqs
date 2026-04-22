# cqs serve — first-paint + responsiveness pass

Status: spec
Date: 2026-04-22
Owner: jamie

## Problem

On the cqs corpus (~16k chunks, ~53k call edges), opening `cqs serve` in a
browser takes ~60 seconds before the graph is visible, and the UI stays
laggy after first paint. The graph is technically there but you can't
work in it.

The plan in `2026-04-22-cqs-serve-3d-progressive.md` shipped function;
this one ships the speed.

## Root causes

Profiled by reading the code path; numbers below are estimates against the
cqs corpus. Anyone pulling exact numbers should `console.time` each phase
in the browser and add a `tracing::info!` per query in `data::build_graph`.

| Phase | Where | Est. cost | Notes |
|---|---|---|---|
| 1 | `data::build_graph` SQL+rebuild | ~5-15s | Pulls all 16k chunks + all 53k edges, builds hashmaps, computes degrees, **then** truncates to 1500. The cap is applied last; the work is done first. |
| 2 | JSON serialize + wire transfer | ~1-3s | ~1-2 MB JSON, uncompressed, on localhost still adds up. |
| 3 | JSON parse + Cytoscape `add()` | ~1-2s | Main-thread blocking; Cytoscape's element construction is O(V+E). |
| 4 | dagre layout | ~30-45s | The killer. Dagre is hierarchical, ~O(V·E·log V) in practice; 1500 nodes + 4-5k edges is firmly outside its comfort zone. Runs on the main thread. |
| 5 | First Cytoscape paint | ~1-2s | Once layout is done, the canvas paint itself is fast. |
| 6 | Lazy interactivity | continuous | Cytoscape continues to recompute layout on small changes; pan/zoom is fluid only after initial settle. |

The 3D path is faster (3d-force-graph layouts are GPU-accelerated and
incremental) but still pays phases 1, 2, 3 in full, and pays an extra
~600 KB for Three.js + ~600 KB for 3d-force-graph regardless of whether
you wanted 3D.

## Targets

- **First useful paint < 3s** for the cqs-sized corpus.
- **Interactive (pan/zoom, click works) < 5s.**
- **2D-only sessions never download the 3D bundle.**

## Five changes, ordered by impact-to-effort

### 1. Push `max_nodes` into SQL

Today `build_graph` SELECTs every chunk, builds the in-memory map, then
truncates after computing degrees. Inverting the order:

```sql
-- Rank chunks by caller count first (computed on the fly from
-- function_calls), keep top N, then fetch those chunks + only the edges
-- whose endpoints both survive.
WITH caller_counts AS (
    SELECT c.id, COUNT(fc.id) AS n_callers
    FROM chunks c
    LEFT JOIN function_calls fc ON fc.callee_name = c.name
    GROUP BY c.id
),
top_chunks AS (
    SELECT c.*, cc.n_callers
    FROM chunks c
    JOIN caller_counts cc ON cc.id = c.id
    WHERE 1=1  -- file/kind filters splice in here
    ORDER BY cc.n_callers DESC, c.id
    LIMIT ?
)
SELECT * FROM top_chunks;
```

Then a second query for edges restricted to the top set's `(file, name)`
tuples. Backend cost goes from "process the whole corpus" to "process N".
Also cuts response size by the same ratio.

**Estimated win:** phase 1 from 5-15s → 200-500ms at N=1500.

**Files:**
- `src/serve/data.rs` — rewrite `build_graph` with the prerank query
- `src/serve/tests.rs` — add a test asserting the response respects the SQL-level cap

### 2. Lower default `max_nodes`, switch to a faster default layout

Even with backend speed fixed, dagre on 1500 nodes is ~30s of pure JS.
Two independent fixes:

- **Default `?max=300` instead of 1500.** The user can broaden via URL
  (`?max=2000`). 300 nodes is enough to navigate by visual landmarks; the
  rest is reachable via search + click-to-focus.
- **Switch the 2D layout to `fcose` (force-directed)** or, if we don't
  want the dependency, `cose` (built-in to Cytoscape). Both produce a
  layout in <2s on 300 nodes and animate into place — perceived
  responsiveness is dramatically better than waiting on a final dagre
  result.

Dagre stays available behind `?layout=dagre` for users who want the
hierarchical view (it's correct, just slow).

**Estimated win:** phase 4 from 30-45s → 1-2s.

**Files:**
- `src/serve/assets/views/callgraph-2d.js` — accept layout name from URL,
  default to `fcose` (or `cose`), keep `dagre` as opt-in
- `src/serve/assets/app.js` — change `MAX_NODES` default 1500 → 300
- `src/serve/assets/vendor/cytoscape-fcose.min.js` — vendor it (~50 KB)
  if we go that route. Skip if `cose` is good enough.

### 3. Lazy-load the 3D bundle

`index.html` currently includes `three.min.js` + `3d-force-graph.min.js`
unconditionally. ~1.2 MB the 2D-only user pays for. Move them out of the
HTML, inject via `<script>` when the user actually clicks the 3D or
hierarchy or cluster toggle:

```javascript
let threeBundleLoaded = null;
function ensureThreeBundle() {
  if (threeBundleLoaded) return threeBundleLoaded;
  threeBundleLoaded = Promise.all([
    loadScript("/static/vendor/three.min.js"),
    loadScript("/static/vendor/3d-force-graph.min.js"),
  ]);
  return threeBundleLoaded;
}
```

Each 3D view module's `init()` `await`s `ensureThreeBundle()` before
checking `typeof ForceGraph3D`.

**Estimated win:** ~1.2 MB off initial page load → ~300 ms saved on
typical local serve, far more on slow networks.

**Files:**
- `src/serve/assets/index.html` — drop the two 3D `<script>` tags
- `src/serve/assets/app.js` — add `loadScript()` helper
- `src/serve/assets/views/callgraph-3d.js`,
  `views/hierarchy-3d.js`,
  `views/cluster-3d.js` — `await ensureThreeBundle()` at the top of `init`

### 4. gzip middleware

axum + tower-http makes this trivial. The graph payload is ~1-2 MB JSON;
gzip cuts it to ~150-300 KB. Loopback bandwidth is fine but compression
also reduces parse time at the browser end (less data to decode before
JSON.parse can start).

```rust
use tower_http::compression::CompressionLayer;
let app = build_router(state).layer(CompressionLayer::new());
```

**Estimated win:** phase 2 from 1-3s → 200-500ms.

**Files:**
- `Cargo.toml` — bump `tower-http` features to include `compression-gzip`
- `src/serve/mod.rs` — wrap router

### 5. Web Worker for JSON parse + element transform

Phase 3 (~1-2s) is small in absolute terms but it's main-thread blocking.
A worker that fetches `/api/graph`, parses JSON, transforms into
Cytoscape's element format, and posts the result back removes the freeze
window where the page is unresponsive.

This is the lowest-priority of the five — only worth doing if items 1-4
land and the parse phase is still a visible jank source.

**Files:**
- `src/serve/assets/workers/graph-loader.js` (NEW)
- `src/serve/assets.rs` — embed + serve the worker file
- `src/serve/assets/app.js` — `new Worker("/static/workers/graph-loader.js")`,
  `postMessage`, await `onmessage`

## Out of scope

- **Server-sent events / streaming partial graphs.** Tempting (start
  drawing the top-50 nodes immediately, fill in the rest as they arrive)
  but this is a redesign of the data contract and the layout pipeline.
  Park until items 1-5 prove insufficient.
- **Cluster-view perf.** It's already fixed-position (no force layout) so
  it should be the fastest of the three views once items 1+4 land. If
  it's still slow, profile and add to scope then.
- **Hierarchy-view perf.** Already bounded by `depth` (≤10) and the BFS
  frontier; should fall well inside the 3-second target without changes.
- **HTTP/2, TLS, anything network-shaped.** Loopback only.

## Decision gate

After items 1-4 land, profile again on the cqs corpus. If first paint
is < 3s and interactive < 5s, **stop**. Don't ship item 5 unless the
worker actually removes a visible freeze.

## File touch summary

| Item | Files |
|---|---|
| 1. SQL-side cap | `src/serve/data.rs`, `src/serve/tests.rs` |
| 2. Default + layout | `src/serve/assets/app.js`, `views/callgraph-2d.js`, optionally `assets/vendor/cytoscape-fcose.min.js` + `src/serve/assets.rs` |
| 3. Lazy 3D | `src/serve/assets/index.html`, `src/serve/assets/app.js`, all three 3D view modules |
| 4. gzip | `Cargo.toml`, `src/serve/mod.rs` |
| 5. Worker (gated) | `src/serve/assets/workers/graph-loader.js`, `src/serve/assets.rs`, `src/serve/assets/app.js` |

Plus a new test or two per item, and a `tracing::info!` per backend phase
so the next round of profiling is data, not guesses.
