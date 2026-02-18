//! Call graph storage and queries

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use sqlx::Row;

use super::helpers::{
    clamp_line_number, CallGraph, CallerInfo, CallerWithContext, ChunkRow, ChunkSummary, StoreError,
};
use super::Store;
use crate::parser::{ChunkType, Language};

/// A dead function with confidence scoring.
///
/// Wraps a `ChunkSummary` with a confidence level indicating how likely
/// the function is truly dead (not just invisible to static analysis).
#[derive(Debug, Clone)]
pub struct DeadFunction {
    /// The code chunk (function/method metadata + content)
    pub chunk: ChunkSummary,
    /// How confident we are that this function is dead
    pub confidence: DeadConfidence,
}

/// Confidence level for dead code detection.
///
/// Ordered from least to most confident, enabling `>=` filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeadConfidence {
    /// Likely a false positive (methods, functions in active files)
    Low,
    /// Possibly dead but uncertain (private functions in active files)
    Medium,
    /// Almost certainly dead (private, in files with no callers)
    High,
}

/// Well-known entry point names excluded from dead code detection.
///
/// These functions are called by the runtime, framework, or build system
/// rather than by other indexed code.
const ENTRY_POINT_NAMES: &[&str] = &[
    "main",
    "init",
    "__init__",
    "setup",
    "teardown",
    "beforeEach",
    "afterEach",
    "beforeAll",
    "afterAll",
    "handler",
    "middleware",
];

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
    // Common constructor/builder patterns (often called via type, not name)
    "new",
    "build",
    "builder",
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

    /// Get all chunks that call the named function.
    ///
    /// Takes a function **name** (not ID) because multiple definitions may share
    /// a name across files. Returns full [`ChunkSummary`] with file, line, and
    /// signature context for display.
    ///
    /// For the reverse direction, see [`get_callees`] which returns callee names
    /// from a specific chunk ID.
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

    /// Get all function names called by a given chunk.
    ///
    /// Takes a chunk **ID** (unique) rather than a name. Returns only callee
    /// **names** (not full chunks) because:
    /// - Callees may not exist in the index (external functions)
    /// - Callers typically chain: `get_callees` → `get_callers` for graph traversal
    ///
    /// For richer callee data, see [`get_callers_with_context`].
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
        let file_str = file.to_string_lossy().replace('\\', "/");
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

    /// Load the call graph as forward + reverse adjacency lists.
    ///
    /// Single SQL scan of `function_calls`, capped at 500K edges to prevent OOM
    /// on adversarial databases. Typical projects have ~2000 edges.
    /// Used by trace (forward BFS), impact (reverse BFS), and test-map (reverse BFS).
    pub fn get_call_graph(&self) -> Result<CallGraph, StoreError> {
        self.rt.block_on(async {
            const MAX_CALL_GRAPH_EDGES: i64 = 500_000;
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT caller_name, callee_name FROM function_calls LIMIT ?1")
                    .bind(MAX_CALL_GRAPH_EDGES)
                    .fetch_all(&self.pool)
                    .await?;

            let mut forward: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            let mut reverse: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();

            // Each string is cloned exactly once: callee is cloned for the reverse
            // entry key (original moved into forward's Vec), caller is cloned for the
            // reverse Vec (original moved into forward's entry key). This is optimal
            // for HashMap<String, Vec<String>> — Arc<str> would add indirection cost
            // that exceeds the clone savings for typical call graph sizes (~2K edges).
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

    /// Batch-fetch callers with context for multiple callee names.
    ///
    /// Returns `callee_name -> Vec<CallerWithContext>` using a single
    /// `WHERE callee_name IN (...)` query per batch of 500 names.
    /// Avoids N+1 `get_callers_with_context` calls in diff impact analysis.
    pub fn get_callers_with_context_batch(
        &self,
        callee_names: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<CallerWithContext>>, StoreError> {
        if callee_names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: std::collections::HashMap<String, Vec<CallerWithContext>> =
                std::collections::HashMap::new();

            const BATCH_SIZE: usize = 200; // 200 names * 5 cols = 1000 binds, but we only bind names
            for batch in callee_names.chunks(BATCH_SIZE) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
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
    ///
    /// Returns `callee_name -> Vec<CallerInfo>` using a single
    /// `WHERE callee_name IN (...)` query per batch of 500 names.
    /// Avoids N+1 `get_callers_full` calls in the context command.
    pub fn get_callers_full_batch(
        &self,
        callee_names: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<CallerInfo>>, StoreError> {
        if callee_names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: std::collections::HashMap<String, Vec<CallerInfo>> =
                std::collections::HashMap::new();

            const BATCH_SIZE: usize = 250; // 250 * 4 cols = 1000, but only binding names
            for batch in callee_names.chunks(BATCH_SIZE) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
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
    ///
    /// Returns `caller_name -> Vec<(callee_name, call_line)>` using a single
    /// `WHERE caller_name IN (...)` query per batch of 500 names.
    /// Avoids N+1 `get_callees_full` calls in the context command.
    ///
    /// Unlike [`get_callees_full`], does not support file scoping — returns
    /// callees across all files. This is acceptable for the context command
    /// which later filters by origin.
    pub fn get_callees_full_batch(
        &self,
        caller_names: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<(String, u32)>>, StoreError> {
        if caller_names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: std::collections::HashMap<String, Vec<(String, u32)>> =
                std::collections::HashMap::new();

            const BATCH_SIZE: usize = 250;
            for batch in caller_names.chunks(BATCH_SIZE) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
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

    /// Find functions/methods never called by indexed code (dead code detection).
    ///
    /// Returns two lists:
    /// - `confident`: Functions with no callers that are likely dead (with confidence scores)
    /// - `possibly_dead_pub`: Public functions with no callers (may be used externally)
    ///
    /// Uses two-phase query: lightweight metadata first, then content only for
    /// candidates that pass name/test/path filters (avoids loading large function bodies).
    ///
    /// Exclusions applied:
    /// - Entry point names (`main`, `init`, `handler`, etc.)
    /// - Test functions (via `find_test_chunks()` heuristics)
    /// - Functions in test files
    /// - Trait implementations (dynamic dispatch invisible to call graph)
    /// - `#[no_mangle]` functions (FFI)
    ///
    /// Confidence scoring:
    /// - **High**: Private function in a file where no other function has callers
    /// - **Medium**: Private function in an active file (other functions are called)
    /// - **Low**: Method, or function with constructor-like name patterns
    pub fn find_dead_code(
        &self,
        include_pub: bool,
    ) -> Result<(Vec<DeadFunction>, Vec<DeadFunction>), StoreError> {
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
                language: Language,
                chunk_type: ChunkType,
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
                    language: {
                        let raw: String = row.get(2);
                        raw.parse().unwrap_or_else(|_| {
                            tracing::warn!(raw = %raw, "Unknown language in DB, defaulting to Rust");
                            Language::Rust
                        })
                    },
                    chunk_type: {
                        let raw: String = row.get(3);
                        raw.parse().unwrap_or_else(|_| {
                            tracing::warn!(raw = %raw, "Unknown chunk_type in DB, defaulting to Function");
                            ChunkType::Function
                        })
                    },
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

            // Build set of files that have at least one function with callers
            // (used for confidence scoring: "active file" = has called functions)
            let files_with_callers: std::collections::HashSet<String> = {
                let rows: Vec<(String,)> = sqlx::query_as(
                    "SELECT DISTINCT c.origin
                     FROM chunks c
                     WHERE c.name IN (SELECT DISTINCT callee_name FROM function_calls)",
                )
                .fetch_all(&self.pool)
                .await?;
                rows.into_iter().map(|(f,)| f).collect()
            };

            // Files with type-edge activity (functions that reference types)
            let files_with_type_activity: std::collections::HashSet<String> = {
                let rows: Vec<(String,)> = sqlx::query_as(
                    "SELECT DISTINCT c.origin FROM chunks c
                     JOIN type_edges te ON c.id = te.source_chunk_id",
                )
                .fetch_all(&self.pool)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "Failed to query files with type activity");
                    Vec::new()
                });
                rows.into_iter().map(|(f,)| f).collect()
            };

            // Phase 1 filtering: name/test/path checks (don't need content)
            let mut candidates: Vec<LightChunk> = Vec::new();

            for chunk in all_uncalled {
                // Skip entry points (main, init, handler, etc.)
                if ENTRY_POINT_NAMES.contains(&chunk.name.as_str()) {
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
                if chunk.chunk_type == ChunkType::Method && TRAIT_METHOD_NAMES.contains(&chunk.name.as_str())
                {
                    continue;
                }

                // Signature-only trait impl check
                if chunk.chunk_type == ChunkType::Method && TRAIT_IMPL_RE.is_match(&chunk.signature) {
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

            // Phase 2 filtering with content + confidence scoring
            let mut confident = Vec::new();
            let mut possibly_dead_pub = Vec::new();

            for light in candidates {
                let (content, doc) = content_map
                    .remove(&light.id)
                    .unwrap_or_else(|| (String::new(), None));

                // Content-based trait impl check for methods
                if light.chunk_type == ChunkType::Method && TRAIT_IMPL_RE.is_match(&content) {
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

                // Confidence scoring
                let is_method = light.chunk_type == ChunkType::Method;
                let file_str = light.file.to_string_lossy();
                let file_is_active = files_with_callers.contains(file_str.as_ref())
                    || files_with_type_activity.contains(file_str.as_ref());

                let confidence = if is_method {
                    // Methods are more likely trait impls or interface implementations
                    DeadConfidence::Low
                } else if !file_is_active {
                    // File has no functions with callers — likely entirely unused
                    DeadConfidence::High
                } else {
                    // Function in an active file — could be a helper
                    DeadConfidence::Medium
                };

                let chunk = ChunkSummary::from(ChunkRow {
                    id: light.id,
                    origin: light.file.to_string_lossy().into_owned(),
                    language: light.language.to_string(),
                    chunk_type: light.chunk_type.to_string(),
                    name: light.name,
                    signature: light.signature,
                    content,
                    doc,
                    line_start: light.line_start,
                    line_end: light.line_end,
                    parent_id: None,
                });

                let dead_fn = DeadFunction { chunk, confidence };

                if is_pub && !include_pub {
                    possibly_dead_pub.push(dead_fn);
                } else {
                    confident.push(dead_fn);
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
    ///
    /// Loads only lightweight columns (no content/doc) since callers only need
    /// name, file, and line_start. The SQL WHERE clause still filters on content
    /// (for test markers like `#[test]`) but avoids returning it.
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

        // Select only lightweight columns; content/doc filtering happens in WHERE
        // but we don't need them in the result set
        let sql = format!(
            "SELECT id, origin, language, chunk_type, name, signature,
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
            .map(|row| {
                ChunkSummary::from(ChunkRow {
                    id: row.get(0),
                    origin: row.get(1),
                    language: row.get(2),
                    chunk_type: row.get(3),
                    name: row.get(4),
                    signature: row.get(5),
                    content: String::new(),
                    doc: None,
                    line_start: clamp_line_number(row.get::<i64, _>(6)),
                    line_end: clamp_line_number(row.get::<i64, _>(7)),
                    parent_id: row.get(8),
                })
            })
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

    /// Batch count query for call graph columns.
    ///
    /// Shared implementation for caller/callee count queries. Filters by `filter_column`
    /// and groups by `group_column` to count edges.
    async fn batch_count_query(
        &self,
        filter_column: &str,
        group_column: &str,
        count_expr: &str,
        names: &[&str],
    ) -> Result<std::collections::HashMap<String, u64>, StoreError> {
        let mut result = std::collections::HashMap::new();

        const BATCH_SIZE: usize = 500;
        for batch in names.chunks(BATCH_SIZE) {
            let placeholders: String = (1..=batch.len())
                .map(|i| format!("?{}", i))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT {group_column}, {count_expr} FROM function_calls WHERE {filter_column} IN ({placeholders}) GROUP BY {group_column}",
            );
            let mut q = sqlx::query(&sql);
            for name in batch {
                q = q.bind(name);
            }
            let rows: Vec<_> = q.fetch_all(&self.pool).await?;
            for row in rows {
                let name: String = row.get(0);
                let count: i64 = row.get(1);
                result.insert(name, count as u64);
            }
        }

        Ok(result)
    }

    /// Caller counts for multiple functions in one query.
    ///
    /// Returns how many callers each function has. Functions not in the call graph
    /// won't appear in the result map (caller count is implicitly 0).
    pub fn get_caller_counts_batch(
        &self,
        names: &[&str],
    ) -> Result<std::collections::HashMap<String, u64>, StoreError> {
        if names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt
            .block_on(self.batch_count_query("callee_name", "callee_name", "COUNT(*)", names))
    }

    /// Callee counts for multiple functions in one query.
    ///
    /// Returns how many callees each function has. Functions not in the call graph
    /// won't appear in the result map (callee count is implicitly 0).
    pub fn get_callee_counts_batch(
        &self,
        names: &[&str],
    ) -> Result<std::collections::HashMap<String, u64>, StoreError> {
        if names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt.block_on(self.batch_count_query(
            "caller_name",
            "caller_name",
            "COUNT(DISTINCT callee_name)",
            names,
        ))
    }

    /// Functions that share callers with target (called by the same functions).
    ///
    /// For target X, finds functions Y where some function A calls both X and Y.
    /// Returns (function_name, overlap_count) sorted by overlap descending.
    pub fn find_shared_callers(
        &self,
        target: &str,
        limit: usize,
    ) -> Result<Vec<(String, u32)>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT fc2.callee_name, COUNT(DISTINCT fc2.caller_name) AS overlap
                 FROM function_calls fc1
                 JOIN function_calls fc2 ON fc1.caller_name = fc2.caller_name
                 WHERE fc1.callee_name = ?1 AND fc2.callee_name != ?1
                 GROUP BY fc2.callee_name
                 ORDER BY overlap DESC
                 LIMIT ?2",
            )
            .bind(target)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(name, count)| (name, count as u32))
                .collect())
        })
    }

    /// Functions that share callees with target (call the same functions).
    ///
    /// For target X, finds functions Y where X and Y both call some function C.
    /// Returns (function_name, overlap_count) sorted by overlap descending.
    pub fn find_shared_callees(
        &self,
        target: &str,
        limit: usize,
    ) -> Result<Vec<(String, u32)>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT fc2.caller_name, COUNT(DISTINCT fc2.callee_name) AS overlap
                 FROM function_calls fc1
                 JOIN function_calls fc2 ON fc1.callee_name = fc2.callee_name
                 WHERE fc1.caller_name = ?1 AND fc2.caller_name != ?1
                 GROUP BY fc2.caller_name
                 ORDER BY overlap DESC
                 LIMIT ?2",
            )
            .bind(target)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(name, count)| (name, count as u32))
                .collect())
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::helpers::ModelInfo;

    fn setup_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();
        (store, dir)
    }

    fn seed_call_graph(store: &Store) {
        // A calls B and C; B calls C; D calls B
        let calls = vec![
            crate::parser::FunctionCalls {
                name: "func_a".to_string(),
                line_start: 1,
                calls: vec![
                    crate::parser::CallSite {
                        callee_name: "func_b".to_string(),
                        line_number: 2,
                    },
                    crate::parser::CallSite {
                        callee_name: "func_c".to_string(),
                        line_number: 3,
                    },
                ],
            },
            crate::parser::FunctionCalls {
                name: "func_b".to_string(),
                line_start: 10,
                calls: vec![crate::parser::CallSite {
                    callee_name: "func_c".to_string(),
                    line_number: 11,
                }],
            },
            crate::parser::FunctionCalls {
                name: "func_d".to_string(),
                line_start: 20,
                calls: vec![crate::parser::CallSite {
                    callee_name: "func_b".to_string(),
                    line_number: 21,
                }],
            },
        ];
        store
            .upsert_function_calls(Path::new("src/test.rs"), &calls)
            .unwrap();
    }

    #[test]
    fn test_get_caller_counts_batch() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let counts = store
            .get_caller_counts_batch(&["func_b", "func_c"])
            .unwrap();
        // func_b is called by func_a and func_d
        assert_eq!(counts.get("func_b").copied().unwrap_or(0), 2);
        // func_c is called by func_a and func_b
        assert_eq!(counts.get("func_c").copied().unwrap_or(0), 2);
    }

    #[test]
    fn test_get_callee_counts_batch() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let counts = store
            .get_callee_counts_batch(&["func_a", "func_b", "func_d"])
            .unwrap();
        // func_a calls func_b and func_c
        assert_eq!(counts.get("func_a").copied().unwrap_or(0), 2);
        // func_b calls func_c
        assert_eq!(counts.get("func_b").copied().unwrap_or(0), 1);
        // func_d calls func_b
        assert_eq!(counts.get("func_d").copied().unwrap_or(0), 1);
    }

    #[test]
    fn test_get_caller_counts_batch_empty() {
        let (store, _dir) = setup_store();
        let counts = store.get_caller_counts_batch(&[]).unwrap();
        assert!(counts.is_empty());
    }

    #[test]
    fn test_get_callee_counts_batch_empty() {
        let (store, _dir) = setup_store();
        let counts = store.get_callee_counts_batch(&[]).unwrap();
        assert!(counts.is_empty());
    }

    #[test]
    fn test_get_caller_counts_batch_unknown_names() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let counts = store
            .get_caller_counts_batch(&["nonexistent_func", "also_missing"])
            .unwrap();
        // Unknown names shouldn't appear in result
        assert!(counts.is_empty());
    }

    #[test]
    fn test_get_callee_counts_batch_unknown_names() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let counts = store
            .get_callee_counts_batch(&["nonexistent_func"])
            .unwrap();
        assert!(counts.is_empty());
    }

    // ===== find_shared_callers / find_shared_callees tests =====

    #[test]
    fn test_find_shared_callers() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        // func_b and func_c are both called by func_a
        // So func_c shares caller func_a with func_b
        let shared = store.find_shared_callers("func_b", 10).unwrap();
        let names: Vec<&str> = shared.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"func_c"),
            "func_c should share caller func_a with func_b"
        );
    }

    #[test]
    fn test_find_shared_callees() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        // func_a and func_b both call func_c
        // So func_b shares callee func_c with func_a
        let shared = store.find_shared_callees("func_a", 10).unwrap();
        let names: Vec<&str> = shared.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"func_b"),
            "func_b should share callee func_c with func_a"
        );
    }

    #[test]
    fn test_find_shared_callers_no_callers() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        // func_a has no callers, so nothing shares callers with it
        let shared = store.find_shared_callers("func_a", 10).unwrap();
        assert!(shared.is_empty());
    }

    #[test]
    fn test_find_shared_callees_no_callees() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        // func_c has no callees, so nothing shares callees with it
        let shared = store.find_shared_callees("func_c", 10).unwrap();
        assert!(shared.is_empty());
    }

    #[test]
    fn test_find_shared_callers_limit() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let shared = store.find_shared_callers("func_b", 1).unwrap();
        assert!(shared.len() <= 1);
    }

    #[test]
    fn test_find_shared_callers_unknown() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let shared = store.find_shared_callers("nonexistent", 10).unwrap();
        assert!(shared.is_empty());
    }

    // ===== Dead code: entry point exclusion tests =====

    #[test]
    fn test_entry_point_exclusion() {
        let (store, _dir) = setup_store();

        // Insert chunks for known entry points
        let emb = crate::embedder::Embedding::new(vec![0.0; 769]);
        for name in &["main", "init", "handler", "middleware"] {
            let chunk = crate::parser::Chunk {
                id: format!("src/app.rs:1:{name}"),
                file: std::path::PathBuf::from("src/app.rs"),
                language: crate::parser::Language::Rust,
                chunk_type: crate::parser::ChunkType::Function,
                name: name.to_string(),
                signature: format!("fn {name}()"),
                content: format!("fn {name}() {{}}"),
                doc: None,
                line_start: 1,
                line_end: 3,
                content_hash: format!("{name}_hash"),
                parent_id: None,
                window_idx: None,
                parent_type_name: None,
            };
            store.upsert_chunk(&chunk, &emb, Some(12345)).unwrap();
        }

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let all_names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        for ep in &["main", "init", "handler", "middleware"] {
            assert!(
                !all_names.contains(ep),
                "Entry point '{ep}' should be excluded from dead code"
            );
        }
    }

    // ===== Dead code: confidence scoring tests =====

    #[test]
    fn test_confidence_assignment() {
        let (store, _dir) = setup_store();

        // Insert a function and a method, both uncalled
        let emb = crate::embedder::Embedding::new(vec![0.0; 769]);

        let func_chunk = crate::parser::Chunk {
            id: "src/orphan.rs:1:func_hash".to_string(),
            file: std::path::PathBuf::from("src/orphan.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "orphan_func".to_string(),
            signature: "fn orphan_func()".to_string(),
            content: "fn orphan_func() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 3,
            content_hash: "func_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        };
        store.upsert_chunk(&func_chunk, &emb, Some(12345)).unwrap();

        let method_chunk = crate::parser::Chunk {
            id: "src/orphan.rs:5:meth_hash".to_string(),
            file: std::path::PathBuf::from("src/orphan.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Method,
            name: "orphan_method".to_string(),
            signature: "fn orphan_method(&self)".to_string(),
            content: "fn orphan_method(&self) {}".to_string(),
            doc: None,
            line_start: 5,
            line_end: 7,
            content_hash: "meth_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        };
        store
            .upsert_chunk(&method_chunk, &emb, Some(12345))
            .unwrap();

        let (confident, _) = store.find_dead_code(true).unwrap();

        let func_dead = confident.iter().find(|d| d.chunk.name == "orphan_func");
        let method_dead = confident.iter().find(|d| d.chunk.name == "orphan_method");

        // Function in a file with no callers should be High confidence
        assert!(
            func_dead.is_some(),
            "orphan_func should be in dead code list"
        );
        assert_eq!(
            func_dead.unwrap().confidence,
            DeadConfidence::High,
            "Private function in inactive file should be High confidence"
        );

        // Method should be Low confidence
        assert!(
            method_dead.is_some(),
            "orphan_method should be in dead code list"
        );
        assert_eq!(
            method_dead.unwrap().confidence,
            DeadConfidence::Low,
            "Method should be Low confidence"
        );
    }
}
