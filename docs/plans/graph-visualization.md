# Graph Visualization — `cqs serve`

Rough spec. Parked.

## Goal

Interactive web UI for exploring cqs index data — call graphs, chunk types, impact radius, test coverage, embedding clusters. Single binary, no npm, no external dependencies.

## Architecture

```
cqs serve [--port 8080] [--open]
  └── axum server (tokio, already in dep tree via sqlx)
       ├── GET /                → index.html (embedded via include_str!)
       ├── GET /api/graph       → full graph: nodes + edges
       ├── GET /api/graph?file= → subgraph for one file
       ├── GET /api/chunk/:id   → chunk detail (source, callers, callees, tests)
       ├── GET /api/impact/:fn  → impact radius subgraph
       ├── GET /api/search?q=   → search results as highlighted nodes
       ├── GET /api/dead        → dead code nodes
       ├── GET /api/stats       → index stats
       └── static/              → cytoscape.js + CSS (embedded)
```

## Stack

- **Server:** axum (Rust, zero new runtime deps — tokio already in tree)
- **Visualization:** Cytoscape.js (MIT, ~460KB bundle, 10.9K GitHub stars, 5M weekly npm downloads)
- **Layout:** dagre (hierarchical for call trees), fCoSE (fast force-directed for clusters)
- **Embedding:** All JS/CSS embedded in binary via `include_str!` / `include_bytes!`
- **WebGL:** Enable Cytoscape v3.31+ WebGL renderer for larger graphs

### Why Cytoscape.js over Sigma.js + Graphology

Evaluated both. Cytoscape wins 5/6 requirements (interactions, layouts, filtering, API, ecosystem). Sigma wins only on raw WebGL rendering. The gap is manageable:

| Axis | Cytoscape.js | Sigma.js + Graphology |
|------|-------------|----------------------|
| Rendering 11K nodes | ~3-10 FPS (WebGL preview) | 30-60 FPS (native WebGL) |
| Click/hover/select | Rich collection API, built-in | Wire everything yourself |
| Dagre hierarchical layout | First-class extension | External, no animation |
| `predecessors()` for impact | Built-in graph traversal | Manual BFS via graphology |
| Filter by attribute | CSS-like selectors | nodeReducer pattern |
| Ecosystem | 70+ plugins, active (Apr 2026) | Handful, 10mo since release |
| Bundle size | ~460KB / 142KB gzip | ~230KB / 70KB gzip |

Performance mitigations:
1. Never render all 11K at once — file-level clusters first, drill down on click
2. WebGL renderer (v3.31+) — 3-10x over canvas
3. `hideEdgesOnViewport: true` for smooth pan/zoom
4. Pre-compute layouts server-side, use `preset` layout (instant)
5. Lazy-load subgraphs via API endpoints

Fallback if Cytoscape can't keep up: graphology for data model + dagre for layout + sigma for rendering.

## Data Model

### Nodes
```json
{
  "id": "chunk_id",
  "name": "search_filtered",
  "type": "function",       // chunk type → color
  "language": "rust",
  "file": "src/search/query.rs",
  "line": 245,
  "tested": true,           // has test coverage
  "callers": 8,             // direct caller count → node size
  "dead": false             // zero callers
}
```

### Edges
```json
{
  "source": "caller_id",
  "target": "callee_id",
  "kind": "call",           // call | type_dep | test_covers
  "cross_project": false    // true if crosses project boundary
}
```

## Views

### 1. Call Graph (default)
- Force-directed layout of all chunks
- Node color = chunk type (function=blue, struct=green, test=yellow, etc.)
- Node size = caller count (more callers = bigger)
- Edge arrows show call direction
- Click node → sidebar with source, callers, callees, tests
- Filter by: chunk type, language, file, module

### 2. Impact View
- Select a function → highlight all transitive callers in red
- Depth rings show BFS distance
- Tests that cover the function highlighted in yellow
- "What breaks if I change this?" — visual answer

### 3. File/Module View
- Hierarchical layout: directory → file → chunks
- Collapse/expand modules
- Color by test coverage (green/red)

### 4. Dead Code View
- Highlight all zero-caller functions
- Filter by confidence (High/Medium/Low from `cqs dead`)

### 5. Embedding Clusters
- 2D projection of embedding space (UMAP or t-SNE, precomputed)
- Similar functions cluster together
- Hover to see function name + nearest neighbors

### 6. Cross-Project View
- Separate clusters per project, edges crossing boundaries highlighted
- Which functions are "bridges" between projects

## Interaction

- **Click** node → sidebar with chunk detail, source preview
- **Double-click** → open file in editor (`$EDITOR +line file`)
- **Hover** → tooltip with name, type, file:line
- **Search** → type query, matching nodes pulse/highlight
- **Filter** → checkboxes for chunk types, languages, tested/untested
- **Select** → click + drag to select region, show stats (N functions, M tests, etc.)

## Performance

- 11K nodes + ~50K edges is manageable for Cytoscape.js
- Lazy load: initial view shows file-level graph (~200 nodes), expand on click
- WebGL renderer (Sigma.js) as fallback if Cytoscape is too slow at scale
- Server-side filtering: `/api/graph?file=src/search/` returns subgraph

## Non-Goals (v1)

- Real-time updates (no WebSocket, manual refresh)
- Editing code from the UI
- Multi-user / authentication
- Deployment as a service (local only)

## Open Questions

- Embed JS libraries (~500KB) or fetch from CDN?
  - Embed: works offline, single binary ethos
  - CDN: smaller binary, always latest
  - Leaning embed for consistency with cqs's local-first philosophy
- UMAP/t-SNE for embedding view: precompute during `cqs index` or on-demand?
  - Precompute: ~5s for 11K points, store in SQLite
  - On-demand: slower first load but no schema change
- How to handle >50K node codebases?
  - Progressive disclosure: start with module-level, drill down
  - Server-side pagination of the graph API
