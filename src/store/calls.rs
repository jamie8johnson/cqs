//! Call graph storage and queries

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use sqlx::Row;

use super::helpers::{
    clamp_line_number, CallGraph, CallerInfo, CallerWithContext, ChunkRow, ChunkSummary, StoreError,
};
use super::Store;

/// Statistics about call graph entries (chunk-level calls table)
#[derive(Debug, Clone, Default)]
pub struct CallStats {
    /// Total number of call edges
    pub total_calls: u64,
    /// Number of distinct callee names
    pub unique_callees: u64,
}

/// Detailed function call statistics (function_calls table)
#[derive(Debug, Clone, Default)]
pub struct FunctionCallStats {
    /// Total number of call edges
    pub total_calls: u64,
    /// Number of distinct caller function names
    pub unique_callers: u64,
    /// Number of distinct callee function names
    pub unique_callees: u64,
}

/// Matches `impl SomeTrait for SomeType` patterns to detect trait implementations.
/// Used by `find_dead_code` to skip trait impl methods (invisible to static call graph).
static TRAIT_IMPL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"impl\s+\w+\s+for\s+").expect("hardcoded regex"));

/// Test function/method name patterns (SQL LIKE syntax).
/// Matches naming conventions: `test_*` (Rust/Python), `Test*` (Go).
const TEST_NAME_PATTERNS: &[&str] = &["test_%", "Test%"];

/// Test content markers — language-specific annotations/decorators.
/// Detected via `content LIKE '%marker%'` in SQL.
const TEST_CONTENT_MARKERS: &[&str] = &["#[test]", "@Test"];

/// Test path patterns — directories and file suffixes (SQL LIKE syntax).
/// Uses ESCAPE '\\' for literal underscores.
const TEST_PATH_PATTERNS: &[&str] = &[
    "%/tests/%",
    "%\\_test.%",
    "%.test.%",
    "%.spec.%",
    "%_test.go",
    "%_test.py",
];

/// Well-known trait method names across languages.
///
/// Methods with these names inside `impl` blocks are almost always trait implementations
/// that won't appear in the static call graph (called via dynamic dispatch).
/// Used as a fallback when `TRAIT_IMPL_RE` can't match (method chunks don't include
/// the enclosing `impl Trait for Type` header).
const TRAIT_METHOD_NAMES: &[&str] = &[
    // std::fmt
    "fmt",
    // std::convert
    "from",
    "into",
    "try_from",
    "try_into",
    // std::ops
    "deref",
    "deref_mut",
    "drop",
    "index",
    "index_mut",
    "add",
    "sub",
    "mul",
    "div",
    "rem",
    "neg",
    "not",
    "bitor",
    "bitand",
    "bitxor",
    "shl",
    "shr",
    // std::cmp
    "eq",
    "ne",
    "partial_cmp",
    "cmp",
    // std::hash
    "hash",
    // std::clone
    "clone",
    "clone_from",
    // std::default
    "default",
    // std::iter
    "next",
    "into_iter",
    // std::io
    "read",
    "write",
    "flush",
    // std::str
    "from_str",
    // std::convert / std::borrow
    "as_ref",
    "as_mut",
    "borrow",
    "borrow_mut",
    // serde
    "serialize",
    "deserialize",
    // std::error
    "source",
    // std::future
    "poll",
];

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
    pub fn call_stats(&self) -> Result<CallStats, StoreError> {
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
    ///
    /// When `file` is provided, scopes to callees of that function in that specific file.
    /// When `None`, returns callees across all files (backwards compatible, but ambiguous
    /// for common names like `new`, `parse`, `from_str`).
    pub fn get_callees_full(
        &self,
        caller_name: &str,
        file: Option<&str>,
    ) -> Result<Vec<(String, u32)>, StoreError> {
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
                reverse
                    .entry(callee.clone())
                    .or_default()
                    .push(caller.clone());
                forward.entry(caller).or_default().push(callee);
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
    /// Uses two-phase query: lightweight metadata first, then content only for
    /// candidates that pass name/test/path filters (avoids loading large function bodies).
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
            // Phase 1: Lightweight query without content/doc
            let rows: Vec<_> = sqlx::query(
                "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature,
                        c.line_start, c.line_end, c.parent_id
                 FROM chunks c
                 WHERE c.chunk_type IN ('function', 'method')
                   AND c.name NOT IN (SELECT DISTINCT callee_name FROM function_calls)
                   AND c.parent_id IS NULL
                 ORDER BY c.origin, c.line_start",
            )
            .fetch_all(&self.pool)
            .await?;

            // Build lightweight summaries (no content/doc yet)
            struct LightChunk {
                id: String,
                file: PathBuf,
                language: String,
                chunk_type: String,
                name: String,
                signature: String,
                line_start: u32,
                line_end: u32,
            }

            let all_uncalled: Vec<LightChunk> = rows
                .into_iter()
                .map(|row| LightChunk {
                    id: row.get(0),
                    file: PathBuf::from(row.get::<String, _>(1)),
                    language: row.get(2),
                    chunk_type: row.get(3),
                    name: row.get(4),
                    signature: row.get(5),
                    line_start: clamp_line_number(row.get::<i64, _>(6)),
                    line_end: clamp_line_number(row.get::<i64, _>(7)),
                })
                .collect();

            let total_uncalled = all_uncalled.len();

            // Build test name set for exclusion
            let test_names: std::collections::HashSet<String> = self
                .find_test_chunks_async()
                .await?
                .into_iter()
                .map(|c| c.name)
                .collect();

            // Phase 1 filtering: name/test/path checks (don't need content)
            let mut candidates: Vec<LightChunk> = Vec::new();

            for chunk in all_uncalled {
                if chunk.name == "main" {
                    continue;
                }
                if test_names.contains(&chunk.name) {
                    continue;
                }
                let path_str = chunk.file.to_string_lossy();
                if path_str.contains("/tests/")
                    || path_str.contains("_test.")
                    || path_str.contains(".test.")
                    || path_str.contains(".spec.")
                {
                    continue;
                }

                // Methods with well-known trait names can be skipped without content
                if chunk.chunk_type == "method" && TRAIT_METHOD_NAMES.contains(&chunk.name.as_str())
                {
                    continue;
                }

                // Signature-only trait impl check
                if chunk.chunk_type == "method" && TRAIT_IMPL_RE.is_match(&chunk.signature) {
                    continue;
                }

                candidates.push(chunk);
            }

            // Phase 2: Batch-fetch content for remaining candidates
            let candidate_ids: Vec<String> = candidates.iter().map(|c| c.id.clone()).collect();
            let mut content_map: std::collections::HashMap<String, (String, Option<String>)> =
                std::collections::HashMap::new();

            const BATCH_SIZE: usize = 500;
            for batch in candidate_ids.chunks(BATCH_SIZE) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT id, content, doc FROM chunks WHERE id IN ({})",
                    placeholders
                );
                let mut q = sqlx::query(&sql);
                for id in batch {
                    q = q.bind(id);
                }
                let rows: Vec<_> = q.fetch_all(&self.pool).await?;
                for row in rows {
                    let id: String = row.get(0);
                    let content: String = row.get(1);
                    let doc: Option<String> = row.get(2);
                    content_map.insert(id, (content, doc));
                }
            }

            // Phase 2 filtering with content
            let mut confident = Vec::new();
            let mut possibly_dead_pub = Vec::new();

            for light in candidates {
                let (content, doc) = content_map
                    .remove(&light.id)
                    .unwrap_or_else(|| (String::new(), None));

                // Content-based trait impl check for methods
                if light.chunk_type == "method" && TRAIT_IMPL_RE.is_match(&content) {
                    continue;
                }

                // Skip #[no_mangle] FFI functions
                if content.contains("no_mangle") {
                    continue;
                }

                // Check if public
                let is_pub = content.starts_with("pub ")
                    || content.starts_with("pub(")
                    || light.signature.starts_with("pub ")
                    || light.signature.starts_with("pub(");

                let chunk = ChunkSummary::from(ChunkRow {
                    id: light.id,
                    origin: light.file.to_string_lossy().into_owned(),
                    language: light.language,
                    chunk_type: light.chunk_type,
                    name: light.name,
                    signature: light.signature,
                    content,
                    doc,
                    line_start: light.line_start,
                    line_end: light.line_end,
                    parent_id: None,
                });

                if is_pub && !include_pub {
                    possibly_dead_pub.push(chunk);
                } else {
                    confident.push(chunk);
                }
            }

            tracing::debug!(
                total_uncalled,
                confident = confident.len(),
                possibly_dead = possibly_dead_pub.len(),
                "Dead code analysis complete"
            );

            Ok((confident, possibly_dead_pub))
        })
    }

    /// Async helper for find_test_chunks (reused by find_dead_code)
    async fn find_test_chunks_async(&self) -> Result<Vec<ChunkSummary>, StoreError> {
        // Build OR clauses from centralized test pattern constants
        let mut clauses: Vec<String> = Vec::new();
        for pat in TEST_NAME_PATTERNS {
            clauses.push(format!("name LIKE '{pat}'"));
        }
        for marker in TEST_CONTENT_MARKERS {
            clauses.push(format!("content LIKE '%{marker}%'"));
        }
        for pat in TEST_PATH_PATTERNS {
            if pat.contains("\\_") {
                clauses.push(format!("origin LIKE '{pat}' ESCAPE '\\'"));
            } else {
                clauses.push(format!("origin LIKE '{pat}'"));
            }
        }
        let filter = clauses.join("\n                 OR ");

        let sql = format!(
            "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                    line_start, line_end, parent_id
             FROM chunks
             WHERE chunk_type IN ('function', 'method')
               AND (
                 {filter}
               )
             ORDER BY origin, line_start"
        );

        let rows: Vec<_> = sqlx::query(&sql).fetch_all(&self.pool).await?;

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
            let count = result.rows_affected();
            if count > 0 {
                tracing::info!(pruned = count, "Pruned stale call graph entries");
            }
            Ok(count)
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
        self.rt.block_on(self.find_test_chunks_async())
    }

    /// Get full call graph statistics
    pub fn function_call_stats(&self) -> Result<FunctionCallStats, StoreError> {
        self.rt.block_on(async {
            let (total_calls, unique_callers, unique_callees): (i64, i64, i64) = sqlx::query_as(
                "SELECT COUNT(*), COUNT(DISTINCT caller_name), COUNT(DISTINCT callee_name) FROM function_calls",
            )
            .fetch_one(&self.pool)
            .await?;

            Ok(FunctionCallStats {
                total_calls: total_calls as u64,
                unique_callers: unique_callers as u64,
                unique_callees: unique_callees as u64,
            })
        })
    }
}
