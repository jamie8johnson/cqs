# Phase 3 — Reranker V2 Cross-Encoder Training

**Created:** 2026-04-17  
**Sequencing:** runs after Phase 2 labeling completes (~16:20 CDT 2026-04-17). ColBERT path (`docs/plans/2026-04-17-colbert-xm.md`) runs after this lands or fails.  
**Owner:** human-driven; literature-informed defaults below.

## Goal

Train a code-aware cross-encoder reranker on the 200k Gemma-labeled corpus from Phase 2. Ship as a `--rerank` model swap; A/B against the v1.27.0 baseline on `v3_test.v2.json`. Decision gate: ≥3pp R@5 lift OR drop.

## Prereq sanity check (before training starts)

```bash
# Phase 2 deliverables expected at the worktree
WORKTREE=/mnt/c/Projects/cqs/.claude/worktrees/agent-a499dc70
ls -la $WORKTREE/evals/reranker_v2_train_200k.jsonl              # pairwise A/B/TIE
ls -la $WORKTREE/evals/reranker_v2_train_200k_pointwise.jsonl    # graded relevance
ls -la $WORKTREE/evals/queries/reranker_v2_corpus_quality.json   # final quality report
cat /tmp/post_labeling_final_report.txt                          # chain-monitor summary
```

Validation gates before training:
- Pointwise file has ≥190k rows (allow ~5% to drop in pairwise→pointwise conversion)
- Per-language balance: each of {rust, python, js, ts, go, java, cpp, ruby, php} ≥15k samples
- Overall corpus agreement ≥95% (Phase 1 calibration target was 85% so this is comfortable)
- Label distribution within {A: 35-50%, B: 35-50%, TIE: 5-15%}

If any gate fails: investigate before training. Don't auto-proceed.

## Loss + data format decision

**Use BiXSE pointwise (BCE on graded relevance), not pairwise loss.** Literature signal:
- BiXSE (2508.06781, Aug 2025): pointwise BCE with LLM-graded relevance outperforms pairwise/contrastive on dense retrieval
- Instruction Distillation (2311.01555): distilling pairwise → pointwise gives 10-100x inference efficiency without quality loss

Phase 2 already produced the pointwise file via the chain monitor's `pairwise_to_pointwise.py` step. Use it directly.

Pointwise format expected:
```jsonl
{"query": "...", "passage": "...", "label": 0.0|0.5|1.0}
```
(0.0 = labeled losing chunk, 0.5 = TIE chunk, 1.0 = labeled winning chunk; verify by reading first 5 lines)

## Base model decision tree

Strategy doc requires "code-pretrained base, NOT MS-MARCO." Three candidates ranked:

1. **`microsoft/unixcoder-base`** (125M) — code-pretrained, well-supported, fast on A6000. Cheapest first pass; default unless evidence pushes elsewhere.
2. **`Salesforce/codet5p-110m-embedding`** (110M) — code-pretrained-from-scratch encoder, contrastive-trained. Better dense-retrieval prior than UniXcoder.
3. **`DeepSoftwareAnalytics/CoCoSoDa`** (CoCoSoDa fine-tuned over CodeBERT, 125M) — strongest CSN baseline (+10.5% vs GraphCodeBERT) but bi-encoder pretrain; cross-encoder fine-tune may not transfer all the gains.

Approach: ship UniXcoder run first. If it underperforms baseline OR is +/-1pp, run CodeT5+ as the second config. Don't sweep base models in parallel — sequenced runs use the A6000 cleaner.

## Training hyperparameters (UniXcoder default)

```python
# evals/train_reranker_v2.py — to be written
model = "microsoft/unixcoder-base"
loss = "BCE"  # graded labels in [0.0, 1.0]
max_seq_length = 512
batch_size = 32           # A6000 has headroom; raise if VRAM allows
learning_rate = 2e-5      # standard cross-encoder fine-tune LR
warmup_ratio = 0.1
epochs = 3
weight_decay = 0.01
gradient_accumulation = 1
fp16 = True               # ~2x speedup, A6000 supports
seed = 42                 # reproducible
```

**One-shot, no sweep.** If first run misses the 3pp gate, then sweep LR ∈ {1e-5, 5e-5} and epochs ∈ {2, 4}.

## Wall-time estimate (A6000)

- 200k pointwise rows × 3 epochs / batch 32 = ~18.75k steps
- Step time UniXcoder cross-encoder fp16 seq 512: ~50-80ms
- Total: ~25 minutes per epoch × 3 = **~1h20m** wall time
- Plus eval pass: ~10min
- Total: **~1.5h end-to-end** for the default config

CodeT5+ second pass adds another ~1.5h if needed. Single LR/epoch sweep adds ~6h.

## GPU contention with vLLM

vLLM Gemma 4 31B AWQ uses ~48GB on A6000 during Phase 2 labeling. **Cannot train while vLLM is up.** Bring vLLM down BEFORE training:

```bash
# Find vLLM PID and stop it cleanly
pkill -f 'vllm serve cyankiwi/gemma'
# Wait for shutdown; verify
nvidia-smi --query-gpu=memory.used --format=csv,noheader
```

After Phase 3 training: restart vLLM if any further labeling work is queued (currently none).

## Eval protocol

Use the new `cqs eval --baseline X --tolerance N` from PR #1030 against the regenerated `v3_test.v2.json` fixture (Tier 1.1).

```bash
# Save current baseline
cqs eval --json > evals/baseline-pre-reranker-v2.json

# After training, swap reranker model
export CQS_RERANKER_MODEL=/path/to/reranker-v2-unixcoder
cqs eval --baseline evals/baseline-pre-reranker-v2.json --tolerance 1.0
# Exit 0 = no per-category regression past 1.0pp; exit 1 = regression
```

Decision gate (apply in order):
1. **R@5 +3pp or more** → ship. Update `CQS_RERANKER_MODEL` default in `cqs model show`. Bump version.
2. **R@5 within ±1pp** → run CodeT5+ second config. If still flat, mark Phase 3 done with no swap.
3. **R@5 −1pp or worse** → triage by category. If improvement on `unexplained` queries (the audit's biggest reranker target) but regression elsewhere, consider category-gated reranker. Otherwise mark training data quality issue and revisit.

## Wiring change scope

Existing reranker integration:
- `src/reranker/` (cross-encoder loader, ms-marco-MiniLM by default)
- `--rerank` CLI flag and `args.rerank` in batch handler
- `CQS_RERANKER_MODEL` env override (already supports local paths per PR ec0e62 from main session)

**No new wiring needed.** Just point `CQS_RERANKER_MODEL` at the trained directory. The existing `populate token_type_ids` fix from main session means UniXcoder/CodeT5+ tokenizers work without further code change.

## Files this plan creates

- `evals/train_reranker_v2.py` — training script (sentence-transformers `CrossEncoder` + BCEWithLogitsLoss on graded labels)
- `evals/reranker_v2_train_run.json` — per-run config + metrics (one per training attempt; numbered)
- `models/reranker-v2-unixcoder/` — trained model directory (ONNX export for production)
- `evals/baseline-pre-reranker-v2.json` — frozen baseline for the regression gate
- `docs/reranker-v2-results.md` — final A/B writeup post-eval

## Stopping conditions

- Training perplexity NaN at step >100 → halve LR, restart
- Eval R@5 regresses ≥3pp on default config → don't auto-proceed to CodeT5+; investigate corpus quality first (per-language label distribution, sample 20 random pairs and human-spot-check)
- Phase 2 chain monitor produced an empty pointwise file → bug in `pairwise_to_pointwise.py`; fix that before training

## What's NOT in scope here

- ColBERT integration (separate plan: `docs/plans/2026-04-17-colbert-xm.md`)
- Multi-reranker ensemble (deferred until both single-model paths are scored)
- Per-category reranker gating (decision gate #3 above considers it conditionally)
- Pairwise loss training (literature consensus is pointwise wins; not running pairwise unless BiXSE result fails to replicate)
