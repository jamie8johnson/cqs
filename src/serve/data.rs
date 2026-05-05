//! Wire-format types for `/api/*` endpoints.
//!
//! Frontend Cytoscape.js consumes the `Node` + `Edge` shapes directly.
//! Field names match Cytoscape's element-data convention so the JS can
//! pass the rows through without transformation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::Row;

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

// SEC-3 absolute ceilings on response shapes — see `crate::limits::serve_*`.
//
// P2.40: previously hardcoded `const` values (50_000 / 500_000 / 50_000 +
// per-list LIMIT 50/50/20 in `build_chunk_detail`). Operators can now
// tune via env vars (`CQS_SERVE_GRAPH_MAX_NODES`, `CQS_SERVE_GRAPH_MAX_EDGES`,
// `CQS_SERVE_CLUSTER_MAX_NODES`, `CQS_SERVE_CHUNK_DETAIL_{CALLERS,CALLEES,TESTS}`)
// without recompiling. The helpers in `limits.rs` clamp to a hard maximum
// so a misconfiguration can't unbound the response.
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
    /// P2.24 — `Option` so a NULL `chunks.signature` (partial write,
    /// SIGKILL between INSERT phases) reaches the frontend as `null`
    /// rather than collapsing to `""`. The frontend renders missing
    /// columns as a `<missing — DB column NULL>` placeholder so an
    /// empty signature pane is no longer indistinguishable from a
    /// successfully-extracted void signature.
    pub signature: Option<String>,
    pub doc: Option<String>,
    /// First N lines of the chunk content for inline preview. `None`
    /// when the underlying `chunks.content` column is NULL — same
    /// reasoning as `signature`. (P2.24.)
    pub content_preview: Option<String>,
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

    store.rt.block_on(async {
        // 1. Chunk fetch.
        //
        // SEC-3: always bind an effective cap. When the client omits
        // `?max_nodes`, fall back to `serve_graph_max_nodes()` so a
        // single request can't materialise a million chunks into
        // memory. The user-supplied value is clamped too so
        // `?max_nodes=999999999` can't be used as a DoS vector either.
        // (Env-tunable via `CQS_SERVE_GRAPH_MAX_NODES`; clamped to a
        // 1M hard ceiling — see `crate::limits`. P2.40.)
        //
        // PF-V1.30 (P2.70): replace per-row correlated subquery with
        // one aggregated subselect joined by name. Previously each
        // scanned row triggered a log-N index probe into function_calls
        // (~50k probes against the 50k node cap on a 30k-edge corpus).
        // One GROUP BY pass is O(M+N). The subquery still counts the
        // *name* not the chunk, which over-counts for shared-name
        // overloads — but that's exactly what the post-fetch resolution
        // does too. ORDER BY ... LIMIT N pushes the truncation down to
        // SQL so we don't pull the whole table.
        let max_graph_nodes = crate::limits::serve_graph_max_nodes();
        let effective_cap = max_nodes.unwrap_or(max_graph_nodes).min(max_graph_nodes);
        let mut node_query = "SELECT c.id, c.name, c.chunk_type, c.language, c.origin, \
                    c.line_start, c.line_end, \
                    COALESCE(cc.n, 0) AS n_callers_global \
             FROM chunks c \
             LEFT JOIN (SELECT callee_name, COUNT(*) AS n \
                        FROM function_calls GROUP BY callee_name) cc \
               ON cc.callee_name = c.name \
             WHERE 1=1"
            .to_string();
        let mut binds: Vec<String> = Vec::new();
        if let Some(file) = file_filter {
            // SEC-5: escape LIKE metacharacters so `%` / `_` in the
            // query string stay literal. Without this, a hostile (or
            // accidental) `%` in `?file=` turns the prefix filter into
            // a full-text contains — changing the semantic contract
            // and potentially widening the matched row set far beyond
            // what the user sees in the URL. The `ESCAPE '\\'` clause
            // tells SQLite the backslash below should be treated as
            // the escape byte.
            let escaped = file
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            node_query.push_str(" AND c.origin LIKE ? ESCAPE '\\'");
            binds.push(format!("{escaped}%"));
        }
        if let Some(kind) = kind_filter {
            node_query.push_str(" AND c.chunk_type = ?");
            binds.push(kind.to_string());
        }
        // Stable tie-break by id so equal-rank chunks don't reshuffle
        // between requests.
        node_query.push_str(" ORDER BY n_callers_global DESC, c.id ASC LIMIT ?");
        binds.push(effective_cap.to_string());

        let mut q = sqlx::query(&node_query);
        for b in &binds {
            q = q.bind(b);
        }
        let rows = q.fetch_all(&store.pool).await?;

        let mut nodes_by_id: HashMap<String, Node> = HashMap::with_capacity(rows.len());
        let mut name_to_ids: HashMap<String, Vec<String>> = HashMap::new();
        // For the capped path we already have global n_callers from SQL.
        // Cache it so the post-resolution loop can pick it up — degree
        // counts derived purely from the visible edge set would understate
        // importance for nodes whose call edges point to chunks outside
        // the capped window.
        let mut prelim_n_callers: HashMap<String, u32> = HashMap::new();
        for row in rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let chunk_type: String = row.get("chunk_type");
            let language: String = row.get("language");
            let origin: String = row.get("origin");
            let line_start: i64 = row.get("line_start");
            let line_end: i64 = row.get("line_end");
            let n: i64 = row.get("n_callers_global");
            prelim_n_callers.insert(id.clone(), clamp_line_to_u32(n));
            nodes_by_id.insert(
                id.clone(),
                Node {
                    id: id.clone(),
                    name: name.clone(),
                    kind: chunk_type,
                    language,
                    file: origin,
                    line_start: clamp_line_to_u32(line_start),
                    line_end: clamp_line_to_u32(line_end),
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
        // (Env-tunable via `CQS_SERVE_GRAPH_MAX_EDGES`; P2.40.)
        let max_graph_edges = crate::limits::serve_graph_max_edges();
        //
        // SEC-4: chunk the IN-list so `name_set.len()` > SQLite's
        // `SQLITE_MAX_VARIABLE_NUMBER` (32766) doesn't overflow the
        // bind cursor. Each row binds the chunk twice (once for
        // callee_name, once for caller_name) so the per-chunk row
        // count is `max_rows_per_statement(2)` (~16233). Dedup via
        // HashSet because an edge whose callee and caller fall into
        // different chunks can surface in both sub-queries. We carry
        // `(file, caller, callee)` tuples rather than raw `SqliteRow`s
        // so the resolver step below doesn't re-parse the row.
        let edge_tuples: Vec<(String, String, String)> = {
            let mut name_set: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for node in nodes_by_id.values() {
                name_set.insert(node.name.as_str());
            }
            if name_set.is_empty() {
                Vec::new()
            } else {
                use crate::store::helpers::sql::max_rows_per_statement;
                const EDGE_CHUNK: usize = max_rows_per_statement(2);
                let names: Vec<&str> = name_set.into_iter().collect();
                // P3.44: dedup keys are u64 hashes of (file, caller, callee)
                // instead of three owned `String`s per row. Cloning three
                // Strings on every dedup-miss row was a HashSet-worth of
                // allocation churn proportional to the edge fan-out; hashing
                // is constant per row and the false-collision rate at u64
                // is negligible for the per-request edge set size we ship.
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
                let mut accum: Vec<(String, String, String)> = Vec::new();
                'chunks: for chunk in names.chunks(EDGE_CHUNK) {
                    if accum.len() >= max_graph_edges {
                        break;
                    }
                    let placeholders =
                        crate::store::helpers::sql::make_placeholders(chunk.len()).into_owned();
                    let edge_sql = format!(
                        "SELECT fc.file, fc.caller_name, fc.callee_name \
                         FROM function_calls fc \
                         WHERE fc.callee_name IN ({placeholders}) \
                            OR fc.caller_name IN ({placeholders}) \
                         LIMIT ?"
                    );
                    let remaining = (max_graph_edges - accum.len()) as i64;
                    let mut eq = sqlx::query(&edge_sql);
                    for n in chunk {
                        eq = eq.bind(*n);
                    }
                    for n in chunk {
                        eq = eq.bind(*n);
                    }
                    eq = eq.bind(remaining);
                    let rows = eq.fetch_all(&store.pool).await?;
                    for row in rows {
                        let file: String = row.get("file");
                        let caller: String = row.get("caller_name");
                        let callee: String = row.get("callee_name");
                        let mut h = DefaultHasher::new();
                        file.hash(&mut h);
                        caller.hash(&mut h);
                        callee.hash(&mut h);
                        if seen.insert(h.finish()) {
                            accum.push((file, caller, callee));
                            if accum.len() >= max_graph_edges {
                                break 'chunks;
                            }
                        }
                    }
                }
                accum
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
    let _span = tracing::info_span!("build_chunk_detail", chunk_id = %chunk_id).entered();

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
        // P2.24: NULL is a real signal (partial write during indexing,
        // SIGKILL between INSERT phases) — preserve it through to the
        // wire format rather than flattening to `""`.
        let signature: Option<String> = row.get("signature");
        let doc: Option<String> = row.get("doc");
        let content: Option<String> = row.get("content");

        // Preview = first 30 lines of content. Bounded so big chunks
        // don't bloat the sidebar JSON. `None` when the row had NULL
        // content (P2.24).
        let content_preview: Option<String> = content
            .as_deref()
            .map(|c| c.lines().take(30).collect::<Vec<_>>().join("\n"));

        // Caller chunks: function_calls WHERE callee_name = this.name.
        // P2.40: LIMIT bound to `serve_chunk_detail_callers_limit()`
        // (env `CQS_SERVE_CHUNK_DETAIL_CALLERS`, default 50).
        let callers_limit = crate::limits::serve_chunk_detail_callers_limit() as i64;
        let callers_rows = sqlx::query(
            "SELECT DISTINCT c.id, c.name, c.origin, c.line_start \
             FROM function_calls fc \
             JOIN chunks c ON c.name = fc.caller_name AND c.origin = fc.file \
             WHERE fc.callee_name = ? \
             ORDER BY c.origin, c.line_start \
             LIMIT ?",
        )
        .bind(&name)
        .bind(callers_limit)
        .fetch_all(&store.pool)
        .await?;
        // RB-V1.29-3: surface out-of-range `line_start` (negative or
        // overflows u32) as a `StoreError::Corruption` rather than
        // silently clamping to 0. A corrupted row here manifests in
        // the UI as a mis-scrolled sidebar; without the explicit
        // error the regression has no diagnostic at all. The error
        // propagates through the axum handler as a 500.
        let to_noderef = |r: sqlx::sqlite::SqliteRow| -> Result<NodeRef, StoreError> {
            let raw: i64 = r.get("line_start");
            let id: String = r.get("id");
            let line_start = u32::try_from(raw).map_err(|_| {
                StoreError::Corruption(format!("chunk {id} has out-of-range line_start {raw}"))
            })?;
            Ok(NodeRef {
                name: r.get("name"),
                file: r.get("origin"),
                id,
                line_start,
            })
        };

        let callers: Vec<NodeRef> = callers_rows
            .into_iter()
            .map(&to_noderef)
            .collect::<Result<_, _>>()?;

        // Callee chunks: function_calls WHERE caller_name = this.name AND file = this.origin.
        // P2.40: LIMIT bound to `serve_chunk_detail_callees_limit()`
        // (env `CQS_SERVE_CHUNK_DETAIL_CALLEES`, default 50).
        let callees_limit = crate::limits::serve_chunk_detail_callees_limit() as i64;
        let callees_rows = sqlx::query(
            "SELECT DISTINCT c.id, c.name, c.origin, c.line_start \
             FROM function_calls fc \
             JOIN chunks c ON c.name = fc.callee_name \
             WHERE fc.caller_name = ? AND fc.file = ? \
             ORDER BY c.origin, c.line_start \
             LIMIT ?",
        )
        .bind(&name)
        .bind(&origin)
        .bind(callees_limit)
        .fetch_all(&store.pool)
        .await?;
        let callees: Vec<NodeRef> = callees_rows
            .into_iter()
            .map(&to_noderef)
            .collect::<Result<_, _>>()?;

        // Tests-that-cover: heuristic — chunks whose chunk_type = 'test'
        // and whose content references this name. Cheap LIKE search.
        //
        // SEC-8: escape LIKE metacharacters in `name` so a chunk named
        // e.g. `%` or `foo_bar` doesn't turn the substring contains
        // into a wildcard that matches every test. Names come from the
        // chunks table, not user input, but parser-produced names can
        // legitimately contain underscores — `foo_bar` would otherwise
        // match `fooXbar` in test content and over-report coverage.
        let escaped_name = name
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        // P2.40: LIMIT bound to `serve_chunk_detail_tests_limit()`
        // (env `CQS_SERVE_CHUNK_DETAIL_TESTS`, default 20).
        let tests_limit = crate::limits::serve_chunk_detail_tests_limit() as i64;
        let tests_rows = sqlx::query(
            "SELECT id, name, origin, line_start \
             FROM chunks \
             WHERE chunk_type = 'test' AND content LIKE ? ESCAPE '\\' \
             ORDER BY origin, line_start \
             LIMIT ?",
        )
        .bind(format!("%{escaped_name}%"))
        .bind(tests_limit)
        .fetch_all(&store.pool)
        .await?;
        let tests: Vec<NodeRef> = tests_rows
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
        // SEC-4: chunk the IN-list for the chunk-metadata fetch. Deep
        // hierarchies (e.g. callers of a heavily-called std helper)
        // can generate >32k visited names, overflowing SQLite's bind
        // cap. Binds once per row, so batch size is
        // `max_rows_per_statement(1)` (~32466).
        //
        // We preserve the "smallest id wins" disambiguation the
        // downstream code relies on by taking the lexicographic min
        // across chunks whenever a name appears in more than one
        // batch (which only happens when the same name resolves to
        // multiple chunks — the disambiguation target).
        use crate::store::helpers::sql::max_rows_per_statement;
        const META_CHUNK: usize = max_rows_per_statement(1);

        let mut name_to_first_id: HashMap<String, String> = HashMap::new();
        let mut chunk_meta: HashMap<String, (String, String, String, String, i64, i64)> =
            HashMap::new();

        for batch in visited_names.chunks(META_CHUNK) {
            let placeholders =
                crate::store::helpers::sql::make_placeholders(batch.len()).into_owned();
            let sql = format!(
                "SELECT id, name, chunk_type, language, origin, line_start, line_end \
                 FROM chunks WHERE name IN ({placeholders}) ORDER BY id"
            );
            let mut q = sqlx::query(&sql);
            for n in batch {
                q = q.bind(n);
            }
            let rows = q.fetch_all(&store.pool).await?;

            for row in rows {
                let id: String = row.get("id");
                let name: String = row.get("name");
                let chunk_type: String = row.get("chunk_type");
                let language: String = row.get("language");
                let origin: String = row.get("origin");
                let line_start: i64 = row.get("line_start");
                let line_end: i64 = row.get("line_end");

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
        //
        // SEC-4: the edge SQL binds visited_names twice (once for
        // caller_name, once for callee_name), so chunk at
        // `max_rows_per_statement(2)` (~16233). Because the WHERE
        // clause is an AND, splitting visited_names into N chunks
        // means an edge whose caller lives in chunk A and callee in
        // chunk B is only found when we query the pair (A, B). Iterate
        // the cartesian product of chunks (N² sub-queries). In
        // practice N is ~1 for normal hierarchies; the outer loop
        // only activates for pathologically deep BFS above the bind
        // cap. Dedup edges with a HashSet because the SELECT DISTINCT
        // is now only per sub-query.
        const EDGE_CHUNK: usize = max_rows_per_statement(2);
        let edge_batches: Vec<&[String]> = visited_names.chunks(EDGE_CHUNK).collect();

        let mut caller_count: HashMap<String, u32> = HashMap::new();
        let mut callee_count: HashMap<String, u32> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut seen_edges: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for caller_batch in &edge_batches {
            for callee_batch in &edge_batches {
                let caller_ph =
                    crate::store::helpers::sql::make_placeholders(caller_batch.len()).into_owned();
                let callee_ph =
                    crate::store::helpers::sql::make_placeholders(callee_batch.len()).into_owned();
                let edge_sql = format!(
                    "SELECT DISTINCT caller_name, callee_name FROM function_calls \
                     WHERE caller_name IN ({caller_ph}) AND callee_name IN ({callee_ph})"
                );
                let mut eq = sqlx::query(&edge_sql);
                for n in caller_batch.iter() {
                    eq = eq.bind(n);
                }
                for n in callee_batch.iter() {
                    eq = eq.bind(n);
                }
                let edge_rows = eq.fetch_all(&store.pool).await?;
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
            }
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

    store.rt.block_on(async {
        // SEC-3: always bind an effective cap. When the client omits
        // `?max_nodes`, fall back to `serve_cluster_max_nodes()` so a
        // single request can't materialise the full chunks table.
        // The user-supplied value is clamped too so `?max_nodes=999999999`
        // can't be used as a DoS vector either. (Env-tunable via
        // `CQS_SERVE_CLUSTER_MAX_NODES`; P2.40.)
        let max_cluster_nodes = crate::limits::serve_cluster_max_nodes();
        let effective_cap = max_nodes
            .unwrap_or(max_cluster_nodes)
            .min(max_cluster_nodes);

        // Chunks that have coords already projected. The ORDER BY id here
        // is preserved from the pre-SEC-3 code; because it's by id rather
        // than n_callers_global, the cap under load picks an arbitrary
        // subset — for the UMAP cluster view that's fine (all points are
        // semantically meaningful) and lets us skip the correlated
        // subquery cost.
        let rows = sqlx::query(
            "SELECT id, name, chunk_type, language, origin, line_start, line_end, umap_x, umap_y \
             FROM chunks \
             WHERE umap_x IS NOT NULL AND umap_y IS NOT NULL \
             ORDER BY id \
             LIMIT ?",
        )
        .bind(effective_cap as i64)
        .fetch_all(&store.pool)
        .await?;

        let skipped_row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE umap_x IS NULL OR umap_y IS NULL")
                .fetch_one(&store.pool)
                .await?;
        let skipped = skipped_row.0.max(0) as u64;

        // Per-chunk caller/callee counts. Same name-based join as
        // build_graph; counts only edges whose endpoints both resolve
        // inside the projected set so the n_callers/n_callees on a node
        // accurately describe what the cluster view shows.
        let mut caller_count: HashMap<String, u32> = HashMap::new();
        let mut callee_count: HashMap<String, u32> = HashMap::new();
        let mut name_to_first_id: HashMap<String, String> = HashMap::new();
        for row in &rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            name_to_first_id.entry(name).or_insert(id);
        }

        // SEC-3: cap the edge fetch too. function_calls can have tens of
        // millions of rows on a large monorepo — even though the loop
        // below filters on `name_to_first_id` membership, Rust-side
        // filtering after pulling every row over the wire is the DoS
        // vector we're closing. (Env-tunable via
        // `CQS_SERVE_GRAPH_MAX_EDGES`; P2.40.)
        let edge_rows = sqlx::query("SELECT caller_name, callee_name FROM function_calls LIMIT ?")
            .bind(crate::limits::serve_graph_max_edges() as i64)
            .fetch_all(&store.pool)
            .await?;
        for row in edge_rows {
            let caller_name: String = row.get("caller_name");
            let callee_name: String = row.get("callee_name");
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
                let id: String = row.get("id");
                let name: String = row.get("name");
                let chunk_type: String = row.get("chunk_type");
                let language: String = row.get("language");
                let origin: String = row.get("origin");
                let line_start: i64 = row.get("line_start");
                let line_end: i64 = row.get("line_end");
                let umap_x: f64 = row.get("umap_x");
                let umap_y: f64 = row.get("umap_y");
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

        // P3.14: explicit warn when corpus has chunks but every one lacks
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
    })
}

/// Pull richer stats than the `Store::base_embedding_count` shortcut.
/// One query for total chunks + files + call edges + type edges.
///
/// PF-V1.30.1-5: collapsed from four sequential `fetch_one` round-trips
/// into a single SELECT with subqueries. SQLite plans these as parallel
/// COUNT scans and we save three pool round-trips per `/stats` request.
pub(crate) fn build_stats(store: &Store<ReadOnly>) -> Result<StatsResponse, StoreError> {
    let _span = tracing::info_span!("build_stats").entered();

    store.rt.block_on(async {
        let row: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT \
                (SELECT COUNT(*) FROM chunks), \
                (SELECT COUNT(DISTINCT origin) FROM chunks), \
                (SELECT COUNT(*) FROM function_calls), \
                (SELECT COUNT(*) FROM type_edges)",
        )
        .fetch_one(&store.pool)
        .await?;
        Ok(StatsResponse {
            total_chunks: row.0.max(0) as u64,
            total_files: row.1.max(0) as u64,
            call_edges: row.2.max(0) as u64,
            type_edges: row.3.max(0) as u64,
        })
    })
}
