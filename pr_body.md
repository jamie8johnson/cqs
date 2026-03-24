## Summary

- **Enriched hard eval** — injects pre-generated contrastive summaries into fixture embeddings. 92.7% R@1, 100% R@5, 0.9624 NDCG@10 (54% coverage). Existing raw eval unchanged as baseline.
- **KeyDAC augmentation script** — `~/training-data/augment_keydac.py` generates keyword-preserving query rewrites. 200k pairs → 443k total. Ready for v8 training.
- **CI Node.js 24** — `actions/checkout` v4→v5 ahead of June 2026 deadline.
- **pymupdf4llm** — verified 1.27.2 API compatible, no changes needed.

## Test plan
- [x] `cargo build --features gpu-index` — clean
- [x] `cargo test --test model_eval -- test_hard_with_summaries --ignored` — 92.7% R@1
- [x] `python3 augment_keydac.py --test` — all tests pass

🤖 Generated with [Claude Code](https://claude.com/claude-code)
