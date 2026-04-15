//! Call graph queries: callers, callees, call graph construction, context.

use std::path::PathBuf;

use sqlx::Row;

use crate::store::helpers::{
    clamp_line_number, CallGraph, CallerInfo, CallerWithContext, StoreError,
};
use crate::store::Store;

impl<Mode> Store<Mode> {
    /// Find all callers of a function (from full call graph)
    pub fn get_callers_full(&self, callee_name: &str) -> Result<Vec<CallerInfo>, StoreError> {
        let _span = tracing::debug_span!("get_callers_full", function = %callee_name).entered();
        tracing::debug!(callee_name, "querying callers from full call graph");

        self.rt.block_on(async {
            let rows: Vec<(String, String, i64)> = sqlx::query_as(
                "SELECT DISTINCT file, caller_name, caller_line
                 FROM function_calls
                 WHERE callee_name = ?1
                 ORDER BY file, caller_line",
            )
            .bind(callee_name)
            .fetch_all(&self.pool)
            .await?;

            let callers: Vec<CallerInfo> = rows
                .into_iter()
                .map(|(file, name, line)| CallerInfo {
                    file: PathBuf::from(file),
                    name,
                    line: clamp_line_number(line),
                })
                .collect();

            Ok(callers)
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
    ) -> Result<Vec<(String, u32)>, StoreError> {
        let _span = tracing::debug_span!("get_callees_full", function = %caller_name).entered();
        self.rt.block_on(async {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT DISTINCT callee_name, call_line
                 FROM function_calls
                 WHERE caller_name = ?1 AND (?2 IS NULL OR file = ?2)
                 ORDER BY call_line",
            )
            .bind(caller_name)
            .bind(file)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(name, line)| (name, clamp_line_number(line)))
                .collect())
        })
    }

    /// Load the call graph as forward + reverse adjacency lists.
    /// Single SQL scan of `function_calls`, capped at 500K edges to prevent OOM
    /// on adversarial databases. Typical projects have ~2000 edges.
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
            const MAX_CALL_GRAPH_EDGES: i64 = 500_000;
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT DISTINCT caller_name, callee_name FROM function_calls LIMIT ?1",
            )
            .bind(MAX_CALL_GRAPH_EDGES)
            .fetch_all(&self.pool)
            .await?;

            let edge_count = rows.len();
            if edge_count as i64 >= MAX_CALL_GRAPH_EDGES {
                tracing::warn!(
                    limit = MAX_CALL_GRAPH_EDGES,
                    "Call graph truncated at {} edges — analysis may be incomplete",
                    MAX_CALL_GRAPH_EDGES
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
            // then shared across forward and reverse maps (PERF-30).
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
            let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
                "SELECT file, caller_name, caller_line, call_line
                 FROM function_calls
                 WHERE callee_name = ?1
                 ORDER BY file, call_line",
            )
            .bind(callee_name)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(file, name, caller_line, call_line)| CallerWithContext {
                    file: PathBuf::from(file),
                    name,
                    line: clamp_line_number(caller_line),
                    call_line: clamp_line_number(call_line),
                })
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

            // PF-V1.25-8: previously hardcoded `BATCH_SIZE = 200`, derived
            // from the pre-3.32 SQLite 999-variable limit. Modern SQLite
            // permits 32766; `max_rows_per_statement(1)` returns ~32466
            // since we bind one variable per row (the callee name). A 5k
            // name batch now runs as 1 statement instead of 25.
            use crate::store::helpers::sql::max_rows_per_statement;
            let batch_size = max_rows_per_statement(1);
            for batch in callee_names.chunks(batch_size) {
                let placeholders = super::super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT callee_name, file, caller_name, caller_line, call_line
                     FROM function_calls
                     WHERE callee_name IN ({})
                     ORDER BY callee_name, file, call_line",
                    placeholders
                );
                let mut q = sqlx::query(&sql);
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

            // PF-V1.25-8: see rationale on `get_callers_with_context_batch`.
            // Bumping from 250 → `max_rows_per_statement(1)` (~32466) cuts
            // SQL round-trips ~130× on large name sets.
            use crate::store::helpers::sql::max_rows_per_statement;
            let batch_size = max_rows_per_statement(1);
            for batch in callee_names.chunks(batch_size) {
                let placeholders = super::super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT DISTINCT callee_name, file, caller_name, caller_line
                     FROM function_calls
                     WHERE callee_name IN ({})
                     ORDER BY callee_name, file, caller_line",
                    placeholders
                );
                let mut q = sqlx::query(&sql);
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

            // PF-V1.25-8: same modernization as `get_callers_full_batch`.
            // One bound variable per row, so `max_rows_per_statement(1)`
            // reuses the whole 32466-slot budget.
            use crate::store::helpers::sql::max_rows_per_statement;
            let batch_size = max_rows_per_statement(1);
            for batch in caller_names.chunks(batch_size) {
                let placeholders = super::super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT DISTINCT caller_name, callee_name, call_line
                     FROM function_calls
                     WHERE caller_name IN ({})
                     ORDER BY caller_name, call_line",
                    placeholders
                );
                let mut q = sqlx::query(&sql);
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
