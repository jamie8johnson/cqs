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
