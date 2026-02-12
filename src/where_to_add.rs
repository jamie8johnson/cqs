//! Placement suggestion for new code
//!
//! Given a description of what you want to add, finds the best file and
//! insertion point based on semantic similarity + local pattern analysis.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::embedder::Embedder;
use crate::parser::Language;
use crate::store::{ChunkSummary, SearchFilter, StoreError};
use crate::Store;

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

/// Suggest where to place new code matching a description.
///
/// 1. Searches for semantically similar code
/// 2. Groups results by file, ranks by aggregate score
/// 3. Extracts local patterns from each file
/// 4. Suggests insertion point after the most similar function
pub fn suggest_placement(
    store: &Store,
    embedder: &Embedder,
    description: &str,
    limit: usize,
) -> Result<PlacementResult, SuggestError> {
    let _span =
        tracing::info_span!("suggest_placement", desc_len = description.len(), limit).entered();
    // Embed the description
    let query_embedding = embedder
        .embed_query(description)
        .map_err(|e| SuggestError::Embedding(e.to_string()))?;

    // Search with RRF hybrid
    let filter = SearchFilter {
        enable_rrf: true,
        query_text: description.to_string(),
        ..SearchFilter::default()
    };

    let results = store.search_filtered(&query_embedding, &filter, 10, 0.1)?;

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

    // Build suggestions
    let mut suggestions = Vec::with_capacity(file_scores.len());

    for (file, score, chunks) in &file_scores {
        // Get all chunks from this file for pattern extraction
        let all_file_chunks = match store.get_chunks_by_origin(&file.to_string_lossy()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(file = %file.display(), error = %e, "Failed to get file chunks");
                Vec::new()
            }
        };

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

/// Extract local coding patterns from a file's chunks
fn extract_patterns(chunks: &[ChunkSummary], language: Option<Language>) -> LocalPatterns {
    let mut imports = Vec::new();
    let mut error_style = String::new();
    let mut has_inline_tests = false;

    // Collect all content for analysis
    let all_content: String = chunks
        .iter()
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let visibility = match language {
        Some(Language::Rust) => {
            // Rust patterns
            for line in all_content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("use ") && !imports.contains(&trimmed.to_string()) {
                    imports.push(trimmed.to_string());
                }
            }
            if all_content.contains("anyhow::") || all_content.contains("anyhow::Result") {
                error_style = "anyhow".to_string();
            } else if all_content.contains("thiserror") {
                error_style = "thiserror".to_string();
            } else if all_content.contains("Result<") {
                error_style = "Result<>".to_string();
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
            has_inline_tests = all_content.contains("#[cfg(test)]");
            if pub_crate >= pub_count && pub_crate >= private {
                "pub(crate)".to_string()
            } else if pub_count >= private {
                "pub".to_string()
            } else {
                "private".to_string()
            }
        }
        Some(Language::Python) => {
            for line in all_content.lines() {
                let trimmed = line.trim();
                if (trimmed.starts_with("import ") || trimmed.starts_with("from "))
                    && !imports.contains(&trimmed.to_string())
                {
                    imports.push(trimmed.to_string());
                }
            }
            if all_content.contains("raise ") {
                error_style = "raise".to_string();
            } else if all_content.contains("try:") {
                error_style = "try/except".to_string();
            }
            "module-level".to_string()
        }
        Some(Language::TypeScript | Language::JavaScript) => {
            for line in all_content.lines() {
                let trimmed = line.trim();
                if (trimmed.starts_with("import ")
                    || (trimmed.starts_with("const ") && trimmed.contains("require(")))
                    && !imports.contains(&trimmed.to_string())
                {
                    imports.push(trimmed.to_string());
                }
            }
            if all_content.contains("throw ") {
                error_style = "throw".to_string();
            } else if all_content.contains(".catch(") || all_content.contains("try {") {
                error_style = "try/catch".to_string();
            }
            let has_export = chunks.iter().any(|c| c.signature.contains("export"));
            if has_export {
                "export".to_string()
            } else {
                "module-private".to_string()
            }
        }
        Some(Language::Go) => {
            for line in all_content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("import ") && !imports.contains(&trimmed.to_string()) {
                    imports.push(trimmed.to_string());
                }
            }
            if all_content.contains("error") {
                error_style = "error return".to_string();
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
            for line in all_content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("import ") && !imports.contains(&trimmed.to_string()) {
                    imports.push(trimmed.to_string());
                }
            }
            if all_content.contains("throws ") {
                error_style = "checked exceptions".to_string();
            } else if all_content.contains("try {") {
                error_style = "try/catch".to_string();
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

    // Cap imports to top 5
    imports.truncate(5);

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

/// Error type for placement suggestions
#[derive(Debug)]
pub enum SuggestError {
    Embedding(String),
    Store(StoreError),
}

impl std::fmt::Display for SuggestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SuggestError::Embedding(e) => write!(f, "Embedding error: {e}"),
            SuggestError::Store(e) => write!(f, "Store error: {e}"),
        }
    }
}

impl std::error::Error for SuggestError {}

impl From<StoreError> for SuggestError {
    fn from(e: StoreError) -> Self {
        SuggestError::Store(e)
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
