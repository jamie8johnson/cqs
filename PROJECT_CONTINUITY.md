# Project Continuity

## Right Now

**Waves A+B+C (3/4) + slow-tests gating merged. #987 hotfix rerunning. Wave D queued. (2026-04-15 ~11:35 CDT)**

Main at `0a9c5e3`. Merged this session:
- #979 Wave A quick wins — `7ccda3b`
- #981 Wave B Commands/BatchCmd unification — `14f122d`
- #983 Wave C2 atomic_replace — `7b24995`
- #984 Wave C3 ModelConfig abstraction — `edd5093`
- #985 Wave C4 CAGRA persistence — `4f23563`
- #982 Wave C1 Store typestate — `f8b6e8d`
- #988 CI slow-tests gating (#980) — `0a9c5e3`

**In flight:** #987 `fix/post-946-cagra-save-with-store-generic` — 1-line fix making `CagraIndex::save_with_store` generic over `Mode`. Merge-order artifact: #985 defined it with default `Mode = ReadWrite`, #982 then made `build_vector_index_with_config<Mode>` pass `&Store<Mode>` through. **Only breaks `--features gpu-index` builds** — CI uses default features so main-on-GitHub compiles fine. The hotfix branch has been rebased onto post-#988 main so its CI now inherits the slow-tests gating (~18 min instead of ~2h). Monitor `bf5vemyx3` polling for CI completion.

### Slow-tests gating — what #988 did

5 CLI integration test binaries were eating ~2h of CI on every PR (`cli_batch_test` 34m39s, `cli_graph_test` 33m30s, `cli_commands_test` 30m40s, `cli_test` 14m49s, `cli_health_test` 5m02s) — each test cold-loads the full ONNX/HNSW/SPLADE/reranker stack per `cqs` subprocess invocation.

Fix: `slow-tests = []` Cargo feature + `#![cfg(feature = "slow-tests")]` file-gate on all 5. New `.github/workflows/slow-tests.yml` runs them daily at 08:00 UTC plus `workflow_dispatch`. PR CI now ~18 min. Follow-up (#980 covers) is to switch them to the in-process fixture pattern like `cli_notes_test`/`router_test` already use.

### Disk cleanup this session

- cargo clean profile dev: **-19G** (33G → 14G target)
- training-data archive: **-25G** uncompressed (96G → 71G, with 18G of compressed provenance in `~/training-data/archive/`)
- 37 stale branches deleted (all squash-merged); 17 stale worktrees removed
- Total reclaimed: **~44G**

Archive contents preserved:
- `loras-removed-from-registry.tar.zst` (6.5G) — 12 removed-from-registry E5 LoRA variants
- `loras-v9-basin.tar.zst` (2.8G) — basin-cluster variants (same-ceiling as v9-200k)
- `splade-nondeployed.tar.zst` (1.4G) — naver baseline + v2 training run (NB: splade-code-v1 kept, it's the source of the deployed model)
- `exp-null-results.tar.zst` (3.4G) — v11-band + v11-distill (null results per sparse.md)
- `sweep-margin-losers.tar.zst` (2.5G) — 0.01/0.03/0.08/0.1/0.15 (0.05 confirmed winner)
- JSONL chains (v9 intermediate, CSN raw, stack intermediate, combined superseded) — ~1.6G combined compressed

### `into_readonly()` design note

`Store<ReadWrite>` gained `pub fn into_readonly(self) -> Store<ReadOnly>`. Zero-cost type-level erasure — the SQLite connection stays open in RW mode but the `ReadOnly` phantom blocks write-gated methods at compile time. Implementation uses `ManuallyDrop` + `ptr::read` because `Store<Mode>` impls `Drop`.

Filed **#986** (tier-2 / refactor / testing) as the follow-up: closure-based `Store::open_readonly_after_init<F>` would give semantic RO at the SQLite level too, at the cost of one WAL checkpoint + reopen per fixture. Migration is mechanical across ~6-8 test call sites.

### Wave D plan (prompts preserved below)

Two worktree-isolated opus agents, parallel:
- **D1 #972** — extract `translate_cli_args_to_batch` pure function from `src/cli/dispatch.rs:457-494`; new `tests/daemon_forward_test.rs` (`#[cfg(unix)]`) with arg-translation tests + mock `UnixListener` E2E + notes-mutation-bypass regression. Regression seed: remove a command from the notes block-list, confirm a test fails.
- **D2 #964** — `LazyLock<Vec<&'static str>>` for `language_names()`; `LazyLock<AhoCorasick>` for `extract_type_hints` / `BEHAVIORAL_VERBS` / `CONCEPTUAL_NOUNS` / `NL_INDICATORS` / `MULTISTEP_PATTERNS` / `STRUCTURAL_PATTERNS`. Classifier outputs must be bitwise-identical.

### Residual puzzles

- **Classifier accuracy** — 4.5pp oracle gap (49.4% − 37.4%) entirely in `classify_query()`, not alpha picks. Wave D #964 is the pre-req for the real investigation (centroid matching on BGE embeddings is the recommended starting point per ROADMAP).
- **Alpha defaults on clean index** — current v1.25.0 defaults were fit on dirty data. Re-sweep pending.

### Eval numbers (honest, post-clean-index)

| Eval | Model | R@1 | R@5 | R@20 |
|---|---|---|---|---|
| V2 (265q clean) | BGE-large fully routed | 37.4% | 55.8% | 77.4% |
| V2 (265q clean) | E5 v9-200k fully routed | 37.4% | 56.6% | 78.1% |
| V2 (265q clean) | Oracle per-category α | 49.4% | — | — |

E5 v9-200k ties BGE on R@1, slight edge on R@5/R@20, at 1/3 the embedding dim. Default switch unlocked by #949 (Wave C3 merged).

## Pending Changes

Uncommitted on main: `docs/notes.toml` (4 re-added session notes after a git-stash-drop incident). Will land after #987 via a `chore/post-wave-c-tears` branch alongside this file and `ROADMAP.md`.

## Architecture state

- **Version:** v1.25.0, Schema v20
- **Binary:** installed from local build of `f8b6e8d` + the pending `save_with_store<Mode>` fix. Daemon active.
- **Index:** clean (13,279 chunks)
- **Test count:** 1415+ lib tests. 5 CLI integration binaries now gated behind `slow-tests` feature.
- **New this session (landed):** `cqs::fs::atomic_replace`, `ModelConfig::{InputNames, PoolingStrategy, output_name}`, `CagraIndex::{save, load, delete_persisted}` + `.cagra.meta`, `Store<ReadOnly>`/`Store<ReadWrite>` typestate + `into_readonly()`, `slow-tests` feature gating.

## Operational pitfalls (do not repeat)

1. **Multiple watchers racing.** Early attempts spawned two bash watchers on the same PRs; the old one merged prematurely. Use `Monitor` (single task lifecycle) not ad-hoc backgrounded shell scripts.
2. **CI clippy is not `--all-targets`.** CI uses `cargo clippy -- -D warnings` (+ `cargo test --verbose` default features, *no* `gpu-index`). Locally `cargo clippy --all-targets --features gpu-index` misses warnings that CI promotes to errors. Match exactly before pushing.
3. **Agent mid-refactor handoff.** Wave C1 agent stopped at commit 2/N with the working tree dirty. Always `git status` on the worktree after an agent reports done.
4. **`/tmp` is transient on WSL.** Shell-script logs in `/tmp/*.log` vanish on restarts. Monitor's output file is harness-managed and survives the session.
5. **Merge-order artifacts from parallel PRs.** Four Wave C PRs were branched from the same base; #982 + #985 independently touched the same `save_with_store` surface with different typing assumptions. Main compiled green per PR (default features) but broke `--features gpu-index` builds once both were squashed. Always `cargo build --release --features gpu-index` on post-merge main after a multi-PR batch.
6. **`git stash drop` is destructive.** A force-drop lost 4 session notes (recoverable via `cqs notes add`) and the tears/ROADMAP diff (this file is the reconstruction). If you must stash with a rebase mid-flight, `git stash apply` first, verify the stash was a dupe, then drop.

## Open Issues (current cut)

Tier 1 remaining: **#972** daemon tests (Wave D), **#964** Aho-Corasick (Wave D), **#953** migration backup, **#980** slow CLI test perf (partially addressed by #988, full in-process rewrite still open), **#986** `open_readonly_after_init` (closes #946 follow-up).

Closed this session: #961, #962, #963, #947, #948, #949, #950, #946 (via #979/#981/#983/#984/#985/#982).

Filter: `gh issue list --state open --label tier-N`

## Architecture notes

- Deterministic search + deterministic eval (end-to-end)
- SPLADE always-on, α controls fusion weight only (SPLADE model at `~/.cache/huggingface/splade-onnx/`, hash-identical to `~/training-data/splade-code-v1/onnx/model.onnx` — keep that training dir)
- HNSW dirty flag self-heals via per-kind checksum verification (AC-V1.25-8)
- cuVS 26.4 patched with `search_with_filter` + `add-serialize-deserialize` (for #950) — tracks rapidsai/cuvs#2019
- CAGRA persistence: `.cagra` + `.cagra.meta` sidecar with blake3 checksum + `CQS_CAGRA_PERSIST` toggle
- `atomic_replace(tmp, final)`: fsync tmp → rename → fsync parent → EXDEV copy fallback
- ModelConfig drives ONNX inputs/output/pooling (BGE/E5/custom all describe via config)
- Store typestate: `Store<ReadOnly>`/`Store<ReadWrite>`; write methods on `impl Store<ReadWrite>`; daemon holds `ReadOnly` so write-on-readonly is a compile error
- Daemon (`cqs watch --serve`), thread-per-connection capped at 64 (SEC-V1.25-1)
