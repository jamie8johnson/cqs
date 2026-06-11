//! Wire-format types for `/api/*` endpoints.
//!
//! Frontend Cytoscape.js consumes the `Node` + `Edge` shapes directly.
//! Field names match Cytoscape's element-data convention so the JS can
//! pass the rows through without transformation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::store::serve_queries::NeighborRow;
use crate::store::{ReadOnly, Store, StoreError};

/// Clamp an i64 SQL line number / count to u32, warning once if the input
/// was negative.
///
/// Negative values in `chunks.line_start` / `line_end` / `n_callers`-style
/// columns signal DB corruption or a migration bug — the schema is `NOT NULL`
/// `INTEGER` and our writers always store non-negative values. Surfacing the
/// clamp as a warn lets operators correlate downstream weirdness with the
/// underlying data issue instead of silently masking it as `0`.
#[inline]
fn clamp_line_to_u32(v: i64) -> u32 {
    if v < 0 {
        tracing::warn!(value = v, "negative SQL line/count clamped to 0");
        0
    } else {
        v.min(u32::MAX as i64) as u32
    }
}

// Absolute ceilings on response shapes — see `crate::limits::serve_*`.
//
// Operators tune via env vars (`CQS_SERVE_GRAPH_MAX_NODES`,
// `CQS_SERVE_GRAPH_MAX_EDGES`, `CQS_SERVE_CLUSTER_MAX_NODES`,
// `CQS_SERVE_CHUNK_DETAIL_{CALLERS,CALLEES,TESTS}`). The helpers in `limits.rs`
// clamp to a hard maximum so a misconfiguration can't unbound the response.
//
// Each helper reads its env var on every call. The cost is negligible
// (one `getenv`) and lets tests flip values inside a single process.

/// One node in the call graph. Cytoscape renders one of these per
/// chunk (or per windowed-chunk row, since each window has its own
/// embedding + identity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Node {
    /// Stable chunk ID — used as Cytoscape `data.id`.
    pub id: String,
    /// Display name (function name, struct name, etc.).
    pub name: String,
    /// Chunk type (function, method, struct, impl, ...). Drives
    /// node color via CSS class.
    #[serde(rename = "type")]
    pub kind: String,
    /// Source language (`rust`, `python`, ...). Drives optional
    /// per-language CSS rules.
    pub language: String,
    /// File path relative to project root.
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    /// Number of incoming call edges. Drives node size (sqrt scaling).
    pub n_callers: u32,
    /// Number of outgoing call edges.
    pub n_callees: u32,
    /// True if the chunk has zero callers and zero tests covering it
    /// — flagged with a red ring + opacity drop in the UI.
    pub dead: bool,
}

/// One edge in the call graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Edge {
    /// Source chunk ID.
    pub source: String,
    /// Target chunk ID.
    pub target: String,
    /// `call` for callee edges, `type_dep` for type dependencies.
    /// v1 only emits `call`; `type_dep` is wired here for future
    /// view-mode toggles.
    pub kind: String,
}

/// Top-level response for `GET /api/graph`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GraphResponse {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Detail payload for the click-sidebar (`GET /api/chunk/:id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChunkDetail {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub language: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    /// `Option` so a NULL `chunks.signature` (partial write, SIGKILL between
    /// INSERT phases) reaches the frontend as `null` rather than collapsing to
    /// `""`. The frontend renders missing columns as a
    /// `<missing — DB column NULL>` placeholder, distinct from a
    /// successfully-extracted void signature.
    pub signature: Option<String>,
    pub doc: Option<String>,
    /// First N lines of the chunk content for inline preview. `None`
    /// when the underlying `chunks.content` column is NULL — same
    /// reasoning as `signature`.
    pub content_preview: Option<String>,
    /// SECURITY.md trust signal. Skip-when-default (`"user-code"`): only
    /// serialized when the chunk is vendored (`"vendored-code"`), matching
    /// the search serializer + graph kind-fallback convention. Serve serves
    /// a single project store with no `cqs ref` references, so the
    /// `"reference-code"` tier never applies here. Default is `None` so a
    /// user-code chunk emits no `trust_level` key at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,
    /// SECURITY.md trust signal. Empty (skip-when-default) when no injection
    /// heuristic fired. Detection runs on the **full** content before preview
    /// truncation so a directive past the 30-line cap still flags.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub injection_flags: Vec<String>,
    pub callers: Vec<NodeRef>,
    pub callees: Vec<NodeRef>,
    pub tests: Vec<NodeRef>,
}

/// Compact reference used in caller/callee/tests lists. Just enough
/// to render a clickable link in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NodeRef {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line_start: u32,
}

/// Response for `GET /api/search?q=...`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SearchResponse {
    pub matches: Vec<NodeRef>,
}

/// Response for `GET /api/stats`. Mirrors a small subset of `cqs stats`
/// for the header bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StatsResponse {
    pub total_chunks: u64,
    pub total_files: u64,
    pub call_edges: u64,
    pub type_edges: u64,
}

/// Direction of BFS expansion for the hierarchy view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HierarchyDirection {
    /// BFS up the call graph (this function's transitive callers).
    Callers,
    /// BFS down the call graph (this function's transitive callees).
    Callees,
}

impl HierarchyDirection {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "callers" => Some(Self::Callers),
            "callees" => Some(Self::Callees),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Callers => "callers",
            Self::Callees => "callees",
        }
    }
}

/// One node in the hierarchy view — same as a graph `Node` plus the
/// BFS depth from the root (root itself is depth 0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HierarchyNode {
    #[serde(flatten)]
    pub base: Node,
    pub bfs_depth: u32,
}

/// Top-level response for `GET /api/hierarchy/:id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HierarchyResponse {
    pub root: String,
    pub direction: String,
    pub max_depth: u32,
    pub nodes: Vec<HierarchyNode>,
    pub edges: Vec<Edge>,
}

/// One node in the embedding cluster view — same metadata as a graph
/// `Node` plus the 2D UMAP coordinates. The frontend places the node at
/// `(umap_x × scale, n_callers × z_scale, umap_y × scale)` so semantically
/// similar chunks cluster together in the X/Z plane and high-degree
/// functions float visibly above.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ClusterNode {
    #[serde(flatten)]
    pub base: Node,
    pub umap_x: f64,
    pub umap_y: f64,
}

/// Response for `GET /api/embed/2d`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ClusterResponse {
    pub nodes: Vec<ClusterNode>,
    /// Chunks that exist but lack UMAP coords (NULL `umap_x` / `umap_y`).
    /// Frontend uses this to surface a "run `cqs index --umap`" hint when
    /// the cluster view boots against a corpus that hasn't been projected.
    pub skipped: u64,
}

/// Build the graph response from the store.
///
/// Always capped: preranks chunks by global caller count in SQL, fetches
/// only the top N + only the edges whose endpoints touch that name set.
/// When the caller passes `max_nodes = None`, `crate::limits::serve_graph_max_nodes()`
/// is substituted; when they pass a value larger than the hard ceiling, the
/// value is clamped. SEC-3 closes the DoS vector of a single unauth
/// request materialising millions of chunk rows into memory.
///
/// The per-node `n_callers`/`n_callees`/`dead` fields reflect the GLOBAL
/// degree of the chunk (so node sizing on the cap'd response still
/// represents real importance, not just visible-edge degree).
pub(crate) fn build_graph(
    store: &Store<ReadOnly>,
    file_filter: Option<&str>,
    kind_filter: Option<&str>,
    max_nodes: Option<usize>,
) -> Result<GraphResponse, StoreError> {
    let _span = tracing::info_span!(
        "build_graph",
        file_filter = ?file_filter,
        kind_filter = ?kind_filter,
        max_nodes = ?max_nodes,
    )
    .entered();

    // 1. Chunk fetch.
    //
    // SEC-3: always bind an effective cap. When the client omits
    // `?max_nodes`, fall back to `serve_graph_max_nodes()` so a
    // single request can't materialise a million chunks into
    // memory. The user-supplied value is clamped too so
    // `?max_nodes=999999999` can't be used as a DoS vector either.
    // (Env-tunable via `CQS_SERVE_GRAPH_MAX_NODES`; clamped to a
    // 1M hard ceiling — see `crate::limits`.)
    let max_graph_nodes = crate::limits::serve_graph_max_nodes();
    let effective_cap = max_nodes.unwrap_or(max_graph_nodes).min(max_graph_nodes);
    let node_rows = store.serve_graph_nodes(file_filter, kind_filter, effective_cap)?;

    let mut nodes_by_id: HashMap<String, Node> = HashMap::with_capacity(node_rows.len());
    let mut name_to_ids: HashMap<String, Vec<String>> = HashMap::new();
    // For the capped path we already have global n_callers from SQL.
    // Cache it so the post-resolution loop can pick it up — degree
    // counts derived purely from the visible edge set would understate
    // importance for nodes whose call edges point to chunks outside
    // the capped window.
    let mut prelim_n_callers: HashMap<String, u32> = HashMap::new();
    for row in node_rows {
        let id = row.id;
        let name = row.name;
        prelim_n_callers.insert(id.clone(), clamp_line_to_u32(row.n_callers_global));
        nodes_by_id.insert(
            id.clone(),
            Node {
                id: id.clone(),
                name: name.clone(),
                kind: row.chunk_type,
                language: row.language,
                file: row.origin,
                line_start: clamp_line_to_u32(row.line_start),
                line_end: clamp_line_to_u32(row.line_end),
                n_callers: 0,
                n_callees: 0,
                dead: false,
            },
        );
        name_to_ids.entry(name).or_default().push(id);
    }

    // 2. Edge fetch.
    //
    // SEC-3: always use the name-scoped edge fetch and always bind
    // a hard LIMIT. The previous uncapped branch (`SELECT fc.*`)
    // would return the entire function_calls table (tens of
    // millions of rows on a large monorepo); the IN-scoped query
    // could also blow up if the visible-node name set grew large,
    // so `serve_graph_max_edges()` caps it unconditionally.
    // (Env-tunable via `CQS_SERVE_GRAPH_MAX_EDGES`.)
    let max_graph_edges = crate::limits::serve_graph_max_edges();
    let edge_tuples: Vec<(String, String, String)> = {
        let mut name_set: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for node in nodes_by_id.values() {
            name_set.insert(node.name.as_str());
        }
        if name_set.is_empty() {
            Vec::new()
        } else {
            let names: Vec<&str> = name_set.into_iter().collect();
            store.serve_graph_edges(&names, max_graph_edges)?
        }
    };

    // 3. Resolve edges. Same overload-disambiguation pattern as before:
    //    caller resolves by (file, name) → first chunk_id by sort;
    //    callee resolves by name only → first chunk_id by sort. Edges
    //    whose endpoints don't both resolve into our visible set are
    //    silently dropped.
    let mut origin_name_to_id: HashMap<(String, String), Vec<String>> = HashMap::new();
    for node in nodes_by_id.values() {
        origin_name_to_id
            .entry((node.file.clone(), node.name.clone()))
            .or_default()
            .push(node.id.clone());
    }

    let mut edges = Vec::with_capacity(edge_tuples.len());
    let mut caller_count: HashMap<String, u32> = HashMap::new();
    let mut callee_count: HashMap<String, u32> = HashMap::new();
    for (file, caller_name, callee_name) in edge_tuples {
        let Some(callers) = origin_name_to_id.get(&(file, caller_name)) else {
            continue;
        };
        let caller_id = match callers.as_slice() {
            [] => continue,
            [single] => single.clone(),
            multiple => multiple[0].clone(),
        };

        let Some(callee_ids) = name_to_ids.get(&callee_name) else {
            continue;
        };
        let callee_id = match callee_ids.as_slice() {
            [] => continue,
            [single] => single.clone(),
            multiple => multiple[0].clone(),
        };

        *caller_count.entry(callee_id.clone()).or_insert(0) += 1;
        *callee_count.entry(caller_id.clone()).or_insert(0) += 1;

        edges.push(Edge {
            source: caller_id,
            target: callee_id,
            kind: "call".to_string(),
        });
    }

    // 4. Populate per-node degree + dead flag.
    //
    // n_callers comes from the SQL prelim count (global); n_callees
    // comes from the resolved-edge count (visible). Reasoning:
    // importance lives in n_callers (drives node size); the visible
    // n_callees is only the calls inside the window. Showing global
    // n_callees would mislead on the cap'd graph because the listed
    // callees might not exist in the visible set.
    for (id, node) in nodes_by_id.iter_mut() {
        node.n_callers = *prelim_n_callers.get(id).unwrap_or(&0);
        node.n_callees = *callee_count.get(id).unwrap_or(&0);
        node.dead = node.n_callers == 0 && node.kind != "test";
    }

    // 5. Drop edges whose endpoints didn't both land in the visible
    //    set. SEC-3 always caps at `serve_graph_max_nodes()`, so this
    //    prune is always meaningful.
    let mut nodes: Vec<Node> = nodes_by_id.into_values().collect();
    let kept: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    edges.retain(|e| kept.contains(e.source.as_str()) && kept.contains(e.target.as_str()));
    // Stable response order: by descending caller count, ties by id.
    nodes.sort_unstable_by(|a, b| b.n_callers.cmp(&a.n_callers).then_with(|| a.id.cmp(&b.id)));

    tracing::info!(
        nodes = nodes.len(),
        edges = edges.len(),
        "build_graph: built response"
    );

    Ok(GraphResponse { nodes, edges })
}

/// Build the chunk-detail response for one chunk_id.
///
/// Pulls the chunk metadata + first 30 lines of content + caller/callee
/// chunk lists + tests-that-cover. Returns None for unknown IDs.
pub(crate) fn build_chunk_detail(
    store: &Store<ReadOnly>,
    chunk_id: &str,
) -> Result<Option<ChunkDetail>, StoreError> {
    let _span = tracing::info_span!("build_chunk_detail", chunk_id = %chunk_id).entered();

    // Fetch the chunk row.
    let Some(row) = store.serve_chunk_detail_row(chunk_id)? else {
        return Ok(None);
    };

    let id = row.id;
    let name = row.name;
    let chunk_type = row.chunk_type;
    let language = row.language;
    let origin = row.origin;
    let line_start = row.line_start;
    let line_end = row.line_end;
    // NULL is a real signal (partial write during indexing, SIGKILL
    // between INSERT phases) — preserve it through to the wire format
    // rather than flattening to `""`.
    let signature = row.signature;
    let doc = row.doc;
    let content = row.content;
    let vendored = row.vendored;

    // SECURITY.md trust signals. Detection runs on the FULL content
    // before the preview truncation below, so a directive past the
    // 30-line cap still surfaces in `injection_flags`. Empty content
    // (NULL column) yields no flags.
    let injection_flags: Vec<String> = content
        .as_deref()
        .map(crate::llm::validation::detect_all_injection_patterns)
        .unwrap_or_default()
        .into_iter()
        .map(String::from)
        .collect();
    // Skip-when-default trust_level: serve serves a single project store
    // with no `cqs ref` references, so the only non-default tier is the
    // vendored downgrade. `None` (user-code) is skipped on the wire.
    let trust_level: Option<String> = if vendored {
        Some("vendored-code".to_string())
    } else {
        None
    };

    // Preview = first 30 lines of content. Bounded so big chunks
    // don't bloat the sidebar JSON. `None` when the row had NULL
    // content.
    let content_preview: Option<String> = content
        .as_deref()
        .map(|c| c.lines().take(30).collect::<Vec<_>>().join("\n"));

    // Surface out-of-range `line_start` (negative or overflows u32)
    // as a `StoreError::Corruption` rather than silently clamping to 0.
    // A corrupted row here manifests in the UI as a mis-scrolled
    // sidebar; without the explicit
    // error the regression has no diagnostic at all. The error
    // propagates through the axum handler as a 500.
    let to_noderef = |r: NeighborRow| -> Result<NodeRef, StoreError> {
        let raw: i64 = r.line_start;
        let id: String = r.id;
        let line_start = u32::try_from(raw).map_err(|_| {
            StoreError::Corruption(format!("chunk {id} has out-of-range line_start {raw}"))
        })?;
        Ok(NodeRef {
            name: r.name,
            file: r.origin,
            id,
            line_start,
        })
    };

    // Caller chunks: function_calls WHERE callee_name = this.name.
    // LIMIT bound to `serve_chunk_detail_callers_limit()`
    // (env `CQS_SERVE_CHUNK_DETAIL_CALLERS`, default 50).
    let callers_limit = crate::limits::serve_chunk_detail_callers_limit() as i64;
    let callers: Vec<NodeRef> = store
        .serve_chunk_detail_callers(&name, callers_limit)?
        .into_iter()
        .map(&to_noderef)
        .collect::<Result<_, _>>()?;

    // Callee chunks: function_calls WHERE caller_name = this.name AND file = this.origin.
    // LIMIT bound to `serve_chunk_detail_callees_limit()`
    // (env `CQS_SERVE_CHUNK_DETAIL_CALLEES`, default 50).
    let callees_limit = crate::limits::serve_chunk_detail_callees_limit() as i64;
    let callees: Vec<NodeRef> = store
        .serve_chunk_detail_callees(&name, &origin, callees_limit)?
        .into_iter()
        .map(&to_noderef)
        .collect::<Result<_, _>>()?;

    // Tests-that-cover: heuristic — chunks whose chunk_type = 'test'
    // and whose content references this name. Cheap LIKE search.
    // LIMIT bound to `serve_chunk_detail_tests_limit()`
    // (env `CQS_SERVE_CHUNK_DETAIL_TESTS`, default 20).
    let tests_limit = crate::limits::serve_chunk_detail_tests_limit() as i64;
    let tests: Vec<NodeRef> = store
        .serve_chunk_detail_tests(&name, tests_limit)?
        .into_iter()
        .map(&to_noderef)
        .collect::<Result<_, _>>()?;

    Ok(Some(ChunkDetail {
        id,
        name,
        kind: chunk_type,
        language,
        file: origin,
        line_start: clamp_line_to_u32(line_start),
        line_end: clamp_line_to_u32(line_end),
        signature,
        doc,
        content_preview,
        trust_level,
        injection_flags,
        callers,
        callees,
        tests,
    }))
}

/// Build a BFS hierarchy rooted at `root_id` (a chunk_id), expanding
/// either upward (callers) or downward (callees) up to `max_depth`.
///
/// Strategy:
/// 1. Resolve `root_id` → root chunk row (must exist; 404 otherwise).
/// 2. Borrow the cached `Store::get_call_graph()` (`Arc<CallGraph>`).
/// 3. BFS by **name** (the call graph is name-keyed), tracking depth.
/// 4. Resolve every visited name → chunk_id by `(file, name)` deterministic
///    pick (same overload-disambiguation pattern as `build_graph`). When
///    a name resolves to multiple chunks across files we keep the first
///    (sorted by id) so renderings are stable.
/// 5. Pull node metadata + per-node degree counts in one batched SQL query.
/// 6. Emit only edges whose endpoints are both inside the BFS frontier.
///
/// `max_depth` is clamped to 1..=10 by the caller.
pub(crate) fn build_hierarchy(
    store: &Store<ReadOnly>,
    root_id: &str,
    direction: HierarchyDirection,
    max_depth: u32,
) -> Result<Option<HierarchyResponse>, StoreError> {
    let _span = tracing::info_span!(
        "build_hierarchy",
        root_id = %root_id,
        direction = direction.as_str(),
        max_depth
    )
    .entered();

    // 1. Resolve root chunk_id → name (and confirm it exists).
    let root_name: Option<String> = store.serve_chunk_name_by_id(root_id)?;

    let Some(root_name) = root_name else {
        tracing::info!(root_id, "build_hierarchy: root chunk not found");
        return Ok(None);
    };

    // 2. Cached call graph (Arc; cheap clone).
    let call_graph = match store.get_call_graph() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "build_hierarchy: get_call_graph failed");
            return Err(e);
        }
    };

    // 3. BFS by name with depth tracking. Visited holds the smallest
    //    depth we've seen for each name (so a node visible at depth 2 via
    //    one path stays at depth 2 even if also reachable at depth 4).
    let mut depth_by_name: HashMap<std::sync::Arc<str>, u32> = HashMap::new();
    let root_arc: std::sync::Arc<str> = std::sync::Arc::from(root_name.as_str());
    depth_by_name.insert(root_arc.clone(), 0);
    let mut frontier: Vec<std::sync::Arc<str>> = vec![root_arc.clone()];

    for d in 0..max_depth {
        let mut next: Vec<std::sync::Arc<str>> = Vec::new();
        for name in &frontier {
            let neighbors: Option<&Vec<std::sync::Arc<str>>> = match direction {
                HierarchyDirection::Callees => call_graph.forward.get(name),
                HierarchyDirection::Callers => call_graph.reverse.get(name),
            };
            let Some(neighbors) = neighbors else { continue };
            for n in neighbors {
                if !depth_by_name.contains_key(n) {
                    depth_by_name.insert(n.clone(), d + 1);
                    next.push(n.clone());
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    let visited_names: Vec<String> = depth_by_name.keys().map(|s| s.to_string()).collect();
    tracing::info!(
        visited = visited_names.len(),
        "build_hierarchy: BFS complete"
    );

    // 4 + 5. Pull chunk rows for every visited name. We need (file, name)
    // tuples to disambiguate overloads. Use IN (?, ?, ...) with a sane cap.
    if visited_names.is_empty() {
        return Ok(Some(HierarchyResponse {
            root: root_id.to_string(),
            direction: direction.as_str().to_string(),
            max_depth,
            nodes: Vec::new(),
            edges: Vec::new(),
        }));
    }

    // SEC-4: the store fetch chunks the IN-list for the chunk-metadata
    // fetch. Deep hierarchies (e.g. callers of a heavily-called std helper)
    // can generate >32k visited names, overflowing SQLite's bind cap.
    //
    // We preserve the "smallest id wins" disambiguation the downstream
    // code relies on by taking the lexicographic min across chunks
    // whenever a name appears in more than one row (which only happens
    // when the same name resolves to multiple chunks — the disambiguation
    // target).
    let meta_rows = store.serve_hierarchy_chunk_meta(&visited_names)?;

    let mut name_to_first_id: HashMap<String, String> = HashMap::new();
    let mut chunk_meta: HashMap<String, (String, String, String, String, i64, i64)> =
        HashMap::new();

    for row in meta_rows {
        let id = row.id;
        let name = row.name;
        let chunk_type = row.chunk_type;
        let language = row.language;
        let origin = row.origin;
        let line_start = row.line_start;
        let line_end = row.line_end;

        // Preserve "smallest id wins" across chunks: if we've
        // already recorded a first id for this name, keep the
        // lexicographic minimum so the result is deterministic
        // regardless of chunk iteration order.
        match name_to_first_id.get(&name) {
            Some(existing) if existing.as_str() <= id.as_str() => {}
            _ => {
                name_to_first_id.insert(name.clone(), id.clone());
            }
        }
        chunk_meta.insert(
            id,
            (name, chunk_type, language, origin, line_start, line_end),
        );
    }

    // Build the node set. For names that didn't resolve to a chunk
    // (e.g. external std-lib calls), they're silently skipped — same
    // behavior as build_graph.
    let mut nodes: Vec<HierarchyNode> = Vec::new();
    let mut emitted_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name_arc, depth) in &depth_by_name {
        let name = name_arc.as_ref();
        let Some(chunk_id) = name_to_first_id.get(name) else {
            continue;
        };
        let Some((cname, ckind, lang, origin, l_start, l_end)) = chunk_meta.get(chunk_id) else {
            continue;
        };
        emitted_ids.insert(chunk_id.clone());
        nodes.push(HierarchyNode {
            base: Node {
                id: chunk_id.clone(),
                name: cname.clone(),
                kind: ckind.clone(),
                language: lang.clone(),
                file: origin.clone(),
                line_start: clamp_line_to_u32(*l_start),
                line_end: clamp_line_to_u32(*l_end),
                n_callers: 0,
                n_callees: 0,
                dead: false,
            },
            bfs_depth: *depth,
        });
    }

    // 6. Pull all edges where both endpoints are inside the visited
    //    name set, then resolve each (caller, callee) name pair to
    //    chunk IDs using the same name_to_first_id map.
    let edge_pairs = store.serve_hierarchy_edges(&visited_names)?;

    let mut caller_count: HashMap<String, u32> = HashMap::new();
    let mut callee_count: HashMap<String, u32> = HashMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen_edges: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for (caller_name, callee_name) in edge_pairs {
        let Some(caller_id) = name_to_first_id.get(&caller_name) else {
            continue;
        };
        let Some(callee_id) = name_to_first_id.get(&callee_name) else {
            continue;
        };
        // Skip self-loops; 3d-force-graph is not happy with them.
        if caller_id == callee_id {
            continue;
        }
        if !seen_edges.insert((caller_id.clone(), callee_id.clone())) {
            continue;
        }
        *caller_count.entry(callee_id.clone()).or_insert(0) += 1;
        *callee_count.entry(caller_id.clone()).or_insert(0) += 1;
        edges.push(Edge {
            source: caller_id.clone(),
            target: callee_id.clone(),
            kind: "call".to_string(),
        });
    }

    // Backfill per-node degrees + dead flags using only the edges
    // visible inside this hierarchy. (Dead is uninteresting in a
    // hierarchy view since the root is by definition reached, so we
    // mostly just want the degree counts to drive node sizing.)
    for node in nodes.iter_mut() {
        node.base.n_callers = *caller_count.get(&node.base.id).unwrap_or(&0);
        node.base.n_callees = *callee_count.get(&node.base.id).unwrap_or(&0);
        node.base.dead = node.base.n_callers == 0 && node.base.kind != "test";
    }

    // Stable order: by depth then id, so frontend rendering is
    // deterministic and reload-friendly.
    nodes.sort_unstable_by(|a, b| {
        a.bfs_depth
            .cmp(&b.bfs_depth)
            .then_with(|| a.base.id.cmp(&b.base.id))
    });

    tracing::info!(
        nodes = nodes.len(),
        edges = edges.len(),
        "build_hierarchy: built response"
    );

    let response = HierarchyResponse {
        root: root_id.to_string(),
        direction: direction.as_str().to_string(),
        max_depth,
        nodes,
        edges,
    };

    Ok(Some(response))
}

/// Build the embedding cluster response — every chunk that has UMAP coords,
/// annotated with caller/callee degree counts so the frontend can size and
/// elevate nodes by importance.
///
/// Skips chunks whose `umap_x` / `umap_y` are NULL (UMAP hasn't been run
/// on those rows yet; the frontend shows a "run `cqs index --umap`" hint
/// when the entire corpus is empty). `max_nodes` caps the response by
/// descending caller count, same convention as `/api/graph?max_nodes=N`.
pub(crate) fn build_cluster(
    store: &Store<ReadOnly>,
    max_nodes: Option<usize>,
) -> Result<ClusterResponse, StoreError> {
    let _span = tracing::info_span!("build_cluster", max_nodes = ?max_nodes).entered();

    // SEC-3: always bind an effective cap. When the client omits
    // `?max_nodes`, fall back to `serve_cluster_max_nodes()` so a
    // single request can't materialise the full chunks table.
    // The user-supplied value is clamped too so `?max_nodes=999999999`
    // can't be used as a DoS vector either. (Env-tunable via
    // `CQS_SERVE_CLUSTER_MAX_NODES`.)
    let max_cluster_nodes = crate::limits::serve_cluster_max_nodes();
    let effective_cap = max_nodes
        .unwrap_or(max_cluster_nodes)
        .min(max_cluster_nodes);

    let rows = store.serve_cluster_nodes(effective_cap as i64)?;
    let skipped = store.serve_cluster_skipped_count()?.max(0) as u64;

    // Per-chunk caller/callee counts. Same name-based join as
    // build_graph; counts only edges whose endpoints both resolve
    // inside the projected set so the n_callers/n_callees on a node
    // accurately describe what the cluster view shows.
    let mut caller_count: HashMap<String, u32> = HashMap::new();
    let mut callee_count: HashMap<String, u32> = HashMap::new();
    let mut name_to_first_id: HashMap<String, String> = HashMap::new();
    for row in &rows {
        name_to_first_id
            .entry(row.name.clone())
            .or_insert(row.id.clone());
    }

    // SEC-3: cap the edge fetch too. function_calls can have tens of
    // millions of rows on a large monorepo — even though the loop
    // below filters on `name_to_first_id` membership, Rust-side
    // filtering after pulling every row over the wire is the DoS
    // vector we're closing. (Env-tunable via
    // `CQS_SERVE_GRAPH_MAX_EDGES`.)
    let edge_rows = store.serve_cluster_edges(crate::limits::serve_graph_max_edges() as i64)?;
    for (caller_name, callee_name) in edge_rows {
        let (Some(caller_id), Some(callee_id)) = (
            name_to_first_id.get(&caller_name),
            name_to_first_id.get(&callee_name),
        ) else {
            continue;
        };
        *caller_count.entry(callee_id.clone()).or_insert(0) += 1;
        *callee_count.entry(caller_id.clone()).or_insert(0) += 1;
    }

    let mut nodes: Vec<ClusterNode> = rows
        .into_iter()
        .map(|row| {
            let id = row.id;
            let name = row.name;
            let chunk_type = row.chunk_type;
            let language = row.language;
            let origin = row.origin;
            let line_start = row.line_start;
            let line_end = row.line_end;
            let umap_x = row.umap_x;
            let umap_y = row.umap_y;
            let n_callers = *caller_count.get(&id).unwrap_or(&0);
            let n_callees = *callee_count.get(&id).unwrap_or(&0);
            let dead = n_callers == 0 && chunk_type != "test";
            ClusterNode {
                base: Node {
                    id: id.clone(),
                    name,
                    kind: chunk_type,
                    language,
                    file: origin,
                    line_start: clamp_line_to_u32(line_start),
                    line_end: clamp_line_to_u32(line_end),
                    n_callers,
                    n_callees,
                    dead,
                },
                umap_x,
                umap_y,
            }
        })
        .collect();

    // SQL already caps at `effective_cap`, so the Rust-side truncate
    // is only meaningful when the client's `max_nodes` is BELOW the
    // SQL cap (e.g. `?max_nodes=100` on a 50k-cap default). Sort by
    // descending caller count so the truncation keeps the most
    // important nodes.
    if nodes.len() > effective_cap {
        nodes.sort_unstable_by(|a, b| {
            b.base
                .n_callers
                .cmp(&a.base.n_callers)
                .then_with(|| a.base.id.cmp(&b.base.id))
        });
        nodes.truncate(effective_cap);
    }

    tracing::info!(
        nodes = nodes.len(),
        skipped,
        "build_cluster: built response"
    );

    // Explicit warn when corpus has chunks but every one lacks
    // UMAP coords. Cluster pane renders blank otherwise — operators
    // staring at the empty view need a journal hint that they need to
    // run `cqs index --umap` (UMAP is opt-in, not in default index).
    if nodes.is_empty() && skipped > 0 {
        tracing::warn!(
            skipped,
            "build_cluster: corpus has chunks but no UMAP coordinates — run `cqs index --umap`",
        );
    }

    Ok(ClusterResponse { nodes, skipped })
}

/// Pull richer stats than the `Store::base_embedding_count` shortcut.
/// One query for total chunks + files + call edges + type edges.
///
/// A single SELECT with subqueries rather than four sequential `fetch_one`
/// round-trips. SQLite plans these as parallel COUNT scans, saving three pool
/// round-trips per `/stats` request.
pub(crate) fn build_stats(store: &Store<ReadOnly>) -> Result<StatsResponse, StoreError> {
    let _span = tracing::info_span!("build_stats").entered();

    let row = store.serve_stats()?;
    Ok(StatsResponse {
        total_chunks: row.total_chunks.max(0) as u64,
        total_files: row.total_files.max(0) as u64,
        call_edges: row.call_edges.max(0) as u64,
        type_edges: row.type_edges.max(0) as u64,
    })
}
