//! Test-code demotion knob (`CQS_TESTCODE_DEMOTE`).
//!
//! Multiplicatively demotes test-origin chunks in the post-RRF boost layer
//! (`finalize_results` step 4b2) for queries that are NOT test-seeking. Test
//! chunks that near-duplicate the production code they exercise crowd the
//! top-5 for non-test-seeking queries (the R@5 near-dup/test-code failure
//! mode); this lever pushes them down without touching test-seeking queries.
//!
//! **Inert by default.** The knob's default is 1.0 — [`apply_testcode_demote`]
//! early-returns before touching any result and reports "nothing demoted", so
//! the caller skips its re-sort and the knob-off score path is byte-identical
//! (ordering-stable) to pre-knob output. The default flip to a real demotion
//! factor happens only after a paired A/B on both eval fixtures, not here.
//!
//! Test-origin detection reuses the canonical [`crate::is_test_chunk`]
//! path+name classifier — the same one `chunk_importance`, scout, and impact
//! use — so there is one definition of "test file", not a second. The
//! test-seeking query gate is a small fixed vocabulary ([`TEST_SEEKING_TERMS`]);
//! it is deliberately NOT wired into the runtime-extensible classifier vocab in
//! `search::router` (scope control — this is a fixed floor, not an
//! operator-tunable list).

use crate::store::helpers::SearchResult;

/// Query tokens that mark a query as *test-seeking* — the user is asking about
/// tests, so test-origin chunks must NOT be demoted for it. Whole-word matched
/// (lowercased, punctuation-trimmed) against the query, mirroring the
/// `search::router` classifier's word-exact token checks. A fixed floor:
/// intentionally small and not runtime-overridable (scope control).
const TEST_SEEKING_TERMS: &[&str] = &[
    "test",
    "tests",
    "testing",
    "tested",
    "testcase",
    "testcases",
    "unittest",
    "unittests",
    "proptest",
    "proptests",
    "e2e",
    "loom",
    "harness",
    "spec",
    "specs",
    "fixture",
    "fixtures",
    "mock",
    "mocks",
    "mocked",
    "mocking",
    "assert",
    "asserts",
    "assertion",
    "assertions",
];

/// Whether the query itself is asking about tests. When true, no test-code
/// demotion applies — the user wants the test chunks.
///
/// Conservative whole-word check: the query is lowercased and split on
/// whitespace, each token stripped of surrounding punctuation, and matched
/// word-exact against [`TEST_SEEKING_TERMS`]. Word-exact (not substring) keeps
/// `latest`, `contest`, `manifest` from reading as test-seeking; the
/// punctuation trim keeps `tests?` / `spec.` matching. Erring toward detecting
/// test-seeking is the safe direction (it withholds demotion rather than
/// over-applying it).
pub(crate) fn query_is_test_seeking(query: &str) -> bool {
    let lower = query.to_lowercase();
    lower.split_whitespace().any(|raw| {
        let tok = raw.trim_matches(|c: char| !c.is_alphanumeric());
        !tok.is_empty() && TEST_SEEKING_TERMS.contains(&tok)
    })
}

/// Resolve the test-code demotion factor from `CQS_TESTCODE_DEMOTE`.
///
/// Default + bounds + env-var name live on the `testcode_demote` row in
/// [`crate::search::scoring::knob::SCORING_KNOBS`]. The default is 1.0 (inert);
/// the resolver parses leniently — an unparseable or out-of-range value warns
/// and falls back to 1.0, and the accepted range is (0.0, 1.0] so the knob can
/// only demote, never boost. `cache: false` is load-bearing: the paired A/B
/// sweep mutates `CQS_TESTCODE_DEMOTE` between queries in one process and
/// expects every call to re-read the env (matching the `type_boost` contract).
pub(crate) fn testcode_demote_factor() -> f32 {
    super::knob::resolve_knob("testcode_demote")
}

/// Multiplicatively demote test-origin chunks in `results` when `factor < 1.0`
/// and the query is not test-seeking. Returns whether any result was demoted,
/// so the caller only re-sorts when the score actually moved.
///
/// **Structurally inert short-circuit.** At the default factor (1.0) or a
/// test-seeking query this returns `false` having touched nothing — the caller
/// then skips its re-sort, so the knob-off path is byte-identical
/// (ordering-stable) to pre-knob. Multiplicative like the type/parent boosts so
/// it stays scale-invariant across cosine `[0,1]` and re-ranker scores.
///
/// Test-origin detection is the canonical [`crate::is_test_chunk`] on the
/// chunk's authoritative `origin` path and name — never a second definition.
pub(crate) fn apply_testcode_demote(
    results: &mut [SearchResult],
    factor: f32,
    query_is_test_seeking: bool,
) -> bool {
    // Inert at the default factor or a test-seeking query: report "nothing
    // demoted" so the caller skips the re-sort. This is the byte-identical
    // no-op the default (knob-unset) path relies on. The demotion gate is
    // `factor < 1.0` expressed via `partial_cmp` so a NaN factor yields `None`
    // (neither `Less` nor otherwise) and does NOT demote — the function stays
    // total on its own inputs even though the resolver already rejects
    // non-finite values upstream. This mirrors the `< 1.0` recording gate in
    // `provenance::signals_for` exactly, so application and record never
    // disagree.
    let is_demoting = matches!(factor.partial_cmp(&1.0), Some(std::cmp::Ordering::Less));
    if !is_demoting || query_is_test_seeking {
        return false;
    }
    let mut demoted_any = false;
    for result in results.iter_mut() {
        if crate::is_test_chunk(
            result.chunk.name.as_str(),
            result.chunk.file.to_str().unwrap_or_default(),
        ) {
            result.score *= factor;
            demoted_any = true;
        }
    }
    demoted_any
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::{ChunkType, Language};
    use crate::store::helpers::ChunkSummary;
    use std::path::PathBuf;

    fn result(name: &str, file: &str, score: f32) -> SearchResult {
        SearchResult::new(
            ChunkSummary {
                id: format!("{file}:1:{name}"),
                file: PathBuf::from(file),
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                name: name.to_string(),
                signature: String::new(),
                content: String::new(),
                doc: None,
                line_start: 1,
                line_end: 1,
                parent_id: None,
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
            },
            score,
        )
    }

    // ===== query_is_test_seeking =====

    #[test]
    fn test_seeking_terms_detected() {
        for q in [
            "how are tests structured",
            "the test harness setup",
            "spec for the parser",
            "mock the store",
            "assertion helpers",
            "fixture loading",
            "testing the daemon",
            // Codebase-specific test terms — misses here are the harmful
            // direction (demoting a genuinely test-seeking query).
            "proptest for the codec round-trip",
            "the loom model for the epoch bitmask",
            "e2e coverage for the daemon",
            "unittests for the resolver",
        ] {
            assert!(query_is_test_seeking(q), "should be test-seeking: {q:?}");
        }
    }

    #[test]
    fn non_test_queries_not_seeking() {
        for q in [
            "parse the config file",
            "the latest results table",
            "run a contest bracket",
            "manifest loading",
            "rrf fusion scoring",
        ] {
            assert!(
                !query_is_test_seeking(q),
                "should NOT be test-seeking: {q:?}"
            );
        }
    }

    #[test]
    fn test_seeking_is_word_exact_not_substring() {
        // Substring traps: `latest`/`contest`/`greatest` contain "test" but are
        // not test-seeking. Word-exact matching keeps them out.
        assert!(!query_is_test_seeking("latest release notes"));
        assert!(!query_is_test_seeking("contest winner logic"));
        assert!(!query_is_test_seeking("the greatest common divisor"));
    }

    #[test]
    fn test_seeking_tolerates_punctuation() {
        assert!(query_is_test_seeking("where are the tests?"));
        assert!(query_is_test_seeking("the spec, please"));
    }

    // ===== apply_testcode_demote =====

    #[test]
    fn demote_multiplies_test_origin_on_plain_query() {
        let mut results = vec![
            result("parse_config", "src/parser.rs", 0.9),
            result("test_parse_config", "src/parser.rs", 0.8),
        ];
        let demoted = apply_testcode_demote(&mut results, 0.5, false);
        assert!(demoted, "a test-origin chunk should have been demoted");
        // The production chunk is untouched.
        assert_eq!(results[0].score, 0.9);
        // The test chunk was multiplied by the factor. Mutation guard: if the
        // `result.score *= factor` line is removed, this equality flips.
        assert!(
            (results[1].score - 0.4).abs() < 1e-6,
            "test chunk score should be 0.8 * 0.5 = 0.4, got {}",
            results[1].score
        );
    }

    #[test]
    fn demote_detects_test_by_path() {
        // Name is not test-shaped, but the file lives under tests/ → test-origin.
        let mut results = vec![result("helper", "tests/integration.rs", 0.8)];
        let demoted = apply_testcode_demote(&mut results, 0.5, false);
        assert!(demoted);
        assert!((results[0].score - 0.4).abs() < 1e-6);
    }

    #[test]
    fn factor_one_is_structural_no_op() {
        // The default factor short-circuits before any per-result work: scores
        // are byte-identical and nothing is reported demoted, so the caller
        // skips the re-sort. This is the "inert when off" contract.
        let before = vec![
            result("parse_config", "src/parser.rs", 0.9),
            result("test_parse_config", "src/parser.rs", 0.8),
        ];
        let mut after = before.clone();
        let demoted = apply_testcode_demote(&mut after, 1.0, false);
        assert!(!demoted, "factor 1.0 must report no demotion");
        for (b, a) in before.iter().zip(after.iter()) {
            assert_eq!(
                b.score.to_bits(),
                a.score.to_bits(),
                "scores must be bit-identical"
            );
        }
    }

    #[test]
    fn nan_factor_is_inert() {
        // The resolver never yields NaN, but the helper stays total on its own
        // inputs: a NaN factor early-returns "nothing demoted" (its
        // `partial_cmp(&1.0)` is `None`, not `Some(Less)`), leaving scores
        // untouched. Guards the `partial_cmp` demotion gate against a regression
        // to a bare `factor >= 1.0` (which NaN would fall through).
        let mut results = vec![result("test_parse_config", "src/parser.rs", 0.8)];
        let demoted = apply_testcode_demote(&mut results, f32::NAN, false);
        assert!(!demoted, "a NaN factor must not demote");
        assert_eq!(results[0].score, 0.8);
    }

    #[test]
    fn test_seeking_query_suppresses_demotion() {
        // Even with an active factor, a test-seeking query demotes nothing.
        let mut results = vec![result("test_parse_config", "src/parser.rs", 0.8)];
        let demoted = apply_testcode_demote(&mut results, 0.5, true);
        assert!(!demoted, "test-seeking query must suppress demotion");
        assert_eq!(results[0].score, 0.8);
    }

    #[test]
    fn no_test_chunks_reports_no_demotion() {
        // Active factor, non-test query, but no test-origin chunks → nothing
        // moves and the caller skips the re-sort.
        let mut results = vec![
            result("parse_config", "src/parser.rs", 0.9),
            result("build_index", "src/index.rs", 0.8),
        ];
        let demoted = apply_testcode_demote(&mut results, 0.5, false);
        assert!(!demoted);
        assert_eq!(results[0].score, 0.9);
        assert_eq!(results[1].score, 0.8);
    }

    #[test]
    fn substring_trap_paths_are_not_test_origin() {
        // `src/contest.rs` and `latest_results.rs` contain "test" as a
        // substring but are NOT test files — the canonical classifier uses
        // anchored patterns (`_test.`, `/tests/`, `.spec.`), never a bare
        // substring. A production chunk in either must not be demoted.
        for file in ["src/contest.rs", "src/latest_results.rs", "src/manifest.rs"] {
            let mut results = vec![result("run", file, 0.9)];
            let demoted = apply_testcode_demote(&mut results, 0.5, false);
            assert!(!demoted, "{file} must not read as test-origin");
            assert_eq!(results[0].score, 0.9);
        }
    }
}
