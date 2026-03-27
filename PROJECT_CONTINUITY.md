# Project Continuity

## Right Now

**v1.8.0 released. BGE-large validated. Default model switch + v1.9.0 next. (2026-03-27)**

### Active — branch `fix/init-model-dim`
- 3 commits on branch: init dim fix, reference.rs fix, convenience wrapper removal
- Uncommitted: 92.7% → 94.5% metric correction in Cargo.toml, README, lib.rs
- Needs: commit metric fixes, PR, merge, then switch default model to BGE-large

### Session Accomplishments (2026-03-27)
1. **7th audit**: 85/95 fixed, P1-P4 all handled. PR #701 merged.
2. **v1.8.0 released**: tagged, published to crates.io, GitHub Actions building binaries
3. **Multi-model verified end-to-end**: found + fixed init dim bug, convenience wrapper bug
4. **All HNSW convenience wrappers deleted**: build(), build_batched(), load(), try_load()
5. **BGE-large eval**: 94.5% R@1 pipeline (vs 83.6% E5-base). +10.9pp. Decisive.
6. **92.7% was Relaxed R@1**: metric confusion across sessions, not GPU non-determinism
7. **200K dataset assembled**: 22,222 × 9, 74% callers, 93% callees. Ready for HF upload.
8. **hnswlib-rs audited**: wilsonzlin/corenn, zero unsafe, VectorStore trait. Viable fork target.
9. **Processing manifest**: retroactive + going forward for HF provenance
10. **Filed anthropics/claude-plugins-official#1071**: wiring verification for superpowers
11. **Complete pipeline eval matrix**: all 7 models (BGE-large + 5 LoRA + base E5) through Config F, 55 queries. BGE-large confirmed best at 94.5% pipeline R@1. Per-language MRR captured.
12. **CQS_ONNX_DIR env var**: enables loading local ONNX models, bypassing HF download. Made the full model comparison possible.

### Pending
1. Commit metric corrections + PR + merge
2. Switch default model to BGE-large → v1.9.0 release
3. Publish HF datasets (200K, 500K, 1M)
4. Delete cloned repos — **done** (103GB freed)
5. Mine hard negatives on 200K
6. Train v9-200k
7. Paper v0.6

## Parked
- Dart language support (guide written)
- Curriculum scheduling (v9-full)
- Ship v9-mini as default (superseded by BGE-large decision)
- hnswlib-rs migration (audited, needs fork — roadmap item)

## Open Issues
- #389, #255, #106, #63 (blocked on upstream)
- #694 EX-30, #695 EX-32, #696 SEC-20, #697 SEC-22, #700 EX-33 (audit P4)

## Architecture
- Version: 1.8.0 (v1.9.0 pending with BGE-large default)
- Models: E5-base default (switching to BGE-large), custom ONNX
- ModelConfig: CLI > env > config > default, resolved once in dispatch
- LlmProvider: Anthropic (extensible via CQS_LLM_PROVIDER)
- Store::dim(): private field + getter + set_dim() for init
- All HNSW convenience wrappers deleted — only _with_dim variants remain
- Languages: 51 (all with skip_line_prefixes)
- nl/ directory: mod.rs, fts.rs, fields.rs, markdown.rs
- Tests: 1490
- Metrics: 94.5% R@1 / 0.966 MRR (BGE-large pipeline), 87.3% / 0.930 (v9-mini), 83.6% / 0.909 (E5-base)
