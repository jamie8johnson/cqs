//! Call graph storage and queries

use std::path::{Path, PathBuf};

use sqlx::Row;

use super::helpers::{clamp_line_number, CallerInfo, ChunkRow, ChunkSummary, StoreError};
use super::Store;

impl Store {
    /// Insert or replace call sites for a chunk
    pub fn upsert_calls(
        &self,
        chunk_id: &str,
        calls: &[crate::parser::CallSite],
    ) -> Result<(), StoreError> {
        tracing::trace!(chunk_id, call_count = calls.len(), "upserting chunk calls");

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            sqlx::query("DELETE FROM calls WHERE caller_id = ?1")
                .bind(chunk_id)
                .execute(&mut *tx)
                .await?;

            // Batch insert all calls at once (instead of N individual inserts)
            if !calls.is_empty() {
                let mut query_builder: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                    "INSERT INTO calls (caller_id, callee_name, line_number) ",
                );
                query_builder.push_values(calls.iter(), |mut b, call| {
                    b.push_bind(chunk_id)
                        .push_bind(&call.callee_name)
                        .push_bind(call.line_number as i64);
                });
                query_builder.build().execute(&mut *tx).await?;
                tracing::debug!(chunk_id, call_count = calls.len(), "Inserted chunk calls");
            }

            tx.commit().await?;
            Ok(())
        })
    }

    /// Find all chunks that call a given function name
    pub fn get_callers(&self, callee_name: &str) -> Result<Vec<ChunkSummary>, StoreError> {
        tracing::debug!(callee_name, "querying callers from chunks");

        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT DISTINCT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature,
                        c.content, c.doc, c.line_start, c.line_end, c.parent_id
                 FROM chunks c
                 JOIN calls ca ON c.id = ca.caller_id
                 WHERE ca.callee_name = ?1
                 ORDER BY c.origin, c.line_start",
            )
            .bind(callee_name)
            .fetch_all(&self.pool)
            .await?;

            let chunks: Vec<ChunkSummary> = rows
                .into_iter()
                .map(|row| {
                    ChunkSummary::from(ChunkRow {
                        id: row.get(0),
                        origin: row.get(1),
                        language: row.get(2),
                        chunk_type: row.get(3),
                        name: row.get(4),
                        signature: row.get(5),
                        content: row.get(6),
                        doc: row.get(7),
                        line_start: clamp_line_number(row.get::<i64, _>(8)),
                        line_end: clamp_line_number(row.get::<i64, _>(9)),
                        parent_id: row.get(10),
                    })
                })
                .collect();

            Ok(chunks)
        })
    }

    /// Get all function names called by a given chunk
    pub fn get_callees(&self, chunk_id: &str) -> Result<Vec<String>, StoreError> {
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

    /// Get call graph statistics
    pub fn call_stats(&self) -> Result<(u64, u64), StoreError> {
        self.rt.block_on(async {
            let (total_calls,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM calls")
                .fetch_one(&self.pool)
                .await?;
            let (unique_callees,): (i64,) =
                sqlx::query_as("SELECT COUNT(DISTINCT callee_name) FROM calls")
                    .fetch_one(&self.pool)
                    .await?;

            Ok((total_calls as u64, unique_callees as u64))
        })
    }

    // ============ Full Call Graph Methods (v5) ============

    /// Insert function calls for a file (full call graph, no size limits)
    pub fn upsert_function_calls(
        &self,
        file: &Path,
        function_calls: &[crate::parser::FunctionCalls],
    ) -> Result<(), StoreError> {
        let file_str = file.to_string_lossy().into_owned();
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
                let mut query_builder: sqlx::QueryBuilder<sqlx::Sqlite> =
                    sqlx::QueryBuilder::new(
                        "INSERT INTO function_calls (file, caller_name, caller_line, callee_name, call_line) ",
                    );
                query_builder.push_values(all_calls.iter(), |mut b, (caller_name, caller_line, callee_name, call_line)| {
                    b.push_bind(&file_str)
                        .push_bind(*caller_name)
                        .push_bind(*caller_line as i64)
                        .push_bind(*callee_name)
                        .push_bind(*call_line as i64);
                });
                query_builder.build().execute(&mut *tx).await?;
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

    /// Find all callers of a function (from full call graph)
    pub fn get_callers_full(&self, callee_name: &str) -> Result<Vec<CallerInfo>, StoreError> {
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
    pub fn get_callees_full(&self, caller_name: &str) -> Result<Vec<(String, u32)>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT DISTINCT callee_name, call_line
                 FROM function_calls
                 WHERE caller_name = ?1
                 ORDER BY call_line",
            )
            .bind(caller_name)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(name, line)| (name, clamp_line_number(line)))
                .collect())
        })
    }

    /// Get full call graph statistics
    pub fn function_call_stats(&self) -> Result<(u64, u64, u64), StoreError> {
        self.rt.block_on(async {
            let (total_calls,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM function_calls")
                .fetch_one(&self.pool)
                .await?;
            let (unique_callers,): (i64,) =
                sqlx::query_as("SELECT COUNT(DISTINCT caller_name) FROM function_calls")
                    .fetch_one(&self.pool)
                    .await?;
            let (unique_callees,): (i64,) =
                sqlx::query_as("SELECT COUNT(DISTINCT callee_name) FROM function_calls")
                    .fetch_one(&self.pool)
                    .await?;

            Ok((
                total_calls as u64,
                unique_callers as u64,
                unique_callees as u64,
            ))
        })
    }
}
