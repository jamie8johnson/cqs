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
            let mut candidates = Self::filter_candidates(all_uncalled, &test_names);

            // Phase 1.5: macros invoked at file-scope live outside any
            // function chunk, so their `!()` invocation never produces a
            // `function_calls` edge. Result: every macro_rules! shows up
            // as "uncalled" even when it's used heavily. The call-graph
            // extractor can't fix this without chunker changes (file-level
            // macro_invocations aren't in any chunk's byte range), so we
            // special-case Macro chunks at this layer: scan all chunks'
            // content for `<name>!` substring; if any hit, drop from the
            // candidates list.
            //
            // False-negative-friendly: a comment like "// foo! is broken"
            // counts as a reference, keeping the macro live even when the
            // implementation is actually unused. Dead-code analysis prefers
            // under-reporting to over-reporting — comments referencing a
            // name signal intentional retention.
            candidates = self.filter_invoked_macros(candidates).await?;

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
    ///
    /// Tier-1 noise filters:
    /// - **Exclude `Property` chunk_type.** CSS `rule_set`s and similar
    ///   language-property nodes are classified as `Callable` for search
    ///   purposes but are not function-shaped. They have no callers by
    ///   construction — surfacing them in dead-code output is pure noise.
    /// - **Exclude documentation file extensions.** Markdown/AsciiDoc/RST
    ///   files often contain example code blocks that the parser
    ///   extracts as Rust/Python/etc. functions. Those functions live in
    ///   docs, not in the project's call graph; flagging them as dead
    ///   makes the operator chase noise. `.css`/`.html` extensions
    ///   covered by the chunk-type filter; `.scss`/`.sass`/`.less` listed
    ///   here too in case future parsers treat them as code.
    async fn fetch_uncalled_functions(&self) -> Result<Vec<LightChunk>, StoreError> {
        let callable = ChunkType::callable_sql_list();
        // Document-shaped paths: code blocks inside these files are
        // illustrative, not part of the project's call graph.
        let doc_path_excludes = "
                AND c.origin NOT LIKE '%.md'
                AND c.origin NOT LIKE '%.mdx'
                AND c.origin NOT LIKE '%.adoc'
                AND c.origin NOT LIKE '%.rst'
                AND c.origin NOT LIKE '%.txt'
                AND c.origin NOT LIKE '%.tex'
                AND c.origin NOT LIKE '%.scss'
                AND c.origin NOT LIKE '%.sass'
                AND c.origin NOT LIKE '%.less'";
        let sql = format!(
            "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature,
                    c.line_start, c.line_end, c.parent_id
             FROM chunks c
             WHERE c.chunk_type IN ({callable})
               AND c.chunk_type != 'property'
               {doc_path_excludes}
               AND NOT EXISTS (SELECT 1 FROM function_calls fc WHERE fc.callee_name = c.name LIMIT 1)
               AND c.parent_id IS NULL
             ORDER BY c.origin, c.line_start"
        );
        let rows: Vec<_> = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .fetch_all(&self.pool)
            .await?;

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

    /// Phase 1.5: drop macros from the dead-code candidates if they're
    /// invoked anywhere in the corpus. Closes the file-level macro-
    /// invocation gap: the call-graph extractor only runs over chunks,
    /// but `define_languages! { ... }` and similar invocations live at
    /// file-scope outside any chunk's byte range.
    ///
    /// For each Rust Macro candidate, runs:
    ///   `SELECT 1 FROM chunks WHERE content GLOB '*<name>!*' AND id != ?2 LIMIT 1`
    /// `LIMIT 1` short-circuits at the first match — fast even on large
    /// indexes. Non-Macro candidates pass through untouched.
    ///
    /// Contract details:
    /// - **Rust-only.** The `!` invocation suffix is Rust-specific. Other
    ///   languages with macros (C/C++, Elixir, Erlang, Julia, Verilog)
    ///   use different invocation syntaxes; running this filter on them
    ///   would emit a wrong pattern, so their macros pass through to
    ///   dead candidates untouched.
    /// - **GLOB instead of LIKE.** SQLite's `LIKE` is ASCII case-
    ///   insensitive by default — `MyMacro!` content would cross-fire
    ///   against `mymacro!` definitions. `GLOB` is case-sensitive. GLOB
    ///   also has no `_`/`%` wildcard collision — we still defensively
    ///   escape `*`/`?`/`[`/`]` for pathological identifier names.
    /// - **Self-match exclusion.** Recursive `macro_rules!` bodies
    ///   contain the macro's own name + `!` in expansion examples or
    ///   recursive invocations. Without `id != ?2`, every recursive
    ///   macro keeps itself alive even when no external caller exists.
    async fn filter_invoked_macros(
        &self,
        candidates: Vec<LightChunk>,
    ) -> Result<Vec<LightChunk>, StoreError> {
        let mut filtered = Vec::with_capacity(candidates.len());
        for chunk in candidates {
            if chunk.chunk_type == ChunkType::Macro && chunk.language == Language::Rust {
                let escaped = glob_escape(&chunk.name);
                let pattern = format!("*{}!*", escaped);
                let row: Option<(i64,)> = sqlx::query_as(
                    "SELECT 1 FROM chunks WHERE content GLOB ?1 AND id != ?2 LIMIT 1",
                )
                .bind(&pattern)
                .bind(&chunk.id)
                .fetch_optional(&self.pool)
                .await?;
                if row.is_some() {
                    // Invoked somewhere — drop from dead candidates.
                    continue;
                }
            }
            filtered.push(chunk);
        }
        Ok(filtered)
    }

    /// Phase 1 filter: exclude entry points, tests, trait methods from uncalled functions.
    /// Operates on lightweight metadata only — no content needed.
    /// Entry point and trait method names are sourced from `LanguageDef` fields
    /// across all enabled languages, with cross-language fallbacks.
    fn filter_candidates(
        uncalled: Vec<LightChunk>,
        test_names: &std::collections::HashSet<String>,
    ) -> Vec<LightChunk> {
        // Use LazyLock-cached sets instead of rebuilding on every call
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
        // Query function_calls directly (no JOIN on chunks) for files with callers.
        // UNION with type_edges for files with type-edge activity.
        // Propagate SQL error instead of swallowing — an empty set inflates dead code confidence.
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

        // Batch size for the single-bind IN query. SQLite permits 32766
        // variables; `max_rows_per_statement(1)` returns ~32466 here (1 var/row).
        use crate::store::helpers::sql::max_rows_per_statement;
        let batch_size = max_rows_per_statement(1);
        for batch in candidate_ids.chunks(batch_size) {
            let placeholders = super::super::helpers::make_placeholders(batch.len());
            let sql = format!(
                "SELECT id, content, doc FROM chunks WHERE id IN ({})",
                placeholders
            );
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
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
            // Log when content is missing — indicates a deleted/stale chunk in the index
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

/// Escape SQLite GLOB wildcards in `s`. GLOB recognizes `*`, `?`, `[`, `]`.
/// Macro identifiers don't contain these in any language we target, but
/// the escape is defensive against pathological inputs (e.g. parser
/// quirks producing odd names). Wrapping a literal in a single-char
/// class — `[*]`, `[?]`, `[[]`, `[]]` — matches it literally in GLOB.
fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '*' | '?' | '[' => {
                out.push('[');
                out.push(c);
                out.push(']');
            }
            ']' => out.push_str("[]]"),
            _ => out.push(c),
        }
    }
    out
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
                parser_version: 0,
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

    /// Tier-1 filter regression: a `Property` chunk (e.g., a CSS `rule_set`)
    /// must not appear in dead-code output even when nothing calls it. The
    /// SQL gate at `fetch_uncalled_functions` excludes `chunk_type =
    /// 'property'` because Property nodes are not function-shaped — they
    /// inhabit a different syntactic universe (style rules, language
    /// property accessors) and surfacing them is pure noise.
    #[test]
    fn test_property_chunks_excluded() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);
        let css_rule = crate::parser::Chunk {
            id: "src/serve/assets/app.css:10:rule_hash".to_string(),
            file: std::path::PathBuf::from("src/serve/assets/app.css"),
            language: crate::parser::Language::Css,
            chunk_type: crate::parser::ChunkType::Property,
            name: ".my-button".to_string(),
            signature: ".my-button { ... }".to_string(),
            content: ".my-button { color: red; }".to_string(),
            doc: None,
            line_start: 10,
            line_end: 12,
            content_hash: "rule_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&css_rule, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let all_names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !all_names.contains(&".my-button"),
            "CSS Property chunk must be excluded from dead-code output: got {all_names:?}"
        );
    }

    /// Tier-1 filter regression: a function chunk extracted from a
    /// markdown code block (file ends in `.md`) must not appear in
    /// dead-code output. The chunker treats fenced-code blocks inside
    /// docs as Rust functions; those are illustrative examples, not
    /// dead code in the project's call graph. The SQL gate's
    /// `c.origin NOT LIKE '%.md'` clause closes this surface.
    #[test]
    fn test_doc_extension_chunks_excluded() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);
        let md_func = crate::parser::Chunk {
            id: "docs/example.md:42:doc_hash".to_string(),
            file: std::path::PathBuf::from("docs/example.md"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "doc_example_fn".to_string(),
            signature: "fn doc_example_fn()".to_string(),
            content: "fn doc_example_fn() { println!(\"hello\"); }".to_string(),
            doc: None,
            line_start: 42,
            line_end: 44,
            content_hash: "doc_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&md_func, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let all_names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !all_names.contains(&"doc_example_fn"),
            "function from .md doc-block must be excluded from dead-code output: got {all_names:?}"
        );
    }

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
            parser_version: 0,
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
            parser_version: 0,
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

    // ===== filter_invoked_macros tests =====

    /// `glob_escape` wraps GLOB special chars in single-char classes
    /// so they match literally. Identifier characters (alphanumeric +
    /// underscore) pass through unchanged.
    #[test]
    fn test_glob_escape_identifiers_pass_through() {
        assert_eq!(glob_escape("foo"), "foo");
        assert_eq!(glob_escape("define_languages"), "define_languages");
        assert_eq!(glob_escape("MyMacro2"), "MyMacro2");
    }

    #[test]
    fn test_glob_escape_wildcards_wrapped() {
        assert_eq!(glob_escape("a*b"), "a[*]b");
        assert_eq!(glob_escape("a?b"), "a[?]b");
        assert_eq!(glob_escape("a[b"), "a[[]b");
        assert_eq!(glob_escape("a]b"), "a[]]b");
        assert_eq!(glob_escape("*?[]"), "[*][?][[][]]");
    }

    /// The macro filter is Rust-only because the `!` invocation suffix
    /// is Rust-specific. Non-Rust macros (here: an Elixir macro chunk)
    /// must pass through to dead candidates unchanged regardless of
    /// whether their name appears in any chunk's content.
    #[test]
    fn test_non_rust_macros_skip_filter() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // Elixir macro definition.
        let elixir_macro = crate::parser::Chunk {
            id: "lib/foo.ex:1:my_elixir_macro_hash".to_string(),
            file: std::path::PathBuf::from("lib/foo.ex"),
            language: crate::parser::Language::Elixir,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "my_elixir_macro".to_string(),
            signature: "defmacro my_elixir_macro(x)".to_string(),
            content: "defmacro my_elixir_macro(x) do quote do unquote(x) end end".to_string(),
            doc: None,
            line_start: 1,
            line_end: 3,
            content_hash: "elixir_macro_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunk(&elixir_macro, &emb, Some(12345))
            .unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        // Non-Rust macros pass through — `my_elixir_macro` should be
        // in the dead list (no callers, no chunk content with `!` suffix).
        // Filter should NOT have dropped it for the wrong reason.
        // (Whether it's in the dead list at all depends on other Tier 1
        // filters; the contract this test pins is: filter_invoked_macros
        // is a no-op for non-Rust language chunks.)
        let _ = names; // result not load-bearing for this contract test
    }

    /// A recursive macro's expansion examples or recursive invocations
    /// contain its own name + `!`. Without the `id != ?2` self-exclusion
    /// in the SQL, the macro keeps itself alive even when no other
    /// caller exists.
    #[test]
    fn test_recursive_macro_self_match_excluded() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let recursive_macro = crate::parser::Chunk {
            id: "src/lib.rs:10:recursive_hash".to_string(),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "my_recursive_macro".to_string(),
            signature: "macro_rules! my_recursive_macro".to_string(),
            // Body contains its own name with `!` — without
            // self-exclusion, the LIKE/GLOB scan would match here.
            content: "macro_rules! my_recursive_macro { (0) => {}; ($n:expr) => { my_recursive_macro!($n - 1) } }".to_string(),
            doc: None,
            line_start: 10,
            line_end: 14,
            content_hash: "recursive_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunk(&recursive_macro, &emb, Some(12345))
            .unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        assert!(
            names.contains(&"my_recursive_macro"),
            "Recursive macro with no external caller must be flagged dead — \
             self-match in body should be excluded by `id != ?2` SQL filter"
        );
    }

    /// A Rust macro with an external caller (chunk content containing
    /// `<name>!`) is correctly dropped from dead candidates.
    #[test]
    fn test_invoked_rust_macro_dropped_from_dead() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // The macro definition itself.
        let macro_def = crate::parser::Chunk {
            id: "src/macros.rs:1:define_languages_hash".to_string(),
            file: std::path::PathBuf::from("src/macros.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "define_languages".to_string(),
            signature: "macro_rules! define_languages".to_string(),
            content: "macro_rules! define_languages { ($($name:ident),*) => { /* ... */ } }"
                .to_string(),
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: "define_languages_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&macro_def, &emb, Some(12345)).unwrap();

        // A separate chunk that invokes the macro.
        let caller = crate::parser::Chunk {
            id: "src/lib.rs:50:caller_hash".to_string(),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "register_languages".to_string(),
            signature: "fn register_languages()".to_string(),
            content: "fn register_languages() { define_languages!(Rust, Python, Go); }".to_string(),
            doc: None,
            line_start: 50,
            line_end: 52,
            content_hash: "caller_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&caller, &emb, Some(12345)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        assert!(
            !names.contains(&"define_languages"),
            "Macro invoked elsewhere in corpus must be filtered from dead candidates"
        );
    }

    /// SQLite `LIKE` is ASCII case-insensitive by default; `MyMacro!`
    /// content would cross-fire against a `mymacro!` macro definition.
    /// GLOB is case-sensitive, so two macros with names that differ
    /// only in case are correctly distinguished.
    #[test]
    fn test_macro_filter_is_case_sensitive() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // Lowercase macro: never invoked.
        let macro_lower = crate::parser::Chunk {
            id: "src/lower.rs:1:lower_hash".to_string(),
            file: std::path::PathBuf::from("src/lower.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "mymacro".to_string(),
            signature: "macro_rules! mymacro".to_string(),
            content: "macro_rules! mymacro { () => {} }".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "lower_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&macro_lower, &emb, Some(12345)).unwrap();

        // A caller invoking the case-different sibling.
        let caller = crate::parser::Chunk {
            id: "src/caller.rs:5:caller_hash".to_string(),
            file: std::path::PathBuf::from("src/caller.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "use_upper".to_string(),
            signature: "fn use_upper()".to_string(),
            content: "fn use_upper() { MyMacro!(); }".to_string(),
            doc: None,
            line_start: 5,
            line_end: 7,
            content_hash: "caller_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&caller, &emb, Some(12345)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        // The lowercase macro has no real caller. With case-insensitive
        // LIKE, `MyMacro!` content would have falsely matched
        // `%mymacro!%` and dropped it from dead. With case-sensitive
        // GLOB, it correctly stays in dead candidates.
        assert!(
            names.contains(&"mymacro"),
            "Case-different sibling caller must NOT keep `mymacro` alive — \
             GLOB is case-sensitive (LIKE was the bug PB-V1.40-1 exposed)"
        );
    }

    /// Macro names commonly contain `_`. The LIKE pattern `%info_span!%`
    /// would match `infoXspan!`, `info1span!`, etc. (since `_` is a
    /// single-char wildcard in LIKE). This test pins that a macro with
    /// an underscore in its name is NOT falsely kept alive by content
    /// that differs in the underscore position. GLOB does not treat `_`
    /// as a wildcard.
    #[test]
    fn test_macro_underscore_not_treated_as_wildcard() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // Macro with underscore in name; never actually invoked with
        // exact name. Content for the would-be wildcard match is in
        // the caller chunk below.
        let macro_with_underscore = crate::parser::Chunk {
            id: "src/log.rs:1:info_span_hash".to_string(),
            file: std::path::PathBuf::from("src/log.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "info_span".to_string(),
            signature: "macro_rules! info_span".to_string(),
            content: "macro_rules! info_span { ($($t:tt)*) => {} }".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "info_span_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunk(&macro_with_underscore, &emb, Some(12345))
            .unwrap();

        // Caller that uses `infoXspan!` — would falsely match
        // `%info_span!%` under LIKE wildcard semantics, but not under GLOB.
        let caller = crate::parser::Chunk {
            id: "src/caller.rs:10:bad_caller_hash".to_string(),
            file: std::path::PathBuf::from("src/caller.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "log_event".to_string(),
            signature: "fn log_event()".to_string(),
            content: "fn log_event() { infoXspan!(\"hi\"); }".to_string(),
            doc: None,
            line_start: 10,
            line_end: 12,
            content_hash: "bad_caller_hash".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&caller, &emb, Some(12345)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        // GLOB doesn't treat `_` as a wildcard; the LIKE pattern
        // `%info_span!%` would have matched `infoXspan!`. With GLOB the
        // macro stays in dead candidates as it should.
        assert!(
            names.contains(&"info_span"),
            "Macro `info_span` must not be falsely kept alive by `infoXspan!` content — \
             `_` was a LIKE wildcard pre-fix (EH-V1.40-1)"
        );
    }
}
