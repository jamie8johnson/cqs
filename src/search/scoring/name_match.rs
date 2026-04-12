//! Name matching and boosting logic.

use std::collections::HashSet;

use crate::nl::tokenize_identifier;

use super::config::ScoringConfig;

/// Detect whether a query looks like a code identifier vs natural language.
/// Name-like: "parseConfig", "handle_error", "CircuitBreaker"
/// NL-like: "function that handles errors", "how does parsing work"
/// Used to gate name_boost — boosting by name similarity is harmful for
/// NL queries because it rewards coincidental substring matches over
/// semantic relevance.
pub(crate) fn is_name_like_query(query: &str) -> bool {
    let words: Vec<&str> = query.split_whitespace().collect();
    // Single token or two-token queries are likely identifiers
    if words.len() <= 2 {
        return true;
    }
    // NL indicators: common function words that never appear in identifiers
    const NL_WORDS: &[&str] = &[
        "the",
        "a",
        "an",
        "is",
        "are",
        "was",
        "were",
        "that",
        "which",
        "how",
        "what",
        "where",
        "when",
        "does",
        "do",
        "can",
        "should",
        "would",
        "could",
        "for",
        "with",
        "from",
        "into",
        "this",
        "these",
        "those",
        "function",
        "method",
        "code",
        "implement",
        "find",
        "search",
    ];
    let lower = query.to_lowercase();
    let lower_words: Vec<&str> = lower.split_whitespace().collect();
    for w in &lower_words {
        if NL_WORDS.contains(w) {
            return false;
        }
    }
    // 3+ words with no NL indicators — still likely NL if all lowercase
    // (identifiers are usually camelCase or snake_case)
    if words.len() >= 3 && lower == query && !query.contains('_') {
        return false;
    }
    true
}

/// Pre-tokenized query for efficient name matching in loops
/// Create once before iterating over search results, then call `score()` for each name.
/// Avoids re-tokenizing the query for every result.
pub(crate) struct NameMatcher {
    query_lower: String,
    query_words: Vec<String>,
}

impl NameMatcher {
    /// Create a new matcher with pre-tokenized query
    pub fn new(query: &str) -> Self {
        Self {
            query_lower: query.to_lowercase(),
            // tokenize_identifier already lowercases all tokens internally
            query_words: tokenize_identifier(query),
        }
    }

    /// Compute name match score against pre-tokenized query
    pub fn score(&self, name: &str) -> f32 {
        let cfg = &ScoringConfig::DEFAULT;
        let name_lower = name.to_lowercase();

        // Exact match
        if name_lower == self.query_lower {
            return cfg.name_exact;
        }

        // Name contains query as substring
        if name_lower.contains(&self.query_lower) {
            return cfg.name_contains;
        }

        // Query contains name as substring
        if self.query_lower.contains(&name_lower) {
            return cfg.name_contained_by;
        }

        // Word overlap scoring
        if self.query_words.is_empty() {
            return 0.0;
        }

        // Trade-off: Building name_words Vec per result adds allocation overhead,
        // but pre-indexing names would require storing tokenized names in the DB
        // (increasing schema complexity and storage ~20%). Given name_words are
        // typically 1-5 words and this only runs for top-N results after filtering,
        // the per-result allocation is acceptable.
        // tokenize_identifier already lowercases all tokens internally
        let name_words: Vec<String> = tokenize_identifier(name);

        if name_words.is_empty() {
            return 0.0;
        }

        // Fast path: build HashSet for O(1) exact match lookup
        let name_word_set: HashSet<&str> = name_words.iter().map(String::as_str).collect();

        // O(m*n) substring matching trade-off:
        // - m = query words (typically 1-5), n = name words (typically 1-5)
        // - Worst case: ~25 comparisons per name, but short-circuits on exact match
        // - Alternative (pre-indexing substring tries) would add complexity for minimal gain
        //   since names are short and search results are already capped by limit
        let overlap = self
            .query_words
            .iter()
            .filter(|w| {
                // Fast path: exact word match
                if name_word_set.contains(w.as_str()) {
                    return true;
                }
                // Slow path: substring matching (only if no exact match)
                // Intentionally excludes equal-length substrings: if lengths are equal
                // but strings differ, they're not substrings of each other (would need
                // exact match, handled above). This avoids redundant contains() calls.
                name_words.iter().any(|nw| {
                    // Short-circuit: check length before expensive substring search
                    (nw.len() > w.len() && nw.contains(w.as_str()))
                        || (w.len() > nw.len() && w.contains(nw.as_str()))
                })
            })
            .count() as f32;
        let total = self.query_words.len().max(1) as f32;

        (overlap / total) * cfg.name_max_overlap
    }
}

/// Compute name match score for hybrid search
/// For repeated calls with the same query, use `NameMatcher::new(query).score(name)` instead.
#[cfg(test)]
pub(crate) fn name_match_score(query: &str, name: &str) -> f32 {
    NameMatcher::new(query).score(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== name_match_score tests =====

    #[test]
    fn test_name_match_exact() {
        assert_eq!(name_match_score("parse", "parse"), 1.0);
    }

    #[test]
    fn test_name_match_contains() {
        assert_eq!(name_match_score("parse", "parseConfig"), 0.8);
    }

    #[test]
    fn test_name_match_contained() {
        assert_eq!(name_match_score("parseConfigFile", "parse"), 0.6);
    }

    #[test]
    fn test_name_match_partial_overlap() {
        let score = name_match_score("parseConfig", "configParser");
        assert!(score > 0.0 && score <= 0.5);
    }

    #[test]
    fn test_name_match_no_match() {
        assert_eq!(name_match_score("foo", "bar"), 0.0);
    }

    // ===== is_name_like_query tests =====

    #[test]
    fn test_name_like_single_token() {
        assert!(is_name_like_query("parseConfig"));
        assert!(is_name_like_query("CircuitBreaker"));
        assert!(is_name_like_query("handle_error"));
    }

    #[test]
    fn test_name_like_two_tokens() {
        assert!(is_name_like_query("parse config"));
        assert!(is_name_like_query("error handler"));
    }

    #[test]
    fn test_nl_query_with_indicators() {
        assert!(!is_name_like_query("function that handles errors"));
        assert!(!is_name_like_query("how does parsing work"));
        assert!(!is_name_like_query("find error handling code"));
        assert!(!is_name_like_query("code that implements retry logic"));
    }

    #[test]
    fn test_nl_query_all_lowercase_3_plus_words() {
        assert!(!is_name_like_query("error handling retry"));
    }

    #[test]
    fn test_name_like_snake_case_multi() {
        // snake_case with 3+ words is still name-like
        assert!(is_name_like_query("handle_error_retry"));
    }
}
