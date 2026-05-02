//! SQL filter building, glob compilation, and chunk ID parsing.

use crate::store::helpers::sql::make_placeholders_offset;
use crate::store::helpers::SearchFilter;

use super::name_match::is_name_like_query;

/// Extract file path from a chunk ID.
/// Standard format: `"path:line_start:hash_prefix"` (3 segments from right)
/// Windowed format: `"path:line_start:hash_prefix:wN"` (4 segments)
/// Markdown table-window format: `"path:line_start:hash_prefix:tNwM"` (4 segments,
/// emitted by `parser/markdown/tables.rs::emit_table_window`)
/// The hash_prefix is always 8 hex chars. Windowed chunk IDs append a window
/// suffix: either `wN` (generic windowed chunks) or `tNwM` (markdown tables).
pub(crate) fn extract_file_from_chunk_id(id: &str) -> &str {
    // Strip last segment
    let Some(last_colon) = id.rfind(':') else {
        return id;
    };
    let last_seg = &id[last_colon + 1..];

    // Determine how many segments to strip from the right:
    // - Standard: 2 (hash_prefix, line_start)
    // - Windowed: 3 (wN or tNwM, hash_prefix, line_start)
    // Window suffix formats:
    //   - "w0", "w1", ..., "w99" (generic)
    //   - "t0w0", "t1w3", ..., "tNwM" (markdown table windows)
    let segments_to_strip = if is_window_suffix(last_seg) { 3 } else { 2 };

    let mut end = id.len();
    for _ in 0..segments_to_strip {
        if let Some(i) = id[..end].rfind(':') {
            end = i;
        } else {
            break;
        }
    }
    &id[..end]
}

/// Returns `true` if `seg` looks like a window suffix produced by the parser:
/// either `wN` (generic windowed chunks) or `tNwM` (markdown table windows
/// from `parser/markdown/tables.rs::emit_table_window`).
fn is_window_suffix(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    // Generic: "wN" — 'w' followed by 1+ ASCII digits, total length ≤ 3
    if bytes.first() == Some(&b'w')
        && bytes.len() >= 2
        && bytes.len() <= 3
        && bytes[1..].iter().all(u8::is_ascii_digit)
    {
        return true;
    }
    // Table-window: "tNwM" — 't' + digits + 'w' + digits
    if bytes.first() == Some(&b't') && bytes.len() >= 4 {
        // Find the 'w' separator after the t-digits
        let mut i = 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        // Need at least one digit after 't', then 'w', then at least one digit
        if i >= 2 && i < bytes.len() && bytes[i] == b'w' {
            let rest = &bytes[i + 1..];
            if !rest.is_empty() && rest.iter().all(u8::is_ascii_digit) {
                return true;
            }
        }
    }
    false
}

/// Compile a glob pattern into a matcher, logging and ignoring invalid patterns.
/// Returns `None` if the pattern is `None` or invalid (with a warning logged).
pub(crate) fn compile_glob_filter(pattern: Option<&String>) -> Option<globset::GlobMatcher> {
    pattern.and_then(|p| match globset::Glob::new(p) {
        Ok(g) => Some(g.compile_matcher()),
        Err(e) => {
            tracing::warn!(pattern = %p, error = %e, "Invalid glob pattern, ignoring filter");
            None
        }
    })
}

/// Result of assembling SQL WHERE conditions from a [`SearchFilter`].
/// Separates filter analysis (testable without a database) from SQL execution.
/// The caller combines these pieces with cursor-specific clauses (rowid, LIMIT).
pub(crate) struct FilterSql {
    /// SQL WHERE conditions (e.g., `"language IN (?1,?2)"`)
    pub conditions: Vec<String>,
    /// Bind values corresponding to the placeholders in `conditions`, in order
    pub bind_values: Vec<String>,
    /// Column list for SELECT (includes `name` when hybrid scoring or demotion is needed)
    pub columns: &'static str,
    /// Whether hybrid name+embedding scoring is active
    pub use_hybrid: bool,
    /// Whether RRF fusion with FTS keyword search is active
    pub use_rrf: bool,
}

/// Build SQL filter components from a [`SearchFilter`].
/// Pure function — no database access. Returns conditions, bind values, and
/// the column list needed for the scoring loop. Bind parameter indices are
/// 1-based and contiguous.
pub(crate) fn build_filter_sql(filter: &SearchFilter) -> FilterSql {
    let mut conditions = Vec::new();
    let mut bind_values: Vec<String> = Vec::new();

    // P3 #130: each branch uses the cached `make_placeholders_offset` helper
    // instead of inlining `(0..n).map(format!).collect::<Vec<_>>().join(",")`.
    // Bind indices stay 1-based and contiguous across branches.
    if let Some(ref langs) = filter.languages {
        let placeholders = make_placeholders_offset(langs.len(), bind_values.len() + 1);
        conditions.push(format!("language COLLATE NOCASE IN ({placeholders})"));
        for lang in langs {
            bind_values.push(lang.to_string());
        }
    }

    if let Some(ref types) = filter.include_types {
        let placeholders = make_placeholders_offset(types.len(), bind_values.len() + 1);
        conditions.push(format!("chunk_type IN ({placeholders})"));
        for ct in types {
            bind_values.push(ct.to_string());
        }
    }

    if let Some(ref types) = filter.exclude_types {
        let placeholders = make_placeholders_offset(types.len(), bind_values.len() + 1);
        conditions.push(format!("chunk_type NOT IN ({placeholders})"));
        for ct in types {
            bind_values.push(ct.to_string());
        }
    }

    let use_hybrid = filter.name_boost > 0.0
        && !filter.query_text.is_empty()
        && is_name_like_query(&filter.query_text);
    let use_rrf = filter.enable_rrf && !filter.query_text.is_empty();

    // Select columns: always id + embedding, optionally name for hybrid scoring
    // or demotion (test function detection needs the name)
    let need_name = use_hybrid || filter.enable_demotion;
    let columns = if need_name {
        "rowid, id, embedding, name"
    } else {
        "rowid, id, embedding"
    };

    FilterSql {
        conditions,
        bind_values,
        columns,
        use_hybrid,
        use_rrf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ===== compile_glob_filter tests =====

    #[test]
    fn test_compile_glob_filter_none() {
        assert!(compile_glob_filter(None).is_none());
    }

    #[test]
    fn test_compile_glob_filter_valid() {
        let pattern = "src/**/*.rs".to_string();
        let matcher = compile_glob_filter(Some(&pattern));
        assert!(matcher.is_some());
        let m = matcher.unwrap();
        assert!(m.is_match("src/cli/mod.rs"));
        assert!(!m.is_match("tests/foo.py"));
    }

    #[test]
    fn test_compile_glob_filter_invalid() {
        let pattern = "[invalid".to_string();
        assert!(compile_glob_filter(Some(&pattern)).is_none());
    }

    // ===== extract_file_from_chunk_id tests =====

    #[test]
    fn test_extract_file_standard_chunk_id() {
        // Standard: "path:line_start:hash_prefix"
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_windowed_chunk_id() {
        // Windowed: "path:line_start:hash_prefix:wN"
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w0"),
            "src/foo.rs"
        );
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w3"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_nested_path() {
        assert_eq!(
            extract_file_from_chunk_id("src/cli/commands/mod.rs:42:deadbeef"),
            "src/cli/commands/mod.rs"
        );
        assert_eq!(
            extract_file_from_chunk_id("src/cli/commands/mod.rs:42:deadbeef:w1"),
            "src/cli/commands/mod.rs"
        );
    }

    #[test]
    fn test_extract_file_windowed_chunk_id_w_prefix() {
        // Windowed IDs use "wN" format (not bare digits)
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w0"),
            "src/foo.rs"
        );
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w12"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_hash_not_confused_with_window() {
        // 8-char hex hash should NOT be mistaken for a window index
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:deadbeef"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_markdown_table_window() {
        // AC-V1.33-1: markdown table windows produce `:tNwM` suffix
        // (from src/parser/markdown/tables.rs::emit_table_window)
        assert_eq!(
            extract_file_from_chunk_id("docs/x.md:10:abc12345:t0w3"),
            "docs/x.md"
        );
        assert_eq!(
            extract_file_from_chunk_id("docs/x.md:42:abc12345:t1w0"),
            "docs/x.md"
        );
        assert_eq!(
            extract_file_from_chunk_id("docs/foo/bar.md:5:cafebabe:t12w99"),
            "docs/foo/bar.md"
        );
    }

    #[test]
    fn test_is_window_suffix_recognizes_both_formats() {
        // Generic wN
        assert!(is_window_suffix("w0"));
        assert!(is_window_suffix("w99"));
        // Table tNwM
        assert!(is_window_suffix("t0w0"));
        assert!(is_window_suffix("t12w99"));
        // Negative: hash prefixes, plain hex
        assert!(!is_window_suffix("deadbeef"));
        assert!(!is_window_suffix("abc12345"));
        assert!(!is_window_suffix("w")); // no digits
        assert!(!is_window_suffix("t1w")); // no digits after w
        assert!(!is_window_suffix("tw0")); // no digits after t
        assert!(!is_window_suffix(""));
    }

    #[test]
    fn test_extract_file_no_colons() {
        assert_eq!(extract_file_from_chunk_id("justanid"), "justanid");
    }

    // ===== build_filter_sql tests =====

    #[test]
    fn test_build_filter_sql_default() {
        let filter = SearchFilter::default();
        let fsql = build_filter_sql(&filter);
        assert!(fsql.conditions.is_empty());
        assert!(fsql.bind_values.is_empty());
        // Default has enable_demotion=true, which requires name column
        assert_eq!(fsql.columns, "rowid, id, embedding, name");
        assert!(!fsql.use_hybrid);
        assert!(!fsql.use_rrf);
    }

    #[test]
    fn test_build_filter_sql_no_name_column() {
        // Explicitly disable demotion + no hybrid → no name column needed
        let filter = SearchFilter {
            enable_demotion: false,
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.columns, "rowid, id, embedding");
    }

    #[test]
    fn test_build_filter_sql_language_filter() {
        use crate::parser::Language;
        let filter = SearchFilter {
            languages: Some(vec![Language::Rust, Language::Python]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 1);
        assert!(fsql.conditions[0].starts_with("language COLLATE NOCASE IN"));
        assert_eq!(fsql.bind_values.len(), 2);
        assert_eq!(fsql.bind_values[0], "rust");
        assert_eq!(fsql.bind_values[1], "python");
    }

    #[test]
    fn test_build_filter_sql_chunk_type_filter() {
        use crate::parser::ChunkType;
        let filter = SearchFilter {
            include_types: Some(vec![ChunkType::Function]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 1);
        assert!(fsql.conditions[0].starts_with("chunk_type IN"));
        assert_eq!(fsql.bind_values.len(), 1);
    }

    #[test]
    fn test_build_filter_sql_combined_filters() {
        use crate::parser::{ChunkType, Language};
        let filter = SearchFilter {
            languages: Some(vec![Language::Rust]),
            include_types: Some(vec![ChunkType::Function, ChunkType::Method]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 2);
        // 1 language + 2 chunk types = 3 bind values
        assert_eq!(fsql.bind_values.len(), 3);
        // Verify contiguous bind param indices: language gets ?1, include_types get ?2,?3
        assert!(fsql.conditions[0].contains("?1"));
        assert!(fsql.conditions[1].contains("?2"));
        assert!(fsql.conditions[1].contains("?3"));
    }

    #[test]
    fn test_build_filter_sql_hybrid_flags() {
        let filter = SearchFilter {
            name_boost: 0.3,
            query_text: "parse".to_string(),
            enable_rrf: true,
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert!(fsql.use_hybrid);
        assert!(fsql.use_rrf);
        // name needed for hybrid scoring
        assert!(fsql.columns.contains("name"));
    }

    #[test]
    fn test_build_filter_sql_demotion_includes_name() {
        let filter = SearchFilter {
            enable_demotion: true,
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert!(fsql.columns.contains("name"));
    }

    #[test]
    fn test_build_filter_sql_rrf_needs_query_text() {
        // RRF enabled but empty query text → use_rrf should be false
        let filter = SearchFilter {
            enable_rrf: true,
            query_text: String::new(),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert!(!fsql.use_rrf);
    }

    // ===== language/chunk_type filter set tests (TC-3) =====

    #[test]
    fn test_lang_filter_set_membership() {
        use crate::language::Language;
        let langs = [Language::Rust, Language::Python];
        let lang_set: HashSet<String> =
            langs.iter().map(|l| l.to_string().to_lowercase()).collect();
        assert!(lang_set.contains("rust"));
        assert!(lang_set.contains("python"));
        assert!(!lang_set.contains("typescript"));
        assert!(!lang_set.contains("go"));
    }

    #[test]
    fn test_chunk_type_filter_set_membership() {
        use crate::language::ChunkType;
        let types = [ChunkType::Function, ChunkType::Method];
        let type_set: HashSet<String> =
            types.iter().map(|t| t.to_string().to_lowercase()).collect();
        assert!(type_set.contains("function"));
        assert!(type_set.contains("method"));
        assert!(!type_set.contains("struct"));
        assert!(!type_set.contains("class"));
    }

    #[test]
    fn test_lang_filter_case_insensitive() {
        use crate::language::Language;
        let langs = [Language::Rust];
        let lang_set: HashSet<String> =
            langs.iter().map(|l| l.to_string().to_lowercase()).collect();
        // eq_ignore_ascii_case avoids per-candidate allocation (PERF-17)
        assert!(lang_set.iter().any(|l| "rust".eq_ignore_ascii_case(l)));
        assert!(lang_set.iter().any(|l| "Rust".eq_ignore_ascii_case(l)));
        assert!(!lang_set.iter().any(|l| "Python".eq_ignore_ascii_case(l)));
    }

    #[test]
    fn test_lang_filter_none_passes_all() {
        // When filter.languages is None, lang_set is None and all candidates pass
        let lang_set: Option<HashSet<String>> = None;
        let candidate_lang = "rust";
        let passes = lang_set
            .as_ref()
            .is_none_or(|s| s.iter().any(|l| candidate_lang.eq_ignore_ascii_case(l)));
        assert!(passes);
    }

    #[test]
    fn test_type_filter_none_passes_all() {
        // When filter.include_types is None, type_set is None and all candidates pass
        let type_set: Option<HashSet<String>> = None;
        let candidate_type = "struct";
        let passes = type_set
            .as_ref()
            .is_none_or(|s| s.iter().any(|t| candidate_type.eq_ignore_ascii_case(t)));
        assert!(passes);
    }

    #[test]
    fn test_lang_filter_empty_rejects_all() {
        // Empty language list means nothing passes
        let lang_set: Option<HashSet<String>> = Some(HashSet::new());
        let passes = lang_set
            .as_ref()
            .is_none_or(|s| s.iter().any(|l| "rust".eq_ignore_ascii_case(l)));
        assert!(!passes);
    }

    // ===== TC-27: exclude_types filter tests =====

    #[test]
    fn tc27_exclude_types_generates_not_in_condition() {
        use crate::parser::ChunkType;
        let filter = SearchFilter {
            exclude_types: Some(vec![ChunkType::Function]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 1);
        assert!(
            fsql.conditions[0].contains("NOT IN"),
            "exclude_types should produce NOT IN, got: {}",
            fsql.conditions[0]
        );
        assert_eq!(fsql.bind_values.len(), 1);
        assert_eq!(fsql.bind_values[0].to_lowercase(), "function");
    }

    #[test]
    fn tc27_exclude_types_multiple_types() {
        use crate::parser::ChunkType;
        let filter = SearchFilter {
            exclude_types: Some(vec![
                ChunkType::Function,
                ChunkType::Test,
                ChunkType::Variable,
            ]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 1);
        assert!(fsql.conditions[0].contains("NOT IN"));
        assert_eq!(fsql.bind_values.len(), 3);
    }

    #[test]
    fn tc27_include_and_exclude_types_both_applied() {
        use crate::parser::ChunkType;
        // When both include_types and exclude_types are set, both SQL conditions
        // should be generated. The SQL engine handles the overlap.
        let filter = SearchFilter {
            include_types: Some(vec![ChunkType::Function, ChunkType::Method]),
            exclude_types: Some(vec![ChunkType::Function]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(
            fsql.conditions.len(),
            2,
            "Should have both IN and NOT IN conditions"
        );
        let has_in = fsql
            .conditions
            .iter()
            .any(|c| c.starts_with("chunk_type IN"));
        let has_not_in = fsql
            .conditions
            .iter()
            .any(|c| c.starts_with("chunk_type NOT IN"));
        assert!(has_in, "Should have include_types IN condition");
        assert!(has_not_in, "Should have exclude_types NOT IN condition");
        // include: Function + Method = 2 bind values, exclude: Function = 1 bind value
        assert_eq!(fsql.bind_values.len(), 3);
    }

    #[test]
    fn tc27_exclude_types_bind_params_contiguous() {
        use crate::parser::{ChunkType, Language};
        // Verify bind param indices are correct when combined with language filter
        let filter = SearchFilter {
            languages: Some(vec![Language::Rust]),
            exclude_types: Some(vec![ChunkType::Test]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 2);
        // Language gets ?1, exclude_types gets ?2
        assert!(fsql.conditions[0].contains("?1"));
        assert!(fsql.conditions[1].contains("?2"));
        assert_eq!(fsql.bind_values.len(), 2);
    }
}
