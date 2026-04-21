# Project Continuity

## Right Now

**Both α-routing arc and HyDE empirically dead. Building 10x N v4 eval fixture for definitive baseline before next lever.** Late-night 2026-04-20 → 2026-04-21.

### Active task

**v4 fixture generation** (pid 78195) — Gemma-validated synthetic queries, 400 per category × 8 cats targeted, held out from v3 gold + Phase 1.3 seeds. Output: `evals/queries/v4_generated.json`. Will split 50/50 into `v4_test.v2.json` + `v4_dev.v2.json` via `evals/split_v4.py`. ~50 min ETA.

Then re-run full lever sweep on v4 for final ship/kill on each previously-tested cell at proper N.

### Just-parked levers (this session)

| Lever | v3 result | Verdict |
|---|---|---|
| Distilled head (Phase 1.4b retrain, 88.1% val acc) | test ±0/dev +0.9 R@5 | parked — accuracy not the bottleneck |
| **Fused head** (contrastive ranking, continuous α + corpus fingerprint) | test ±0/dev -0.9 R@5 | parked — α is α-insensitive on this corpus state |
| **HyDE** (query-time, Gemma-generated synthetic code) | test -12.8/dev -22.0 R@5 | killed — every category negative on dev |

The α-routing diagnosis: oracle's +9.2pp came from category-driven per-category default flips, not continuous α refinement. R@5 is α-insensitive in [0.0, 1.0] range. Continuous α can't break the convex hull AND the convex hull doesn't matter for this metric on this corpus.

### Phase 1.4b conclusion (this session)

| Metric | Phase 1.4 (79.8% acc) | Phase 1.4b (88.1% acc, retrained) |
|---|---|---|
| test R@5 vs baseline | ±0pp | **±0pp** |
| dev R@5 vs baseline | −0.9pp | +0.9pp |
| Per-category test R@5 | mixed | identical OFF/ON across all 8 categories |

Asymmetric-error-cost theory was wrong (or at most a second-order effect). The true constraint is the **convex hull of 8 fixed α defaults** {1.0, 0.9, 0.8, 0.7, 0.1}. Head's category prediction → router-default α path can't break out of that hull regardless of accuracy. Oracle test (+9.2pp test R@5) measured a different thing — Oracle forced the *α value* per query, not just the *category*. Continuous α is the unlock; classifier accuracy was a red herring.

Decision: deprecate the distilled head (`CQS_DISTILLED_CLASSIFIER*` envs, `src/classifier_head.rs`, the v1 ONNX artifact) the moment the fused head ships green. Cleanup steps documented in the spec's "Decision: deprecate" section.

### Open PRs

- **PR #1067** — MERGED (docs/roadmap follow-up to v1.28.3).
- **PR #1068** chore/scope-pre-edit-hook — clippy + fmt pass; test still pending. Merge when green.

### Branch state

- **feat/distilled-query-classifier** (local only) carries the now-superseded distilled head code. Will be reused for the fused head implementation OR rebranched to `feat/fused-alpha-head` — TBD.

### Strategic spec: fused alpha + classifier head

Path: `docs/plans/2026-04-20-fused-alpha-classifier-head.md` (~12KB).

Key decisions baked in:
- **Contrastive ranking loss** instead of regression-on-best-α — no α-label sweep needed, optimizes against the actual ranking objective
- **Corpus fingerprint as trunk input** (1024-dim normalized mean of all chunk embeddings) — locks in the input contract from v1, even though MVP trains single-corpus on cqs only
- **Linear-blend score in training** (`α·sparse + (1−α)·dense`) accepts a controlled mismatch with production RRF; differentiable RRF surrogate is future work
- **Deprecate distilled head on green ship** — no parallel-head maintenance

Three open questions, all addressable in follow-on work:
1. Multi-corpus training data collection (4-5 corpora from a 6-row candidate table; need ~500 generated queries each)
2. Cache-invalidation race — durable solution layered as push (socket message from `cqs index`) + pull (mtime stat every Nth query) + inotify (Linux-native bonus)
3. Single-corpus training risk — fingerprint dropout p=0.2 + optional Gaussian jitter σ=0.01 as stopgaps

Eight ablation knobs (τ, distractor sampling, score normalization, trunk hidden dim, λ_α, K, fingerprint dropout, fingerprint jitter) — sweep + report alongside headline result.

### Implementation order (next session)

1. `src/corpus_fingerprint.rs` (lazy compute + cache, invalidation hook in `cqs index`)
2. `evals/build_contrastive_shards.py` (pre-compute per-query sparse + dense scores for top-50 candidates × 4376 queries)
3. `evals/train_fused_head.py` (PyTorch trunk + 2 heads, CE + contrastive ranking loss, ablation sweeps)
4. `src/fused_head.rs` (ORT inference, parallel to existing `src/classifier_head.rs`)
5. `src/search/router.rs::reclassify_with_fused_head` + 3 call-site wirings
6. `evals/fused_head_ab_eval.py` (3-cell A/B: baseline / distilled / fused)
7. If green per decision matrix → v1.28.4 ship + distilled head cleanup

### Pending uncommitted (feat branch — superseded code)

- `src/classifier_head.rs` (220 LOC) — to be deleted post-fused-ship
- `src/{lib.rs, search/router.rs, cli/...}` distilled head wiring — to be replaced with fused equivalents
- `evals/{train_query_classifier, measure_gemma_classify_accuracy, rerank_ab_oracle_eval, distilled_head_ab_daemon, distilled_head_ab_eval, hyde_per_category_eval}.py`
- `evals/classifier_head/{model.onnx, state_dict.pt, run_meta.json}` — superseded artifacts
- `evals/queries/v3_generated_round1.json` (1.8MB, 3833 synthetic queries, 42% pass rate) — REUSE for fused head training corpus

### Background processes

- **cqs-watch daemon** clean (no env overrides; cleanup ran at end of A/B). Default config.
- vLLM Gemma 4 31B AWQ still up on A6000:0 (47GB) — idle but loaded, ready for next generation pass.

### Collaboration calibration (still load-bearing)

1. **"Self-starter and self-orienter" is the favored mode.** Default toward action over consultation when the next move is clear.
2. **"Little give-ups" are the failure pattern.** Verify artifacts; investigate silences; redo thin returns; don't tolerate Monitor timeouts as longer waits.
3. **No time estimates in specs.** Wall-time predictions are unreliable; describe what/why/gate-criteria, not effort.
4. **Knobs that are knobs, not blockers, go in an Ablations table** — not in Open Questions. (Updated this session per user feedback on the fused head spec.)

### Reranker V2 — PARKED

Three loss regimens (BCE / weighted BCE / pairwise margin) on 9k cqs-domain graded rows. All converged on −5 to −9pp R@5; pairwise hit 98% train accuracy without generalizing. Corpus too thin for 125M cross-encoder. Weights at `~/training-data/reranker-v2-cqs-{graded,pairwise}/`. Re-attempt only with 10x corpus + bge-reranker-large.

### Notes A/B — ZERO IMPACT on retrieval

`scoring.note_boost_factor = 0.0` vs `0.15` on v3.v2: identical R@K. Default left at 0.15.

### Daemon/reindex lock conflict — FIXED in v1.28.2 (#1061)

`cqs index --force` now fails fast vs running daemon with the exact stop/restart command. Was hanging 60+ min in `locks_lock_inode_wait`.

### Lever-by-lever results

| Lever | Result | Status |
|---|---|---|
| Tier 1.1 — eval-data hygiene | strict==permissive after `regenerate_v3_test.py` | done; canonical baseline R@5=63.3% on `v3_test.v2.json` |
| Tier 1.2 — MMR re-rank (surface-feature) | regressed at every λ < 1.0 | shipped inert opt-in via `CQS_MMR_LAMBDA`; embedding-MMR is the obvious follow-up |
| Tier 1.3 — chunk-type aware boost | within ±1pp noise of default 1.2 | default stays |
| Tier 2 — Reranker V2 (Phase 3 cross-encoder) | −24pp R@5 (domain shift + binary-label loss) | weights stay local at `~/training-data/reranker-v2-unixcoder/`; not shipped |
| Tier 2 — ColBERT 2-stage (mxbai-edge-colbert-v0-32m) | marginal/inconsistent: test α=0.9 +2.8pp R@5, dev α=0.9 +0.9pp | eval tool shipped; default OFF; PR #1037 |
| **Tier 3 — chunker doc fallback for short chunks** | **+2.8pp R@5 test, +0.9pp R@5 dev, +4.6pp R@20 test, +2.8pp R@20 dev** (fresh-fixture comparison post-v1.28.1) | shipped in #1040 + #1041 P1 #3-#4 hardening + v1.28.1 LanguageDef wiring (P2 #53/#55 recovery) |

### What landed this session arc (post-v1.27.0)

| PR | Highlight |
|---|---|
| #1023 | release v1.27.0 (audit-wave + MSRV bump 1.93→1.95) |
| #1024 | post-v1.27.0 ROADMAP refresh + embedder swap workflow plan |
| #1025 | publish 413 fix (excluded `evals/`, `samples/`, `tools/`, `cuvs-fork-push/` from package) |
| #1026 | embedder hygiene (index-aware resolution + dim-mismatch error) + proactive GC (startup + retroactive gitignore + idle-time periodic) |
| #1027 | `cqs stats` field expansion + `cqs doctor --verbose` + `cqs ping` + `cqs eval` subcommand |
| #1028 | `--limit` standardization + `--json` propagation through batch |
| #1029 | `.gitattributes` + LF renormalize (closed CRLF tax) |
| #1030 | `cqs model swap` + `cqs eval --baseline` regression gate |
| #1031 | Reranker V2 Phase 1 calibration → GEMMA_ONLY (98.3% inter-rater, kappa 0.97) |
| #1032 | docs(plans): Phase 3 cross-encoder + sequenced ColBERT-XM |
| #1033 | docs(plans): research recheck 2026-04-17 + Phase 3 training script |
| #1034 | chore(agents): tune `.claude/agents/` prompts for Opus 4.7 |
| #1035 | fix(train): accept `content` field in pointwise rows |
| #1036 | fix(reranker): detect ONNX input shape, skip token_type_ids for RoBERTa-family |
| #1037 | feat(evals): ColBERT 2-stage + RRF fusion eval tool |
| #1038 | feat(cli): uniform JSON output envelope across all commands (Task #17, BREAKING) |
| #1039 | chore(deps): bump rustls-webpki 0.103.10 → 0.103.12 (Dependabot #7, #8) |
| #1040 | fix(parser): doc enrichment for short chunks (truncated_gold lever) |
| #1041 | chore(audit): land 26 P1 fixes from post-v1.27.0 audit |
| #1045 | chore(audit): land 47 P2 fixes from post-v1.27.0 audit (wave 1) |
| #1046 | chore(audit): land 69 P3 fixes from post-v1.27.0 audit (audit complete) |
| #1050 | chore: Release v1.28.0 |
| #1051 | docs(tears): refresh for v1.28.0 release |
| #1052 | docs(roadmap): refresh header for v1.28.0 |
| #1053 | chore: Release v1.28.1 — recover 8 P2 audit fixes lost in v1.28.0 wave |

Reranker V2 work also produced commits in the private `cqs-training` repo (research/reranker.md updated with Phase 1/2/3 + ColBERT results + post-mortem).

### v3 baselines (current, after #1040 reindex 2026-04-18)

`evals/queries/v3_test.v2.json` (109 queries) and `v3_dev.v2.json` (109 queries):

| Config | test R@1 | test R@5 | test R@20 | dev R@1 | dev R@5 | dev R@20 |
|---|---|---|---|---|---|---|
| **current (2026-04-20, post-fused-A/B)** | 41.3% | **68.8%** | **85.3%** | 45.0% | **78.0%** | **88.1%** |
| canonical pre-#1040 (2026-04-17) | 41.3% | 63.3% | 80.7% | 41.3% | 74.3% | 86.2% |
| Δ | 0.0 | **+5.5** | **+4.6** | **+3.7** | **+3.7** | **+1.9** |

All metrics now above canonical. The earlier post-#1040 dip on R@20 (−5.5pp test, −6.4pp dev) self-resolved via successive reindexes restoring chunk count (14,734 → 16,150). Subsequent A/B should always quote both test AND dev — wins on test alone don't generalize (saw this with ColBERT 2-stage).

## What's queued

The Tier 3 chunker fix unlocked R@5 lift; remaining options — pick by appetite:

1. **Re-train Reranker V2 with post-mortem fixes** — re-mine hard negatives against cqs's own enriched index, keep TIE labels in pointwise, cap reranker pool at 20. ~1-2 weeks. Plausibly lands where the off-the-shelf attempts didn't.
2. ~~**Investigate dev R@20 regression from #1040**~~ — RESOLVED 2026-04-20. Successive reindexes restored chunk count (14,734 → 15,603 → 15,991 → 16,150). All metrics now ABOVE canonical: test R@5 +5.5pp, R@20 +4.6pp; dev R@5 +3.7pp, R@20 +1.9pp. Pruning-artifact theory confirmed.
3. ~~**Per-category HyDE re-validation**~~ — KILLED 2026-04-20. test R@5 −12.8pp, dev R@5 −22.0pp. Every category regressed on dev. Multi_step's +7.1pp test win flipped to −14.3pp on dev (noise). Per-category routing can't save it — no category positive across both splits. v2-era predictions (structural/type_filtered helping) reversed completely. Eval at `/tmp/hyde-{test,dev}.json`.
4. **ColBERT integration into cqs proper** with per-token index — multi-week architectural work; eval-tool gain didn't justify it yet.
5. **Embedder swap (CodeBERT / CodeT5+ / CodeR)** — same risk profile as the v9-200k experiment that already failed.
6. ~~**JSON output schema standardization (Task #17)**~~ — landed in #1038.

## Architecture state

- **Version:** v1.28.1 (live on crates.io 2026-04-20; GitHub Release with binaries)
- **MSRV:** 1.95
- **Local binary:** built from main; reinstall after merge with `cargo build --release --features gpu-index && systemctl --user stop cqs-watch && cp ~/.cargo-target/cqs/release/cqs ~/.cargo/bin/cqs && systemctl --user start cqs-watch`
- **Index:** 15,603 chunks (BGE-large; reindexed 2026-04-20 on v1.28.1 with v20→v21 migration applied). 7,675 LLM summaries cached (49% coverage).
- **Production R@5 on v3.v2 test (post-#1053, fresh fixture):** **66.1%** (+2.8pp vs v1.27.0 canonical 63.3%). Dev R@5 **75.2%** (+0.9pp). R@20: +4.6pp test / +2.8pp dev.
- **Open PRs:** none committed yet; one tiny one queued for the regenerate_v3_test envelope fix + fresh fixture
- **Open issues:** 5 pre-audit (tier-3 deferred / external-blocked: #106, #255, #717, #916, #956) plus 6 newly-filed audit deferrals (#1042-#1044 hard P4, #1047-#1049 trivial P4)
- **cqs-watch daemon:** running latest binary (post-#1040 chunker fix installed at `~/.cargo/bin/cqs`, daemon restarted 2026-04-18)
- **Pending uncommitted:** 4 files in `evals/queries/colbert_rerank_{test,dev}.{json,events.jsonl}` — eval artifacts from PR #1037 work; intentionally not staged (reproducible from script)

## Reranker V2 post-mortem (recorded for future revisit)

Phase 3 trained `microsoft/unixcoder-base` on the 382k pointwise corpus. Result: −24pp R@5 (full pool), still −4.6pp at smallest pool. Three causes, all fixable but combined ~1-2 weeks:

1. **TIE labels were dropped from pointwise.** Phase 2's `pairwise_to_pointwise.py` filtered 8641 TIE pairs entirely — model trained on binary labels, weaker ordering signal than BiXSE assumes. Fix: keep TIE as label=0.5, OR use original pairwise data with margin loss.
2. **Domain shift Stack v2 → cqs index.** Trained on raw Stack v2 chunks; cqs serves *enriched* chunks (NL desc + signature + content + doc). Fix: re-mine hard negatives from cqs's actual index; smaller corpus (~16k chunks) but domain-matched.
3. **Pool-size brittleness.** `(limit * 4).min(100)` over-retrieves; weak rerankers get amplified by large pools. Fix: cap reranker pool at ~20.

Full detail in `~/training-data/research/reranker.md`.

## Operational pitfalls (rolling forward)

- **Agent worktree leak via absolute paths** — `isolation: "worktree"` is *soft* isolation; agents using absolute paths in tool calls write to parent tree. Add explicit path-discipline text to every parallel-agent prompt. Filed as Anthropic feedback.
- **WSL git credential helper** — out-of-the-box, `git push` from `~/training-data` (and any WSL-native path) fails with "could not read Username." Fix: `git config --global credential.helper '/mnt/c/Program\ Files/Git/mingw64/bin/git-credential-manager.exe'`. Saved as memory `reference_wsl_git_creds.md`. Already configured globally; future repos work without setup.
- **Cargo publish 413 = "exclude" list missing** — `evals/queries/v3_*.json` pushed package over 10MB. `Cargo.toml` exclude list now blocks `evals/`, `samples/`, `tools/`, `cuvs-fork-push/`. Re-check after adding any new heavy dir.
- **Always run `cqs eval --baseline` after retrieval changes** — the regression gate from #1030 catches per-category R@K drops automatically. Save baselines per release: `evals/baseline-v1.27.0.json` etc.
- **Single-split A/B is noisy at N=109** — always confirm test wins on dev before declaring. ColBERT 2-stage taught this by showing +5.5pp R@5 on test that dropped to +0.9pp on dev.
- **Smoke-test against real producer output** — synthetic fixtures only catch what you anticipate. Phase 3 training failed first launch because synthetic smoke used `passage` field; real Phase 2 output used `content`. Saved as memory `feedback_smoke_real_shape.md`.
- **No time estimates in specs** — they're systemically too long. Frame in compute units / GPU hours / step counts. Wall-time predictions get better when anchored on concrete reference frames (size, count, throughput).

## What's parked

- **HyDE on v3 dev** — most promising untested representation lever. Per-category routing required.
- **ColBERT integration with per-token index** — eval tool exists, default off; full integration multi-week.
- **Code-aware embedder switch** — CodeBERT, CodeT5+-110M-embedding, UniXcoder all untested on v3. v9-200k didn't help.
- **Knowledge-augmented retrieval** — call/type graph as structured filter. Multi_step queries weakest at 28-43% R@1.
- **Meta-routing** — current router commits to one strategy; ensemble with learned weights could stop the wins-vanishing pattern.
- **Properly-retrained Reranker V2** — see post-mortem; gated on appetite for the 1-2 week re-mine + retrain.
