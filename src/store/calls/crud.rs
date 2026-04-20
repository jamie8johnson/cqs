// DS-5: WRITE_LOCK guard is held across .await inside block_on().
// This is safe — block_on runs single-threaded, no concurrent tasks can deadlock.
#![allow(clippy::await_holding_lock)]
//! Call graph upsert, delete, batch operations, and basic stats.

use std::path::{Path, PathBuf};

use super::CallStats;
use crate::store::helpers::StoreError;
use crate::store::{ReadWrite, Store};

impl Store<ReadWrite> {
    /// Insert or replace call sites for a chunk
    pub fn upsert_calls(
        &self,
        chunk_id: &str,
        calls: &[crate::parser::CallSite],
    ) -> Result<(), StoreError> {
        let _span = tracing::info_span!("upsert_calls", count = calls.len()).entered();
        tracing::trace!(chunk_id, call_count = calls.len(), "upserting chunk calls");

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            sqlx::query("DELETE FROM calls WHERE caller_id = ?1")
                .bind(chunk_id)
                .execute(&mut *tx)
                .await?;

            if !calls.is_empty() {
                use crate::store::helpers::sql::max_rows_per_statement;
                const INSERT_BATCH: usize = max_rows_per_statement(3);
                for batch in calls.chunks(INSERT_BATCH) {
                    let mut query_builder: sqlx::QueryBuilder<sqlx::Sqlite> =
                        sqlx::QueryBuilder::new(
                            "INSERT INTO calls (caller_id, callee_name, line_number) ",
                        );
                    query_builder.push_values(batch.iter(), |mut b, call| {
                        b.push_bind(chunk_id)
                            .push_bind(&call.callee_name)
                            .push_bind(call.line_number as i64);
                    });
                    query_builder.build().execute(&mut *tx).await?;
                }
                tracing::debug!(chunk_id, call_count = calls.len(), "Inserted chunk calls");
            }

            tx.commit().await?;
            Ok(())
        })
    }

    /// Insert call sites for multiple chunks in a single transaction.
    /// Takes `(chunk_id, CallSite)` pairs and batches them into one transaction.
    pub fn upsert_calls_batch(
        &self,
        calls: &[(String, crate::parser::CallSite)],
    ) -> Result<(), StoreError> {
        let _span = tracing::info_span!("upsert_calls_batch", count = calls.len()).entered();
        if calls.is_empty() {
            return Ok(());
        }

        tracing::trace!(call_count = calls.len(), "upserting calls batch");

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            // Collect unique chunk IDs to delete old calls
            let mut seen_ids = std::collections::HashSet::new();
            for (chunk_id, _) in calls {
                if seen_ids.insert(chunk_id.as_str()) {
                    sqlx::query("DELETE FROM calls WHERE caller_id = ?1")
                        .bind(chunk_id)
                        .execute(&mut *tx)
                        .await?;
                }
            }

            use crate::store::helpers::sql::max_rows_per_statement;
            const INSERT_BATCH: usize = max_rows_per_statement(3);
            for batch in calls.chunks(INSERT_BATCH) {
                let mut query_builder: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                    "INSERT INTO calls (caller_id, callee_name, line_number) ",
                );
                query_builder.push_values(batch.iter(), |mut b, (chunk_id, call)| {
                    b.push_bind(chunk_id)
                        .push_bind(&call.callee_name)
                        .push_bind(call.line_number as i64);
                });
                query_builder.build().execute(&mut *tx).await?;
            }

            tx.commit().await?;
            Ok(())
        })
    }
}

impl<Mode> Store<Mode> {
    /// Check which chunk IDs from a set actually exist in the database.
    /// Used by periodic deferred-flush to filter calls whose FK targets are present.
    pub fn existing_chunk_ids(
        &self,
        ids: &std::collections::HashSet<&str>,
    ) -> Result<std::collections::HashSet<String>, StoreError> {
        let _span = tracing::debug_span!("existing_chunk_ids", candidates = ids.len()).entered();
        if ids.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        self.rt.block_on(async {
            let mut found = std::collections::HashSet::new();
            let id_vec: Vec<&str> = ids.iter().copied().collect();
            use crate::store::helpers::make_placeholders;
            use crate::store::helpers::sql::max_rows_per_statement;
            for batch in id_vec.chunks(max_rows_per_statement(1)) {
                // P3 #130: cached helper — `Cow::Borrowed(&'static str)` on hit.
                let placeholders = make_placeholders(batch.len());
                let sql = format!("SELECT id FROM chunks WHERE id IN ({placeholders})");
                let mut query = sqlx::query_scalar::<_, String>(&sql);
                for id in batch {
                    query = query.bind(*id);
                }
                let rows: Vec<String> = query.fetch_all(&self.pool).await?;
                found.extend(rows);
            }
            Ok(found)
        })
    }

    /// Get all function names called by a given chunk.
    /// Takes a chunk **ID** (unique) rather than a name. Returns only callee
    /// **names** (not full chunks) because:
    /// - Callees may not exist in the index (external functions)
    /// - Callers typically chain: `get_callees` → `get_callers_full` for graph traversal
    /// For richer callee data, see [`get_callers_with_context`].
    pub fn get_callees(&self, chunk_id: &str) -> Result<Vec<String>, StoreError> {
        let _span = tracing::debug_span!("get_callees", chunk_id = %chunk_id).entered();
        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT callee_name FROM calls WHERE caller_id = ?1 ORDER BY line_number",
            )
            .bind(chunk_id)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows.into_iter().map(|(s,)| s).collect())
        })
    }

    /// Retrieves aggregated statistics about function calls from the database.
    /// Queries the calls table to obtain the total number of calls and the count of distinct callees, returning this information as a CallStats structure.
    /// # Arguments
    /// * `&self` - A reference to the store instance containing the database connection pool and async runtime.
    /// # Returns
    /// Returns a `Result` containing:
    /// * `Ok(CallStats)` - A struct with `total_calls` (total number of recorded calls) and `unique_callees` (number of distinct functions called).
    /// * `Err(StoreError)` - If the database query fails.
    /// # Errors
    /// Returns `StoreError` if the SQL query execution fails or if database connectivity issues occur.
    pub fn call_stats(&self) -> Result<CallStats, StoreError> {
        let _span = tracing::debug_span!("call_stats").entered();
        self.rt.block_on(async {
            let (total_calls, unique_callees): (i64, i64) =
                sqlx::query_as("SELECT COUNT(*), COUNT(DISTINCT callee_name) FROM calls")
                    .fetch_one(&self.pool)
                    .await?;

            Ok(CallStats {
                total_calls: total_calls as u64,
                unique_callees: unique_callees as u64,
            })
        })
    }
}

impl Store<ReadWrite> {
    // ============ Full Call Graph Methods (v5) ============

    /// Insert function calls for a file (full call graph, no size limits)
    pub fn upsert_function_calls(
        &self,
        file: &Path,
        function_calls: &[crate::parser::FunctionCalls],
    ) -> Result<(), StoreError> {
        let _span =
            tracing::info_span!("upsert_function_calls", count = function_calls.len()).entered();
        let file_str = crate::normalize_path(file);
        let total_calls: usize = function_calls.iter().map(|fc| fc.calls.len()).sum();
        tracing::trace!(
            file = %file_str,
            functions = function_calls.len(),
            total_calls,
            "upserting function calls"
        );

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            sqlx::query("DELETE FROM function_calls WHERE file = ?1")
                .bind(&file_str)
                .execute(&mut *tx)
                .await?;

            // Flatten all calls and batch insert (instead of N individual inserts)
            let all_calls: Vec<_> = function_calls
                .iter()
                .flat_map(|fc| {
                    fc.calls.iter().map(move |call| {
                        (&fc.name, fc.line_start, &call.callee_name, call.line_number)
                    })
                })
                .collect();

            if !all_calls.is_empty() {
                use crate::store::helpers::sql::max_rows_per_statement;
                const INSERT_BATCH: usize = max_rows_per_statement(5);
                for batch in all_calls.chunks(INSERT_BATCH) {
                    let mut query_builder: sqlx::QueryBuilder<sqlx::Sqlite> =
                        sqlx::QueryBuilder::new(
                            "INSERT INTO function_calls (file, caller_name, caller_line, callee_name, call_line) ",
                        );
                    query_builder.push_values(batch.iter(), |mut b, (caller_name, caller_line, callee_name, call_line)| {
                        b.push_bind(&file_str)
                            .push_bind(*caller_name)
                            .push_bind(*caller_line as i64)
                            .push_bind(*callee_name)
                            .push_bind(*call_line as i64);
                    });
                    query_builder.build().execute(&mut *tx).await?;
                }
                tracing::info!(
                    file = %file_str,
                    functions = function_calls.len(),
                    calls = all_calls.len(),
                    "Indexed function calls"
                );
            }

            tx.commit().await?;
            Ok(())
        })
    }

    /// Insert function calls for multiple files in a single transaction.
    ///
    /// P2 #64 (recovery wave): mirrors the existing `upsert_type_edges_for_files`
    /// pattern. The previous CLI hot path (`pipeline/upsert.rs:152-163`) called
    /// `upsert_function_calls(file, calls)` once per file, opening one
    /// transaction per file. On a 2,500-file project this was 2,500 separate
    /// `BEGIN ... COMMIT` round-trips; batching to one transaction is the same
    /// shape we already use for type edges.
    ///
    /// Inside the single transaction:
    /// - Batched DELETE WHERE file IN (?, ?, ?) for all files in the batch
    ///   (chunked under the SQLite parameter limit).
    /// - Batched multi-row INSERT for all rows from all files (5 binds per row,
    ///   chunked under the same parameter limit).
    pub fn upsert_function_calls_for_files(
        &self,
        entries: &[(PathBuf, Vec<crate::parser::FunctionCalls>)],
    ) -> Result<(), StoreError> {
        let total_files = entries.len();
        let _span =
            tracing::info_span!("upsert_function_calls_for_files", files = total_files).entered();
        if entries.is_empty() {
            return Ok(());
        }

        // Pre-normalize file paths so the borrow lives for the whole tx.
        let file_strs: Vec<String> = entries
            .iter()
            .map(|(file, _)| crate::normalize_path(file))
            .collect();

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            use crate::store::helpers::sql::max_rows_per_statement;

            // Phase 1: batched DELETE WHERE file IN (?, ?, ?) — one bind per
            // file, chunked under the SQLite param limit.
            const DELETE_PER_STMT: usize = max_rows_per_statement(1);
            for chunk in file_strs.chunks(DELETE_PER_STMT) {
                let placeholders = crate::store::helpers::make_placeholders(chunk.len());
                let sql =
                    format!("DELETE FROM function_calls WHERE file IN ({})", placeholders);
                let mut q = sqlx::query(&sql);
                for fs in chunk {
                    q = q.bind(fs);
                }
                q.execute(&mut *tx).await?;
            }

            // Phase 2: collect all rows tagged with their file string, then
            // batched multi-row INSERT.
            let mut all_rows: Vec<(&str, &str, u32, &str, u32)> = Vec::new();
            for ((_file, function_calls), file_str) in entries.iter().zip(file_strs.iter()) {
                for fc in function_calls {
                    for call in &fc.calls {
                        all_rows.push((
                            file_str.as_str(),
                            fc.name.as_str(),
                            fc.line_start,
                            call.callee_name.as_str(),
                            call.line_number,
                        ));
                    }
                }
            }

            if !all_rows.is_empty() {
                const INSERT_BATCH: usize = max_rows_per_statement(5);
                for batch in all_rows.chunks(INSERT_BATCH) {
                    let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                        "INSERT INTO function_calls (file, caller_name, caller_line, callee_name, call_line) ",
                    );
                    qb.push_values(
                        batch.iter(),
                        |mut b, (file, caller_name, caller_line, callee_name, call_line)| {
                            b.push_bind(*file)
                                .push_bind(*caller_name)
                                .push_bind(*caller_line as i64)
                                .push_bind(*callee_name)
                                .push_bind(*call_line as i64);
                        },
                    );
                    qb.build().execute(&mut *tx).await?;
                }
            }

            tx.commit().await?;
            tracing::info!(
                files = total_files,
                rows = all_rows.len(),
                "Batch-indexed function calls"
            );
            Ok(())
        })
    }
}
