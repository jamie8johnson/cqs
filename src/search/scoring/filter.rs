//! SQL filter building and glob compilation.

use crate::store::helpers::sql::make_placeholders_offset;
use crate::store::helpers::SearchFilter;

use super::name_match::is_name_like_query;

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

    // Each branch uses the cached `make_placeholders_offset` helper instead
    // of inlining `(0..n).map(format!).collect::<Vec<_>>().join(",")`.
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

    // Select columns: always id + origin + embedding, optionally name for
    // hybrid scoring or demotion (test function detection needs the name).
    // `origin` is the authoritative file path the scoring loop feeds to the
    // glob/note-boost/importance signals — never a substring parsed from `id`.
    let need_name = use_hybrid || filter.enable_demotion;
    let columns = if need_name {
        "rowid, id, origin, embedding, name"
    } else {
        "rowid, id, origin, embedding"
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

    // ===== build_filter_sql tests =====

    #[test]
    fn test_build_filter_sql_default() {
        let filter = SearchFilter::default();
        let fsql = build_filter_sql(&filter);
        assert!(fsql.conditions.is_empty());
        assert!(fsql.bind_values.is_empty());
        // Default has enable_demotion=true, which requires name column
        assert_eq!(fsql.columns, "rowid, id, origin, embedding, name");
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
        assert_eq!(fsql.columns, "rowid, id, origin, embedding");
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

    // ===== language/chunk_type filter set tests =====

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
        // eq_ignore_ascii_case avoids per-candidate allocation
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

    // ===== exclude_types filter tests =====

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
