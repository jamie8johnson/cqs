//! Call graph queries: callers, callees, call graph construction, context.

use std::path::PathBuf;

use sqlx::Row;

use crate::store::helpers::{
    clamp_line_number, CallGraph, CalleeInfo, CallerInfo, CallerWithContext, StoreError,
};
use crate::store::Store;

impl<Mode> Store<Mode> {
    /// Find all callers of a function (from full call graph)
    pub fn get_callers_full(&self, callee_name: &str) -> Result<Vec<CallerInfo>, StoreError> {
        let _span = tracing::debug_span!("get_callers_full", function = %callee_name).entered();
        tracing::debug!(callee_name, "querying callers from full call graph");

        self.rt.block_on(async {
            // Collapse duplicate (file, caller, line) edges to a single row,
            // keeping the most-trusted kind. `MIN(rank_case)` over the explicit
            // trust rank (call < serde < macro < fn-pointer < doc-reference)
            // replaces a lexical `MIN(edge_kind)` — alphabetical ordering was a
            // coincidence and `doc_reference` ('d') breaks it. SQLite's
            // bare-column rule lets the sibling `edge_kind` take its value from
            // the same min-rank row, so we read the kind and discard the rank.
            // Trust rank also leads the ORDER BY so a limited window can never
            // be filled by low-trust `doc_reference` edges while direct `call`
            // edges sit below it.
            let rank_case = crate::parser::CallEdgeKind::rank_case_sql("edge_kind");
            let sql = format!(
                "SELECT file, caller_name, caller_line, edge_kind, MIN({rank_case}) AS trust_rank
                 FROM function_calls
                 WHERE callee_name = ?1
                 GROUP BY file, caller_name, caller_line
                 ORDER BY trust_rank, file, caller_line"
            );
            let rows: Vec<(String, String, i64, String, i64)> =
                sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(callee_name)
                    .fetch_all(&self.pool)
                    .await?;

            let callers: Vec<CallerInfo> = rows
                .into_iter()
                .map(|(file, name, line, kind, _rank)| CallerInfo {
                    file: PathBuf::from(file),
                    name,
                    line: clamp_line_number(line),
                    edge_kind: crate::parser::CallEdgeKind::from_str_or_default(&kind),
                })
                .collect();

            Ok(callers)
        })
    }

    /// Group a method name's definitions by enclosing type. Returns
    /// `(parent_type_name, count)` rows — `None` covers free functions and any
    /// def with no enclosing type. Used to (a) detect that a bare name has
    /// more than one definition so `cqs callers`/`callees` can advertise the
    /// `Type::method` disambiguation, and (b) decide, during `Type::method`
    /// resolution, which *other* types own a same-named method (so callers
    /// parented to those types are excluded rather than over-reported).
    ///
    /// Only callable chunk kinds are counted — a same-named struct/const is
    /// not a method definition and must not inflate the candidate list.
    pub fn count_method_defs_by_type(
        &self,
        method: &str,
    ) -> Result<Vec<(Option<String>, usize)>, StoreError> {
        let _span = tracing::debug_span!("count_method_defs_by_type", method = %method).entered();
        self.rt.block_on(async {
            let callable = crate::parser::ChunkType::callable_sql_list();
            let sql = format!(
                "SELECT parent_type_name, COUNT(*) AS n
                 FROM chunks
                 WHERE name = ?1 AND chunk_type IN ({callable})
                 GROUP BY parent_type_name
                 ORDER BY n DESC, parent_type_name"
            );
            let rows: Vec<(Option<String>, i64)> =
                sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(method)
                    .fetch_all(&self.pool)
                    .await?;
            Ok(rows
                .into_iter()
                .map(|(ty, n)| (ty, n.max(0) as usize))
                .collect())
        })
    }

    /// Origins (file paths) where a `Type::method` is defined. Used by
    /// the callees `Type::method` path to scope `get_callees_full` to the
    /// right definition. Only callable chunk kinds qualify.
    pub fn get_type_method_origins(
        &self,
        qualifier_type: &str,
        method: &str,
    ) -> Result<Vec<String>, StoreError> {
        let _span = tracing::debug_span!(
            "get_type_method_origins",
            qualifier_type = %qualifier_type,
            method = %method
        )
        .entered();
        self.rt.block_on(async {
            let callable = crate::parser::ChunkType::callable_sql_list();
            let sql = format!(
                "SELECT DISTINCT origin
                 FROM chunks
                 WHERE name = ?1 AND parent_type_name = ?2 AND chunk_type IN ({callable})"
            );
            let rows: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(method)
                .bind(qualifier_type)
                .fetch_all(&self.pool)
                .await?;
            Ok(rows.into_iter().map(|(o,)| o).collect())
        })
    }

    /// Callers of `Type::method`, attributed by receiver type.
    ///
    /// Resolution is read-side from `chunks.parent_type_name` — no new
    /// columns, no parser changes. For each caller of the bare `method`, the
    /// caller's own enclosing type is looked up by joining `function_calls`
    /// back to `chunks` on `(file=origin, caller_name=name,
    /// caller_line=line_start)`. Attribution:
    ///
    /// - caller's enclosing type == `qualifier_type` → self-call, included as
    ///   [`CallerAttribution::SelfType`];
    /// - caller's enclosing type is a *different* type that itself defines a
    ///   `method` (per `other_owner_types`) → excluded (it calls that other
    ///   type's method, not ours);
    /// - caller has no enclosing type, an enclosing type with no `method` def,
    ///   or no resolvable chunk → included as [`CallerAttribution::Ambiguous`]
    ///   (over-report with a flag, never silent exclusion, never false
    ///   certainty).
    ///
    /// `other_owner_types` is the set of enclosing types (other than
    /// `qualifier_type`) that define a same-named method, derived once by the
    /// caller from [`count_method_defs_by_type`] so this method stays a single
    /// scan.
    pub fn get_callers_attributed(
        &self,
        method: &str,
        qualifier_type: &str,
        other_owner_types: &std::collections::HashSet<String>,
    ) -> Result<Vec<crate::store::AttributedCaller>, StoreError> {
        use crate::store::{AttributedCaller, CallerAttribution, CallerInfo};
        let _span = tracing::debug_span!(
            "get_callers_attributed",
            method = %method,
            qualifier_type = %qualifier_type
        )
        .entered();
        self.rt.block_on(async {
            // LEFT JOIN so callers with no matching chunk (e.g. >100-line
            // functions skipped at extraction) still appear — they resolve to
            // a NULL enclosing type and fall into the Ambiguous bucket rather
            // than vanishing. Trust rank leads the collapse + ordering exactly
            // as `get_callers_full`.
            let rank_case = crate::parser::CallEdgeKind::rank_case_sql("fc.edge_kind");
            let sql = format!(
                "SELECT fc.file, fc.caller_name, fc.caller_line, fc.edge_kind,
                        MIN({rank_case}) AS trust_rank, c.parent_type_name
                 FROM function_calls fc
                 LEFT JOIN chunks c
                   ON c.origin = fc.file
                  AND c.name = fc.caller_name
                  AND c.line_start = fc.caller_line
                 WHERE fc.callee_name = ?1
                 GROUP BY fc.file, fc.caller_name, fc.caller_line
                 ORDER BY trust_rank, fc.file, fc.caller_line"
            );
            let rows: Vec<(String, String, i64, String, i64, Option<String>)> =
                sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(method)
                    .fetch_all(&self.pool)
                    .await?;

            let mut out = Vec::new();
            for (file, name, line, kind, _rank, parent_type) in rows {
                let attribution = match parent_type.as_deref() {
                    Some(t) if t == qualifier_type => CallerAttribution::SelfType,
                    // Parented to a different type that owns its own `method`:
                    // this caller targets that type's method, not ours.
                    Some(t) if other_owner_types.contains(t) => continue,
                    // No enclosing type, or an enclosing type with no same-named
                    // method, or an unresolved chunk: receiver unproven.
                    _ => CallerAttribution::Ambiguous,
                };
                out.push(AttributedCaller {
                    caller: CallerInfo {
                        file: PathBuf::from(file),
                        name,
                        line: clamp_line_number(line),
                        edge_kind: crate::parser::CallEdgeKind::from_str_or_default(&kind),
                    },
                    attribution,
                });
            }
            Ok(out)
        })
    }

    /// Get all callees of a function (from full call graph)
    /// When `file` is provided, scopes to callees of that function in that specific file.
    /// When `None`, returns callees across all files (backwards compatible, but ambiguous
    /// for common names like `new`, `parse`, `from_str`).
    pub fn get_callees_full(
        &self,
        caller_name: &str,
        file: Option<&str>,
    ) -> Result<Vec<CalleeInfo>, StoreError> {
        let _span = tracing::debug_span!("get_callees_full", function = %caller_name).entered();
        self.rt.block_on(async {
            // MIN over the explicit trust rank per (callee, line) — same
            // most-trusted-wins collapse as get_callers_full, replacing the old
            // lexical `MIN(edge_kind)` that `doc_reference` would break.
            // Trust rank leads the ORDER BY so direct call edges
            // outrank doc_reference within any `--limit` window.
            let rank_case = crate::parser::CallEdgeKind::rank_case_sql("edge_kind");
            let sql = format!(
                "SELECT callee_name, call_line, edge_kind, MIN({rank_case}) AS trust_rank
                 FROM function_calls
                 WHERE caller_name = ?1 AND (?2 IS NULL OR file = ?2)
                 GROUP BY callee_name, call_line
                 ORDER BY trust_rank, call_line"
            );
            let rows: Vec<(String, i64, String, i64)> =
                sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(caller_name)
                    .bind(file)
                    .fetch_all(&self.pool)
                    .await?;

            Ok(rows
                .into_iter()
                .map(|(name, line, kind, _rank)| CalleeInfo {
                    name,
                    line: clamp_line_number(line),
                    edge_kind: crate::parser::CallEdgeKind::from_str_or_default(&kind),
                })
                .collect())
        })
    }

    /// Load the call graph as forward + reverse adjacency lists.
    /// Single SQL scan of `function_calls`, capped at 500K edges (override via
    /// `CQS_CALL_GRAPH_MAX_EDGES`) to prevent OOM on adversarial databases.
    /// Typical projects have ~2000 edges.
    /// Used by trace (forward BFS), impact (reverse BFS), and test-map (reverse BFS).
    /// Cached call graph — populated on first access, returns clone from OnceLock.
    /// **No invalidation by design.** The cache lives for the `Store` lifetime and is
    /// never cleared. Normal usage is one `Store` per CLI command, so the index cannot
    /// change while the cache is live. In long-lived modes (batch, watch), callers must
    /// re-open the `Store` to pick up index changes — do not add a `clear()` here.
    /// ~15 call sites benefit from this single-scan caching.
    pub fn get_call_graph(&self) -> Result<std::sync::Arc<CallGraph>, StoreError> {
        if let Some(cached) = self.call_graph_cache.get() {
            return Ok(std::sync::Arc::clone(cached));
        }
        let _span = tracing::info_span!("get_call_graph").entered();
        let graph = self.rt.block_on(async {
            // Cap is env-overridable via CQS_CALL_GRAPH_MAX_EDGES so
            // monorepos above 500K edges can lift the ceiling.
            let max_edges = crate::limits::call_graph_max_edges() as i64;
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT DISTINCT caller_name, callee_name FROM function_calls LIMIT ?1",
            )
            .bind(max_edges)
            .fetch_all(&self.pool)
            .await?;

            let edge_count = rows.len();
            if edge_count as i64 >= max_edges {
                tracing::warn!(
                    limit = max_edges,
                    "Call graph truncated at {} edges — analysis may be incomplete; \
                     bump CQS_CALL_GRAPH_MAX_EDGES if your corpus is legitimately larger",
                    max_edges
                );
            } else {
                tracing::info!(edges = edge_count, "Call graph loaded");
            }

            let mut forward: std::collections::HashMap<
                std::sync::Arc<str>,
                Vec<std::sync::Arc<str>>,
            > = std::collections::HashMap::new();
            let mut reverse: std::collections::HashMap<
                std::sync::Arc<str>,
                Vec<std::sync::Arc<str>>,
            > = std::collections::HashMap::new();

            // String interner: each unique name is allocated once as Arc<str>,
            // then shared across forward and reverse maps.
            let mut interner: std::collections::HashMap<String, std::sync::Arc<str>> =
                std::collections::HashMap::new();
            let mut intern = |s: String| -> std::sync::Arc<str> {
                interner
                    .entry(s)
                    .or_insert_with_key(|k| std::sync::Arc::from(k.as_str()))
                    .clone()
            };

            for (caller, callee) in rows {
                let caller = intern(caller);
                let callee = intern(callee);
                reverse
                    .entry(callee.clone())
                    .or_default()
                    .push(caller.clone());
                forward.entry(caller).or_default().push(callee);
            }

            Ok::<_, StoreError>(CallGraph { forward, reverse })
        })?;
        let arc = std::sync::Arc::new(graph);
        let _ = self.call_graph_cache.set(std::sync::Arc::clone(&arc));
        Ok(arc)
    }

    /// Find callers with call-site line numbers for impact analysis.
    /// Returns the caller function name, file, start line, and the specific line
    /// where the call to `callee_name` occurs.
    pub fn get_callers_with_context(
        &self,
        callee_name: &str,
    ) -> Result<Vec<CallerWithContext>, StoreError> {
        let _span =
            tracing::debug_span!("get_callers_with_context", function = %callee_name).entered();
        self.rt.block_on(async {
            // Trust rank leads the ORDER BY: `cqs impact` flows through
            // here, so doc_reference edges must never displace direct call
            // edges in a capped impact window.
            let rank_case = crate::parser::CallEdgeKind::rank_case_sql("edge_kind");
            let sql = format!(
                "SELECT file, caller_name, caller_line, call_line, edge_kind
                 FROM function_calls
                 WHERE callee_name = ?1
                 ORDER BY {rank_case}, file, call_line"
            );
            let rows: Vec<(String, String, i64, i64, String)> =
                sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(callee_name)
                    .fetch_all(&self.pool)
                    .await?;

            Ok(rows
                .into_iter()
                .map(
                    |(file, name, caller_line, call_line, kind)| CallerWithContext {
                        file: PathBuf::from(file),
                        name,
                        line: clamp_line_number(caller_line),
                        call_line: clamp_line_number(call_line),
                        edge_kind: crate::parser::CallEdgeKind::from_str_or_default(&kind),
                    },
                )
                .collect())
        })
    }

    /// Batch-fetch callers with context for multiple callee names.
    /// Returns `callee_name -> Vec<CallerWithContext>` using a single
    /// `WHERE callee_name IN (...)` query per batch of 500 names.
    /// Avoids N+1 `get_callers_with_context` calls in diff impact analysis.
    pub fn get_callers_with_context_batch(
        &self,
        callee_names: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<CallerWithContext>>, StoreError> {
        let _span =
            tracing::debug_span!("get_callers_with_context_batch", count = callee_names.len())
                .entered();
        if callee_names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: std::collections::HashMap<String, Vec<CallerWithContext>> =
                std::collections::HashMap::new();

            // `max_rows_per_statement(1)` returns ~32466 against SQLite's
            // 32766-variable limit (one bound variable per row — the callee
            // name), so a 5k-name batch runs as a single statement.
            use crate::store::helpers::sql::max_rows_per_statement;
            let batch_size = max_rows_per_statement(1);
            // Trust rank leads the per-callee ORDER BY, matching the
            // single-name `get_callers_with_context`.
            let rank_case = crate::parser::CallEdgeKind::rank_case_sql("edge_kind");
            for batch in callee_names.chunks(batch_size) {
                let placeholders = super::super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT callee_name, file, caller_name, caller_line, call_line, edge_kind
                     FROM function_calls
                     WHERE callee_name IN ({})
                     ORDER BY callee_name, {}, file, call_line",
                    placeholders, rank_case
                );
                let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for name in batch {
                    q = q.bind(name);
                }
                let rows: Vec<_> = q.fetch_all(&self.pool).await?;
                for row in rows {
                    let callee: String = row.get(0);
                    let caller = CallerWithContext {
                        file: PathBuf::from(row.get::<String, _>(1)),
                        name: row.get(2),
                        line: clamp_line_number(row.get::<i64, _>(3)),
                        call_line: clamp_line_number(row.get::<i64, _>(4)),
                        edge_kind: crate::parser::CallEdgeKind::from_str_or_default(
                            &row.get::<String, _>(5),
                        ),
                    };
                    result.entry(callee).or_default().push(caller);
                }
            }

            Ok(result)
        })
    }

    /// Batch-fetch callers (full call graph) for multiple callee names.
    /// Returns `callee_name -> Vec<CallerInfo>` using a single
    /// `WHERE callee_name IN (...)` query per batch of 500 names.
    /// Avoids N+1 `get_callers_full` calls in the context command.
    pub fn get_callers_full_batch(
        &self,
        callee_names: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<CallerInfo>>, StoreError> {
        let _span =
            tracing::debug_span!("get_callers_full_batch", count = callee_names.len()).entered();
        if callee_names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: std::collections::HashMap<String, Vec<CallerInfo>> =
                std::collections::HashMap::new();

            // See rationale on `get_callers_with_context_batch`.
            // `max_rows_per_statement(1)` (~32466) keeps SQL round-trips low
            // on large name sets.
            use crate::store::helpers::sql::max_rows_per_statement;
            let batch_size = max_rows_per_statement(1);
            // MIN over the explicit trust rank, with `edge_kind` taking its
            // value from the same min-rank row (SQLite bare-column rule). Same
            // most-trusted-wins collapse as `get_callers_full`.
            let rank_case = crate::parser::CallEdgeKind::rank_case_sql("edge_kind");
            for batch in callee_names.chunks(batch_size) {
                let placeholders = super::super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT callee_name, file, caller_name, caller_line, edge_kind, \
                            MIN({rank_case}) AS trust_rank
                     FROM function_calls
                     WHERE callee_name IN ({placeholders})
                     GROUP BY callee_name, file, caller_name, caller_line
                     ORDER BY callee_name, trust_rank, file, caller_line",
                    rank_case = rank_case,
                    placeholders = placeholders
                );
                let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for name in batch {
                    q = q.bind(name);
                }
                let rows: Vec<_> = q.fetch_all(&self.pool).await?;
                for row in rows {
                    let callee: String = row.get(0);
                    let caller = CallerInfo {
                        file: PathBuf::from(row.get::<String, _>(1)),
                        name: row.get(2),
                        line: clamp_line_number(row.get::<i64, _>(3)),
                        edge_kind: crate::parser::CallEdgeKind::from_str_or_default(
                            &row.get::<String, _>(4),
                        ),
                    };
                    result.entry(callee).or_default().push(caller);
                }
            }

            Ok(result)
        })
    }

    /// Batch-fetch callees (full call graph) for multiple caller names.
    /// Returns `caller_name -> Vec<(callee_name, call_line)>` using a single
    /// `WHERE caller_name IN (...)` query per batch of 500 names.
    /// Avoids N+1 `get_callees_full` calls in the context command.
    /// Unlike [`get_callees_full`], does not support file scoping — returns
    /// callees across all files. This is acceptable for the context command
    /// which later filters by origin.
    pub fn get_callees_full_batch(
        &self,
        caller_names: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<(String, u32)>>, StoreError> {
        let _span =
            tracing::debug_span!("get_callees_full_batch", count = caller_names.len()).entered();
        if caller_names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: std::collections::HashMap<String, Vec<(String, u32)>> =
                std::collections::HashMap::new();

            // Same shape as `get_callers_full_batch`. One bound variable per
            // row, so `max_rows_per_statement(1)` reuses the whole
            // 32466-slot budget.
            use crate::store::helpers::sql::max_rows_per_statement;
            let batch_size = max_rows_per_statement(1);
            // Trust rank leads the per-caller ORDER BY. GROUP BY
            // collapses a (callee, line) pair appearing as both a call and a
            // doc_reference edge to a single row at its most-trusted rank,
            // replacing the old DISTINCT.
            let rank_case = crate::parser::CallEdgeKind::rank_case_sql("edge_kind");
            for batch in caller_names.chunks(batch_size) {
                let placeholders = super::super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT caller_name, callee_name, call_line, MIN({rank_case}) AS trust_rank
                     FROM function_calls
                     WHERE caller_name IN ({placeholders})
                     GROUP BY caller_name, callee_name, call_line
                     ORDER BY caller_name, trust_rank, call_line",
                    rank_case = rank_case,
                    placeholders = placeholders
                );
                let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for name in batch {
                    q = q.bind(name);
                }
                let rows: Vec<_> = q.fetch_all(&self.pool).await?;
                for row in rows {
                    let caller: String = row.get(0);
                    let callee_name: String = row.get(1);
                    let call_line = clamp_line_number(row.get::<i64, _>(2));
                    result
                        .entry(caller)
                        .or_default()
                        .push((callee_name, call_line));
                }
            }

            Ok(result)
        })
    }
}
