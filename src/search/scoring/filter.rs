//! SQL filter building, glob compilation, and chunk ID parsing.

use crate::store::helpers::SearchFilter;

use super::name_match::is_name_like_query;

/// Extract file path from a chunk ID.
/// Standard format: `"path:line_start:hash_prefix"` (3 segments from right)
/// Windowed format: `"path:line_start:hash_prefix:wN"` (4 segments)
/// The hash_prefix is always 8 hex chars. Windowed chunk IDs append `:wN` where
/// N is a small integer (0-99). We detect windowed IDs by checking if the last
/// segment starts with 'w' followed by digits.
pub(crate) fn extract_file_from_chunk_id(id: &str) -> &str {
    // Strip last segment
    let Some(last_colon) = id.rfind(':') else {
        return id;
    };
    let last_seg = &id[last_colon + 1..];

    // Determine how many segments to strip from the right:
    // - Standard: 2 (hash_prefix, line_start)
    // - Windowed: 3 (wN, hash_prefix, line_start)
    // Window suffix format: "w0", "w1", ..., "w99"
    let segments_to_strip = if !last_seg.is_empty()
        && last_seg.starts_with('w')
        && last_seg.len() <= 3
        && last_seg[1..].bytes().all(|b| b.is_ascii_digit())
    {
        3
    } else {
        2
    };

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

    if let Some(ref langs) = filter.languages {
        let placeholders: Vec<_> = (0..langs.len())
            .map(|i| format!("?{}", bind_values.len() + i + 1))
            .collect();
        conditions.push(format!(
            "language COLLATE NOCASE IN ({})",
            placeholders.join(",")
        ));
        for lang in langs {
            bind_values.push(lang.to_string());
        }
    }

    if let Some(ref types) = filter.chunk_types {
        let placeholders: Vec<_> = (0..types.len())
            .map(|i| format!("?{}", bind_values.len() + i + 1))
            .collect();
        conditions.push(format!("chunk_type IN ({})", placeholders.join(",")));
        for ct in types {
            bind_values.push(ct.to_string());
        }
    }

    if let Some(ref types) = filter.exclude_types {
        let placeholders: Vec<_> = (0..types.len())
            .map(|i| format!("?{}", bind_values.len() + i + 1))
            .collect();
        conditions.push(format!("chunk_type NOT IN ({})", placeholders.join(",")));
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
            chunk_types: Some(vec![ChunkType::Function]),
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
            chunk_types: Some(vec![ChunkType::Function, ChunkType::Method]),
            ..Default::default()
        };
        let fsql = build_filter_sql(&filter);
        assert_eq!(fsql.conditions.len(), 2);
        // 1 language + 2 chunk types = 3 bind values
        assert_eq!(fsql.bind_values.len(), 3);
        // Verify contiguous bind param indices: language gets ?1, chunk_types get ?2,?3
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
        let langs = vec![Language::Rust, Language::Python];
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
        let types = vec![ChunkType::Function, ChunkType::Method];
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
        let langs = vec![Language::Rust];
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
        let passes = lang_set.as_ref().map_or(true, |s| {
            s.iter().any(|l| candidate_lang.eq_ignore_ascii_case(l))
        });
        assert!(passes);
    }

    #[test]
    fn test_type_filter_none_passes_all() {
        // When filter.chunk_types is None, type_set is None and all candidates pass
        let type_set: Option<HashSet<String>> = None;
        let candidate_type = "struct";
        let passes = type_set.as_ref().map_or(true, |s| {
            s.iter().any(|t| candidate_type.eq_ignore_ascii_case(t))
        });
        assert!(passes);
    }

    #[test]
    fn test_lang_filter_empty_rejects_all() {
        // Empty language list means nothing passes
        let lang_set: Option<HashSet<String>> = Some(HashSet::new());
        let passes = lang_set
            .as_ref()
            .map_or(true, |s| s.iter().any(|l| "rust".eq_ignore_ascii_case(l)));
        assert!(!passes);
    }
}
