# Project Continuity

## Right Now

**Session closing 2026-04-16. Reranker V2 experiment complete — net-negative, but two cqs bugs found and fixed along the way.**

Branch: `chore/post-v1.26.0-tears-vllm-infra`. PR #1010 open with full session work.

### Session findings in order

1. **v3 eval dataset built.** 544 high-confidence dual-judge consensus queries (Claude Haiku + Gemma 4 31B), train/dev/test 326/109/109. The first honest baseline on real-code retrieval: R@1=44.0% on dev.
2. **Centroid classifier: net-negative even at perfect accuracy.** Breakeven simulation showed −9.1pp R@1 at p=1.00. Root cause: per-category alphas were tuned on rule-classified queries; applying them to Unknown-population queries hurts regardless of classification correctness. Architectural dead end.
3. **Reranker V2 pipeline built and tested.** Fine-tuned MiniLM cross-encoder on v3 pool triples. R@1=38.5% vs baseline 44.0% (−5.5pp). Default ms-marco at 28.4% (−15.6pp). Our fine-tune +10pp over base but still net-negative.
4. **Two real cqs bugs found:**
   - `RefCell` panic in `batch/mod.rs` when staleness check fires mid-search. Fixed with `try_borrow_mut` + deferred retry.
   - `token_type_ids` zeroed in `reranker.rs` inference. BERT-family rerankers use segment IDs to distinguish query vs passage. Default ms-marco was robust to this; fine-tuned models break catastrophically. Fixed to use `Encoding::get_type_ids()`.

### Lessons

- **Classifier for Unknown → category is a dead end.** Unknown queries are a different population from rule-classified queries; per-category alphas don't transfer. The "oracle gap" in the ROADMAP was an illusion.
- **Reranking hurts when hybrid retrieval is already good.** The top-20 from dense+SPLADE is well-calibrated; a cross-encoder re-scoring without the SPLADE signal adds noise. Over-retrieval (4× for reranking) pushes gold out of top-20.
- **Always match training and inference input shapes precisely.** The token_type_ids bug and the earlier signature+content vs content-only mismatch both silently produced wrong scores that looked plausible in isolation.
- **Simulate end metric before building.** The centroid and reranker experiments both would've been killed faster by a 20-query mock test of the end metric before building the full pipeline.

### What's live

- **v3 eval pipeline** in `evals/` (14 scripts). Dataset at `evals/queries/v3_{all,train,dev,test,consensus,pools}.json`.
- **vLLM Gemma 4 31B** infra (launch command in `reference_vllm_gemma.md` memory).
- **Anthropic Claude API** client (`evals/claude_client.py`) for judge calls.
- **Centroid classifier** — disabled by default, opt-in via `CQS_CENTROID_CLASSIFIER=1`. Infrastructure preserved (centroids in `~/.local/share/cqs/classifier_centroids.v1.json`, alpha-floor wiring in both search paths).
- **Reranker v2 model** at `~/.local/share/cqs/reranker-v2/` — disabled by default, opt-in via `CQS_RERANKER_MODEL=/path/to/dir` + `--rerank` flag.
- **Reranker bug fix** (`token_type_ids`) — always active now.

### What's parked

- **Logistic regression classifier** — architecturally same as centroid, will fail the same way per the breakeven simulation. Skip.
- **Reranker V2 at scale** — needs the 200k-pair Gemma labeling pipeline + a code-pretrained base model (CodeBERT, CodeT5+). Dedicated project, not a drive-by.
- **Reranker RRF fusion** — combine reranker logit with original hybrid score rather than replacing. Cheapest potential win, queued for next session.
- **HyDE for structural queries** — per old ROADMAP data, +14pp structural. Needs fresh eval on v3 with SPLADE active.

## Architecture state

- **Version:** v1.26.0 (local binary: v1.26.0 + RefCell fix + token_type_ids fix + centroid infra + local reranker path)
- **Index:** 14,917 chunks, 100% SPLADE coverage
- **Eval baseline (v3 dev, no classifier, no reranker):** R@1=44.0%, R@5=72.5%, R@20=89.0%
- **Open PRs:** #1010 (session work)
- **Open issues:** 18 (0 tier-1)

## Operational pitfalls captured this session

1. **cqs CLI cold-start = ~5 GB** — use `cqs batch` for scripted workloads. Never fan out N subprocesses on `/mnt/c` (WSL 9P + SQLite WAL = deadlock + OOM).
2. **Python `subprocess.Popen(stderr=PIPE)`** without draining → deadlocks at 64 KB stderr buffer. Redirect to file.
3. **cqs batch RefCell panic** — fixed. Needed `try_borrow_mut` because staleness-check invalidation could fire while a search handler held a `Ref`.
4. **Reranker `token_type_ids`** — fixed. All-zeros was silently wrong for any BERT-family model that learned to use segment IDs.
5. **vLLM on WSL** — stable when coexisting with cqs-batch (~20 GB RAM total). The earlier crashes were from concurrent cqs CLI fan-out, not vLLM.
6. **ONNX external-data split** — newer torch.onnx.export auto-splits weights into `.data` file. Use `onnx.save(..., save_as_external_data=False)` to inline.
7. **v3 eval is the new baseline** — v2 265q numbers are not comparable. All future work should report on v3 dev/test splits.
