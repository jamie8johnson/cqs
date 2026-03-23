//! Call graph upsert, delete, batch operations, and basic stats.

use std::path::Path;

use super::CallStats;
use crate::store::helpers::StoreError;
use crate::store::Store;

impl Store {
    /// Insert or replace call sites for a chunk
    pub fn upsert_calls(
        &self,
        chunk_id: &str,
        calls: &[crate::parser::CallSite],
    ) -> Result<(), StoreError> {
        let _span = tracing::info_span!("upsert_calls", count = calls.len()).entered();
        tracing::trace!(chunk_id, call_count = calls.len(), "upserting chunk calls");

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            sqlx::query("DELETE FROM calls WHERE caller_id = ?1")
                .bind(chunk_id)
                .execute(&mut *tx)
                .await?;

            // Batch insert calls (300 rows * 3 binds = 900 < SQLite's 999 limit)
            if !calls.is_empty() {
                const INSERT_BATCH: usize = 300;
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
    ///
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
            let mut tx = self.pool.begin().await?;

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

            // Batch insert all calls (300 rows * 3 binds = 900 < SQLite's 999 limit)
            const INSERT_BATCH: usize = 300;
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

    /// Get all function names called by a given chunk.
    ///
    /// Takes a chunk **ID** (unique) rather than a name. Returns only callee
    /// **names** (not full chunks) because:
    /// - Callees may not exist in the index (external functions)
    /// - Callers typically chain: `get_callees` → `get_callers_full` for graph traversal
    ///
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
    ///
    /// Queries the calls table to obtain the total number of calls and the count of distinct callees, returning this information as a CallStats structure.
    ///
    /// # Arguments
    ///
    /// * `&self` - A reference to the store instance containing the database connection pool and async runtime.
    ///
    /// # Returns
    ///
    /// Returns a `Result` containing:
    /// * `Ok(CallStats)` - A struct with `total_calls` (total number of recorded calls) and `unique_callees` (number of distinct functions called).
    /// * `Err(StoreError)` - If the database query fails.
    ///
    /// # Errors
    ///
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
            let mut tx = self.pool.begin().await?;

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
                // 190 rows * 5 binds = 950 < SQLite's 999 limit
                const INSERT_BATCH: usize = 190;
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
}
