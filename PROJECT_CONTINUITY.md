# Project Continuity

## Right Now

**Full CoIR benchmark run with pipeline enrichment (2026-03-21).**

### Active
Research direction identified: **hard negative mining** is the biggest untapped lever.

CoRNStack ablation: +9.4pp from hard negs alone, independent of data quality. Our random negatives are the main gap vs SOTA. Consistency filtering was already tested (`filter_csn.py` — 0 pairs filtered, CSN is clean). The gain from filtering only applies to noisy data like The Stack, not CSN.

Pipeline CoIR run confirmed enrichment hurts on benchmarks (-4.5pp). Honest leaderboard number is v5 raw = 0.683.

### Discovery: v5 is better than v3
v3 was shipped as default before v5 results existed (v3 decision at 11:17, v5 results at 12:33 on 2026-03-20). Verified head-to-head:

| Metric | v3 | v5 | Delta |
|--------|-----|-----|-------|
| CSN avg NDCG@10 | 0.671 | **0.683** | **+1.2pp** |
| CosQA transfer | 0.334 | **0.348** | **+1.4pp** |

v5 wins on 5/6 languages (Ruby is -0.0006). v5 merged model exists at `~/training-data/e5-code-search-lora-v5/merged_model/` (safetensors, needs ONNX conversion to ship).

**Action needed:** Convert v5 to ONNX, upload to HuggingFace, switch default.

### What happened this session
1. **API key updated** — old key had no credits, new key works
2. **`--improve-all` flag** added to `cqs index` — regenerates docs for all callable functions
3. **Test function skip** — `is_test_chunk()` filters by `test_` prefix, test paths, `#[test]` in content
4. **Source file filter** — `is_source_file()` prevents doc injection into non-source files
5. **Full improve-all run** — 629 doc comments across 182 source files via Haiku Batches API
6. **Eval results improved**: R@1 90.9% → 92.7%, NDCG@10 0.951 → 0.965
7. **Research log created** — `docs/research-log.md` with all 11 experiments, CoIR data, leaderboard
8. **CoIR pipeline wrapper** — `E5Pipeline` class in `~/training-data/run_coir.py` applies free enrichment layers to CoIR corpus items before encoding

### Pending code changes (uncommitted)
- `src/llm.rs` — `is_test_chunk()`, `is_source_file()`, `--improve-all` in `doc_comment_pass()`
- `src/cli/mod.rs` — `--improve-all` CLI flag wiring
- `src/cli/commands/index.rs` — validation and pass-through
- 182 source files with new doc comments
- `docs/research-log.md` — new file
- `docs/audit-findings.md` — fresh (archived previous as `*-v1.2.0.md`)

## Parked
- Full audit (14-category) — deferred, do after committing this
- Contrastive descriptions (two-pass, future optimization)
- Language-balanced training data from popular repos
- Phase 2/3 CoIR: LLM summaries on small tasks (~$2), CSN subsample (~$12)

## Architecture
- Version: 1.2.0, Schema: v16
- Embeddings: 768-dim E5-base-v2 + signatures (SQ-11)
- LLM: summaries (SQ-6), doc comments (SQ-8), hyde (SQ-12)
- Metrics: 92.7% Recall@1, 0.965 NDCG@10 (DocFirst template, hard eval)
- Tests: 1095+ lib pass (with gpu-index)
