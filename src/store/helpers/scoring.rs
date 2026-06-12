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

/// Zero-alloc analogue of `score_name_match_pre_lower` for the ASCII fast
/// path. Both inputs must be ASCII; `query_lower` must already be lowercase.
/// Returns the same 1.0 / 0.9 / 0.8 / 0.7 / 0.0 tiers as the reference
/// function (see `helpers::scoring`). Avoids per-row
/// `chunk.name.to_lowercase()` allocations on the dominant code-identifier
/// path inside `search_by_name`.
///
/// Tier order (must match `score_name_match_pre_lower` exactly):
///   1.0 — `name == query` (case-insensitive)
///   0.9 — `name.starts_with(query)` (case-insensitive)
///   0.8 — `query.contains(name)` (i.e. name is substring of query)
///   0.7 — `name.contains(query)` (i.e. query is substring of name)
///   0.0 — no relationship
///
/// Empty-name corner case: `query.contains("")` is true for any query, so
/// the reference returns 0.8 when `name == ""` and `query` is non-empty
/// (`std::str::contains` semantics). We preserve that quirk for parity.
pub(crate) fn score_name_match_ascii(name_raw: &str, query_lower: &str) -> f32 {
    debug_assert!(name_raw.is_ascii());
    debug_assert!(query_lower.is_ascii());
    debug_assert!(query_lower.bytes().all(|b| !b.is_ascii_uppercase()));
    if query_lower.is_empty() {
        return 0.0;
    }
    if name_raw.eq_ignore_ascii_case(query_lower) {
        return 1.0;
    }
    let n = name_raw.as_bytes();
    let q = query_lower.as_bytes();
    // 0.9 — case-insensitive prefix match. `starts_with("")` is true, but
    // `query_lower.is_empty()` is already short-circuited above, so
    // `q.len() == 0` is unreachable here.
    if n.len() >= q.len()
        && n[..q.len()]
            .iter()
            .zip(q)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        return 0.9;
    }
    // 0.8 — name is substring of query. `ascii_substring_ignore_case` mirrors
    // `str::contains` and returns true for an empty needle, preserving the
    // reference's empty-name → 0.8 quirk.
    if q.len() >= n.len() && ascii_substring_ignore_case(q, n) {
        return 0.8;
    }
    // 0.7 — query is substring of name (so query shorter; name "do_parse"
    // contains "parse"). Scan `n` for `q`.
    if n.len() >= q.len() && ascii_substring_ignore_case(n, q) {
        return 0.7;
    }
    0.0
}

#[inline]
fn ascii_substring_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    let last_start = haystack.len() - needle.len();
    for i in 0..=last_start {
        if haystack[i..i + needle.len()]
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `score_name_match_ascii` must produce the same tier as
    /// `score_name_match_pre_lower` for every ASCII case. Pins parity so the
    /// per-row `to_lowercase()` skip doesn't silently change ranking.
    #[test]
    fn score_name_match_ascii_matches_reference_for_ascii_inputs() {
        let cases: &[(&str, &str)] = &[
            ("parse_diff", "parse_diff"),    // 1.0 exact
            ("Parse_Diff", "parse_diff"),    // 1.0 case-insensitive exact
            ("parse_diff_hunks", "parse"),   // 0.9 prefix
            ("ParseDiff", "parse"),          // 0.9 prefix case-insensitive
            ("foo", "foo_bar_qux"),          // 0.8 query contains name
            ("do_parse_diff", "parse_diff"), // 0.7 name contains query
            ("foo", "bar"),                  // 0.0 no relation
            ("", "anything"),                // 0.0 empty name
        ];
        for (raw_name, query) in cases {
            let q_lower = query.to_lowercase();
            let n_lower = raw_name.to_lowercase();
            let reference = score_name_match_pre_lower(&n_lower, &q_lower);
            let ascii = score_name_match_ascii(raw_name, &q_lower);
            assert_eq!(
                ascii, reference,
                "mismatch for ({raw_name:?}, {query:?}): ascii={ascii}, ref={reference}",
            );
        }
    }

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
