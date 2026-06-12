//! Store-side SQL for the `/api/*` serve endpoints.
//!
//! These methods own every raw `sqlx` query that the `serve::data` wire
//! builders used to run directly against the pool. They return plain typed
//! rows (tuples / small `pub(crate)` structs) — no serde, no Cytoscape wire
//! types. All wire-shaping (NodeRef/Node/Edge construction, injection-flag
//! detection, trust-level computation, preview truncation, BFS/dedup/degree
//! logic) stays in `serve::data`.

use super::{Store, StoreError};
use sqlx::Row;

/// One chunk row backing a graph node. Global `n_callers` is pre-aggregated
/// in SQL so the serve degree pass can pick it up without a correlated
/// subquery. Line numbers stay `i64` (raw SQL ints); serve clamps to `u32`.
pub(crate) struct GraphNodeRow {
    pub id: String,
    pub name: String,
    pub chunk_type: String,
    pub language: String,
    pub origin: String,
    pub line_start: i64,
    pub line_end: i64,
    pub n_callers_global: i64,
}

/// The full chunk row for the chunk-detail sidebar. NULL columns
/// (`signature`/`doc`/`content`) are preserved as `None` so serve can
/// distinguish a partial write from an empty value.
pub(crate) struct ChunkDetailRow {
    pub id: String,
    pub name: String,
    pub chunk_type: String,
    pub language: String,
    pub origin: String,
    pub line_start: i64,
    pub line_end: i64,
    pub signature: Option<String>,
    pub doc: Option<String>,
    pub content: Option<String>,
    pub vendored: bool,
}

/// A caller/callee/test reference row. `line_start` stays `i64` so serve's
/// `to_noderef` can run the same out-of-range corruption check it used to.
pub(crate) struct NeighborRow {
    pub id: String,
    pub name: String,
    pub origin: String,
    pub line_start: i64,
}

/// A chunk metadata row for the hierarchy view. Line numbers stay `i64`.
pub(crate) struct HierarchyChunkRow {
    pub id: String,
    pub name: String,
    pub chunk_type: String,
    pub language: String,
    pub origin: String,
    pub line_start: i64,
    pub line_end: i64,
}

/// A chunk row for the embedding-cluster view, including its 2D UMAP coords.
pub(crate) struct ClusterNodeRow {
    pub id: String,
    pub name: String,
    pub chunk_type: String,
    pub language: String,
    pub origin: String,
    pub line_start: i64,
    pub line_end: i64,
    pub umap_x: f64,
    pub umap_y: f64,
}

/// Stats counts for `GET /api/stats`. Raw `i64`s straight from the COUNT
/// subqueries; serve clamps to `u64`.
pub(crate) struct StatsRow {
    pub total_chunks: i64,
    pub total_files: i64,
    pub call_edges: i64,
    pub type_edges: i64,
}

impl<Mode> Store<Mode> {
    /// Fetch the capped, optionally filtered set of graph nodes, prerank'd by
    /// global caller count. `file_filter` LIKE-escaping and the `effective_cap`
    /// LIMIT clamp are SQL-coupled, so they live here next to the query.
    pub(crate) fn serve_graph_nodes(
        &self,
        file_filter: Option<&str>,
        kind_filter: Option<&str>,
        effective_cap: usize,
    ) -> Result<Vec<GraphNodeRow>, StoreError> {
        let _span = tracing::info_span!("serve_graph_nodes").entered();
        self.rt.block_on(async {
            // An aggregated subselect joined by name avoids a per-row correlated
            // subquery (which would trigger a log-N index probe into
            // function_calls per scanned row). One GROUP BY pass is O(M+N). The
            // subquery still counts the
            // *name* not the chunk, which over-counts for shared-name
            // overloads — but that's exactly what the post-fetch resolution
            // does too. ORDER BY ... LIMIT N pushes the truncation down to
            // SQL so we don't pull the whole table.
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
                // Escape LIKE metacharacters so `%` / `_` in the
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

            let mut q = sqlx::query(sqlx::AssertSqlSafe(node_query.as_str()));
            for b in &binds {
                q = q.bind(b);
            }
            let rows = q.fetch_all(&self.pool).await?;

            let mut out: Vec<GraphNodeRow> = Vec::with_capacity(rows.len());
            for row in rows {
                out.push(GraphNodeRow {
                    id: row.get("id"),
                    name: row.get("name"),
                    chunk_type: row.get("chunk_type"),
                    language: row.get("language"),
                    origin: row.get("origin"),
                    line_start: row.get("line_start"),
                    line_end: row.get("line_end"),
                    n_callers_global: row.get("n_callers_global"),
                });
            }
            Ok(out)
        })
    }

    /// Fetch deduped `(file, caller_name, callee_name)` edge tuples whose
    /// endpoints touch `names`, capped at `max_edges`. Owns the IN-list
    /// chunking (bind-cursor management) + hash dedup, both SQL-adjacent.
    pub(crate) fn serve_graph_edges(
        &self,
        names: &[&str],
        max_edges: usize,
    ) -> Result<Vec<(String, String, String)>, StoreError> {
        let _span = tracing::info_span!("serve_graph_edges").entered();
        self.rt.block_on(async {
            // Chunk the IN-list so `names.len()` > SQLite's
            // `SQLITE_MAX_VARIABLE_NUMBER` (32766) doesn't overflow the
            // bind cursor. Each row binds the chunk twice (once for
            // callee_name, once for caller_name) so the per-chunk row
            // count is `max_rows_per_statement(2)` (~16233). Dedup via
            // HashSet because an edge whose callee and caller fall into
            // different chunks can surface in both sub-queries. We carry
            // `(file, caller, callee)` tuples rather than raw `SqliteRow`s
            // so the resolver step below doesn't re-parse the row.
            use crate::store::helpers::sql::max_rows_per_statement;
            const EDGE_CHUNK: usize = max_rows_per_statement(2);
            // Dedup keys are u64 hashes of (file, caller, callee) rather
            // than three owned `String`s per row — hashing is constant per
            // row and avoids allocation churn proportional to the edge
            // fan-out. The false-collision rate at u64 is negligible for
            // the per-request edge set size we ship.
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
            let mut accum: Vec<(String, String, String)> = Vec::new();
            'chunks: for chunk in names.chunks(EDGE_CHUNK) {
                if accum.len() >= max_edges {
                    break;
                }
                let placeholders = vec!["?"; chunk.len()].join(",");
                let edge_sql = format!(
                    "SELECT fc.file, fc.caller_name, fc.callee_name \
                     FROM function_calls fc \
                     WHERE fc.callee_name IN ({placeholders}) \
                        OR fc.caller_name IN ({placeholders}) \
                     LIMIT ?"
                );
                let remaining = (max_edges - accum.len()) as i64;
                let mut eq = sqlx::query(sqlx::AssertSqlSafe(edge_sql.as_str()));
                for n in chunk {
                    eq = eq.bind(*n);
                }
                for n in chunk {
                    eq = eq.bind(*n);
                }
                eq = eq.bind(remaining);
                let rows = eq.fetch_all(&self.pool).await?;
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
                        if accum.len() >= max_edges {
                            break 'chunks;
                        }
                    }
                }
            }
            Ok(accum)
        })
    }

    /// Fetch the full chunk-detail row for `chunk_id`, or `None` if unknown.
    pub(crate) fn serve_chunk_detail_row(
        &self,
        chunk_id: &str,
    ) -> Result<Option<ChunkDetailRow>, StoreError> {
        let _span = tracing::info_span!("serve_chunk_detail_row").entered();
        self.rt.block_on(async {
            let row = sqlx::query(
                "SELECT id, name, chunk_type, language, origin, line_start, line_end, \
                        signature, doc, content, vendored \
                 FROM chunks WHERE id = ?",
            )
            .bind(chunk_id)
            .fetch_optional(&self.pool)
            .await?;

            let Some(row) = row else { return Ok(None) };

            Ok(Some(ChunkDetailRow {
                id: row.get("id"),
                name: row.get("name"),
                chunk_type: row.get("chunk_type"),
                language: row.get("language"),
                origin: row.get("origin"),
                line_start: row.get("line_start"),
                line_end: row.get("line_end"),
                // NULL is a real signal (partial write during indexing, SIGKILL
                // between INSERT phases) — preserve it through to the wire format
                // rather than flattening to `""`.
                signature: row.get("signature"),
                doc: row.get("doc"),
                content: row.get("content"),
                // `vendored` is INTEGER NOT NULL DEFAULT 0 (schema v24).
                vendored: row.get::<i64, _>("vendored") != 0,
            }))
        })
    }

    /// Fetch caller chunks for a callee `name`, capped at `limit`.
    pub(crate) fn serve_chunk_detail_callers(
        &self,
        name: &str,
        limit: i64,
    ) -> Result<Vec<NeighborRow>, StoreError> {
        let _span = tracing::info_span!("serve_chunk_detail_callers").entered();
        self.rt.block_on(async {
            let rows = sqlx::query(
                "SELECT DISTINCT c.id, c.name, c.origin, c.line_start \
                 FROM function_calls fc \
                 JOIN chunks c ON c.name = fc.caller_name AND c.origin = fc.file \
                 WHERE fc.callee_name = ? \
                 ORDER BY c.origin, c.line_start \
                 LIMIT ?",
            )
            .bind(name)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|r| NeighborRow {
                    id: r.get("id"),
                    name: r.get("name"),
                    origin: r.get("origin"),
                    line_start: r.get("line_start"),
                })
                .collect())
        })
    }

    /// Fetch callee chunks for a caller `name` defined at `origin`, capped at
    /// `limit`.
    pub(crate) fn serve_chunk_detail_callees(
        &self,
        name: &str,
        origin: &str,
        limit: i64,
    ) -> Result<Vec<NeighborRow>, StoreError> {
        let _span = tracing::info_span!("serve_chunk_detail_callees").entered();
        self.rt.block_on(async {
            let rows = sqlx::query(
                "SELECT DISTINCT c.id, c.name, c.origin, c.line_start \
                 FROM function_calls fc \
                 JOIN chunks c ON c.name = fc.callee_name \
                 WHERE fc.caller_name = ? AND fc.file = ? \
                 ORDER BY c.origin, c.line_start \
                 LIMIT ?",
            )
            .bind(name)
            .bind(origin)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|r| NeighborRow {
                    id: r.get("id"),
                    name: r.get("name"),
                    origin: r.get("origin"),
                    line_start: r.get("line_start"),
                })
                .collect())
        })
    }

    /// Fetch test chunks that reference `name` via a LIKE substring match,
    /// capped at `limit`. The LIKE-escaping of `name` is SQL-coupled, so it
    /// runs here.
    pub(crate) fn serve_chunk_detail_tests(
        &self,
        name: &str,
        limit: i64,
    ) -> Result<Vec<NeighborRow>, StoreError> {
        let _span = tracing::info_span!("serve_chunk_detail_tests").entered();
        self.rt.block_on(async {
            // Escape LIKE metacharacters in `name` so a chunk named
            // e.g. `%` or `foo_bar` doesn't turn the substring contains
            // into a wildcard that matches every test. Names come from the
            // chunks table, not user input, but parser-produced names can
            // legitimately contain underscores — `foo_bar` would otherwise
            // match `fooXbar` in test content and over-report coverage.
            let escaped_name = name
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let rows = sqlx::query(
                "SELECT id, name, origin, line_start \
                 FROM chunks \
                 WHERE chunk_type = 'test' AND content LIKE ? ESCAPE '\\' \
                 ORDER BY origin, line_start \
                 LIMIT ?",
            )
            .bind(format!("%{escaped_name}%"))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|r| NeighborRow {
                    id: r.get("id"),
                    name: r.get("name"),
                    origin: r.get("origin"),
                    line_start: r.get("line_start"),
                })
                .collect())
        })
    }

    /// Resolve a chunk_id to its name, or `None` if the chunk doesn't exist.
    /// Used by the hierarchy view's root-resolution step.
    pub(crate) fn serve_chunk_name_by_id(
        &self,
        chunk_id: &str,
    ) -> Result<Option<String>, StoreError> {
        let _span = tracing::info_span!("serve_chunk_name_by_id").entered();
        self.rt.block_on(async {
            let row = sqlx::query("SELECT name FROM chunks WHERE id = ?")
                .bind(chunk_id)
                .fetch_optional(&self.pool)
                .await?;
            Ok(row.map(|r| r.get::<String, _>("name")))
        })
    }

    /// Fetch chunk metadata for every name in `names`. Owns the IN-list
    /// chunking (bind-cursor management). Returns one row per resolved chunk
    /// (a name may resolve to several chunks across files); serve does the
    /// "smallest id wins" disambiguation.
    pub(crate) fn serve_hierarchy_chunk_meta(
        &self,
        names: &[String],
    ) -> Result<Vec<HierarchyChunkRow>, StoreError> {
        let _span = tracing::info_span!("serve_hierarchy_chunk_meta").entered();
        self.rt.block_on(async {
            // Chunk the IN-list for the chunk-metadata fetch. Deep
            // hierarchies (e.g. callers of a heavily-called std helper)
            // can generate >32k visited names, overflowing SQLite's bind
            // cap. Binds once per row, so batch size is
            // `max_rows_per_statement(1)` (~32466).
            use crate::store::helpers::sql::max_rows_per_statement;
            const META_CHUNK: usize = max_rows_per_statement(1);

            let mut out: Vec<HierarchyChunkRow> = Vec::new();
            for batch in names.chunks(META_CHUNK) {
                let placeholders = vec!["?"; batch.len()].join(",");
                let sql = format!(
                    "SELECT id, name, chunk_type, language, origin, line_start, line_end \
                     FROM chunks WHERE name IN ({placeholders}) ORDER BY id"
                );
                let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for n in batch {
                    q = q.bind(n);
                }
                let rows = q.fetch_all(&self.pool).await?;
                for row in rows {
                    out.push(HierarchyChunkRow {
                        id: row.get("id"),
                        name: row.get("name"),
                        chunk_type: row.get("chunk_type"),
                        language: row.get("language"),
                        origin: row.get("origin"),
                        line_start: row.get("line_start"),
                        line_end: row.get("line_end"),
                    });
                }
            }
            Ok(out)
        })
    }

    /// Fetch raw `(caller_name, callee_name)` edge pairs whose endpoints both
    /// fall inside `names`. Owns the N² IN-list chunking (bind-cursor
    /// management) and emits each sub-query's `SELECT DISTINCT` rows in order.
    /// Self-loop skipping and id-level dedup are id-based, so they stay in
    /// serve's resolution pass.
    pub(crate) fn serve_hierarchy_edges(
        &self,
        names: &[String],
    ) -> Result<Vec<(String, String)>, StoreError> {
        let _span = tracing::info_span!("serve_hierarchy_edges").entered();
        self.rt.block_on(async {
            // The edge SQL binds visited_names twice (once for
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
            use crate::store::helpers::sql::max_rows_per_statement;
            const EDGE_CHUNK: usize = max_rows_per_statement(2);
            let edge_batches: Vec<&[String]> = names.chunks(EDGE_CHUNK).collect();

            let mut out: Vec<(String, String)> = Vec::new();
            for caller_batch in &edge_batches {
                for callee_batch in &edge_batches {
                    let caller_ph = vec!["?"; caller_batch.len()].join(",");
                    let callee_ph = vec!["?"; callee_batch.len()].join(",");
                    let edge_sql = format!(
                        "SELECT DISTINCT caller_name, callee_name FROM function_calls \
                         WHERE caller_name IN ({caller_ph}) AND callee_name IN ({callee_ph})"
                    );
                    let mut eq = sqlx::query(sqlx::AssertSqlSafe(edge_sql.as_str()));
                    for n in caller_batch.iter() {
                        eq = eq.bind(n);
                    }
                    for n in callee_batch.iter() {
                        eq = eq.bind(n);
                    }
                    let edge_rows = eq.fetch_all(&self.pool).await?;
                    for row in edge_rows {
                        let caller_name: String = row.get("caller_name");
                        let callee_name: String = row.get("callee_name");
                        out.push((caller_name, callee_name));
                    }
                }
            }
            Ok(out)
        })
    }

    /// Fetch the projected (UMAP-coord-bearing) chunk rows, capped at `limit`.
    pub(crate) fn serve_cluster_nodes(
        &self,
        limit: i64,
    ) -> Result<Vec<ClusterNodeRow>, StoreError> {
        let _span = tracing::info_span!("serve_cluster_nodes").entered();
        self.rt.block_on(async {
            // Chunks that have coords already projected. The ORDER BY id here
            // is preserved from the pre-cap code; because it's by id rather
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
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|row| ClusterNodeRow {
                    id: row.get("id"),
                    name: row.get("name"),
                    chunk_type: row.get("chunk_type"),
                    language: row.get("language"),
                    origin: row.get("origin"),
                    line_start: row.get("line_start"),
                    line_end: row.get("line_end"),
                    umap_x: row.get("umap_x"),
                    umap_y: row.get("umap_y"),
                })
                .collect())
        })
    }

    /// Count chunks that lack UMAP coords (NULL `umap_x` / `umap_y`).
    pub(crate) fn serve_cluster_skipped_count(&self) -> Result<i64, StoreError> {
        let _span = tracing::info_span!("serve_cluster_skipped_count").entered();
        self.rt.block_on(async {
            let skipped_row: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM chunks WHERE umap_x IS NULL OR umap_y IS NULL",
            )
            .fetch_one(&self.pool)
            .await?;
            Ok(skipped_row.0)
        })
    }

    /// Fetch `(caller_name, callee_name)` edge pairs, capped at `limit`.
    /// Used by the cluster view's degree pass.
    pub(crate) fn serve_cluster_edges(
        &self,
        limit: i64,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let _span = tracing::info_span!("serve_cluster_edges").entered();
        self.rt.block_on(async {
            // Cap the edge fetch too. function_calls can have tens of
            // millions of rows on a large monorepo — even though the loop
            // below filters on `name_to_first_id` membership, Rust-side
            // filtering after pulling every row over the wire is the DoS
            // vector we're closing. (Env-tunable via
            // `CQS_SERVE_GRAPH_MAX_EDGES`.)
            let rows = sqlx::query("SELECT caller_name, callee_name FROM function_calls LIMIT ?")
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    let caller_name: String = row.get("caller_name");
                    let callee_name: String = row.get("callee_name");
                    (caller_name, callee_name)
                })
                .collect())
        })
    }

    /// Fetch the four corpus counts for `GET /api/stats` in one round-trip.
    pub(crate) fn serve_stats(&self) -> Result<StatsRow, StoreError> {
        let _span = tracing::info_span!("serve_stats").entered();
        self.rt.block_on(async {
            let row: (i64, i64, i64, i64) = sqlx::query_as(
                "SELECT \
                    (SELECT COUNT(*) FROM chunks), \
                    (SELECT COUNT(DISTINCT origin) FROM chunks), \
                    (SELECT COUNT(*) FROM function_calls), \
                    (SELECT COUNT(*) FROM type_edges)",
            )
            .fetch_one(&self.pool)
            .await?;
            Ok(StatsRow {
                total_chunks: row.0,
                total_files: row.1,
                call_edges: row.2,
                type_edges: row.3,
            })
        })
    }
}
