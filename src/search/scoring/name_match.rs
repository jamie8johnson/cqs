//! Name matching and boosting logic.

use std::collections::HashSet;

use crate::nl::tokenize_identifier;

use super::config::ScoringConfig;

/// Stack budget for per-candidate name token ranges on the ASCII hot path.
/// Identifiers rarely exceed 8 words; 16 leaves headroom before overflowing
/// to a heap `Vec`. Each entry is `(u32, u32)` = 8 bytes, so the inline buffer
/// costs 128 bytes of stack.
const NAME_TOKEN_STACK: usize = 16;

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

/// Pre-tokenized query for efficient name matching in loops.
/// Create once before iterating over search results, then call `score()` for each name.
/// Avoids re-tokenizing the query for every result.
///
/// PF-V1.25-12: on the hot scoring path `score()` avoids all per-candidate
/// `String`/`Vec<String>` allocation for ASCII inputs (the overwhelming
/// common case for code identifiers). Word overlap uses byte-range slices
/// of the candidate name, held in a 16-slot stack buffer. Unicode inputs
/// fall back to `to_lowercase()` + `tokenize_identifier` to preserve the
/// previous behavior.
pub(crate) struct NameMatcher {
    query_lower: String,
    /// True if `query_lower` is pure ASCII — allows byte-wise case-insensitive
    /// comparisons with names, skipping `to_lowercase()` allocation entirely.
    query_is_ascii: bool,
    query_words: Vec<String>,
    /// True if every entry in `query_words` is pure ASCII. Used to gate the
    /// zero-alloc word-overlap path (which relies on byte-level comparisons).
    query_words_ascii: bool,
}

impl NameMatcher {
    /// Create a new matcher with pre-tokenized query
    pub fn new(query: &str) -> Self {
        let query_lower = query.to_lowercase();
        let query_is_ascii = query_lower.is_ascii();
        // tokenize_identifier already lowercases all tokens internally
        let query_words = tokenize_identifier(query);
        let query_words_ascii = query_words.iter().all(|w| w.is_ascii());
        Self {
            query_lower,
            query_is_ascii,
            query_words,
            query_words_ascii,
        }
    }

    /// Compute name match score against pre-tokenized query.
    ///
    /// PF-V1.25-12: exact-match, substring, and word-overlap tiers are all
    /// allocation-free for ASCII inputs (hot path for source code
    /// identifiers). The ASCII word-overlap tier tokenizes `name` into a
    /// stack-resident array of byte ranges and compares against
    /// `self.query_words` bytewise with `eq_ignore_ascii_case` (query words
    /// are already lowercase, so this is effectively direct byte equality).
    /// Tier 2 of the issue (pre-tokenized names stored in DB) would further
    /// eliminate the on-the-fly tokenize pass, but requires a schema
    /// migration.
    pub fn score(&self, name: &str) -> f32 {
        let cfg = ScoringConfig::current();

        // Fast path: both strings ASCII. Byte-wise case-insensitive
        // comparisons skip `to_lowercase()` allocation for the exact and
        // substring tiers.
        let both_ascii = self.query_is_ascii && name.is_ascii();
        if both_ascii {
            if name.eq_ignore_ascii_case(&self.query_lower) {
                return cfg.name_exact;
            }
            if ascii_contains_ignore_case(name, &self.query_lower) {
                return cfg.name_contains;
            }
            if ascii_contains_ignore_case(&self.query_lower, name) {
                return cfg.name_contained_by;
            }
        } else {
            // Slow path: Unicode inputs. Preserve pre-existing behavior by
            // materializing the Unicode-lowercased form for substring checks.
            let name_lower = name.to_lowercase();
            if name_lower == self.query_lower {
                return cfg.name_exact;
            }
            if name_lower.contains(&self.query_lower) {
                return cfg.name_contains;
            }
            if self.query_lower.contains(&name_lower) {
                return cfg.name_contained_by;
            }
        }

        // Word overlap scoring
        if self.query_words.is_empty() {
            return 0.0;
        }

        // ASCII fast path: tokenize `name` into byte-range slices on the
        // stack and compare against already-lowercase `query_words` bytewise.
        // No `Vec<String>` allocation, no `HashSet` allocation. Falls back
        // to the allocating path if the name tokenizes to >16 words (rare)
        // or if either side is Unicode.
        if both_ascii && self.query_words_ascii {
            let mut ranges: [(u32, u32); NAME_TOKEN_STACK] = [(0, 0); NAME_TOKEN_STACK];
            match ascii_tokenize_ranges(name, &mut ranges) {
                Ok(n_ranges) => {
                    if n_ranges == 0 {
                        return 0.0;
                    }
                    let bytes = name.as_bytes();
                    let ranges = &ranges[..n_ranges];
                    return ascii_word_overlap_score(&self.query_words, bytes, ranges, cfg);
                }
                Err(()) => {
                    // Overflowed the stack buffer — fall through to the
                    // allocating path below.
                }
            }
        }

        // Slow path: allocate a Vec<String> of name tokens. Used for Unicode
        // inputs and the rare case of >16 tokens.
        // tokenize_identifier already lowercases all tokens internally.
        let name_words: Vec<String> = tokenize_identifier(name);

        if name_words.is_empty() {
            return 0.0;
        }

        // Build HashSet for O(1) exact match lookup
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

/// Compute the word-overlap score for an ASCII candidate using byte-range
/// slices of `name_bytes` rather than owned `String`s.
///
/// `query_words` entries are assumed to be pre-lowercased ASCII (the case
/// produced by `tokenize_identifier` on ASCII queries). Each token of the
/// candidate is a slice `name_bytes[start..end]` — bytes are compared
/// case-insensitively against the already-lowercase query words.
///
/// O(m*n) matches the behavior of the legacy `HashSet`+linear-scan path;
/// for the typical 1–5 query words × 1–5 name words the constant factors
/// of direct byte comparison beat hash-set probing.
fn ascii_word_overlap_score(
    query_words: &[String],
    name_bytes: &[u8],
    name_ranges: &[(u32, u32)],
    cfg: &ScoringConfig,
) -> f32 {
    let overlap = query_words
        .iter()
        .filter(|w| {
            let w_bytes = w.as_bytes();
            // Fast path: exact word match (case-insensitive byte compare).
            if name_ranges.iter().any(|&(s, e)| {
                let slice = &name_bytes[s as usize..e as usize];
                slice.len() == w_bytes.len() && slice.eq_ignore_ascii_case(w_bytes)
            }) {
                return true;
            }
            // Slow path: substring matching (case-insensitive). Excludes
            // equal-length substrings — those can only match exactly, which
            // is handled above.
            name_ranges.iter().any(|&(s, e)| {
                let nw = &name_bytes[s as usize..e as usize];
                (nw.len() > w_bytes.len() && ascii_bytes_contains_ignore_case(nw, w_bytes))
                    || (w_bytes.len() > nw.len() && ascii_bytes_contains_ignore_case(w_bytes, nw))
            })
        })
        .count() as f32;
    let total = query_words.len().max(1) as f32;
    (overlap / total) * cfg.name_max_overlap
}

/// Tokenize an ASCII `&str` into byte-range slices of the input, writing
/// into the caller-provided `out` buffer.
///
/// Matches the semantics of `crate::nl::tokenize_identifier` for pure-ASCII
/// input:
/// - `_`, `-`, and ` ` are delimiters (token boundary, not emitted).
/// - An ASCII uppercase letter starts a new token when the current token is
///   non-empty.
/// - All other characters extend the current token.
///
/// Returns the number of ranges written, or `Err(())` if the name produced
/// more than `out.len()` tokens (caller must fall back to allocating path).
///
/// The returned ranges point at the *raw* bytes of `name`, not lowercased
/// forms — case folding is deferred to the comparison step.
fn ascii_tokenize_ranges(name: &str, out: &mut [(u32, u32)]) -> Result<usize, ()> {
    let bytes = name.as_bytes();
    let mut count = 0usize;
    let mut start: Option<u32> = None;
    let mut i: u32 = 0;
    while (i as usize) < bytes.len() {
        let b = bytes[i as usize];
        let is_delim = b == b'_' || b == b'-' || b == b' ';
        let is_upper = b.is_ascii_uppercase();
        if is_delim {
            if let Some(s) = start.take() {
                if count >= out.len() {
                    return Err(());
                }
                out[count] = (s, i);
                count += 1;
            }
            // Skip delimiter byte; start remains None.
            i += 1;
            continue;
        }
        if is_upper {
            if let Some(s) = start.take() {
                if count >= out.len() {
                    return Err(());
                }
                out[count] = (s, i);
                count += 1;
            }
            start = Some(i);
            i += 1;
            continue;
        }
        // Ordinary character — start a token if one isn't open.
        if start.is_none() {
            start = Some(i);
        }
        i += 1;
    }
    if let Some(s) = start.take() {
        if count >= out.len() {
            return Err(());
        }
        out[count] = (s, i);
        count += 1;
    }
    Ok(count)
}

/// Byte-level analogue of `ascii_contains_ignore_case` used by the ASCII
/// word-overlap path where inputs are already `&[u8]` slices.
fn ascii_bytes_contains_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    for i in 0..=last_start {
        if haystack[i].eq_ignore_ascii_case(&first)
            && haystack[i + 1..i + needle.len()]
                .iter()
                .zip(&needle[1..])
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return true;
        }
    }
    false
}

/// Case-insensitive ASCII substring search: is `needle` a substring of `haystack`?
///
/// Both arguments must be ASCII. Bytes are compared with
/// `u8::eq_ignore_ascii_case` on the fly, so callers do not need to
/// pre-lowercase either side. Zero allocation.
///
/// Empty needle matches (mirrors `str::contains("")` semantics).
fn ascii_contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    if n.len() > h.len() {
        return false;
    }
    let first = n[0];
    let last_start = h.len() - n.len();
    for i in 0..=last_start {
        if h[i].eq_ignore_ascii_case(&first)
            && h[i + 1..i + n.len()]
                .iter()
                .zip(&n[1..])
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return true;
        }
    }
    false
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

    // ===== ascii_contains_ignore_case tests =====

    #[test]
    fn test_ascii_contains_ignore_case_basic() {
        assert!(ascii_contains_ignore_case("parseConfig", "parse"));
        assert!(ascii_contains_ignore_case("parseConfig", "Config"));
        assert!(ascii_contains_ignore_case("parseConfig", "config"));
        assert!(ascii_contains_ignore_case("parseConfig", "PARSE"));
        assert!(!ascii_contains_ignore_case("parseConfig", "missing"));
    }

    #[test]
    fn test_ascii_contains_ignore_case_edges() {
        // Empty needle matches (matches std::str::contains behavior)
        assert!(ascii_contains_ignore_case("parse", ""));
        // Needle longer than haystack
        assert!(!ascii_contains_ignore_case("p", "parse"));
        // Exact match counts as substring
        assert!(ascii_contains_ignore_case("parse", "PARSE"));
        // Substring at end
        assert!(ascii_contains_ignore_case("abcdef", "ef"));
        // Substring at start
        assert!(ascii_contains_ignore_case("abcdef", "ab"));
    }

    #[test]
    fn test_ascii_contains_ignore_case_matches_std() {
        // Cross-check against std::str::contains after to_lowercase to confirm
        // we produce identical results on ASCII inputs.
        let cases = [
            ("parseConfig", "parse"),
            ("parseConfig", "Config"),
            ("parseConfig", "xyz"),
            ("CircuitBreaker", "break"),
            ("", ""),
            ("a", "a"),
            ("ab", "ba"),
            ("parse", "parseConfig"),
        ];
        for (h, n) in cases {
            let expected = h.to_lowercase().contains(&n.to_lowercase());
            let got = ascii_contains_ignore_case(h, &n.to_lowercase());
            assert_eq!(got, expected, "mismatch for ({h:?}, {n:?})");
        }
    }

    // ===== Unicode fallback path =====

    #[test]
    fn test_name_match_unicode_fallback_exact() {
        // Non-ASCII path must preserve to_lowercase semantics.
        assert_eq!(name_match_score("获取用户", "获取用户"), 1.0);
    }

    #[test]
    fn test_name_match_unicode_fallback_contains() {
        // "getUser获取" vs "获取": name contains query after to_lowercase
        let score = name_match_score("获取", "getUser获取");
        assert_eq!(score, 0.8);
    }

    // ===== Behavior-equivalence spot checks =====

    /// Mirror the pre-refactor algorithm verbatim so we can confirm the new
    /// implementation produces identical scores across a range of inputs.
    fn legacy_score(query: &str, name: &str) -> f32 {
        let cfg = ScoringConfig::current();
        let query_lower = query.to_lowercase();
        let query_words: Vec<String> = tokenize_identifier(query);
        legacy_score_with_prebuilt(&query_lower, &query_words, name, cfg)
    }

    /// Legacy algorithm with the query pre-tokenized (matches production usage
    /// where `NameMatcher::new` hoists query tokenization out of the candidate
    /// loop). Used by the micro-bench for an apples-to-apples comparison.
    fn legacy_score_with_prebuilt(
        query_lower: &str,
        query_words: &[String],
        name: &str,
        cfg: &ScoringConfig,
    ) -> f32 {
        let name_lower = name.to_lowercase();
        if name_lower == query_lower {
            return cfg.name_exact;
        }
        if name_lower.contains(query_lower) {
            return cfg.name_contains;
        }
        if query_lower.contains(&name_lower) {
            return cfg.name_contained_by;
        }
        if query_words.is_empty() {
            return 0.0;
        }
        let name_words: Vec<String> = tokenize_identifier(name);
        if name_words.is_empty() {
            return 0.0;
        }
        let name_word_set: HashSet<&str> = name_words.iter().map(String::as_str).collect();
        let overlap = query_words
            .iter()
            .filter(|w| {
                if name_word_set.contains(w.as_str()) {
                    return true;
                }
                name_words.iter().any(|nw| {
                    (nw.len() > w.len() && nw.contains(w.as_str()))
                        || (w.len() > nw.len() && w.contains(nw.as_str()))
                })
            })
            .count() as f32;
        let total = query_words.len().max(1) as f32;
        (overlap / total) * cfg.name_max_overlap
    }

    #[test]
    fn test_refactor_matches_legacy() {
        let cases: &[(&str, &str)] = &[
            ("parse", "parse"),
            ("parse", "PARSE"),
            ("parse", "parseConfig"),
            ("parseConfig", "parse"),
            ("parseConfig", "configParser"),
            ("foo", "bar"),
            ("CircuitBreaker", "circuit_breaker"),
            ("handle_error", "handleError"),
            ("xml_parser", "XMLParser"),
            ("", "anything"),
            ("anything", ""),
            ("", ""),
            ("parseConfigFile", "parseConfigFile"),
            ("获取用户", "获取用户"),
            ("获取", "getUser获取"),
            ("get", "GETTER"),
            ("snake_case_id", "snake_case_id"),
            ("a", "ab"),
            ("ab", "a"),
            ("foo bar", "foo_bar"),
        ];
        for (q, n) in cases {
            let expected = legacy_score(q, n);
            let got = name_match_score(q, n);
            assert_eq!(
                got, expected,
                "score mismatch for query={q:?} name={n:?}: got {got} expected {expected}"
            );
        }
    }

    // ===== Micro-benchmark =====
    //
    // PF-V1.25-12: log per-call cost so we can track regressions.
    // Does not assert; just prints elapsed_per_call_ns.
    #[test]
    fn bench_score_hot_path() {
        let matcher = NameMatcher::new("parseConfig");
        // 50-char ASCII candidate name (typical upper bound for real identifiers)
        let name = "parseConfigurationAndHandleErrorForExternalClient";
        assert_eq!(name.len(), 49);

        const ITERS: u32 = 10_000;

        // Refactored path.
        let start = std::time::Instant::now();
        let mut acc: f32 = 0.0;
        for _ in 0..ITERS {
            acc += matcher.score(name);
        }
        let refactor_elapsed = start.elapsed();
        assert!(acc >= 0.0);

        // Legacy path (inline copy, for before/after comparison on the same
        // hardware and build profile). Kept alongside the refactored path so
        // we can detect regressions without cherry-picking old revisions.
        // Query tokenization is hoisted out of the loop to match production's
        // `NameMatcher::new` + per-candidate `score()` shape.
        let cfg = ScoringConfig::current();
        let legacy_query_lower = "parseConfig".to_lowercase();
        let legacy_query_words = tokenize_identifier("parseConfig");
        let start = std::time::Instant::now();
        let mut acc2: f32 = 0.0;
        for _ in 0..ITERS {
            acc2 += legacy_score_with_prebuilt(&legacy_query_lower, &legacy_query_words, name, cfg);
        }
        let legacy_elapsed = start.elapsed();
        assert!(acc2 >= 0.0);

        let refactor_ns = refactor_elapsed.as_nanos() as f64 / ITERS as f64;
        let legacy_ns = legacy_elapsed.as_nanos() as f64 / ITERS as f64;
        eprintln!(
            "bench_score_hot_path: {iters} iters over {candidate_len}-char ASCII name",
            iters = ITERS,
            candidate_len = name.len(),
        );
        eprintln!("  refactored: {refactor_elapsed:?} total, {refactor_ns:.1} ns/call");
        eprintln!("  legacy:     {legacy_elapsed:?} total, {legacy_ns:.1} ns/call");
        eprintln!("  speedup:    {:.2}x", legacy_ns / refactor_ns);
    }

    // ===== ASCII tokenizer tests =====

    fn ranges_to_strings(name: &str, ranges: &[(u32, u32)]) -> Vec<String> {
        ranges
            .iter()
            .map(|&(s, e)| name[s as usize..e as usize].to_ascii_lowercase())
            .collect()
    }

    #[test]
    fn test_ascii_tokenize_ranges_matches_tokenize_identifier() {
        // For ASCII input, the byte-range tokenizer must produce the same
        // tokens (after lowercasing) as `tokenize_identifier`.
        let cases = [
            "parseConfig",
            "parseConfigFile",
            "handle_error",
            "snake_case_id",
            "XMLParser",
            "ABCD",
            "A",
            "",
            "foo bar",
            "foo-bar",
            "a__b",
            "_leading",
            "trailing_",
            "mixCase_and-dashes",
        ];
        for name in cases {
            let mut buf = [(0u32, 0u32); NAME_TOKEN_STACK];
            let n = ascii_tokenize_ranges(name, &mut buf).expect("should fit");
            let got = ranges_to_strings(name, &buf[..n]);
            let expected = tokenize_identifier(name);
            assert_eq!(got, expected, "tokenizer mismatch for {name:?}");
        }
    }

    #[test]
    fn test_ascii_tokenize_ranges_overflow() {
        // Force more tokens than the buffer can hold. Each underscore starts
        // a new token; 20 single-letter tokens overflows the 16-slot buffer.
        let name = "a_b_c_d_e_f_g_h_i_j_k_l_m_n_o_p_q_r_s_t";
        let mut buf = [(0u32, 0u32); NAME_TOKEN_STACK];
        let result = ascii_tokenize_ranges(name, &mut buf);
        assert!(result.is_err(), "expected Err on overflow, got {result:?}");
    }

    #[test]
    fn test_score_overflow_falls_back_to_allocating_path() {
        // When the name has more than NAME_TOKEN_STACK tokens the ASCII fast
        // path must fall through to the allocating path with identical
        // scores. Build a 20-token name and score against a query that hits
        // one token exactly.
        let name = "a_b_c_d_e_f_g_h_i_j_k_l_m_n_o_p_q_r_s_t";
        let q = "p"; // will exact-match token "p"
        let expected = {
            // Run the legacy algorithm to produce the reference score.
            let cfg = ScoringConfig::current();
            let query_lower = q.to_lowercase();
            let query_words: Vec<String> = tokenize_identifier(q);
            legacy_score_with_prebuilt(&query_lower, &query_words, name, cfg)
        };
        let got = name_match_score(q, name);
        assert_eq!(
            got, expected,
            "overflow fallback produced different score ({got} vs {expected})"
        );
        assert!(got > 0.0, "expected positive score for token hit");
    }

    #[test]
    fn test_ascii_bytes_contains_ignore_case() {
        assert!(ascii_bytes_contains_ignore_case(b"parseConfig", b"parse"));
        assert!(ascii_bytes_contains_ignore_case(b"parseConfig", b"CONFIG"));
        assert!(!ascii_bytes_contains_ignore_case(b"parse", b"parseConfig"));
        assert!(ascii_bytes_contains_ignore_case(b"abc", b""));
        assert!(!ascii_bytes_contains_ignore_case(b"", b"a"));
    }
}
