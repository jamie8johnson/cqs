//! Call graph queries: callers, callees, call graph construction, context.

use std::path::PathBuf;

use sqlx::Row;

use crate::store::helpers::{
    clamp_line_number, CallGraph, CalleeInfo, CallerInfo, CallerWithContext, StoreError,
};
use crate::store::Store;

/// One attributed-caller row from `get_callers_attributed`'s merged scan:
/// `(file, caller_name, caller_line, edge_kind, pick_rank,
/// caller_parent_type, callee_name)`. Aliased to keep the query under
/// `clippy::type_complexity`.
type AttributedCallerRow = (String, String, i64, String, i64, Option<String>, String);

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

    /// Definition sites `(origin, line_start)` where a `Type::method` is
    /// defined. Used by the callees `Type::method` path to scope
    /// `get_callees_full` to the right definition — both the file AND the
    /// def's start line, so two same-named methods sharing a file (a `Store`
    /// and a `StoreBuilder` both defining `build` in `store.rs`) resolve to
    /// disjoint callee sets rather than merging. Only callable chunk kinds
    /// qualify.
    pub fn get_type_method_def_sites(
        &self,
        qualifier_type: &str,
        method: &str,
    ) -> Result<Vec<(String, i64)>, StoreError> {
        let _span = tracing::debug_span!(
            "get_type_method_def_sites",
            qualifier_type = %qualifier_type,
            method = %method
        )
        .entered();
        self.rt.block_on(async {
            let callable = crate::parser::ChunkType::callable_sql_list();
            let sql = format!(
                "SELECT DISTINCT origin, line_start
                 FROM chunks
                 WHERE name = ?1 AND parent_type_name = ?2 AND chunk_type IN ({callable})"
            );
            let rows: Vec<(String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(method)
                .bind(qualifier_type)
                .fetch_all(&self.pool)
                .await?;
            Ok(rows.into_iter().collect())
        })
    }

    /// Callers of `Type::method`, attributed by receiver type. Returns the
    /// attributed callers plus the count of callers excluded as belonging to a
    /// different owning type (`other_owner_types`).
    ///
    /// Resolution is read-side from `chunks.parent_type_name` — no new
    /// columns, no parser changes. Up to two edge populations are matched,
    /// gated by `include_bare`:
    ///
    /// 1. **Bare-method edges** (`callee_name = method`, only when
    ///    `include_bare` is true): code call sites extracted with the receiver
    ///    stripped (`store.search()` records callee `search`). The caller's own
    ///    enclosing type is looked up by joining `function_calls` back to
    ///    `chunks` on `(file=origin, caller_name=name, caller_line=line_start)`,
    ///    then attributed:
    ///    - enclosing type == `qualifier_type` → self-call,
    ///      [`CallerAttribution::SelfType`];
    ///    - enclosing type is a *different* type that itself defines a
    ///      same-named method (per `other_owner_types`) → **heuristically**
    ///      excluded and counted. This is a heuristic, not a proof: a method
    ///      on `Index` that calls `store.search()` genuinely targets
    ///      `Store::search`, yet is excluded because `Index` also defines
    ///      `search`. The exclusion count is surfaced so the narrowing is
    ///      visible.
    ///    - no enclosing type / enclosing type without a same-named method /
    ///      unresolved chunk → [`CallerAttribution::Ambiguous`] (over-report
    ///      with a flag, never a silent drop).
    /// 2. **Exact-qualified edges** (`callee_name = 'Type::method'`, always
    ///    matched): doc references store the full backticked string verbatim
    ///    (markdown `` `Store::open()` `` records callee `Store::open`). These
    ///    name the receiver explicitly, so they are unambiguously this method's
    ///    edges — included at [`CallerAttribution::SelfType`]. Without this arm
    ///    a `Type::method` query would never reach them (the bare-method query
    ///    can't match a qualified callee), making them unreachable.
    ///
    /// `include_bare` is set false when `qualifier_type` has no local
    /// definition of `method` — an external / module qualifier like
    /// `std::fs::read_to_string`, whose only edges are the exact-qualified doc
    /// references. Running the bare arm there would mis-attribute every local
    /// `read_to_string` call site as an ambiguous caller under a fabricated
    /// `std::fs` type, so the bare arm is gated off and only the exact arm runs.
    ///
    /// The GROUP BY on `(file, caller_name, caller_line)` collapses a co-located
    /// bare code edge and exact doc edge to one row (the most-trusted kind), so
    /// a caller present in both populations is reported once.
    ///
    /// `other_owner_types` is the set of enclosing types (other than
    /// `qualifier_type`) that define a same-named method, derived once by the
    /// caller from [`count_method_defs_by_type`] so this method stays few
    /// scans. It is unused (and typically empty) when `include_bare` is false.
    pub fn get_callers_attributed(
        &self,
        method: &str,
        qualifier_type: &str,
        other_owner_types: &std::collections::HashSet<String>,
        include_bare: bool,
    ) -> Result<(Vec<crate::store::AttributedCaller>, usize), StoreError> {
        use crate::store::{AttributedCaller, CallerAttribution, CallerInfo};
        let _span = tracing::debug_span!(
            "get_callers_attributed",
            method = %method,
            qualifier_type = %qualifier_type,
            include_bare
        )
        .entered();
        let qualified = format!("{qualifier_type}::{method}");
        self.rt.block_on(async {
            let rank_case = crate::parser::CallEdgeKind::rank_case_sql("fc.edge_kind");
            // The exact-qualified arm always runs. The bare-method arm is gated
            // by `include_bare` — off for external/module qualifiers with no
            // local def, so their local same-named call sites aren't
            // mis-attributed. A NULL `parent_type_name` means the caller chunk
            // didn't resolve (>100-line skip) or the row is an exact edge. LEFT
            // JOIN keeps unresolved callers rather than dropping them. Trust rank
            // leads the collapse + ordering exactly as `get_callers_full`.
            //
            // `?1` binds the bare name only when `include_bare`; otherwise it is
            // bound to the exact-qualified string too, so the predicate reduces
            // to the exact arm (no extra placeholder shape to manage).
            let bare_bind = if include_bare {
                method
            } else {
                qualified.as_str()
            };
            // `pick_rank` is the collapse key: `rank_case * 2` plus a 0/1
            // qualified-vs-bare tiebreaker (exact-qualified ⇒ 0). The GROUP BY
            // bare-column rule then sources `edge_kind`, `parent_type_name`, and
            // `callee_name` from the row with the minimum `pick_rank`. The
            // tiebreaker makes that selection deterministic when a bare code
            // edge and an exact-qualified doc edge tie on trust rank at the same
            // call site: the exact edge wins, so the SelfType-vs-ambiguous label
            // is stable across runs rather than picked arbitrarily by SQLite.
            // `pick_rank` preserves trust order (qualified-first within a rank),
            // so it also leads the ORDER BY.
            let sql = format!(
                "SELECT fc.file, fc.caller_name, fc.caller_line, fc.edge_kind,
                        MIN({rank_case} * 2
                            + (CASE WHEN fc.callee_name = ?2 THEN 0 ELSE 1 END))
                          AS pick_rank,
                        c.parent_type_name, fc.callee_name
                 FROM function_calls fc
                 LEFT JOIN chunks c
                   ON c.origin = fc.file
                  AND c.name = fc.caller_name
                  AND c.line_start = fc.caller_line
                 WHERE fc.callee_name = ?1 OR fc.callee_name = ?2
                 GROUP BY fc.file, fc.caller_name, fc.caller_line
                 ORDER BY pick_rank, fc.file, fc.caller_line"
            );
            let rows: Vec<AttributedCallerRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(bare_bind)
                .bind(&qualified)
                .fetch_all(&self.pool)
                .await?;

            let mut out = Vec::new();
            let mut excluded = 0usize;
            // The GROUP BY (file, caller_name, caller_line) already yields one
            // row per call site, collapsing a co-located bare code edge and
            // exact doc edge into a single min-trust-rank row — so the Call
            // edge (rank 0) wins over the doc edge (rank 4) and the site is
            // attributed via its enclosing type. No further de-dupe needed.
            for (file, name, line, kind, _rank, parent_type, callee) in rows {
                // An exact-qualified edge names the receiver — proven self.
                let is_exact = callee == qualified;
                let attribution = if is_exact {
                    CallerAttribution::SelfType
                } else {
                    match parent_type.as_deref() {
                        Some(t) if t == qualifier_type => CallerAttribution::SelfType,
                        // Parented to a different type that owns its own
                        // same-named method — heuristically excluded (the
                        // receiver could still be ours; the count surfaces it).
                        Some(t) if other_owner_types.contains(t) => {
                            excluded += 1;
                            continue;
                        }
                        // No enclosing type, enclosing type without a same-named
                        // method, or an unresolved chunk: receiver unproven.
                        _ => CallerAttribution::Ambiguous,
                    }
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
            Ok((out, excluded))
        })
    }

    /// Get all callees of a function (from full call graph)
    /// When `file` is provided, scopes to callees of that function in that specific file.
    /// When `None`, returns callees across all files (backwards compatible, but ambiguous
    /// for common names like `new`, `parse`, `from_str`).
    /// When `caller_line` is provided, additionally scopes to the definition
    /// starting at that line (`function_calls.caller_line` is the caller's
    /// `line_start`). This disambiguates two same-named methods sharing a file —
    /// e.g. a `Store` and a `StoreBuilder` both defining `build` in `store.rs` —
    /// which `(caller_name, file)` alone would merge. `None` keeps the
    /// file-or-global scope.
    pub fn get_callees_full(
        &self,
        caller_name: &str,
        file: Option<&str>,
        caller_line: Option<i64>,
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
                 WHERE caller_name = ?1
                   AND (?2 IS NULL OR file = ?2)
                   AND (?3 IS NULL OR caller_line = ?3)
                 GROUP BY callee_name, call_line
                 ORDER BY trust_rank, call_line"
            );
            let rows: Vec<(String, i64, String, i64)> =
                sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(caller_name)
                    .bind(file)
                    .bind(caller_line)
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
            // Collapse duplicate (caller, callee) rows to the single
            // most-trusted edge in SQL: `MIN(rank_case)` picks the lowest trust
            // rank (call < serde < macro < fn-pointer < doc-reference) and the
            // sibling bare columns (`edge_kind`, `file`, `caller_line`,
            // `call_line`) take their value from that min-rank row (SQLite
            // bare-column rule), so the in-memory edge metadata mirrors the
            // local `get_callers_full` collapse rather than picking an arbitrary
            // kind. Names are read directly from the GROUP BY keys.
            let rank_case = crate::parser::CallEdgeKind::rank_case_sql("edge_kind");
            let sql = format!(
                "SELECT caller_name, callee_name, edge_kind, file, caller_line, call_line,
                        MIN({rank_case}) AS trust_rank
                 FROM function_calls
                 GROUP BY caller_name, callee_name
                 LIMIT ?1"
            );
            let rows: Vec<(String, String, String, String, i64, i64, i64)> =
                sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
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
            let mut edges: std::collections::HashMap<
                (std::sync::Arc<str>, std::sync::Arc<str>),
                crate::store::helpers::CallEdgeMeta,
            > = std::collections::HashMap::new();

            // String interner: each unique name is allocated once as Arc<str>,
            // then shared across forward, reverse, and the edge-metadata map.
            let mut interner: std::collections::HashMap<String, std::sync::Arc<str>> =
                std::collections::HashMap::new();
            let mut intern = |s: String| -> std::sync::Arc<str> {
                interner
                    .entry(s)
                    .or_insert_with_key(|k| std::sync::Arc::from(k.as_str()))
                    .clone()
            };

            for (caller, callee, kind, file, caller_line, call_line, _rank) in rows {
                let caller = intern(caller);
                let callee = intern(callee);
                reverse
                    .entry(callee.clone())
                    .or_default()
                    .push(caller.clone());
                forward
                    .entry(caller.clone())
                    .or_default()
                    .push(callee.clone());
                edges.insert(
                    (caller, callee),
                    crate::store::helpers::CallEdgeMeta {
                        edge_kind: crate::parser::CallEdgeKind::from_str_or_default(&kind),
                        file,
                        caller_line: crate::store::helpers::clamp_line_number(caller_line),
                        call_line: crate::store::helpers::clamp_line_number(call_line),
                    },
                );
            }

            Ok::<_, StoreError>(CallGraph {
                forward,
                reverse,
                edges,
            })
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

    /// Distinct callee names reached by a real-caller edge whose call-site origin
    /// (`function_calls.file`) is in `origins`. The worktree-overlay dead-code
    /// merge (#1858 Part B) uses this against the PARENT store: these are exactly
    /// the functions the delta files USED to call, so they are the only
    /// candidates that a now-masked caller-origin could flip from live to dead.
    /// Restricted to real-caller kinds (excludes `doc_reference`) so the merged
    /// dead verdict agrees with `fetch_uncalled_functions`'s own real-caller
    /// contract. Returns an empty vec when `origins` is empty (no delta).
    pub fn distinct_callees_from_origins(
        &self,
        origins: &[String],
    ) -> Result<Vec<String>, StoreError> {
        let _span = tracing::debug_span!("distinct_callees_from_origins", origins = origins.len())
            .entered();
        if origins.is_empty() {
            return Ok(Vec::new());
        }
        let real_callers = crate::parser::CallEdgeKind::real_caller_kinds_sql();
        self.rt.block_on(async {
            use crate::store::helpers::sql::max_rows_per_statement;
            let batch_size = max_rows_per_statement(1);
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for batch in origins.chunks(batch_size) {
                let placeholders = super::super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT DISTINCT callee_name FROM function_calls
                     WHERE file IN ({placeholders})
                       AND edge_kind IN ({real_callers})"
                );
                let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for o in batch {
                    q = q.bind(o);
                }
                let rows: Vec<_> = q.fetch_all(&self.pool).await?;
                for row in rows {
                    let name: String = row.get(0);
                    if seen.insert(name.clone()) {
                        out.push(name);
                    }
                }
            }
            Ok(out)
        })
    }
}
