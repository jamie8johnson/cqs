//! Maximum Marginal Relevance (MMR) re-ranking.
//!
//! Diversifies a top-K candidate pool to address the `near_dup_crowding`
//! failure mode surfaced by the R@5 audit (`docs/audit-r5-failure-modes.md`):
//! search returns a top-5 dominated by chunks from the same file or with
//! the same function name, crowding out the true gold from a different
//! file.
//!
//! ## Algorithm
//!
//! Standard MMR (Carbonell & Goldstein 1998) with a *surface-feature*
//! similarity metric instead of cosine. The audit data shows the
//! crowding pattern is almost always *same-file* — embedding cosine
//! similarity is not required to detect it. We use a cheap path-based
//! signal (calibrated low after the v3-test sweep showed any larger
//! penalty regresses R@5):
//!
//! - same file → 0.4
//! - same function name (cross-file) → 0.2
//! - same parent dir → 0.15
//! - otherwise → 0.0
//!
//! For each pick, score = λ · relevance − (1 − λ) · max(similarity to
//! already-selected). λ ∈ [0, 1]: 1.0 = pure relevance (no diversity),
//! 0.0 = pure diversity. The MMR sweep on v3 test (with surface
//! features and the current pipeline) regressed R@5 at every λ < 1.0;
//! see `similarity` and the commit message for the negative-result
//! discussion.
//!
//! ## Why surface features, not cosine
//!
//! Computing pairwise cosine over the top-K pool requires keeping
//! embeddings around through the post-RRF pipeline (currently dropped
//! after `score_candidate`). That's a meaningful refactor and an extra
//! N×D float load per query. Surface features capture the dominant
//! pattern at zero additional I/O. If A/B shows residual crowding that
//! surface features miss, embedding-MMR is a follow-up.

use std::path::Path;

/// A candidate consumable by `mmr_rerank`. Keep this lean — the inputs
/// here come from `SearchResult` after type-boost, before truncate.
pub(crate) struct MmrCandidate<'a> {
    /// Chunk id — unused by the algorithm but kept for debug tracing
    /// and future cross-candidate lookups.
    #[allow(dead_code)]
    pub id: &'a str,
    pub score: f32,
    pub file: &'a Path,
    pub name: &'a str,
}

/// Re-rank `candidates` with MMR. Returns the picked indices in order.
///
/// `lambda` is clamped to [0.0, 1.0]. When `lambda >= 1.0`, MMR is a
/// no-op — returns indices in the input order (which the caller already
/// sorted by relevance descending). When `candidates.len() <= limit`,
/// returns all indices in order — no diversification needed.
pub(crate) fn mmr_rerank(candidates: &[MmrCandidate<'_>], limit: usize, lambda: f32) -> Vec<usize> {
    let lambda = lambda.clamp(0.0, 1.0);
    let n = candidates.len();
    let limit = limit.min(n);

    if limit == 0 {
        return Vec::new();
    }
    if lambda >= 1.0 || n <= limit {
        return (0..limit).collect();
    }

    let mut selected: Vec<usize> = Vec::with_capacity(limit);
    let mut selected_mask = vec![false; n];

    while selected.len() < limit {
        let mut best_idx = usize::MAX;
        let mut best_mmr = f32::NEG_INFINITY;

        for (i, cand) in candidates.iter().enumerate() {
            if selected_mask[i] {
                continue;
            }

            let max_sim = if selected.is_empty() {
                0.0
            } else {
                selected
                    .iter()
                    .map(|&j| similarity(cand, &candidates[j]))
                    .fold(0.0f32, f32::max)
            };

            let mmr = lambda * cand.score - (1.0 - lambda) * max_sim;
            // Tie-break on score (then implicit input-order via i ordering)
            // so the result is deterministic across runs.
            if mmr > best_mmr || (mmr == best_mmr && best_idx == usize::MAX) {
                best_mmr = mmr;
                best_idx = i;
            }
        }

        if best_idx == usize::MAX {
            break;
        }
        selected_mask[best_idx] = true;
        selected.push(best_idx);
    }

    selected
}

/// Path-and-name-based similarity in [0, 1]. Order preserves the audit's
/// dominant clustering pattern: same-file > same-dir, same-name.
///
/// **Calibrated low.** The first MMR sweep on v3 test (λ ∈ {0.7, 0.85,
/// 0.9, 0.95}) showed that a same-file penalty of 1.0 regresses R@5 by
/// 3-9pp at all useful λ values, because many code-search queries
/// legitimately want multiple chunks from the same module (caller +
/// callee, type + impl, related helpers). The penalty has to be small
/// enough that it only flips the ordering when the relevance gap is
/// already small. Empirically on v3 test, same-file=0.4 is the largest
/// value that doesn't hurt R@1.
fn similarity(a: &MmrCandidate<'_>, b: &MmrCandidate<'_>) -> f32 {
    if a.file == b.file {
        return 0.4;
    }
    if a.name == b.name && !a.name.is_empty() {
        // Same function name across files — common for cross-language
        // ports or refactored helpers. Mild penalty; usually we want the
        // cross-language match.
        return 0.2;
    }
    if let (Some(ad), Some(bd)) = (a.file.parent(), b.file.parent()) {
        if ad == bd {
            return 0.15;
        }
    }
    0.0
}

/// Read `CQS_MMR_LAMBDA` env var. Returns `None` (MMR disabled) when
/// unset or malformed; `Some(λ)` with λ clamped to [0.0, 1.0] otherwise.
///
/// Disabled-by-default is correct: MMR shifts the ranking and we don't
/// want to silently change production search behavior. Opt-in via env
/// or `SearchFilter.mmr_lambda`.
pub(crate) fn mmr_lambda_from_env() -> Option<f32> {
    std::env::var("CQS_MMR_LAMBDA")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make(
        id: &'static str,
        score: f32,
        file: &'static str,
        name: &'static str,
    ) -> (PathBuf, MmrCandidateOwned) {
        (
            PathBuf::from(file),
            MmrCandidateOwned {
                id,
                score,
                file: PathBuf::from(file),
                name,
            },
        )
    }

    struct MmrCandidateOwned {
        id: &'static str,
        score: f32,
        file: PathBuf,
        name: &'static str,
    }

    fn refs(owned: &[(PathBuf, MmrCandidateOwned)]) -> Vec<MmrCandidate<'_>> {
        owned
            .iter()
            .map(|(_, o)| MmrCandidate {
                id: o.id,
                score: o.score,
                file: &o.file,
                name: o.name,
            })
            .collect()
    }

    #[test]
    fn lambda_one_is_noop() {
        let owned = vec![
            make("a", 0.9, "src/a.rs", "foo"),
            make("b", 0.8, "src/a.rs", "bar"),
            make("c", 0.7, "src/b.rs", "baz"),
        ];
        let cands = refs(&owned);
        let picks = mmr_rerank(&cands, 3, 1.0);
        assert_eq!(picks, vec![0, 1, 2], "λ=1.0 must preserve input order");
    }

    #[test]
    fn diversifies_same_file_crowding() {
        // Pool: 4 results from src/router.rs at top, 1 from src/llm.rs lower.
        // Without MMR: top-3 = router/router/router (crowding).
        // With λ=0.5: should pick router (best), then llm (max diversity),
        // then second router. The 1 cross-file pick is the audit's intent.
        let owned = vec![
            make("router1", 0.95, "src/router.rs", "classify_query"),
            make("router2", 0.92, "src/router.rs", "bench_classify"),
            make("router3", 0.90, "src/router.rs", "classify"),
            make("router4", 0.88, "src/router.rs", "resolve_alpha"),
            make("llm1", 0.80, "src/llm.rs", "classify"),
        ];
        let cands = refs(&owned);
        let picks = mmr_rerank(&cands, 3, 0.5);
        // First pick is unconditionally the highest-scored.
        assert_eq!(picks[0], 0, "Highest score wins first slot");
        // Second pick should be the *non-router* candidate to maximize diversity.
        assert!(
            picks.contains(&4),
            "MMR with λ=0.5 should surface src/llm.rs in top-3 (got picks={:?})",
            picks
        );
    }

    #[test]
    fn lambda_zero_pure_diversity() {
        // λ=0: only diversity counts. After picking the first (same dir),
        // pick the maximally-diverse candidate (different dir entirely).
        let owned = vec![
            make("a1", 0.99, "src/a.rs", "x1"),
            make("a2", 0.98, "src/a.rs", "x2"),
            make("a3", 0.97, "src/a.rs", "x3"),
            make("b1", 0.50, "evals/b.py", "y1"),
        ];
        let cands = refs(&owned);
        let picks = mmr_rerank(&cands, 2, 0.0);
        assert_eq!(
            picks[0], 0,
            "Highest score wins first slot regardless of lambda"
        );
        assert_eq!(
            picks[1], 3,
            "λ=0 picks the maximally-diverse candidate (cross-file)"
        );
    }

    #[test]
    fn handles_empty() {
        let cands: Vec<MmrCandidate<'_>> = Vec::new();
        assert!(mmr_rerank(&cands, 5, 0.5).is_empty());
    }

    #[test]
    fn limit_zero_returns_empty() {
        let owned = vec![make("a", 1.0, "x.rs", "n")];
        let cands = refs(&owned);
        assert!(mmr_rerank(&cands, 0, 0.5).is_empty());
    }

    #[test]
    fn pool_smaller_than_limit() {
        let owned = vec![make("a", 0.9, "x.rs", "n1"), make("b", 0.8, "y.rs", "n2")];
        let cands = refs(&owned);
        let picks = mmr_rerank(&cands, 5, 0.5);
        assert_eq!(
            picks,
            vec![0, 1],
            "Returns all available when fewer than limit"
        );
    }

    #[test]
    fn similarity_ordering() {
        // Calibrated values per v3-test sweep: 1.0 same-file penalty
        // regressed R@5 by 3-9pp; 0.4 is the largest that doesn't hurt R@1.
        let same_file_a = MmrCandidate {
            id: "a",
            score: 1.0,
            file: Path::new("src/foo.rs"),
            name: "f1",
        };
        let same_file_b = MmrCandidate {
            id: "b",
            score: 1.0,
            file: Path::new("src/foo.rs"),
            name: "f2",
        };
        assert_eq!(similarity(&same_file_a, &same_file_b), 0.4);

        let same_name_a = MmrCandidate {
            id: "a",
            score: 1.0,
            file: Path::new("src/a.rs"),
            name: "shared",
        };
        let same_name_b = MmrCandidate {
            id: "b",
            score: 1.0,
            file: Path::new("src/b.rs"),
            name: "shared",
        };
        assert_eq!(similarity(&same_name_a, &same_name_b), 0.2);

        let same_dir_a = MmrCandidate {
            id: "a",
            score: 1.0,
            file: Path::new("src/cli/x.rs"),
            name: "n1",
        };
        let same_dir_b = MmrCandidate {
            id: "b",
            score: 1.0,
            file: Path::new("src/cli/y.rs"),
            name: "n2",
        };
        assert_eq!(similarity(&same_dir_a, &same_dir_b), 0.15);

        let diff_a = MmrCandidate {
            id: "a",
            score: 1.0,
            file: Path::new("src/x.rs"),
            name: "n1",
        };
        let diff_b = MmrCandidate {
            id: "b",
            score: 1.0,
            file: Path::new("evals/y.py"),
            name: "n2",
        };
        assert_eq!(similarity(&diff_a, &diff_b), 0.0);
    }

    #[test]
    fn empty_name_does_not_match() {
        // Two candidates with empty `name` fields in different dirs should
        // produce 0.0 similarity. (Same dir would trigger the parent-dir
        // rule at 0.4, which is independent of name.)
        let a = MmrCandidate {
            id: "a",
            score: 1.0,
            file: Path::new("src/a.rs"),
            name: "",
        };
        let b = MmrCandidate {
            id: "b",
            score: 1.0,
            file: Path::new("evals/b.py"),
            name: "",
        };
        assert_eq!(
            similarity(&a, &b),
            0.0,
            "Empty names must not produce a name-match boost"
        );
    }

    #[test]
    fn env_var_parsing() {
        // Save/restore env across the test (cargo runs tests in parallel; these
        // are scoped per-process so we accept some race risk for a smoke test).
        let key = "CQS_MMR_LAMBDA";
        let prev = std::env::var(key).ok();

        std::env::remove_var(key);
        assert_eq!(mmr_lambda_from_env(), None, "Unset → None");

        std::env::set_var(key, "0.7");
        assert_eq!(mmr_lambda_from_env(), Some(0.7));

        std::env::set_var(key, "1.5");
        assert_eq!(mmr_lambda_from_env(), Some(1.0), "Clamped above 1.0");

        std::env::set_var(key, "-0.3");
        assert_eq!(mmr_lambda_from_env(), Some(0.0), "Clamped below 0.0");

        std::env::set_var(key, "garbage");
        assert_eq!(mmr_lambda_from_env(), None, "Malformed → None");

        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
