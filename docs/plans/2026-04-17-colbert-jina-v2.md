# ColBERT Integration — Jina-ColBERT-v2 as 2-stage Reranker

**Created:** 2026-04-17  
**Sequencing:** runs after Phase 3 reranker training lands or fails (`docs/plans/2026-04-17-phase3-reranker-training.md`).  
**Why now:** literature survey (2026-04-17) found WARP engine (41x faster than ColBERT reference) and Jina-ColBERT-v2 (multilingual, 50% smaller). Original "1-3 month architectural lift" estimate in the strategy doc is wrong — off-the-shelf 2-stage path is ~3-5 days.

## Goal

Add ColBERT-style late-interaction reranking as a `Reranker` trait implementation alongside the cross-encoder. Run it as the **last** stage of retrieval (after dense+SPLADE+RRF + cross-encoder), starting with no per-token index — pure inference cost only. A/B against the Phase 3 cross-encoder result.

Decision gate: ColBERT 2-stage rerank wins by ≥2pp R@5 over the cross-encoder. If yes, then plan the per-token index build (separate, larger work). If no, parked.

## Why 2-stage, not full late-interaction index

The full ColBERT setup requires per-token indexes (~50× the storage of dense embeddings). Off-the-shelf inference only needs:
- Query encoding (one forward pass, ~30ms on RTX 4000)
- Per-candidate document encoding (top-50 candidates × ~30ms each = ~1.5s overhead)

The 1.5s is acceptable for a final-stage reranker on a small candidate pool. If results are good, then the per-token index becomes a latency optimization, not a correctness requirement.

This staging matches the "Reranker V2 first, ColBERT as confirmation" sequencing.

## Off-the-shelf path

- **Model:** `jinaai/jina-colbert-v2` from HuggingFace
  - Multilingual (89 languages), but the 9 we care about (rust, python, js, ts, go, java, cpp, ruby, php) are all in-distribution for Jina's training mix
  - 137M params, fp16 fits in <12GB → RTX 4000 (8GB) at fp16/int8, or A6000 if convenient
  - Released under Apache-2.0
- **Engine:** PyLate or sentence-transformers integration. Skip WARP for the first pass — it's a latency optimization that only matters once we have a per-token index.
- **Inference shape:** `MaxSim(query_tokens, doc_tokens)` for each (query, candidate) pair. Sum over query token max-similarities to a doc token. The "late interaction" is that token-level matching happens at scoring time, not at index time.

## Hardware decision

- Phase 3 cross-encoder runs on A6000 (needs ~6GB)
- Jina-ColBERT-v2 inference fits on RTX 4000 (8GB) at fp16
- Could run BOTH simultaneously if Phase 3 sequential A/B is going on

Practical: run Phase 3 first (A6000 free of vLLM after labeling), land it, THEN bring up ColBERT on RTX 4000 for the confirmation A/B.

## Wiring change scope

Bigger than Phase 3, because we need a `Reranker` trait abstraction:

```rust
// src/reranker/mod.rs (new trait)
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, candidates: &[Candidate], top_k: usize)
        -> Result<Vec<RerankedResult>, RerankerError>;
    fn name(&self) -> &str;
    fn model_id(&self) -> &str;
}

// src/reranker/cross_encoder.rs — existing implementation refactored to trait
pub struct CrossEncoderReranker { ... }
impl Reranker for CrossEncoderReranker { ... }

// src/reranker/colbert.rs — new
pub struct ColbertReranker { /* ONNX session for jina-colbert-v2 */ }
impl Reranker for ColbertReranker { ... }
```

CLI flag changes:
- `--rerank` (existing) → defaults to cross-encoder
- `--rerank colbert` → uses ColBERT
- `--rerank both` → cross-encoder THEN ColBERT (ensemble — cross-encoder pool of top-50 → ColBERT top-K)
- `CQS_RERANKER_KIND` env var: `cross_encoder` | `colbert` | `cross_then_colbert`

## ONNX export

Jina-ColBERT-v2 doesn't ship an ONNX file by default. Two options:

1. **Use sentence-transformers Python inference via subprocess.** Slower per-query (Python startup), simpler to wire. Acceptable for proof-of-value.
2. **Export to ONNX with `optimum`** (`optimum-cli export onnx --model jinaai/jina-colbert-v2 --task feature-extraction colbert.onnx`). Fast-path for production. Requires manual MaxSim implementation in Rust (not trivial — token-level outputs need to be summed over query maxes).

Decision: start with option 1 for the A/B (cheaper to abandon if R@5 doesn't move). Move to option 2 only if we ship.

## Eval protocol

Same as Phase 3:

```bash
# A/B against Phase 3 baseline
export CQS_RERANKER_KIND=cross_then_colbert
cqs eval --baseline evals/baseline-post-reranker-v2.json --tolerance 1.0
```

Decision gate:
1. **R@5 +2pp over cross-encoder alone** → ship 2-stage. Plan per-token index work as a latency follow-up.
2. **R@5 within ±1pp** → don't ship. Document negative result.
3. **R@5 worse** → ColBERT is fighting cross-encoder; the multilingual training mix may not match our code distribution well enough. Revisit only after exhausting cross-encoder retraining.

## Wall-time estimate

- ONNX/sentence-transformers inference setup: ~0.5 day
- `Reranker` trait refactor: ~0.5 day
- ColBERT impl wiring: ~0.5 day
- A/B eval + report: ~0.5 day
- Total: **~2 days** if option 1 (Python subprocess); **~3-5 days** if ONNX export + Rust MaxSim

## Files this plan creates

- `src/reranker/mod.rs` — Reranker trait (refactor existing)
- `src/reranker/cross_encoder.rs` — existing impl refactored
- `src/reranker/colbert.rs` — new
- `evals/colbert_inference_smoke.py` — sentence-transformers inference smoke test
- `models/colbert-jina-v2/` — downloaded model artifacts (gitignored — track via cqs model swap)
- `docs/colbert-rerank-results.md` — A/B writeup

## Stopping conditions

- ColBERT inference >2s/query at top-50 candidates → drop top-K to 20 and re-A/B; if still slow, drop the plan
- Per-language R@5 regresses on rust/cpp (lower-resource in Jina's mix) → restrict ColBERT to high-resource languages (python/java/js)
- Cross-encoder + ColBERT ensemble is no better than cross-encoder alone → ship cross-encoder, mark ColBERT as parked-with-evidence

## What's NOT in scope here

- Per-token index (only if 2-stage proves out)
- WARP engine integration (latency optimization, post-shipping)
- Training a custom ColBERT on our 200k corpus (much bigger work; only if off-the-shelf wins enough to justify)
- Replacing dense+SPLADE+RRF entirely with ColBERT (that's the per-token index path)
