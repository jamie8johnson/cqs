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
    #[serde(serialize_with = "crate::serialize_path_normalized")]
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
    /// Creates a new instance with default configuration values for placement search parameters.
    /// # Returns
    /// A new `Self` instance with `search_limit` set to `DEFAULT_PLACEMENT_SEARCH_LIMIT`, `search_threshold` set to `DEFAULT_PLACEMENT_SEARCH_THRESHOLD`, `max_imports` set to `MAX_IMPORT_COUNT`, and `query_embedding` set to `None`.
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
/// Uses default search parameters. For custom parameters, use [`suggest_placement_with_options`].
pub fn suggest_placement<Mode>(
    store: &Store<Mode>,
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
/// If `opts.query_embedding` is set, reuses it (avoids redundant ONNX inference).
/// Otherwise, computes the embedding from `description` using `embedder`.
/// 1. Searches for semantically similar code
/// 2. Groups results by file, ranks by aggregate score
/// 3. Extracts local patterns from each file
/// 4. Suggests insertion point after the most similar function
pub fn suggest_placement_with_options<Mode>(
    store: &Store<Mode>,
    embedder: &Embedder,
    description: &str,
    limit: usize,
    opts: &PlacementOptions,
) -> Result<PlacementResult, AnalysisError> {
    // P3 #132: entry-level span so an `embed_query` failure (which short-
    // circuits before `_core`'s span fires) still has tracing identity for
    // the placement call.
    let _span = tracing::info_span!(
        "suggest_placement_with_options",
        desc_len = description.len(),
        limit
    )
    .entered();
    if opts.query_embedding.is_some() {
        return suggest_placement_with_options_core(store, description, limit, opts);
    }
    let query_embedding = embedder.embed_query(description)?;
    let mut owned_opts = opts.clone();
    owned_opts.query_embedding = Some(query_embedding);
    suggest_placement_with_options_core(store, description, limit, &owned_opts)
}

/// Core placement logic. Requires `opts.query_embedding` to be set.
fn suggest_placement_with_options_core<Mode>(
    store: &Store<Mode>,
    description: &str,
    limit: usize,
    opts: &PlacementOptions,
) -> Result<PlacementResult, AnalysisError> {
    let query_embedding = opts
        .query_embedding
        .as_ref()
        .ok_or_else(|| AnalysisError::Phase {
            phase: "placement",
            message: "query_embedding required in PlacementOptions".to_string(),
        })?;
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
    // Secondary sort on file path keeps equal-score files deterministically
    // ordered across process invocations so the truncate() below picks the
    // same files on every run.
    file_scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    file_scores.truncate(limit);

    // Batch-fetch all file chunks upfront (single query instead of per-file N+1)
    let origin_strings: Vec<String> = file_scores
        .iter()
        .map(|(f, _, _)| f.to_string_lossy().into_owned())
        .collect();
    let origin_refs: Vec<&str> = origin_strings.iter().map(|s| s.as_str()).collect();
    let mut all_origins_chunks = store.get_chunks_by_origins_batch(&origin_refs)?;

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
/// Deduplicates imports using a HashSet and caps at `max` entries. This is the
/// shared extraction logic used by all language arms in `extract_patterns`.
fn extract_imports(chunks: &[ChunkSummary], prefixes: &[&str], max: usize) -> Vec<String> {
    // P3.45: dedupe via borrowed `&str` keys; allocate the owned `String`
    // only on accept (when the line is actually pushed into `imports`).
    // The previous shape did `seen.insert(trimmed.to_string())` for every
    // candidate line — including lines that were dropped because `imports`
    // had already hit `max` — wasting one allocation per duplicate hit.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut imports: Vec<String> = Vec::new();
    for chunk in chunks {
        for line in chunk.content.lines() {
            let trimmed = line.trim();
            for &prefix in prefixes {
                if trimmed.starts_with(prefix) && imports.len() < max && seen.insert(trimmed) {
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

// ---------------------------------------------------------------------------
// Data-driven pattern extraction
//
// Most languages follow the same 4-step pattern:
//   1. extract_imports(chunks, prefixes, MAX_IMPORT_COUNT)
//   2. detect_error_style(chunks, error_patterns)
//   3. Visibility counting via signature inspection
//   4. Return (imports, visibility)
//
// Languages with truly custom logic (Rust, TS/JS, Go) keep dedicated arms.
// Everything else is driven by `LanguagePatternDef` lookup tables.
// ---------------------------------------------------------------------------

/// How to detect dominant visibility from chunk signatures.
///
/// This enum is slightly broader than its name suggests: `RegexImportSet` also
/// overrides import extraction for languages that need more than simple prefix
/// matching (e.g., TS/JS `const x = require(...)`). Keeping everything in one
/// enum means one `LanguagePatternDef::visibility` field covers all the
/// per-language quirks without introducing a parallel knob.
pub enum VisibilityRule {
    /// Fixed string, no detection needed (e.g., "module-level", "default").
    Fixed(&'static str),
    /// Majority wins: count chunks where signature contains `keyword`.
    /// `(keyword, if_majority, if_minority)`.
    SigContainsMajority {
        keyword: &'static str,
        if_majority: &'static str,
        if_minority: &'static str,
    },
    /// Majority wins: count chunks where signature starts with `prefix`.
    /// `(prefix, if_majority, if_minority)`.
    SigStartsMajority {
        prefix: &'static str,
        if_majority: &'static str,
        if_minority: &'static str,
    },
    /// Two-keyword comparison: public vs internal (for .NET languages).
    /// Counts `contains(pub_kw)` vs `contains(int_kw)`.
    TwoKeywordCompare {
        pub_keyword: &'static str,
        int_keyword: &'static str,
        if_pub_wins: &'static str,
        if_int_wins: &'static str,
    },
    /// Solidity: `public || external` vs rest.
    SigContainsEitherMajority {
        keyword_a: &'static str,
        keyword_b: &'static str,
        if_majority: &'static str,
        if_minority: &'static str,
    },
    /// Three-way triage by `starts_with` prefix (Rust's `pub(crate)` / `pub` /
    /// private split). Counts chunks whose signature starts with `a`, those
    /// starting with `b`, and the rest. Tie-break: `a` wins when
    /// `count_a >= count_b && count_a >= count_else`; otherwise `b` wins when
    /// `count_b >= count_else`; otherwise `else_`. The visibility label is the
    /// matched prefix trimmed (so `b = "pub "` → label `"pub"`), except
    /// `else_` which is returned as-is.
    SigStartsTriage {
        a: &'static str,
        b: &'static str,
        else_: &'static str,
    },
    /// TS/JS-style imports with a visibility companion. The regex patterns
    /// replace prefix-based extraction (`import_prefixes` is ignored when this
    /// variant is the visibility rule) — a trimmed line counts as an import
    /// when it matches any pattern. Visibility itself is a simple signature
    /// check: if any chunk's signature contains `"export"` → `"export"`,
    /// otherwise `"module-private"`. The hardcoded labels match the legacy
    /// TS/JS branch in `extract_patterns`.
    RegexImportSet { patterns: &'static [&'static str] },
    /// Go-style name-case classification: count chunks whose name starts with
    /// an uppercase letter. Majority → `if_upper`; otherwise `if_lower`.
    NameCase {
        if_upper: &'static str,
        if_lower: &'static str,
    },
}

/// Data-driven definition for per-language pattern extraction.
///
/// Stored on `LanguageDef::patterns` so adding a new language with patterns
/// is one edit at the language row in `src/language/languages.rs` rather
/// than a second match arm here.
pub struct LanguagePatternDef {
    pub import_prefixes: &'static [&'static str],
    pub error_patterns: &'static [(&'static str, &'static str)],
    pub visibility: VisibilityRule,
    /// Substrings whose presence in any chunk's content flips
    /// `LocalPatterns::has_inline_tests` to `true`. Empty `&[]` keeps it
    /// `false` for languages without an inline-test convention.
    /// Rust uses `&["#[cfg(test)]"]` to mirror its `mod tests` idiom.
    pub inline_test_markers: &'static [&'static str],
}

/// Evaluate a `VisibilityRule` against chunks, returning the visibility string.
fn eval_visibility(rule: &VisibilityRule, chunks: &[ChunkSummary]) -> String {
    match rule {
        VisibilityRule::Fixed(s) => (*s).to_string(),
        VisibilityRule::SigContainsMajority {
            keyword,
            if_majority,
            if_minority,
        } => {
            let count = chunks
                .iter()
                .filter(|c| c.signature.contains(keyword))
                .count();
            if count > chunks.len() / 2 {
                if_majority
            } else {
                if_minority
            }
            .to_string()
        }
        VisibilityRule::SigStartsMajority {
            prefix,
            if_majority,
            if_minority,
        } => {
            let count = chunks
                .iter()
                .filter(|c| c.signature.starts_with(prefix))
                .count();
            if count > chunks.len() / 2 {
                if_majority
            } else {
                if_minority
            }
            .to_string()
        }
        VisibilityRule::TwoKeywordCompare {
            pub_keyword,
            int_keyword,
            if_pub_wins,
            if_int_wins,
        } => {
            let pub_count = chunks
                .iter()
                .filter(|c| c.signature.contains(pub_keyword))
                .count();
            let int_count = chunks
                .iter()
                .filter(|c| c.signature.contains(int_keyword))
                .count();
            if pub_count >= int_count {
                if_pub_wins
            } else {
                if_int_wins
            }
            .to_string()
        }
        VisibilityRule::SigContainsEitherMajority {
            keyword_a,
            keyword_b,
            if_majority,
            if_minority,
        } => {
            let count = chunks
                .iter()
                .filter(|c| c.signature.contains(keyword_a) || c.signature.contains(keyword_b))
                .count();
            if count > chunks.len() / 2 {
                if_majority
            } else {
                if_minority
            }
            .to_string()
        }
        VisibilityRule::SigStartsTriage { a, b, else_ } => {
            let count_a = chunks.iter().filter(|c| c.signature.starts_with(a)).count();
            let count_b = chunks.iter().filter(|c| c.signature.starts_with(b)).count();
            let count_else = chunks
                .iter()
                .filter(|c| !c.signature.starts_with(a) && !c.signature.starts_with(b))
                .count();
            if count_a >= count_b && count_a >= count_else {
                a.trim().to_string()
            } else if count_b >= count_else {
                b.trim().to_string()
            } else {
                (*else_).to_string()
            }
        }
        VisibilityRule::RegexImportSet { .. } => {
            // Import regexes drive extraction; visibility is a simple
            // signature contains check matching the legacy TS/JS semantics.
            if chunks.iter().any(|c| c.signature.contains("export")) {
                "export".to_string()
            } else {
                "module-private".to_string()
            }
        }
        VisibilityRule::NameCase { if_upper, if_lower } => {
            let exported = chunks
                .iter()
                .filter(|c| c.name.starts_with(|ch: char| ch.is_uppercase()))
                .count();
            if exported > chunks.len() / 2 {
                if_upper
            } else {
                if_lower
            }
            .to_string()
        }
    }
}

/// Per-language pattern defaults, exported for use by `LanguageDef` rows.
///
/// Each `pub static` is referenced by exactly one `LanguageDef::patterns`
/// slot in `src/language/languages.rs`. Adding a new language with patterns
/// means writing the row's `patterns: Some(&YOUR_DEF),` and (if reusing an
/// existing family) pointing at one of these statics.
pub mod patterns_data {
    use super::{LanguagePatternDef, VisibilityRule};

    pub static PYTHON: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["import ", "from "],
        error_patterns: &[("raise ", "raise"), ("try:", "try/except")],
        visibility: VisibilityRule::Fixed("module-level"),
        inline_test_markers: &[],
    };
    pub static C: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["#include"],
        error_patterns: &[("errno", "errno"), ("perror", "perror")],
        visibility: VisibilityRule::SigStartsMajority {
            prefix: "static ",
            if_majority: "static",
            if_minority: "extern",
        },
        inline_test_markers: &[],
    };
    pub static CPP_LIKE: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["#include"],
        error_patterns: &[
            ("errno", "errno"),
            ("throw ", "throw"),
            ("try {", "try/catch"),
        ],
        visibility: VisibilityRule::SigStartsMajority {
            prefix: "static ",
            if_majority: "static",
            if_minority: "extern",
        },
        inline_test_markers: &[],
    };
    pub static JAVA: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["import "],
        error_patterns: &[("throws ", "checked exceptions"), ("try {", "try/catch")],
        visibility: VisibilityRule::SigContainsMajority {
            keyword: "public",
            if_majority: "public",
            if_minority: "package-private",
        },
        inline_test_markers: &[],
    };
    pub static JVM: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["import "],
        error_patterns: &[("throws ", "checked exceptions"), ("try {", "try/catch")],
        visibility: VisibilityRule::SigContainsMajority {
            keyword: "public",
            if_majority: "public",
            if_minority: "package-private",
        },
        inline_test_markers: &[],
    };
    pub static DOTNET: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["using ", "open "],
        error_patterns: &[("throw ", "throw"), ("try {", "try/catch")],
        visibility: VisibilityRule::TwoKeywordCompare {
            pub_keyword: "public",
            int_keyword: "internal",
            if_pub_wins: "public",
            if_int_wins: "internal",
        },
        inline_test_markers: &[],
    };
    pub static RUBY: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["require ", "require_relative "],
        error_patterns: &[("raise ", "raise"), ("rescue", "begin/rescue")],
        visibility: VisibilityRule::Fixed("module-level"),
        inline_test_markers: &[],
    };
    pub static PHP: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["require ", "require_once ", "include ", "use "],
        error_patterns: &[("throw ", "throw"), ("try {", "try/catch")],
        visibility: VisibilityRule::SigContainsMajority {
            keyword: "public",
            if_majority: "public",
            if_minority: "default",
        },
        inline_test_markers: &[],
    };
    pub static PERL: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["use ", "require "],
        error_patterns: &[("die ", "die"), ("croak", "croak")],
        visibility: VisibilityRule::Fixed("module-level"),
        inline_test_markers: &[],
    };
    pub static LUA: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["require(", "require \"", "require '"],
        error_patterns: &[("error(", "error"), ("pcall(", "pcall")],
        visibility: VisibilityRule::Fixed("module-level"),
        inline_test_markers: &[],
    };
    pub static HASKELL: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["import "],
        error_patterns: &[("error ", "error"), ("throwIO", "throwIO")],
        visibility: VisibilityRule::Fixed("module-level"),
        inline_test_markers: &[],
    };
    pub static OCAML: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["open "],
        error_patterns: &[("raise ", "raise"), ("Result.", "Result")],
        visibility: VisibilityRule::Fixed("module-level"),
        inline_test_markers: &[],
    };
    pub static ELIXIR: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["import ", "alias ", "use ", "require "],
        error_patterns: &[("raise ", "raise"), ("{:error,", "{:error, _}")],
        visibility: VisibilityRule::SigStartsMajority {
            prefix: "defp ",
            if_majority: "private",
            if_minority: "public",
        },
        inline_test_markers: &[],
    };
    pub static ERLANG: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["-include"],
        error_patterns: &[("throw(", "throw"), ("{error,", "{error, _}")],
        visibility: VisibilityRule::Fixed("module-level"),
        inline_test_markers: &[],
    };
    pub static GLEAM: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["import "],
        error_patterns: &[("Error(", "Error"), ("Result(", "Result")],
        visibility: VisibilityRule::SigStartsMajority {
            prefix: "pub ",
            if_majority: "pub",
            if_minority: "private",
        },
        inline_test_markers: &[],
    };
    pub static R_LANG: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["library(", "require("],
        error_patterns: &[],
        visibility: VisibilityRule::Fixed("default"),
        inline_test_markers: &[],
    };
    pub static JULIA: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["using ", "import "],
        error_patterns: &[("throw(", "throw"), ("error(", "error")],
        visibility: VisibilityRule::Fixed("module-level"),
        inline_test_markers: &[],
    };
    pub static ZIG: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["@import("],
        error_patterns: &[("error.", "error set"), ("catch", "catch")],
        visibility: VisibilityRule::SigStartsMajority {
            prefix: "pub ",
            if_majority: "pub",
            if_minority: "private",
        },
        inline_test_markers: &[],
    };
    pub static SWIFT: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["import "],
        error_patterns: &[("throw ", "throw"), ("try ", "do/catch")],
        visibility: VisibilityRule::SigContainsMajority {
            keyword: "public",
            if_majority: "public",
            if_minority: "internal",
        },
        inline_test_markers: &[],
    };
    pub static SOLIDITY: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["import "],
        error_patterns: &[("revert ", "revert"), ("require(", "require")],
        visibility: VisibilityRule::SigContainsEitherMajority {
            keyword_a: "public",
            keyword_b: "external",
            if_majority: "public",
            if_minority: "internal",
        },
        inline_test_markers: &[],
    };
    pub static BASH: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["source ", ". "],
        error_patterns: &[("exit ", "exit code"), ("set -e", "set -e")],
        visibility: VisibilityRule::Fixed("default"),
        inline_test_markers: &[],
    };
    pub static POWERSHELL: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["Import-Module ", "using module "],
        error_patterns: &[("throw ", "throw"), ("try {", "try/catch")],
        visibility: VisibilityRule::Fixed("default"),
        inline_test_markers: &[],
    };

    // --- New rows that fold the formerly-custom Rust / TS-JS / Go arms back
    // into data. Each row mirrors the exact semantics of the match arm it
    // replaces — see the doc comments on `SigStartsTriage`, `RegexImportSet`,
    // and `NameCase` for evaluation details.
    pub static RUST: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["use "],
        error_patterns: &[
            ("anyhow::", "anyhow"),
            ("thiserror", "thiserror"),
            ("Result<", "Result<>"),
        ],
        visibility: VisibilityRule::SigStartsTriage {
            a: "pub(crate)",
            b: "pub ",
            else_: "private",
        },
        inline_test_markers: &["#[cfg(test)]"],
    };
    pub static TS_JS: LanguagePatternDef = LanguagePatternDef {
        // `import_prefixes` is ignored for RegexImportSet — extraction uses
        // the regex list below. Kept empty so the intent is obvious.
        import_prefixes: &[],
        error_patterns: &[
            ("throw ", "throw"),
            (".catch(", "try/catch"),
            ("try {", "try/catch"),
        ],
        visibility: VisibilityRule::RegexImportSet {
            // `^import\s` matches `import foo` and `import{…}`; the second
            // pattern covers CJS `const x = require(...)`. Compile is per
            // call (two patterns, small input).
            patterns: &[r"^import\s", r"^const\s.*require\("],
        },
        inline_test_markers: &[],
    };
    pub static GO: LanguagePatternDef = LanguagePatternDef {
        import_prefixes: &["import "],
        error_patterns: &[("error", "error return")],
        visibility: VisibilityRule::NameCase {
            if_upper: "exported",
            if_lower: "unexported",
        },
        inline_test_markers: &[],
    };
}

/// Look up the per-language pattern definition.
///
/// Delegates to `LanguageDef::patterns` so that adding or changing the
/// pattern data for a language is a one-line edit at the language row in
/// `src/language/languages.rs`, not a second match arm here. Returns `None`
/// only for non-code languages (SQL, Markdown, JSON, etc.) that have no
/// meaningful local patterns — every code-carrying language ships a
/// `LanguagePatternDef` row.
fn pattern_def_for(lang: Language) -> Option<&'static LanguagePatternDef> {
    lang.def().patterns
}

/// Extract imports when the visibility rule is `RegexImportSet`. The regex
/// patterns replace simple prefix matching — a trimmed line counts as an
/// import when any pattern matches. Regex compilation failures skip that
/// pattern (logged) so one broken entry can't kill the whole extraction.
fn extract_imports_regex(
    chunks: &[ChunkSummary],
    patterns: &[&'static str],
    max: usize,
) -> Vec<String> {
    let compiled: Vec<regex::Regex> = patterns
        .iter()
        .filter_map(|p| match regex::Regex::new(p) {
            Ok(re) => Some(re),
            Err(e) => {
                tracing::warn!(pattern = %p, error = %e, "invalid import regex; skipping");
                None
            }
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut imports = Vec::new();
    for chunk in chunks {
        for line in chunk.content.lines() {
            let trimmed = line.trim();
            if compiled.iter().any(|re| re.is_match(trimmed))
                && imports.len() < max
                && seen.insert(trimmed.to_string())
            {
                imports.push(trimmed.to_string());
            }
        }
    }
    imports
}

/// Extract local coding patterns from a file's chunks.
/// Iterates chunks individually instead of concatenating all content into
/// one string (avoids a large allocation for files with many chunks). Every
/// code-carrying language is handled via `LanguagePatternDef` lookup — the
/// formerly-custom Rust/TS-JS/Go arms now live as data rows using the
/// `SigStartsTriage`, `RegexImportSet`, and `NameCase` variants.
fn extract_patterns(chunks: &[ChunkSummary], language: Option<Language>) -> LocalPatterns {
    let mut error_style = String::new();
    let mut has_inline_tests = false;

    let (imports, visibility) = match language.and_then(pattern_def_for) {
        Some(def) => {
            // Import extraction: regex override when `RegexImportSet`, else
            // prefix-based.
            let imports = match &def.visibility {
                VisibilityRule::RegexImportSet { patterns } => {
                    extract_imports_regex(chunks, patterns, MAX_IMPORT_COUNT)
                }
                _ => extract_imports(chunks, def.import_prefixes, MAX_IMPORT_COUNT),
            };
            if !def.error_patterns.is_empty() {
                error_style = detect_error_style(chunks, def.error_patterns);
            }
            // Inline-test marker scan (empty `&[]` short-circuits to false).
            if !def.inline_test_markers.is_empty() {
                has_inline_tests = chunks.iter().any(|c| {
                    def.inline_test_markers
                        .iter()
                        .any(|m| c.content.contains(m))
                });
            }
            let vis = eval_visibility(&def.visibility, chunks);
            (imports, vis)
        }
        // Non-code languages (SQL, Markdown, JSON, etc.) or `language = None`.
        None => (Vec::new(), "default".to_string()),
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

    /// Creates a ChunkSummary struct with test data for a function code chunk.
    /// # Arguments
    /// * `name` - The name of the function chunk
    /// * `sig` - The function signature string
    /// * `content` - The function body content
    /// * `lang` - The programming language of the chunk
    /// # Returns
    /// A ChunkSummary struct populated with the provided parameters and default test values (file path "src/test.rs", lines 1-10, chunk_type as Function, and empty/None fields for doc, parent_id, parent_type_name, content_hash, and window_idx).
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
            parent_type_name: None,
            content_hash: String::new(),
            window_idx: None,
            parser_version: 0,
            vendored: false,
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
