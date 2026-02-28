//! Placement suggestion for new code
//!
//! Given a description of what you want to add, finds the best file and
//! insertion point based on semantic similarity + local pattern analysis.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::embedder::Embedder;
use crate::parser::Language;
use crate::store::{ChunkSummary, SearchFilter};
use crate::{AnalysisError, Store};

/// Local code patterns extracted from existing chunks in the target file/module.
///
/// Uses String fields intentionally rather than an enum — this keeps the design
/// flexible for arbitrary language-specific patterns without requiring type changes
/// when adding new conventions. Adding a new naming convention or error handling
/// style is a single function change in `detect_naming_convention()` or
/// `extract_patterns()`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LocalPatterns {
    /// Common imports/use statements
    pub imports: Vec<String>,
    /// Dominant error handling style (e.g., "anyhow", "thiserror", "Result<>", "try/except")
    pub error_handling: String,
    /// Naming convention (e.g., "snake_case", "camelCase", "PascalCase")
    pub naming_convention: String,
    /// Dominant visibility (e.g., "pub", "pub(crate)", "private")
    pub visibility: String,
    /// Whether the file has inline test module
    pub has_inline_tests: bool,
}

/// Suggestion for where to place new code
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileSuggestion {
    /// File path
    pub file: PathBuf,
    /// Aggregate relevance score
    pub score: f32,
    /// Suggested insertion line
    pub insertion_line: u32,
    /// Function nearest to insertion point
    pub near_function: String,
    /// Why this file was chosen
    pub reason: String,
    /// Local patterns to follow
    pub patterns: LocalPatterns,
}

impl FileSuggestion {
    /// Serialize to JSON, relativizing file paths against the project root.
    pub fn to_json(&self, root: &std::path::Path) -> serde_json::Value {
        serde_json::json!({
            "file": crate::rel_display(&self.file, root),
            "score": self.score,
            "insertion_line": self.insertion_line,
            "near_function": self.near_function,
            "reason": self.reason,
        })
    }
}

/// Result from placement analysis
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlacementResult {
    pub suggestions: Vec<FileSuggestion>,
}

/// Default search result limit for placement suggestions.
pub const DEFAULT_PLACEMENT_SEARCH_LIMIT: usize = 10;

/// Default minimum search score threshold for placement suggestions.
pub const DEFAULT_PLACEMENT_SEARCH_THRESHOLD: f32 = 0.1;

/// Options for customizing placement suggestion behavior.
#[derive(Debug, Clone)]
pub struct PlacementOptions {
    /// Number of search results to retrieve (default: 10)
    pub search_limit: usize,
    /// Minimum search score threshold (default: 0.1)
    pub search_threshold: f32,
    /// Maximum number of imports to extract per file (default: 5)
    pub max_imports: usize,
    /// Pre-computed query embedding (avoids redundant ONNX inference when the
    /// caller already embedded the query, e.g. `task()` embeds once and reuses).
    /// When `None`, the embedding is computed from the description.
    pub query_embedding: Option<crate::Embedding>,
}

impl Default for PlacementOptions {
    fn default() -> Self {
        Self {
            search_limit: DEFAULT_PLACEMENT_SEARCH_LIMIT,
            search_threshold: DEFAULT_PLACEMENT_SEARCH_THRESHOLD,
            max_imports: MAX_IMPORT_COUNT,
            query_embedding: None,
        }
    }
}

/// Suggest where to place new code matching a description.
///
/// Uses default search parameters. For custom parameters, use [`suggest_placement_with_options`].
pub fn suggest_placement(
    store: &Store,
    embedder: &Embedder,
    description: &str,
    limit: usize,
) -> Result<PlacementResult, AnalysisError> {
    suggest_placement_with_options(
        store,
        embedder,
        description,
        limit,
        &PlacementOptions::default(),
    )
}

/// Suggest where to place new code using a pre-computed embedding.
///
/// Avoids redundant ONNX inference when the caller already embedded the query
/// (e.g., `task()` embeds once and reuses across phases).
pub fn suggest_placement_with_embedding(
    store: &Store,
    query_embedding: &crate::Embedding,
    description: &str,
    limit: usize,
) -> Result<PlacementResult, AnalysisError> {
    let opts = PlacementOptions {
        query_embedding: Some(query_embedding.clone()),
        ..Default::default()
    };
    suggest_placement_with_options_core(store, description, limit, &opts)
}

/// Suggest where to place new code matching a description with configurable search parameters.
///
/// If `opts.query_embedding` is set, reuses it (avoids redundant ONNX inference).
/// Otherwise, computes the embedding from `description` using `embedder`.
///
/// 1. Searches for semantically similar code
/// 2. Groups results by file, ranks by aggregate score
/// 3. Extracts local patterns from each file
/// 4. Suggests insertion point after the most similar function
pub fn suggest_placement_with_options(
    store: &Store,
    embedder: &Embedder,
    description: &str,
    limit: usize,
    opts: &PlacementOptions,
) -> Result<PlacementResult, AnalysisError> {
    if opts.query_embedding.is_some() {
        return suggest_placement_with_options_core(store, description, limit, opts);
    }
    let query_embedding = embedder.embed_query(description)?;
    let mut owned_opts = opts.clone();
    owned_opts.query_embedding = Some(query_embedding);
    suggest_placement_with_options_core(store, description, limit, &owned_opts)
}

/// Core placement logic. Requires `opts.query_embedding` to be set.
fn suggest_placement_with_options_core(
    store: &Store,
    description: &str,
    limit: usize,
    opts: &PlacementOptions,
) -> Result<PlacementResult, AnalysisError> {
    let query_embedding = opts.query_embedding.as_ref().expect(
        "suggest_placement_with_options_core called without query_embedding in PlacementOptions",
    );
    let _span =
        tracing::info_span!("suggest_placement", desc_len = description.len(), limit).entered();

    // Search with RRF hybrid
    let filter = SearchFilter {
        enable_rrf: true,
        query_text: description.to_string(),
        ..SearchFilter::default()
    };

    let results = store.search_filtered(
        query_embedding,
        &filter,
        opts.search_limit,
        opts.search_threshold,
    )?;

    if results.is_empty() {
        return Ok(PlacementResult {
            suggestions: Vec::new(),
        });
    }

    // Group by file, compute aggregate score
    let mut by_file: HashMap<PathBuf, Vec<(f32, &ChunkSummary)>> = HashMap::new();
    for r in &results {
        by_file
            .entry(r.chunk.file.clone())
            .or_default()
            .push((r.score, &r.chunk));
    }

    // Rank files by aggregate score (sum of chunk scores)
    let mut file_scores: Vec<_> = by_file
        .into_iter()
        .map(|(file, chunks)| {
            let total_score: f32 = chunks.iter().map(|(s, _)| s).sum();
            (file, total_score, chunks)
        })
        .collect();
    file_scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    file_scores.truncate(limit);

    // Batch-fetch all file chunks upfront (single query instead of per-file N+1)
    let origin_strings: Vec<String> = file_scores
        .iter()
        .map(|(f, _, _)| f.to_string_lossy().into_owned())
        .collect();
    let origin_refs: Vec<&str> = origin_strings.iter().map(|s| s.as_str()).collect();
    let mut all_origins_chunks = match store.get_chunks_by_origins_batch(&origin_refs) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-fetch file chunks for pattern extraction");
            HashMap::new()
        }
    };

    // Build suggestions
    let mut suggestions = Vec::with_capacity(file_scores.len());

    for (file, score, chunks) in &file_scores {
        let origin_key = file.to_string_lossy();
        let all_file_chunks = all_origins_chunks
            .remove(origin_key.as_ref())
            .unwrap_or_default();

        // Find the most similar chunk in this file (highest individual score)
        let best_chunk = chunks.iter().max_by(|a, b| a.0.total_cmp(&b.0));

        let (near_function, insertion_line) = match best_chunk {
            Some((_, chunk)) => (chunk.name.clone(), chunk.line_end + 1),
            None => ("(top of file)".to_string(), 1),
        };

        // Detect language from first chunk
        let language = all_file_chunks.first().map(|c| c.language);

        // Extract patterns
        let patterns = extract_patterns(&all_file_chunks, language);

        let reason = format!(
            "{} similar functions found (best match: {})",
            chunks.len(),
            near_function
        );

        suggestions.push(FileSuggestion {
            file: file.clone(),
            score: *score,
            insertion_line,
            near_function,
            reason,
            patterns,
        });
    }

    Ok(PlacementResult { suggestions })
}

/// Maximum number of imports to extract from a file's patterns.
const MAX_IMPORT_COUNT: usize = 5;

/// Extract import/include statements from chunks by matching line prefixes.
///
/// Deduplicates imports using a HashSet and caps at `max` entries. This is the
/// shared extraction logic used by all language arms in `extract_patterns`.
fn extract_imports(chunks: &[ChunkSummary], prefixes: &[&str], max: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut imports = Vec::new();
    for chunk in chunks {
        for line in chunk.content.lines() {
            let trimmed = line.trim();
            for &prefix in prefixes {
                if trimmed.starts_with(prefix)
                    && imports.len() < max
                    && seen.insert(trimmed.to_string())
                {
                    imports.push(trimmed.to_string());
                    break;
                }
            }
        }
    }
    imports
}

/// Detect the first matching error handling style from chunk content.
fn detect_error_style(chunks: &[ChunkSummary], patterns: &[(&str, &str)]) -> String {
    for chunk in chunks {
        for &(needle, label) in patterns {
            if chunk.content.contains(needle) {
                return label.to_string();
            }
        }
    }
    String::new()
}

/// Extract local coding patterns from a file's chunks.
///
/// Iterates chunks individually instead of concatenating all content into
/// one string (avoids a large allocation for files with many chunks).
fn extract_patterns(chunks: &[ChunkSummary], language: Option<Language>) -> LocalPatterns {
    let mut error_style = String::new();
    let mut has_inline_tests = false;

    let (imports, visibility) = match language {
        Some(Language::Rust) => {
            let imports = extract_imports(chunks, &["use "], MAX_IMPORT_COUNT);
            has_inline_tests = chunks.iter().any(|c| c.content.contains("#[cfg(test)]"));
            error_style = detect_error_style(
                chunks,
                &[
                    ("anyhow::", "anyhow"),
                    ("thiserror", "thiserror"),
                    ("Result<", "Result<>"),
                ],
            );
            // Dominant visibility
            let pub_crate = chunks
                .iter()
                .filter(|c| c.signature.contains("pub(crate)"))
                .count();
            let pub_count = chunks
                .iter()
                .filter(|c| c.signature.starts_with("pub ") || c.signature.starts_with("pub fn"))
                .count();
            let private = chunks
                .iter()
                .filter(|c| !c.signature.contains("pub"))
                .count();
            let vis = if pub_crate >= pub_count && pub_crate >= private {
                "pub(crate)"
            } else if pub_count >= private {
                "pub"
            } else {
                "private"
            };
            (imports, vis.to_string())
        }
        Some(Language::Python) => {
            let imports = extract_imports(chunks, &["import ", "from "], MAX_IMPORT_COUNT);
            error_style =
                detect_error_style(chunks, &[("raise ", "raise"), ("try:", "try/except")]);
            (imports, "module-level".to_string())
        }
        Some(Language::TypeScript | Language::JavaScript) => {
            // TS/JS also matches `const x = require(...)` — use custom logic
            let mut seen = std::collections::HashSet::new();
            let mut imports = Vec::new();
            for chunk in chunks {
                for line in chunk.content.lines() {
                    let trimmed = line.trim();
                    if (trimmed.starts_with("import ")
                        || (trimmed.starts_with("const ") && trimmed.contains("require(")))
                        && imports.len() < MAX_IMPORT_COUNT
                        && seen.insert(trimmed.to_string())
                    {
                        imports.push(trimmed.to_string());
                    }
                }
            }
            error_style = detect_error_style(
                chunks,
                &[
                    ("throw ", "throw"),
                    (".catch(", "try/catch"),
                    ("try {", "try/catch"),
                ],
            );
            let has_export = chunks.iter().any(|c| c.signature.contains("export"));
            let vis = if has_export {
                "export"
            } else {
                "module-private"
            };
            (imports, vis.to_string())
        }
        Some(Language::Go) => {
            let imports = extract_imports(chunks, &["import "], MAX_IMPORT_COUNT);
            error_style = detect_error_style(chunks, &[("error", "error return")]);
            let exported = chunks
                .iter()
                .filter(|c| c.name.starts_with(|ch: char| ch.is_uppercase()))
                .count();
            let vis = if exported > chunks.len() / 2 {
                "exported"
            } else {
                "unexported"
            };
            (imports, vis.to_string())
        }
        Some(Language::C) => {
            let imports = extract_imports(chunks, &["#include"], MAX_IMPORT_COUNT);
            error_style = detect_error_style(chunks, &[("errno", "errno"), ("perror", "perror")]);
            // C: "static" means file-local, otherwise externally visible
            let statics = chunks
                .iter()
                .filter(|c| c.signature.starts_with("static "))
                .count();
            let vis = if statics > chunks.len() / 2 {
                "static"
            } else {
                "extern"
            };
            (imports, vis.to_string())
        }
        Some(Language::Java) => {
            let imports = extract_imports(chunks, &["import "], MAX_IMPORT_COUNT);
            error_style = detect_error_style(
                chunks,
                &[("throws ", "checked exceptions"), ("try {", "try/catch")],
            );
            let public = chunks
                .iter()
                .filter(|c| c.signature.contains("public"))
                .count();
            let vis = if public > chunks.len() / 2 {
                "public"
            } else {
                "package-private"
            };
            (imports, vis.to_string())
        }
        Some(Language::Sql) => {
            // SQL has no imports or visibility conventions
            (Vec::new(), "default".to_string())
        }
        Some(Language::Markdown) => {
            // Markdown has no code patterns
            (Vec::new(), "default".to_string())
        }
        _ => (Vec::new(), "default".to_string()),
    };

    LocalPatterns {
        imports,
        error_handling: error_style,
        naming_convention: detect_naming_convention(chunks),
        visibility,
        has_inline_tests,
    }
}

/// Detect naming convention from chunk names
fn detect_naming_convention(chunks: &[ChunkSummary]) -> String {
    let mut snake = 0usize;
    let mut camel = 0usize;
    let mut pascal = 0usize;

    for c in chunks {
        if crate::is_test_chunk(&c.name, &c.file.to_string_lossy()) {
            continue; // Skip test functions
        }
        if c.name.contains('_') {
            snake += 1;
        } else if c.name.starts_with(|ch: char| ch.is_lowercase())
            && c.name.chars().any(|ch| ch.is_uppercase())
        {
            camel += 1;
        } else if c.name.starts_with(|ch: char| ch.is_uppercase()) && c.name.len() > 1 {
            pascal += 1;
        }
    }

    if snake >= camel && snake >= pascal {
        "snake_case".to_string()
    } else if camel >= pascal {
        "camelCase".to_string()
    } else {
        "PascalCase".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ChunkType;

    fn make_chunk(name: &str, sig: &str, content: &str, lang: Language) -> ChunkSummary {
        ChunkSummary {
            id: format!("id-{name}"),
            file: PathBuf::from("src/test.rs"),
            language: lang,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: sig.to_string(),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 10,
            parent_id: None,
        }
    }

    #[test]
    fn test_detect_naming_snake_case() {
        let chunks = vec![
            make_chunk("find_related", "fn find_related()", "", Language::Rust),
            make_chunk(
                "search_filtered",
                "fn search_filtered()",
                "",
                Language::Rust,
            ),
        ];
        assert_eq!(detect_naming_convention(&chunks), "snake_case");
    }

    #[test]
    fn test_detect_naming_camel_case() {
        let chunks = vec![
            make_chunk(
                "findRelated",
                "function findRelated()",
                "",
                Language::JavaScript,
            ),
            make_chunk(
                "searchFiltered",
                "function searchFiltered()",
                "",
                Language::JavaScript,
            ),
        ];
        assert_eq!(detect_naming_convention(&chunks), "camelCase");
    }

    #[test]
    fn test_detect_naming_pascal_case() {
        let chunks = vec![
            make_chunk("FindRelated", "func FindRelated()", "", Language::Go),
            make_chunk("SearchFiltered", "func SearchFiltered()", "", Language::Go),
        ];
        assert_eq!(detect_naming_convention(&chunks), "PascalCase");
    }

    #[test]
    fn test_detect_naming_skips_tests() {
        let chunks = vec![
            make_chunk("test_something", "fn test_something()", "", Language::Rust),
            make_chunk("TestSomething", "func TestSomething()", "", Language::Rust),
            make_chunk("findRelated", "fn findRelated()", "", Language::Rust),
        ];
        assert_eq!(detect_naming_convention(&chunks), "camelCase");
    }

    #[test]
    fn test_extract_patterns_rust() {
        let chunks = vec![
            make_chunk(
                "search_filtered",
                "pub(crate) fn search_filtered()",
                "use crate::store::Store;\nuse anyhow::Result;\n#[cfg(test)]",
                Language::Rust,
            ),
            make_chunk(
                "search_by_name",
                "pub(crate) fn search_by_name()",
                "use crate::embedder::Embedder;",
                Language::Rust,
            ),
        ];
        let patterns = extract_patterns(&chunks, Some(Language::Rust));
        assert_eq!(patterns.error_handling, "anyhow");
        assert_eq!(patterns.visibility, "pub(crate)");
        assert!(patterns.has_inline_tests);
        assert!(!patterns.imports.is_empty());
    }

    #[test]
    fn test_extract_patterns_python() {
        let chunks = vec![make_chunk(
            "find_items",
            "def find_items()",
            "import os\nfrom pathlib import Path\nraise ValueError('bad')",
            Language::Python,
        )];
        let patterns = extract_patterns(&chunks, Some(Language::Python));
        assert_eq!(patterns.error_handling, "raise");
        assert_eq!(patterns.visibility, "module-level");
        assert!(patterns.imports.iter().any(|i| i.contains("import os")));
    }

    #[test]
    fn test_extract_patterns_empty() {
        let patterns = extract_patterns(&[], None);
        assert!(patterns.imports.is_empty());
        assert_eq!(patterns.visibility, "default");
        assert!(!patterns.has_inline_tests);
    }

    #[test]
    fn test_extract_patterns_c() {
        let chunks = vec![
            make_chunk(
                "read_file",
                "int read_file(const char *path)",
                "#include <stdio.h>\n#include <stdlib.h>\nint read_file() { if (errno) {} }",
                Language::C,
            ),
            make_chunk(
                "write_file",
                "int write_file(const char *path)",
                "#include <stdio.h>\nint write_file() { perror(\"fail\"); }",
                Language::C,
            ),
        ];
        let patterns = extract_patterns(&chunks, Some(Language::C));
        assert!(!patterns.imports.is_empty());
        assert!(
            patterns
                .imports
                .iter()
                .any(|i| i.contains("#include <stdio.h>")),
            "Expected stdio.h import, got: {:?}",
            patterns.imports
        );
        // errno found first
        assert_eq!(patterns.error_handling, "errno");
        assert_eq!(patterns.naming_convention, "snake_case");
    }

    #[test]
    fn test_extract_patterns_c_static_visibility() {
        let chunks = vec![
            make_chunk("helper", "static int helper()", "", Language::C),
            make_chunk(
                "other_helper",
                "static void other_helper()",
                "",
                Language::C,
            ),
            make_chunk("public_fn", "int public_fn()", "", Language::C),
        ];
        let patterns = extract_patterns(&chunks, Some(Language::C));
        assert_eq!(patterns.visibility, "static");
    }

    #[test]
    fn test_extract_patterns_sql() {
        let chunks = vec![make_chunk(
            "get_users",
            "CREATE FUNCTION get_users()",
            "SELECT * FROM users WHERE active = 1",
            Language::Sql,
        )];
        let patterns = extract_patterns(&chunks, Some(Language::Sql));
        assert!(patterns.imports.is_empty());
        assert_eq!(patterns.visibility, "default");
        assert!(patterns.error_handling.is_empty());
    }

    #[test]
    fn test_extract_patterns_markdown() {
        let chunks = vec![make_chunk(
            "heading",
            "# Getting Started",
            "# Hello World\n\nThis is a guide.",
            Language::Markdown,
        )];
        let patterns = extract_patterns(&chunks, Some(Language::Markdown));
        assert!(patterns.imports.is_empty());
        assert_eq!(patterns.visibility, "default");
        assert!(patterns.error_handling.is_empty());
    }

    #[test]
    fn test_extract_imports_dedup() {
        let chunks = vec![make_chunk(
            "a",
            "fn a()",
            "use std::io;\nuse std::io;\nuse std::path;",
            Language::Rust,
        )];
        let imports = extract_imports(&chunks, &["use "], 10);
        // "use std::io;" should appear only once
        let io_count = imports.iter().filter(|i| i.contains("std::io")).count();
        assert_eq!(io_count, 1);
        assert_eq!(imports.len(), 2); // std::io + std::path
    }

    #[test]
    fn test_extract_imports_respects_max() {
        let chunks = vec![make_chunk(
            "a",
            "fn a()",
            "use a;\nuse b;\nuse c;\nuse d;\nuse e;\nuse f;\nuse g;",
            Language::Rust,
        )];
        let imports = extract_imports(&chunks, &["use "], 3);
        assert_eq!(imports.len(), 3);
    }

    #[test]
    fn test_placement_empty_result() {
        // PlacementResult with empty suggestions is valid
        let result = PlacementResult {
            suggestions: Vec::new(),
        };
        assert!(result.suggestions.is_empty());
    }
}
