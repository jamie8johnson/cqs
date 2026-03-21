# Project Continuity

## Right Now

**Ready for v1.2.1 release (2026-03-21).**

### Release checklist
- [x] PR #640: `--improve-all` flag, 629 doc comments, research log, Windows build fix (`libc::EXDEV`)
- [x] PR #641: `aws-lc-sys` 0.38→0.39 (2 high CVEs)
- [x] HuggingFace: v5 model (166k/1ep) uploaded + model card updated
- [ ] Version bump 1.2.0 → 1.2.1
- [ ] CHANGELOG update
- [ ] Tag + release

### Key decisions this session
- **v5 beats v3**: 166k/1ep is strictly better than shipped 50k/1ep on both CSN (+1.2pp) and CosQA (+1.4pp). v5 ONNX uploaded to HF, replacing v3.
- **Pipeline enrichment hurts CoIR**: -4.5pp. NL enrichment is a product feature, not a benchmark trick. Honest number is v5 raw = 0.683.
- **Hard negative mining is next**: CoRNStack ablation shows +9.4pp from hard negs alone. Our random negatives are the main gap vs SOTA. `filter_csn.py` already exists (ran it, CSN is clean — filtering only helps noisy data like The Stack).
- **Rank is not a lever**: rank-32 ≈ rank-16 at same data. Skip rank-64.
- **Doc comments improve embeddings**: R@1 90.9% → 92.7%, NDCG@10 0.951 → 0.965 via DocFirst template.

### Research log
Created `docs/research-log.md` — all 11 experiments with verified metrics from `~/training-data/coir-results/`. Full CoIR leaderboard from archersama.github.io/coir.

## Parked
- Full audit (14-category) — deferred
- Hard negative mining implementation (CoRNStack recipe)
- Language-specific LoRA adapters (LoRACode ICLR 2025)
- 166k/2ep experiment (midpoint between v5 and over-specialized v4)
- Full 10-task CoIR run for v5
- Contrastive descriptions (two-pass, future optimization)

## Architecture
- Version: 1.2.0 (1.2.1 pending release)
- Schema: v16
- Embeddings: 768-dim E5-base-v2 LoRA v5 (166k/1ep)
- Metrics: 92.7% Recall@1, 0.965 NDCG@10 (hard eval, DocFirst)
- CoIR CSN: 0.683 NDCG@10 (v5 raw), 0.348 CosQA
- LLM: discriminating summaries (SQ-6), doc comments (SQ-8), hyde (SQ-12)
- Tests: 1095+ lib pass (with gpu-index)
