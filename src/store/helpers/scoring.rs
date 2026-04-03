//! Name scoring functions for definition search.

/// Score a chunk name against a query for definition search (search_by_name).
///
/// Returns a score between 0.0 and 1.0:
/// - 1.0: exact match (case-insensitive)
/// - 0.9: prefix match
/// - 0.7: substring match
/// - 0.0: no name relationship
///
/// For batch/loop usage where the same query is reused, prefer
/// [`score_name_match_pre_lower`] with pre-lowercased strings to avoid
/// redundant `to_lowercase()` allocations.
pub fn score_name_match(name: &str, query: &str) -> f32 {
    if query.is_empty() {
        return 0.0;
    }
    let name_lower = name.to_lowercase();
    let query_lower = query.to_lowercase();
    score_name_match_pre_lower(&name_lower, &query_lower)
}

/// Score a pre-lowercased chunk name against a pre-lowercased query.
///
/// Same scoring logic as [`score_name_match`] but skips `to_lowercase()`.
/// Use when calling in a loop where caller can pre-lowercase outside the loop
/// to avoid redundant heap allocations.
///
/// Returns a score between 0.0 and 1.0:
/// - 1.0: exact match
/// - 0.9: prefix match
/// - 0.7: substring match
/// - 0.0: no name relationship
#[inline]
pub fn score_name_match_pre_lower(name_lower: &str, query_lower: &str) -> f32 {
    if query_lower.is_empty() {
        return 0.0;
    }
    if name_lower == query_lower {
        1.0
    } else if name_lower.starts_with(query_lower) {
        0.9
    } else if query_lower.contains(name_lower) {
        0.8
    } else if name_lower.contains(query_lower) {
        0.7
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_name_match_exact() {
        assert_eq!(score_name_match("parse_diff", "parse_diff"), 1.0);
        assert_eq!(score_name_match("Parse_Diff", "parse_diff"), 1.0);
    }

    #[test]
    fn test_score_name_match_prefix() {
        assert_eq!(score_name_match("parse_diff_hunks", "parse_diff"), 0.9);
    }

    #[test]
    fn test_score_name_match_substring() {
        assert_eq!(score_name_match("do_parse_diff", "parse_diff"), 0.7);
    }

    #[test]
    fn test_score_name_match_no_match_returns_zero() {
        assert_eq!(score_name_match("parse_diff", "reverse_bfs"), 0.0);
        assert_eq!(score_name_match("foo", "bar"), 0.0);
    }

    #[test]
    fn test_score_name_match_empty_query() {
        assert_eq!(score_name_match("foo", ""), 0.0);
    }

    #[test]
    fn test_score_name_match_case_insensitive() {
        assert_eq!(score_name_match("FooBar", "foobar"), 1.0);
    }
}
