# Project Continuity

## Right Now

**v3 eval dataset BUILT + centroid classifier tested (negative result). (2026-04-15 ~21:50 CDT)**

Branch: `chore/post-v1.26.0-tears-vllm-infra`. PR #1010 updated with full session work.

### Key findings

1. **v3 eval**: 544 high-confidence dual-judge consensus queries (Claude Haiku + Gemma 4 31B). Train/dev/test 326/109/109. Every category N≥23.
2. **Centroid classifier**: 76% accuracy on dev, but HURTS R@1 by −4.6pp due to asymmetric alpha error cost. Disabled. Need ~90% accuracy (logistic regression) before re-enabling.
3. **v3 dev baseline (no centroid)**: R@1=44.0%, R@5=72.5%, R@20=89.9%. This is the honest number on the new dataset.
4. **cqs batch RefCell panic fixed**: `try_borrow_mut` with deferred retry.
5. **cqs cold-start = ~5 GB**: never fan out N subprocesses. Use `cqs batch` mode.

### What's parked (for next session)

- **Logistic regression classifier**: should hit 85-90% accuracy (vs centroid's 76%). Same integration path — `reclassify_with_centroid` + alpha floor wiring already in place.
- **Alpha re-sweep on v3**: v1.26.0 alphas were tuned on v2 (265q). The v3 dataset (544q) may produce different optima.
- **PR the cqs RefCell fix separately** (task #23): currently bundled; could extract to its own PR for cleaner review.

### Lesson learned

Before building any "improvement" pipeline, answer two questions:
1. What happens when the component is WRONG? (error cost analysis)
2. Can I test the END METRIC on 20 queries before building the full pipeline? (fast falsification)

We built the entire v3 pipeline before measuring R@1 impact. A 20-query mock test would have shown the regression in 5 minutes.

## Architecture state

- **Version:** v1.26.0 (local binary has post-release fixes: RefCell + centroid infra)
- **Index:** 14,917 chunks, 100% SPLADE coverage
- **Eval:** v3 consensus: 544 queries, 326/109/109 splits
- **Centroid classifier:** disabled by default (`CQS_CENTROID_CLASSIFIER=1` to enable), centroids at `~/.local/share/cqs/classifier_centroids.v1.json`
- **Open PRs:** #1010 (tears + eval pipeline + code fixes)
- **Open issues:** 18 (0 tier-1)
