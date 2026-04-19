# Project Continuity

## Right Now

**v1.28.0 shipped (2026-04-19) — post-audit release closes the post-v1.27.0 16-category audit.** All 150 findings landed across PRs #1041 (P1, 26), #1045 (P2, 47), #1046 (P3, 69); 6 deferred items filed as issues #1042-#1044 (hard P4) + #1047-#1049 (trivial P4). Plus chunker doc fallback (PR #1040, +3.7pp test R@5), uniform JSON envelope (PR #1038, BREAKING), schema v21 migration, 17 new env vars, daemon defaults tuned. Live on crates.io + GitHub Releases.

**Right Now:** v1.28.0 binary installed at `~/.cargo/bin/cqs`, daemon restarted on it. Audit dossier at `docs/audit-{findings,triage}.md` (renamed v1.27.0 archives still in `docs/`). Next strategic question is open: chunker fix lifted test R@5 to 67.0% (above canonical 63.3%) but dev R@5 sits at 71.6% (still below canonical 74.3%) — partly corpus-pruning artifact (16,095 → 14,734 chunks during reindex). Worth a third reindex + re-eval to isolate the chunker contribution from the pruning noise.

**Branch:** main.

### Lever-by-lever results

| Lever | Result | Status |
|---|---|---|
| Tier 1.1 — eval-data hygiene | strict==permissive after `regenerate_v3_test.py` | done; canonical baseline R@5=63.3% on `v3_test.v2.json` |
| Tier 1.2 — MMR re-rank (surface-feature) | regressed at every λ < 1.0 | shipped inert opt-in via `CQS_MMR_LAMBDA`; embedding-MMR is the obvious follow-up |
| Tier 1.3 — chunk-type aware boost | within ±1pp noise of default 1.2 | default stays |
| Tier 2 — Reranker V2 (Phase 3 cross-encoder) | −24pp R@5 (domain shift + binary-label loss) | weights stay local at `~/training-data/reranker-v2-unixcoder/`; not shipped |
| Tier 2 — ColBERT 2-stage (mxbai-edge-colbert-v0-32m) | marginal/inconsistent: test α=0.9 +2.8pp R@5, dev α=0.9 +0.9pp | eval tool shipped; default OFF; PR #1037 |
| **Tier 3 — chunker doc fallback for short chunks** | **+3.7pp R@5 test vs canonical** (interlocked with LLM summary regen; dev ambiguous due to corpus-pruning artifact) | shipped in #1040 + #1041 P1 #3-#4 hardening |

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

Reranker V2 work also produced commits in the private `cqs-training` repo (research/reranker.md updated with Phase 1/2/3 + ColBERT results + post-mortem).

### v3 baselines (current, after #1040 reindex 2026-04-18)

`evals/queries/v3_test.v2.json` (109 queries) and `v3_dev.v2.json` (109 queries):

| Config | test R@1 | test R@5 | test R@20 | dev R@1 | dev R@5 | dev R@20 |
|---|---|---|---|---|---|---|
| **post-#1040 (chunker doc fallback + LLM regen)** | 41.3% | **67.0%** | 75.2% | 40.4% | **71.6%** | 79.8% |
| canonical pre-#1040 (2026-04-17) | 41.3% | 63.3% | 80.7% | 41.3% | 74.3% | 86.2% |
| Δ | 0.0 | **+3.7** | −5.5 | −0.9 | **−2.7** | −6.4 |

Test R@5 surpasses canonical (+3.7pp). Dev R@5 still below canonical (−2.7pp). R@20 down on both — the chunker fix and the LLM regen sharpen short-chunk discrimination at the top, but the deep-rank tail seems noisier post-reindex (chunk count 14,734 vs prior 16,095 — pruning during reindex). Watch the dev R@5 gap; may require a follow-up. Subsequent A/B should always quote both test AND dev — wins on test alone don't generalize (saw this with ColBERT 2-stage; see also dev R@20 here).

## What's queued

The Tier 3 chunker fix unlocked R@5 lift; remaining options — pick by appetite:

1. **Re-train Reranker V2 with post-mortem fixes** — re-mine hard negatives against cqs's own enriched index, keep TIE labels in pointwise, cap reranker pool at 20. ~1-2 weeks. Plausibly lands where the off-the-shelf attempts didn't.
2. **Investigate dev R@20 regression from #1040** — test-only fixture has +3.7pp R@5 / −5.5pp R@20; dev has −2.7pp R@5 / −6.4pp R@20. Likely artifact of corpus pruning during reindex (16,095 → 14,734 chunks); confirm by reindexing a third time and re-evaluating. ~half day.
3. **Per-category HyDE re-validation** — speculative, untested on v3. v2-era data showed +14pp structural / −22pp conceptual. Treat v2 numbers as motivation, not promise.
4. **ColBERT integration into cqs proper** with per-token index — multi-week architectural work; eval-tool gain didn't justify it yet.
5. **Embedder swap (CodeBERT / CodeT5+ / CodeR)** — same risk profile as the v9-200k experiment that already failed.
6. ~~**JSON output schema standardization (Task #17)**~~ — landed in #1038.

## Architecture state

- **Version:** v1.28.0 (live on crates.io 2026-04-19; GitHub Release workflow building binaries)
- **MSRV:** 1.95
- **Local binary:** built from main; reinstall after merge with `cargo build --release --features gpu-index && systemctl --user stop cqs-watch && cp ~/.cargo-target/cqs/release/cqs ~/.cargo/bin/cqs && systemctl --user start cqs-watch`
- **Index:** 14,734 chunks (BGE-large; reindexed 2026-04-18 with chunker doc fallback). 7,018 LLM summaries cached (47.7% coverage; remainder are non-callable chunks not eligible for SQ-6).
- **Production R@5 on v3.v2 test (post-#1040):** **67.0%** (was 63.3% at v1.27.0 shipping)
- **Open PRs:** none (audit + release queue cleared)
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
