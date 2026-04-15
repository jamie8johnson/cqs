//! Dead code detection with confidence scoring.

use std::path::PathBuf;
use std::sync::LazyLock;

use sqlx::Row;

use super::{
    build_entry_point_names, build_trait_method_names, DeadConfidence, DeadFunction, LightChunk,
    TRAIT_IMPL_RE,
};
use crate::parser::{ChunkType, Language};
use crate::store::helpers::{clamp_line_number, ChunkRow, ChunkSummary, StoreError};
use crate::store::Store;

impl<Mode> Store<Mode> {
    /// Find functions/methods never called by indexed code (dead code detection).
    /// Returns two lists:
    /// - `confident`: Functions with no callers that are likely dead (with confidence scores)
    /// - `possibly_dead_pub`: Public functions with no callers (may be used externally)
    /// Uses two-phase query: lightweight metadata first, then content only for
    /// candidates that pass name/test/path filters (avoids loading large function bodies).
    /// Exclusions applied:
    /// - Entry point names (`main`, `init`, `handler`, etc.)
    /// - Test functions (via `find_test_chunks()` heuristics)
    /// - Functions in test files
    /// - Trait implementations (dynamic dispatch invisible to call graph)
    /// - `#[no_mangle]` functions (FFI)
    /// Confidence scoring:
    /// - **High**: Private function in a file where no other function has callers
    /// - **Medium**: Private function in an active file (other functions are called)
    /// - **Low**: Method, or function with constructor-like name patterns
    pub fn find_dead_code(
        &self,
        include_pub: bool,
    ) -> Result<(Vec<DeadFunction>, Vec<DeadFunction>), StoreError> {
        let _span = tracing::info_span!("find_dead_code", include_pub).entered();
        self.rt.block_on(async {
            // Phase 1: Fetch all uncalled functions (lightweight, no content/doc)
            let all_uncalled = self.fetch_uncalled_functions().await?;
            let total_uncalled = all_uncalled.len();

            // Build test name set for exclusion (names-only query avoids ChunkSummary overhead)
            let test_names: std::collections::HashSet<String> = self
                .find_test_chunk_names_async()
                .await?
                .into_iter()
                .collect();

            // Phase 1 filtering: name/test/path/trait checks (don't need content)
            let candidates = Self::filter_candidates(all_uncalled, &test_names);

            // Phase 2: Batch-fetch content and score confidence
            let active_files = self.fetch_active_files().await?;
            let (confident, possibly_dead_pub) = self
                .score_confidence(candidates, &active_files, include_pub)
                .await?;

            tracing::info!(
                total_uncalled,
                confident = confident.len(),
                possibly_dead = possibly_dead_pub.len(),
                "Dead code analysis complete"
            );

            Ok((confident, possibly_dead_pub))
        })
    }

    /// Phase 1: Query all callable chunks with no callers in the call graph.
    /// Returns lightweight metadata without content/doc to minimize memory.
    async fn fetch_uncalled_functions(&self) -> Result<Vec<LightChunk>, StoreError> {
        let callable = ChunkType::callable_sql_list();
        let sql = format!(
            "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature,
                    c.line_start, c.line_end, c.parent_id
             FROM chunks c
             WHERE c.chunk_type IN ({callable})
               AND NOT EXISTS (SELECT 1 FROM function_calls fc WHERE fc.callee_name = c.name LIMIT 1)
               AND c.parent_id IS NULL
             ORDER BY c.origin, c.line_start"
        );
        let rows: Vec<_> = sqlx::query(&sql).fetch_all(&self.pool).await?;

        Ok(rows
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
            .collect())
    }

    /// Phase 1 filter: exclude entry points, tests, trait methods from uncalled functions.
    /// Operates on lightweight metadata only — no content needed.
    /// Entry point and trait method names are sourced from `LanguageDef` fields
    /// across all enabled languages, with cross-language fallbacks.
    fn filter_candidates(
        uncalled: Vec<LightChunk>,
        test_names: &std::collections::HashSet<String>,
    ) -> Vec<LightChunk> {
        // PERF-23: Use LazyLock-cached sets instead of rebuilding on every call
        static ENTRY_POINTS: LazyLock<std::collections::HashSet<&'static str>> =
            LazyLock::new(|| build_entry_point_names().into_iter().collect());
        static TRAIT_METHODS: LazyLock<std::collections::HashSet<&'static str>> =
            LazyLock::new(|| build_trait_method_names().into_iter().collect());
        let entry_points = &*ENTRY_POINTS;
        let trait_methods = &*TRAIT_METHODS;

        let mut candidates = Vec::new();

        for chunk in uncalled {
            // Skip entry points (main, init, handler, etc.)
            if entry_points.contains(chunk.name.as_str()) {
                continue;
            }
            if test_names.contains(&chunk.name) {
                continue;
            }
            let path_str = chunk.file.to_string_lossy();
            if crate::is_test_chunk(&chunk.name, &path_str) {
                continue;
            }

            // Methods with well-known trait names can be skipped without content
            if chunk.chunk_type == ChunkType::Method && trait_methods.contains(chunk.name.as_str())
            {
                continue;
            }

            // Signature-only trait impl check
            if chunk.chunk_type == ChunkType::Method && TRAIT_IMPL_RE.is_match(&chunk.signature) {
                continue;
            }

            candidates.push(chunk);
        }

        candidates
    }

    /// Fetch sets of files with call graph or type-edge activity.
    /// Used for confidence scoring: files with active functions are "active".
    async fn fetch_active_files(&self) -> Result<std::collections::HashSet<String>, StoreError> {
        // PERF-22: Query function_calls directly (no JOIN on chunks) for files with callers.
        // UNION with type_edges for files with type-edge activity.
        // EH-17: propagate SQL error instead of swallowing — empty set inflates dead code confidence
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT file FROM function_calls
             UNION
             SELECT DISTINCT c.origin FROM chunks c
             JOIN type_edges te ON c.id = te.source_chunk_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(f,)| f).collect())
    }

    /// Phase 2: Batch-fetch content for candidates and assign confidence scores.
    /// Splits results into confident dead code and possibly-dead public functions.
    async fn score_confidence(
        &self,
        candidates: Vec<LightChunk>,
        active_files: &std::collections::HashSet<String>,
        include_pub: bool,
    ) -> Result<(Vec<DeadFunction>, Vec<DeadFunction>), StoreError> {
        // Batch-fetch content for remaining candidates (use references to avoid cloning IDs)
        let candidate_ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let mut content_map: std::collections::HashMap<String, (String, Option<String>)> =
            std::collections::HashMap::new();

        // PF-V1.25-8: 500 was the pre-3.32 SQLite-limit-safe constant for
        // a single-bind IN query. Modern SQLite permits 32766 variables;
        // `max_rows_per_statement(1)` returns ~32466 here (1 var/row).
        use crate::store::helpers::sql::max_rows_per_statement;
        let batch_size = max_rows_per_statement(1);
        for batch in candidate_ids.chunks(batch_size) {
            let placeholders = super::super::helpers::make_placeholders(batch.len());
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

        let mut confident = Vec::new();
        let mut possibly_dead_pub = Vec::new();

        for light in candidates {
            // EH-18: log when content is missing — indicates deleted/stale chunk in index
            let (content, doc) = match content_map.remove(&light.id) {
                Some(pair) => pair,
                None => {
                    tracing::warn!(
                        chunk_id = %light.id,
                        name = %light.name,
                        "Content missing for dead code candidate — chunk may be stale"
                    );
                    (String::new(), None)
                }
            };

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
            let file_is_active = active_files.contains(file_str.as_ref());

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

            let chunk = ChunkSummary::from(ChunkRow::from_light_chunk(light, content, doc));

            let dead_fn = DeadFunction { chunk, confidence };

            if is_pub && !include_pub {
                possibly_dead_pub.push(dead_fn);
            } else {
                confident.push(dead_fn);
            }
        }

        Ok((confident, possibly_dead_pub))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::setup_store;

    // ===== Dead code: entry point exclusion tests =====

    #[test]
    fn test_entry_point_exclusion() {
        let (store, _dir) = setup_store();

        // Insert chunks for known entry points
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);
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
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

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
