# ColBERT Integration — mxbai-edge-colbert-v0-32m as 2-stage Reranker

> **Default updated 2026-04-17 (research recheck):** the original draft
> targeted `jinaai/jina-colbert-v2` (CC-BY-NC-4.0 — non-commercial only)
> and was then switched to `antoinelouis/colbert-xm` (MIT, ~270M).
> A literature recheck for late-2025/2026 papers found
> **`mixedbread-ai/mxbai-edge-colbert-v0-32m`** (Apache-2.0, **32M** params,
> **outperforms ColBERTv2 on BEIR**, ONNX-quantized variants already
> published). 8x smaller than ColBERT-XM at higher quality, equally
> permissive license. ColBERT-XM stays as a multilingual fallback for
> low-resource languages where mxbai-edge regresses; Jina-ColBERT-v2
> stays as a research-only ceiling reference.

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

- **Model (default):** `mixedbread-ai/mxbai-edge-colbert-v0-32m`
  - **Apache-2.0** — safe to ship; users can deploy commercially.
  - 32M params; ONNX int8 variants already published (e.g.
    `ryandono/mxbai-edge-colbert-v0-17m-onnx-int8`)
  - Outperforms ColBERTv2 on BEIR per the Mixedbread blog post
    ("Fantastic (small) Retrievers and How to Train Them: mxbai-edge-
    colbert-v0", Oct 2025)
  - English-trained — code identifiers and English natural-language
    docstrings/comments are in-distribution. Per-language A/B will tell
    us if rust/cpp regress.
  - fp16 fits in <2GB; runs comfortably on RTX 4000 alongside other
    workloads. Could even share A6000 with a future Phase 3 ensemble.
- **Multilingual fallback:** `antoinelouis/colbert-xm`
  - MIT, ~270M, 81-language XMOD-based zero-shot transfer
  - Use only if mxbai-edge regresses on rust/cpp/non-English-comment
    test queries
- **Backup option:** `mixedbread-ai/mxbai-edge-colbert-v0-17m`
  - Apache-2.0, 17M (smallest) — for latency-bound deployments if
    32m proves slow at top-100 candidate counts
- **Research-only ceiling:** `jinaai/jina-colbert-v2` (CC-BY-NC-4.0)
  - 137M, 89 langs, generally stronger multilingual numbers per the
    paper. Internal eval comparison only — never shipped default.
  - If Jina opens a meaningful gap over mxbai-edge on our v3 test,
    that's motivation to either license-clear or train our own
    permissive multilingual ColBERT.
- **Engine:** PyLate (preferred — native ColBERT inference) or
  sentence-transformers. WARP (arXiv 2501.17788, 41x faster than
  ColBERT reference engine) is a latency optimization for the per-token
  index path; skip for the off-the-shelf 2-stage proof-of-value.
- **Inference shape:** `MaxSim(query_tokens, doc_tokens)` for each
  (query, candidate) pair. Sum over query token max-similarities to
  doc tokens. The "late interaction" is that token-level matching
  happens at scoring time, not at index time. mxbai-edge's smaller
  hidden dim (compared to BERT-base) means MaxSim is even cheaper.

## Hardware decision

- Phase 3 cross-encoder runs on A6000 (needs ~6GB)
- mxbai-edge-colbert-v0-32m fp16 fits in <2GB; RTX 4000 (8GB) trivially
- Could run BOTH simultaneously if Phase 3 sequential A/B is going on

Practical: run Phase 3 first (A6000 free of vLLM after labeling), land it, THEN bring up mxbai-edge on RTX 4000 for the confirmation A/B.

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
pub struct ColbertReranker { /* ONNX session for mxbai-edge-colbert-v0-32m */ }
impl Reranker for ColbertReranker { ... }
```

CLI flag changes:
- `--rerank` (existing) → defaults to cross-encoder
- `--rerank colbert` → uses ColBERT
- `--rerank both` → cross-encoder THEN ColBERT (ensemble — cross-encoder pool of top-50 → ColBERT top-K)
- `CQS_RERANKER_KIND` env var: `cross_encoder` | `colbert` | `cross_then_colbert`

## ONNX export

ColBERT-XM doesn't ship an ONNX file by default. Two options:

1. **Use sentence-transformers / PyLate Python inference via subprocess.** Slower per-query (Python startup), simpler to wire. Acceptable for proof-of-value.
2. **Pre-built ONNX:** `ryandono/mxbai-edge-colbert-v0-17m-onnx-int8` exists on HuggingFace. Check whether a 32m ONNX is published or run `optimum-cli export onnx --model mixedbread-ai/mxbai-edge-colbert-v0-32m --task feature-extraction colbert.onnx`. mxbai-edge is a clean BERT-style architecture without XMOD adapter routing complexity, so ONNX export should be straightforward. Manual MaxSim still needs implementing in Rust — token-level outputs need to be summed over query maxes.

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
- `models/mxbai-edge-colbert-v0-32m/` — downloaded model artifacts (gitignored — track via cqs model swap)
- `docs/colbert-rerank-results.md` — A/B writeup

## Stopping conditions

- ColBERT inference >2s/query at top-50 candidates → drop top-K to 20 and re-A/B; if still slow, drop the plan
- Per-language R@5 regresses on rust/cpp (lower-resource in XMOD's adapter coverage) → restrict ColBERT to high-resource languages (python/java/js)
- Cross-encoder + ColBERT ensemble is no better than cross-encoder alone → ship cross-encoder, mark ColBERT as parked-with-evidence

## What's NOT in scope here

- Per-token index (only if 2-stage proves out)
- WARP engine integration (latency optimization, post-shipping)
- Training a custom ColBERT on our 200k corpus (much bigger work; only if off-the-shelf wins enough to justify)
- Replacing dense+SPLADE+RRF entirely with ColBERT (that's the per-token index path)
