# Project Continuity

## Right Now

**SPLADE-Code 0.6B re-eval run to completion with persisted SpladeIndex. Flag-driven SPLADE is −0.6pp R@1 net — selective routing is now mandatory. (2026-04-11 14:15 CDT)**

### SPLADE-Code 0.6B re-eval result (2026-04-11, 165q v2 eval, threshold 1.6)

| Config | R@1 | R@5 | R@20 | N |
|--------|-----|-----|------|---|
| BGE-large | 42.4% | 67.9% | 85.5% | 165 |
| BGE-large + SPLADE-Code 0.6B | 41.8% | 66.1% | 86.1% | 165 |

**Flag-driven SPLADE on every query: −0.6pp R@1 net, reverses the 2026-04-09 +1.2pp headline.**

Per-category deltas (same-corpus baseline vs +SPLADE):
- **cross_language +10pp** (30 → 40%, N=10) — only category where SPLADE pays off, same direction as prior +20pp
- conceptual_search −3.7pp (22.2 → 18.5%, N=27)
- multi_step −4.6pp (36.4 → 31.8%, N=22)
- identifier_lookup, behavioral, negation, structural, type_filtered: unchanged

R@5 damage is bigger: cross_language +20pp, conceptual −7.4pp, type_filtered −6.2pp, negation −5.6pp. SPLADE displaces good dense hits at positions 2-5 on categories where lexical expansion isn't the missing signal.

**Conclusion**: Selective SPLADE routing (roadmap CPU lane) is now required, not optional. Route `CrossLanguage` → `DenseWithSplade`, leave every other category on dense. Predicted outcome: cross_language +10pp stays, conceptual/multi_step noise disappears, total 41.8% → ~43.0% (net **+1.2pp** vs always-on, **+0.6pp** vs baseline).

Research writeup: `~/training-data/research/sparse.md` § SPLADE-Code 0.6B Eval Re-run (FLAG-DRIVEN IS NET LOSS).

### Session unblock chain (three layered blockers removed)

1. **`PRAGMA integrity_check(1)` on every `Store::open`** — 85s per CLI invocation on 1.1 GB DB over WSL `/mnt/c`. Every `cqs search` paid it, eval harness was unusable. Shipped in #893: skip on read-only opens, quick_check on write opens. **86s → 6.9s per query.**
2. **`run_ablation.py` passed query as first positional** — single-token queries parsed as unknown subcommands. Shipped in #894: `cqs --json -n 20 -- <query>` form, `CQS_EVAL_TIMEOUT_SECS` env override, per-query timeout handling.
3. **SpladeIndex rebuilt from SQLite on every CLI invocation** — ~45s at 7.58M rows. Shipped in this PR: persist-alongside-HNSW pattern with generation counter and blake3 body checksum. 46.8s cold → 9.7s warm per SPLADE query.

Combined: full 2×165 ablation matrix now runs in ~55 min instead of the 4+ h the naive implementation would have taken.

### Session PRs
- #893 fix: integrity check skip on read-only opens
- #894 fix: eval harness query separator + timeout handling
- This PR: SPLADE index on-disk persistence (new format, generation counter, eager + lazy persist)
- #895 or similar (next): OpenRCT2 spec rewrite (pending)

### Old session log (2026-04-10, preserved for context)

**SPLADE-Code 0.6B encoding cleared. v8 reindex bulk-inserting. Eval is the next concrete step. (2026-04-10 22:15 CDT)**

### Total this session: 14 PRs merged to main
**Phase 5 dual embeddings**
- #876 Phase 5 dual embeddings + DenseBase routing
- #877 `CQS_DISABLE_BASE_INDEX` env var (eval A/B)
- #878 summary eligibility expanded from `is_callable()` → `is_code()`
- #880 bypass test coverage
- #885 routing fix: conceptual back to enriched (later showed as null at N=27)

**SPLADE-Code 0.6B unblock chain**
- #881 `CQS_SPLADE_MODEL` env var + vocab-mismatch probe
- #884 vocab probe accepts benign lm_head padding (151669 → 151936)
- #886 real batched `encode_batch` (replaces serial loop)
- #889 SPLADE encoding GPU memory leak — constant max_seq_len padding + arena reset
- #891 sparse_vectors bulk insert batch size derived from SQLite limit (was tuned for the pre-3.32 999-var limit, now derives from 32766)

**Sweep infrastructure**
- #882 `CQS_TYPE_BOOST` env var (research-side sweep knob)
- #883 `evals/run_sweep.py` parameter sweep harness

**Docs / process**
- #879 roadmap updates (selective SPLADE routing tracked)
- #888 end-of-(first-half-of-)session continuity
- #890 OpenRCT2 Rust port + dual-trail experiment spec

**Closed: #887** (CQS_SPLADE_BATCH env var) — superseded by #889 which has the env var inline with the encoding fix and avoids the same clippy issue.

### Phase 5 eval finding (50% coverage, BGE-large)
- Phase 5 dual-routing: **null at N=27 per category** — all category swings within ±1 query
- Total R@1: 43.0% with or without routing (within noise)
- The "−3.7pp on conceptual from routing" earlier in the day was a misread (one query out of 27 = 3.7pp)
- The historical research finding "summaries hurt conceptual −15pp" was for a different corpus shape (only callables summarized); after PR #878 expanded summaries to type definitions, the per-category effects shifted enough that the original Phase 5 routing rules no longer apply cleanly
- Phase 5 is shipped as infrastructure for further research, not as a measurable quality improvement at the current sample size

### SPLADE-Code 0.6B encoding journey (v1 → v8)
The "SPLADE-Code re-eval is blocked on encoding perf" line from earlier in this doc turned out to be three stacked bugs, all now fixed:

1. **Vocab mismatch (PR #881, #884)**: `~/.cache/huggingface/splade-onnx/` had a hot-swapped 532MB SPLADE-Code 0.6B `model.onnx` paired with the original BERT `tokenizer.json` (711KB). Encoder was feeding BERT-tokenized inputs (30522 vocab) to a Qwen3-trained model (151936 vocab) — semantically garbage but consistently garbage at both index time and search time, so search returned results without crashing. Construction-time vocab probe added.
2. **Encoding GPU memory leak (PR #886, #889)**: `encode_batch` had no real batching (serial loop), and per-batch padding to varying `max_seq_len` made ORT's BFC arena allocate new slots for every batch and never free them. Memory grew 7.4 → 30 GB over an hour with no measurable progress. Fixed by adding real batching AND padding to a CONSTANT `max_seq_len` (configurable via `CQS_SPLADE_MAX_SEQ`, default 256) so ORT can reuse the same arena slots.
3. **Sparse insert slow path (PR #891)**: `BATCH_SIZE = 333` in `upsert_sparse_vectors` was tuned for the pre-3.32 SQLite variable limit (999). The modern limit is 32766. With SPLADE-Code 0.6B's denser sparse vectors (~1000+ tokens per chunk), the per-statement sqlx overhead compounded into 30+ minute "hangs" on the bulk insert phase. Fixed by deriving `ROWS_PER_INSERT` from the actual variable limit (10822 vs the old 333). Restructured the loop to fill batches across chunk boundaries instead of starting fresh per chunk.

The encoding pipeline now runs end-to-end with SPLADE-Code 0.6B. Reindex v8 (binary with all 3 fixes) is currently in the bulk insert phase — WAL is at 3.4 GB and growing, process alive at 20% CPU, work is genuinely happening.

### What's next
1. Wait for v8 reindex to finish (bulk insert + dual HNSW rebuild)
2. Run the SPLADE eval matrix with proper SPLADE-Code 0.6B (`CQS_SPLADE_MODEL` set)
3. Compare results to the previous SPLADE-Code 0.6B finding (+1.2pp R@1, +20pp cross_language)
4. Update `research/sparse.md` and `research/enrichment.md` with the actual numbers
5. Decide whether SPLADE-Code 0.6B becomes the default for cqs (env var → bake into the path resolver)
6. Larger eval set (165q is too small to discriminate ±3pp effects on most categories)

### Lessons saved to memory this session
- **Autopilot no pauses** — when user says "autopilot", don't stop at decision points, pick the most likely option and run it
- **No time estimates** — never put time/effort estimates in specs; the model is structurally bad at it (off by 1–2 OOM)
- **No off-ramps in specs** — distinguish technical fallback chains (keep) from psychological exit hatches (cut)
- **Don't grade substrate** — when user picks a project, spec it well; don't editorialize about whether the substrate deserves to exist
- **Knob count depends on consumer** — for human-facing tools, attack wrong defaults; for agent-facing tools (cqs, Monitor), more knobs are cheap
- **Read before acting** (carried over) — always read files before editing, don't guess at contents

## Open Issues
- #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0
- Schema: v18 (embedding_base column for dual HNSW)
- Tests: 1345 lib pass (+15 SPLADE/sparse from this session)
- Adaptive retrieval Phases 1–5 implemented
- Two HNSW indexes per project: enriched (`index.hnsw.*`) + base (`index_base.hnsw.*`)
- SPLADE-Code 0.6B model files at `~/training-data/splade-code-naver/onnx/`
  - Set `CQS_SPLADE_MODEL` env var to use it (vocab probe verifies tokenizer/model match)
  - **Encoding now works end-to-end** (constant padding, batched encoder, fast bulk insert)
- Env vars added this session: `CQS_DISABLE_BASE_INDEX`, `CQS_SPLADE_MODEL`, `CQS_SPLADE_MAX_SEQ`, `CQS_SPLADE_BATCH`, `CQS_SPLADE_RESET_EVERY`, `CQS_TYPE_BOOST`
