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

/// Local patterns observed in a file
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

/// Result from placement analysis
pub struct PlacementResult {
    pub suggestions: Vec<FileSuggestion>,
}

/// Default search result limit for placement suggestions.
pub const DEFAULT_PLACEMENT_SEARCH_LIMIT: usize = 10;

/// Default minimum search score threshold for placement suggestions.
pub const DEFAULT_PLACEMENT_SEARCH_THRESHOLD: f32 = 0.1;

/// Options for customizing placement suggestion behavior.
pub struct PlacementOptions {
    /// Number of search results to retrieve (default: 10)
    pub search_limit: usize,
    /// Minimum search score threshold (default: 0.1)
    pub search_threshold: f32,
    /// Maximum number of imports to extract per file (default: 5)
    pub max_imports: usize,
}

impl Default for PlacementOptions {
    fn default() -> Self {
        Self {
            search_limit: DEFAULT_PLACEMENT_SEARCH_LIMIT,
            search_threshold: DEFAULT_PLACEMENT_SEARCH_THRESHOLD,
            max_imports: MAX_IMPORT_COUNT,
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

/// Suggest where to place new code matching a description with configurable search parameters.
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
    let _span =
        tracing::info_span!("suggest_placement", desc_len = description.len(), limit).entered();
    // Embed the description
    let query_embedding = embedder
        .embed_query(description)
        .map_err(|e| AnalysisError::Embedder(e.to_string()))?;

    // Search with RRF hybrid
    let filter = SearchFilter {
        enable_rrf: true,
        query_text: description.to_string(),
        ..SearchFilter::default()
    };

    let results = store.search_filtered(
        &query_embedding,
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
    file_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
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
        let best_chunk = chunks
            .iter()
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

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

/// Extract local coding patterns from a file's chunks.
///
/// Iterates chunks individually instead of concatenating all content into
/// one string (avoids a large allocation for files with many chunks).
/// Uses a HashSet for O(1) import dedup instead of Vec::contains.
fn extract_patterns(chunks: &[ChunkSummary], language: Option<Language>) -> LocalPatterns {
    let mut import_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut imports = Vec::new();
    let mut error_style = String::new();
    let mut has_inline_tests = false;

    /// Add an import if not already seen, up to the cap.
    fn try_add_import(
        line: &str,
        import_set: &mut std::collections::HashSet<String>,
        imports: &mut Vec<String>,
    ) {
        if imports.len() < MAX_IMPORT_COUNT && import_set.insert(line.to_string()) {
            imports.push(line.to_string());
        }
    }

    let visibility = match language {
        Some(Language::Rust) => {
            // Rust patterns — scan each chunk individually
            for chunk in chunks {
                for line in chunk.content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("use ") {
                        try_add_import(trimmed, &mut import_set, &mut imports);
                    }
                }
                if !has_inline_tests && chunk.content.contains("#[cfg(test)]") {
                    has_inline_tests = true;
                }
                if error_style.is_empty() {
                    if chunk.content.contains("anyhow::") {
                        error_style = "anyhow".to_string();
                    } else if chunk.content.contains("thiserror") {
                        error_style = "thiserror".to_string();
                    } else if chunk.content.contains("Result<") {
                        error_style = "Result<>".to_string();
                    }
                }
            }
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
            if pub_crate >= pub_count && pub_crate >= private {
                "pub(crate)".to_string()
            } else if pub_count >= private {
                "pub".to_string()
            } else {
                "private".to_string()
            }
        }
        Some(Language::Python) => {
            for chunk in chunks {
                for line in chunk.content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                        try_add_import(trimmed, &mut import_set, &mut imports);
                    }
                }
                if error_style.is_empty() {
                    if chunk.content.contains("raise ") {
                        error_style = "raise".to_string();
                    } else if chunk.content.contains("try:") {
                        error_style = "try/except".to_string();
                    }
                }
            }
            "module-level".to_string()
        }
        Some(Language::TypeScript | Language::JavaScript) => {
            for chunk in chunks {
                for line in chunk.content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("import ")
                        || (trimmed.starts_with("const ") && trimmed.contains("require("))
                    {
                        try_add_import(trimmed, &mut import_set, &mut imports);
                    }
                }
                if error_style.is_empty() {
                    if chunk.content.contains("throw ") {
                        error_style = "throw".to_string();
                    } else if chunk.content.contains(".catch(") || chunk.content.contains("try {") {
                        error_style = "try/catch".to_string();
                    }
                }
            }
            let has_export = chunks.iter().any(|c| c.signature.contains("export"));
            if has_export {
                "export".to_string()
            } else {
                "module-private".to_string()
            }
        }
        Some(Language::Go) => {
            for chunk in chunks {
                for line in chunk.content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("import ") {
                        try_add_import(trimmed, &mut import_set, &mut imports);
                    }
                }
                if error_style.is_empty() && chunk.content.contains("error") {
                    error_style = "error return".to_string();
                }
            }
            // Go: capitalized = exported
            let exported = chunks
                .iter()
                .filter(|c| c.name.starts_with(|ch: char| ch.is_uppercase()))
                .count();
            if exported > chunks.len() / 2 {
                "exported".to_string()
            } else {
                "unexported".to_string()
            }
        }
        Some(Language::Java) => {
            for chunk in chunks {
                for line in chunk.content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("import ") {
                        try_add_import(trimmed, &mut import_set, &mut imports);
                    }
                }
                if error_style.is_empty() {
                    if chunk.content.contains("throws ") {
                        error_style = "checked exceptions".to_string();
                    } else if chunk.content.contains("try {") {
                        error_style = "try/catch".to_string();
                    }
                }
            }
            let public = chunks
                .iter()
                .filter(|c| c.signature.contains("public"))
                .count();
            if public > chunks.len() / 2 {
                "public".to_string()
            } else {
                "package-private".to_string()
            }
        }
        _ => "default".to_string(),
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
        if c.name.starts_with("test_") || c.name.starts_with("Test") {
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
    fn test_placement_empty_result() {
        // PlacementResult with empty suggestions is valid
        let result = PlacementResult {
            suggestions: Vec::new(),
        };
        assert!(result.suggestions.is_empty());
    }
}
