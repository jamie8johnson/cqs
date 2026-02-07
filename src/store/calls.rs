//! Call graph storage and queries

use std::path::{Path, PathBuf};

use sqlx::Row;

use super::helpers::{
    clamp_line_number, CallGraph, CallerInfo, CallerWithContext, ChunkRow, ChunkSummary, StoreError,
};
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

    /// Insert call sites for multiple chunks in a single transaction.
    ///
    /// Takes `(chunk_id, CallSite)` pairs and batches them into one transaction.
    pub fn upsert_calls_batch(
        &self,
        calls: &[(String, crate::parser::CallSite)],
    ) -> Result<(), StoreError> {
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

            // Batch insert all calls
            let mut query_builder: sqlx::QueryBuilder<sqlx::Sqlite> =
                sqlx::QueryBuilder::new("INSERT INTO calls (caller_id, callee_name, line_number) ");
            query_builder.push_values(calls.iter(), |mut b, (chunk_id, call)| {
                b.push_bind(chunk_id)
                    .push_bind(&call.callee_name)
                    .push_bind(call.line_number as i64);
            });
            query_builder.build().execute(&mut *tx).await?;

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
            let (total_calls, unique_callees): (i64, i64) =
                sqlx::query_as("SELECT COUNT(*), COUNT(DISTINCT callee_name) FROM calls")
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

    /// Load the entire call graph as forward + reverse adjacency lists.
    ///
    /// Single SQL scan of `function_calls`. Typically ~2000 edges, fits in memory trivially.
    /// Used by trace (forward BFS), impact (reverse BFS), and test-map (reverse BFS).
    pub fn get_call_graph(&self) -> Result<CallGraph, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT caller_name, callee_name FROM function_calls")
                    .fetch_all(&self.pool)
                    .await?;

            let mut forward: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            let mut reverse: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();

            for (caller, callee) in rows {
                forward
                    .entry(caller.clone())
                    .or_default()
                    .push(callee.clone());
                reverse.entry(callee).or_default().push(caller);
            }

            Ok(CallGraph { forward, reverse })
        })
    }

    /// Find callers with call-site line numbers for impact analysis.
    ///
    /// Returns the caller function name, file, start line, and the specific line
    /// where the call to `callee_name` occurs.
    pub fn get_callers_with_context(
        &self,
        callee_name: &str,
    ) -> Result<Vec<CallerWithContext>, StoreError> {
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

    /// Find functions/methods never called by indexed code (dead code detection).
    ///
    /// Returns two lists:
    /// - `confident`: Functions with no callers that are likely dead
    /// - `possibly_dead_pub`: Public functions with no callers (may be used externally)
    ///
    /// Exclusions applied:
    /// - `main` entry point
    /// - Test functions (via `find_test_chunks()` heuristics)
    /// - Functions in test files
    /// - Trait implementations (dynamic dispatch invisible to call graph)
    /// - `#[no_mangle]` functions (FFI)
    pub fn find_dead_code(
        &self,
        include_pub: bool,
    ) -> Result<(Vec<ChunkSummary>, Vec<ChunkSummary>), StoreError> {
        self.rt.block_on(async {
            // Get all functions/methods with no callers (top-level only, not windowed chunks)
            let rows: Vec<_> = sqlx::query(
                "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature,
                        c.content, c.doc, c.line_start, c.line_end, c.parent_id
                 FROM chunks c
                 WHERE c.chunk_type IN ('function', 'method')
                   AND c.name NOT IN (SELECT DISTINCT callee_name FROM function_calls)
                   AND c.parent_id IS NULL
                 ORDER BY c.origin, c.line_start",
            )
            .fetch_all(&self.pool)
            .await?;

            let all_uncalled: Vec<ChunkSummary> = rows
                .into_iter()
                .map(|row| ChunkSummary::from(ChunkRow::from_row(&row)))
                .collect();

            // Build test name set for exclusion
            let test_names: std::collections::HashSet<String> = self
                .find_test_chunks_async()
                .await?
                .into_iter()
                .map(|c| c.name)
                .collect();

            let mut confident = Vec::new();
            let mut possibly_dead_pub = Vec::new();

            for chunk in all_uncalled {
                // Skip main entry point
                if chunk.name == "main" {
                    continue;
                }

                // Skip test functions
                if test_names.contains(&chunk.name) {
                    continue;
                }

                // Skip functions in test files
                let path_str = chunk.file.to_string_lossy();
                if path_str.contains("/tests/")
                    || path_str.contains("_test.")
                    || path_str.contains(".test.")
                    || path_str.contains(".spec.")
                {
                    continue;
                }

                // Skip trait implementations (content contains "impl ... for ...")
                if chunk.content.contains(" for ")
                    && chunk.chunk_type == crate::parser::ChunkType::Method
                {
                    continue;
                }

                // Skip #[no_mangle] FFI functions
                if chunk.content.contains("no_mangle") {
                    continue;
                }

                // Check if public
                let is_pub = chunk.content.starts_with("pub ")
                    || chunk.content.starts_with("pub(")
                    || chunk.signature.starts_with("pub ")
                    || chunk.signature.starts_with("pub(");

                if is_pub && !include_pub {
                    possibly_dead_pub.push(chunk);
                } else {
                    confident.push(chunk);
                }
            }

            Ok((confident, possibly_dead_pub))
        })
    }

    /// Async helper for find_test_chunks (reused by find_dead_code)
    async fn find_test_chunks_async(&self) -> Result<Vec<ChunkSummary>, StoreError> {
        let rows: Vec<_> = sqlx::query(
            "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                    line_start, line_end, parent_id
             FROM chunks
             WHERE chunk_type IN ('function', 'method')
               AND (
                 name LIKE 'test_%'
                 OR name LIKE 'Test%'
                 OR content LIKE '%#[test]%'
                 OR content LIKE '%@Test%'
                 OR origin LIKE '%/tests/%'
                 OR origin LIKE '%\\_test.%' ESCAPE '\\'
                 OR origin LIKE '%.test.%'
                 OR origin LIKE '%.spec.%'
                 OR origin LIKE '%_test.go'
                 OR origin LIKE '%_test.py'
               )
             ORDER BY origin, line_start",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| ChunkSummary::from(ChunkRow::from_row(&row)))
            .collect())
    }

    /// Delete function_calls for files no longer in the chunks table.
    ///
    /// Used by GC to clean up orphaned call graph entries after pruning chunks.
    pub fn prune_stale_calls(&self) -> Result<u64, StoreError> {
        self.rt.block_on(async {
            let result = sqlx::query(
                "DELETE FROM function_calls WHERE file NOT IN (SELECT DISTINCT origin FROM chunks)",
            )
            .execute(&self.pool)
            .await?;
            Ok(result.rows_affected())
        })
    }

    /// Find test chunks using language-specific heuristics.
    ///
    /// Identifies test functions across all 7 supported languages by:
    /// - Name patterns: `test_*` (Rust/Python), `Test*` (Go)
    /// - Content patterns: `#[test]` (Rust), `@Test` (Java)
    /// - Path patterns: `/tests/`, `_test.rs`, `.test.ts`, `.spec.js`, `_test.go`
    ///
    /// Uses a broad SQL filter then Rust post-filter for precision.
    pub fn find_test_chunks(&self) -> Result<Vec<ChunkSummary>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                        line_start, line_end, parent_id
                 FROM chunks
                 WHERE chunk_type IN ('function', 'method')
                   AND (
                     name LIKE 'test_%'
                     OR name LIKE 'Test%'
                     OR content LIKE '%#[test]%'
                     OR content LIKE '%@Test%'
                     OR origin LIKE '%/tests/%'
                     OR origin LIKE '%\\_test.%' ESCAPE '\\'
                     OR origin LIKE '%.test.%'
                     OR origin LIKE '%.spec.%'
                     OR origin LIKE '%_test.go'
                     OR origin LIKE '%_test.py'
                   )
                 ORDER BY origin, line_start",
            )
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|row| ChunkSummary::from(ChunkRow::from_row(&row)))
                .collect())
        })
    }

    /// Get full call graph statistics
    pub fn function_call_stats(&self) -> Result<(u64, u64, u64), StoreError> {
        self.rt.block_on(async {
            let (total_calls, unique_callers, unique_callees): (i64, i64, i64) = sqlx::query_as(
                "SELECT COUNT(*), COUNT(DISTINCT caller_name), COUNT(DISTINCT callee_name) FROM function_calls",
            )
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
