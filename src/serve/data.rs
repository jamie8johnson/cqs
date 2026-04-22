//! Wire-format types for `/api/*` endpoints.
//!
//! Frontend Cytoscape.js consumes the `Node` + `Edge` shapes directly.
//! Field names match Cytoscape's element-data convention so the JS can
//! pass the rows through without transformation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::store::{ReadOnly, Store, StoreError};

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
    pub signature: String,
    pub doc: Option<String>,
    /// First N lines of the chunk content for inline preview.
    pub content_preview: String,
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

/// Build the full graph response from the store.
///
/// Pulls every chunk + every resolved call edge in two SQL queries, then
/// derives per-node `n_callers` / `n_callees` / `dead` flags from the edge
/// counts. Optional `file` and `kind` filters reduce the candidate set
/// before edge resolution.
///
/// `max_nodes` caps the returned node count by descending caller count
/// (most-called first). When set, the edge list is also pruned to edges
/// whose endpoints both survive the cap.
pub(crate) fn build_graph(
    store: &Store<ReadOnly>,
    file_filter: Option<&str>,
    kind_filter: Option<&str>,
    max_nodes: Option<usize>,
) -> Result<GraphResponse, StoreError> {
    store.rt.block_on(async {
        // 1. Pull all chunks (filtered) into Node prototypes (caller/callee
        //    counts populated in step 3).
        let mut node_query = String::from(
            "SELECT id, name, chunk_type, language, origin, line_start, line_end \
             FROM chunks WHERE 1=1",
        );
        let mut binds: Vec<String> = Vec::new();
        if let Some(file) = file_filter {
            node_query.push_str(" AND origin LIKE ?");
            binds.push(format!("{file}%"));
        }
        if let Some(kind) = kind_filter {
            node_query.push_str(" AND chunk_type = ?");
            binds.push(kind.to_string());
        }
        // Stable order so HNSW reconstruction can't flip the rendering.
        node_query.push_str(" ORDER BY id");

        let mut q = sqlx::query(&node_query);
        for b in &binds {
            q = q.bind(b);
        }
        let rows = q.fetch_all(&store.pool).await?;

        let mut nodes_by_id: HashMap<String, Node> = HashMap::with_capacity(rows.len());
        let mut name_to_ids: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let chunk_type: String = row.get("chunk_type");
            let language: String = row.get("language");
            let origin: String = row.get("origin");
            let line_start: i64 = row.get("line_start");
            let line_end: i64 = row.get("line_end");
            nodes_by_id.insert(
                id.clone(),
                Node {
                    id: id.clone(),
                    name: name.clone(),
                    kind: chunk_type,
                    language,
                    file: origin,
                    line_start: line_start.max(0) as u32,
                    line_end: line_end.max(0) as u32,
                    n_callers: 0,
                    n_callees: 0,
                    dead: false,
                },
            );
            name_to_ids.entry(name).or_default().push(id);
        }

        // 2. Pull edges from function_calls. Resolve caller_name + file →
        //    chunk_id by exact (origin, name, line range) match. Resolve
        //    callee_name → first matching chunk_id (overload simplification
        //    for v1 — multiple-target overload disambiguation is out of
        //    scope; pick deterministically by ordering).
        let edge_rows = sqlx::query(
            "SELECT fc.file, fc.caller_name, fc.callee_name \
             FROM function_calls fc",
        )
        .fetch_all(&store.pool)
        .await?;

        // Build a fast lookup: (file, name) → chunk_id whose line range
        // contains caller_line. function_calls.caller_line tells us where
        // the caller starts; chunks.line_start matches that exactly when
        // the parser captured the same boundary. Skip when no match —
        // means the caller is in a chunk we didn't index (large skipped
        // function, etc.).
        let mut origin_name_to_id: HashMap<(String, String), Vec<String>> = HashMap::new();
        for node in nodes_by_id.values() {
            origin_name_to_id
                .entry((node.file.clone(), node.name.clone()))
                .or_default()
                .push(node.id.clone());
        }

        let mut edges = Vec::with_capacity(edge_rows.len());
        let mut caller_count: HashMap<String, u32> = HashMap::new();
        let mut callee_count: HashMap<String, u32> = HashMap::new();
        for row in edge_rows {
            let file: String = row.get("file");
            let caller_name: String = row.get("caller_name");
            let callee_name: String = row.get("callee_name");

            // Resolve caller: must match (file, name) — drop if ambiguous
            // or unknown.
            let Some(callers) = origin_name_to_id.get(&(file, caller_name)) else {
                continue;
            };
            let caller_id = match callers.as_slice() {
                [] => continue,
                [single] => single.clone(),
                multiple => multiple[0].clone(), // deterministic pick
            };

            // Resolve callee: by name only (could be in any file).
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

        // 3. Populate per-node degree + dead flag.
        for (id, node) in nodes_by_id.iter_mut() {
            node.n_callers = *caller_count.get(id).unwrap_or(&0);
            node.n_callees = *callee_count.get(id).unwrap_or(&0);
            node.dead = node.n_callers == 0 && node.kind != "test";
        }

        // 4. Optional max_nodes cap — keep top by caller count, drop
        //    edges whose endpoints didn't survive.
        let mut nodes: Vec<Node> = nodes_by_id.into_values().collect();
        if let Some(cap) = max_nodes {
            if nodes.len() > cap {
                nodes.sort_unstable_by(|a, b| {
                    b.n_callers.cmp(&a.n_callers).then_with(|| a.id.cmp(&b.id))
                });
                nodes.truncate(cap);
                let kept: std::collections::HashSet<&str> =
                    nodes.iter().map(|n| n.id.as_str()).collect();
                edges.retain(|e| {
                    kept.contains(e.source.as_str()) && kept.contains(e.target.as_str())
                });
            }
        } else {
            // Stable order even without cap (so frontend can hash for cache).
            nodes.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        }

        tracing::info!(
            nodes = nodes.len(),
            edges = edges.len(),
            file_filter = ?file_filter,
            kind_filter = ?kind_filter,
            max_nodes = ?max_nodes,
            "build_graph"
        );

        Ok(GraphResponse { nodes, edges })
    })
}

/// Build the chunk-detail response for one chunk_id.
///
/// Pulls the chunk metadata + first 30 lines of content + caller/callee
/// chunk lists + tests-that-cover. Returns None for unknown IDs.
pub(crate) fn build_chunk_detail(
    store: &Store<ReadOnly>,
    chunk_id: &str,
) -> Result<Option<ChunkDetail>, StoreError> {
    store.rt.block_on(async {
        // Fetch the chunk row.
        let row = sqlx::query(
            "SELECT id, name, chunk_type, language, origin, line_start, line_end, \
                    signature, doc, content \
             FROM chunks WHERE id = ?",
        )
        .bind(chunk_id)
        .fetch_optional(&store.pool)
        .await?;

        let Some(row) = row else { return Ok(None) };

        let id: String = row.get("id");
        let name: String = row.get("name");
        let chunk_type: String = row.get("chunk_type");
        let language: String = row.get("language");
        let origin: String = row.get("origin");
        let line_start: i64 = row.get("line_start");
        let line_end: i64 = row.get("line_end");
        let signature: String = row
            .get::<Option<String>, _>("signature")
            .unwrap_or_default();
        let doc: Option<String> = row.get("doc");
        let content: String = row.get::<Option<String>, _>("content").unwrap_or_default();

        // Preview = first 30 lines of content. Bounded so big chunks
        // don't bloat the sidebar JSON.
        let content_preview: String = content.lines().take(30).collect::<Vec<_>>().join("\n");

        // Caller chunks: function_calls WHERE callee_name = this.name
        let callers_rows = sqlx::query(
            "SELECT DISTINCT c.id, c.name, c.origin, c.line_start \
             FROM function_calls fc \
             JOIN chunks c ON c.name = fc.caller_name AND c.origin = fc.file \
             WHERE fc.callee_name = ? \
             ORDER BY c.origin, c.line_start \
             LIMIT 50",
        )
        .bind(&name)
        .fetch_all(&store.pool)
        .await?;
        let callers: Vec<NodeRef> = callers_rows
            .into_iter()
            .map(|r| NodeRef {
                id: r.get("id"),
                name: r.get("name"),
                file: r.get("origin"),
                line_start: r.get::<i64, _>("line_start").max(0) as u32,
            })
            .collect();

        // Callee chunks: function_calls WHERE caller_name = this.name AND file = this.origin
        let callees_rows = sqlx::query(
            "SELECT DISTINCT c.id, c.name, c.origin, c.line_start \
             FROM function_calls fc \
             JOIN chunks c ON c.name = fc.callee_name \
             WHERE fc.caller_name = ? AND fc.file = ? \
             ORDER BY c.origin, c.line_start \
             LIMIT 50",
        )
        .bind(&name)
        .bind(&origin)
        .fetch_all(&store.pool)
        .await?;
        let callees: Vec<NodeRef> = callees_rows
            .into_iter()
            .map(|r| NodeRef {
                id: r.get("id"),
                name: r.get("name"),
                file: r.get("origin"),
                line_start: r.get::<i64, _>("line_start").max(0) as u32,
            })
            .collect();

        // Tests-that-cover: heuristic — chunks whose chunk_type = 'test'
        // and whose content references this name. Cheap LIKE search.
        let tests_rows = sqlx::query(
            "SELECT id, name, origin, line_start \
             FROM chunks \
             WHERE chunk_type = 'test' AND content LIKE ? \
             ORDER BY origin, line_start \
             LIMIT 20",
        )
        .bind(format!("%{name}%"))
        .fetch_all(&store.pool)
        .await?;
        let tests: Vec<NodeRef> = tests_rows
            .into_iter()
            .map(|r| NodeRef {
                id: r.get("id"),
                name: r.get("name"),
                file: r.get("origin"),
                line_start: r.get::<i64, _>("line_start").max(0) as u32,
            })
            .collect();

        Ok(Some(ChunkDetail {
            id,
            name,
            kind: chunk_type,
            language,
            file: origin,
            line_start: line_start.max(0) as u32,
            line_end: line_end.max(0) as u32,
            signature,
            doc,
            content_preview,
            callers,
            callees,
            tests,
        }))
    })
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
    let root_name: Option<String> = store.rt.block_on(async {
        let row = sqlx::query("SELECT name FROM chunks WHERE id = ?")
            .bind(root_id)
            .fetch_optional(&store.pool)
            .await?;
        Ok::<_, StoreError>(row.map(|r| r.get::<String, _>("name")))
    })?;

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

    let response = store.rt.block_on(async {
        let placeholders = vec!["?"; visited_names.len()].join(",");
        let sql = format!(
            "SELECT id, name, chunk_type, language, origin, line_start, line_end \
             FROM chunks WHERE name IN ({placeholders}) ORDER BY id"
        );
        let mut q = sqlx::query(&sql);
        for n in &visited_names {
            q = q.bind(n);
        }
        let rows = q.fetch_all(&store.pool).await?;

        // For each name, keep the deterministic-first chunk_id (sorted
        // by id from the SQL ORDER BY). If a name has multiple chunks
        // (overloads in different files), this is the first one.
        let mut name_to_first_id: HashMap<String, String> = HashMap::new();
        // Also keep all chunks so we can build node metadata for every
        // unique chunk_id that we emit (we emit one per name).
        let mut chunk_meta: HashMap<String, (String, String, String, String, i64, i64)> =
            HashMap::new();

        for row in rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let chunk_type: String = row.get("chunk_type");
            let language: String = row.get("language");
            let origin: String = row.get("origin");
            let line_start: i64 = row.get("line_start");
            let line_end: i64 = row.get("line_end");

            // First-seen wins (rows already sorted by id).
            name_to_first_id.entry(name.clone()).or_insert(id.clone());
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
            let Some((cname, ckind, lang, origin, l_start, l_end)) = chunk_meta.get(chunk_id)
            else {
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
                    line_start: (*l_start).max(0) as u32,
                    line_end: (*l_end).max(0) as u32,
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
        let edge_sql = format!(
            "SELECT DISTINCT caller_name, callee_name FROM function_calls \
             WHERE caller_name IN ({placeholders}) AND callee_name IN ({placeholders})"
        );
        let mut eq = sqlx::query(&edge_sql);
        for n in &visited_names {
            eq = eq.bind(n);
        }
        for n in &visited_names {
            eq = eq.bind(n);
        }
        let edge_rows = eq.fetch_all(&store.pool).await?;

        let mut caller_count: HashMap<String, u32> = HashMap::new();
        let mut callee_count: HashMap<String, u32> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::with_capacity(edge_rows.len());
        for row in edge_rows {
            let caller_name: String = row.get("caller_name");
            let callee_name: String = row.get("callee_name");
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

        Ok::<_, StoreError>(HierarchyResponse {
            root: root_id.to_string(),
            direction: direction.as_str().to_string(),
            max_depth,
            nodes,
            edges,
        })
    })?;

    Ok(Some(response))
}

/// Pull richer stats than the `Store::base_embedding_count` shortcut.
/// One query for total chunks + files + call edges + type edges.
pub(crate) fn build_stats(store: &Store<ReadOnly>) -> Result<StatsResponse, StoreError> {
    store.rt.block_on(async {
        let chunks_row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
            .fetch_one(&store.pool)
            .await?;
        let files_row: (i64,) = sqlx::query_as("SELECT COUNT(DISTINCT origin) FROM chunks")
            .fetch_one(&store.pool)
            .await?;
        let call_row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM function_calls")
            .fetch_one(&store.pool)
            .await?;
        let type_row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM type_edges")
            .fetch_one(&store.pool)
            .await?;
        Ok(StatsResponse {
            total_chunks: chunks_row.0.max(0) as u64,
            total_files: files_row.0.max(0) as u64,
            call_edges: call_row.0.max(0) as u64,
            type_edges: type_row.0.max(0) as u64,
        })
    })
}
