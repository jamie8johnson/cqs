# Project Continuity

## Right Now

**v1.25.0 shipped + 11th full audit merged. Next: 4 waves of pull-list fixes on autopilot. (2026-04-14 ~20:45 CDT)**

### What landed today (massive day)

**Morning — v1.25.0:**
- #942 determinism fixes (hash iteration + SPLADE-at-α=1.0 + rowid re-sort)
- #943 eval-output-location fix (watch-reindex contamination — root cause of 2 days of drift)
- #944 v1.25.0 release — new per-category alpha defaults + multi_step router fix
- #945 notes mutation daemon bypass

**Afternoon — 11th full audit:**
- 16 categories × 2 batches × 8 parallel opus auditor agents → **236 findings**
- Triaged into P1 (49) / P2 (47) / P3 (97) / P4 trivial (16) / P4 issues (32)
- Executed in 3 waves of implementer agents:
  - Wave 1 (5 agents, shared tree): 32 commits across watch.rs, staleness, dispatch, SPLADE, classifier/sorting
  - Wave 2 (4 agents, worktree-isolated): 24 commits across ingest, cache, HNSW, security
  - Wave 3 (5 agents, worktree-isolated): 60 commits across daemon, resources, tests, perf, scaling/env
- **PR #976 merged 20:40 CDT** — 126 commits closing 166/236 findings
- **PR #977 merged 19:33 CDT** — #856 atexit Mutex UB fix (dependent follow-up)
- 11 stale issues closed (shipped in v1.22/1.23 or superseded by refactor-lane)
- 25 P4 audit issues filed (#951-#975), 5 refactor issues (#946-#950)

**Also pushed:**
- `cqs-training` repo (research/sparse.md with clean-infra alpha re-sweep — source of truth for v1.25.0 defaults; 15 accumulated scripts + gitignore cleanup)

### Next session — pull list queued, autopilot contract

**Wave A** (3 parallel agents, worktree-isolated):
- #961 WSL 9P/NTFS mmap auto-detect → `src/store/mod.rs`
- #963 Reranker batch chunking → `src/reranker.rs`
- #962 CAGRA itopk + graph_degree env overrides → `src/cagra.rs`

**Wave B** (1 agent, bigger refactor):
- #947 Commands/BatchCmd unification — half-day refactor, kills daemon/CLI parity drift class

**Wave C** (4 parallel agents):
- #946 Store typestate (closes write-on-readonly class)
- #948 `atomic_replace` helper (closes fs-persist durability class)
- #949 Model abstraction (unblocks BGE→E5 default switch)
- #950 CAGRA persistence (daemon hot-restart 30s → 5s)

**Wave D** (2 parallel agents):
- #972 Daemon `try_daemon_query` test scaffold
- #964 Aho-Corasick `classify_query` (pre-req for classifier accuracy work)

Order: A → B → C → D sequential between waves (later waves benefit from earlier fixes); parallel within each. Rebuild + install binary after each wave merge so daemon uses current code.

### Architecture state

- **Version:** v1.25.0, Schema v20
- **Binary:** rebuilt + installed from post-merge main (`ea7c72b`), daemon active
- **Index:** clean (post-GC, 13,279 chunks down from 81%-dup 69,444)
- **Per-category SPLADE α defaults:** identifier 0.90, structural 0.60, conceptual 0.85, behavioral 0.05, rest 1.0 (tuned on *dirty* index; may need re-fit on clean index — CPU Lane item)
- **Determinism:** end-to-end after #942 + #943, 15+ sort sites hardened by wave 1
- **Audit test count:** 1404 lib tests + integration suite all pass

### Eval numbers (honest, post-clean-index)

| Eval | Model | R@1 | R@5 | R@20 |
|---|---|---|---|---|
| V2 (265q clean) | BGE-large fully routed | 37.4% | 55.8% | 77.4% |
| V2 (265q clean) | E5 v9-200k fully routed | 37.4% | 56.6% | 78.1% |
| V2 (265q clean) | Oracle per-category α | 49.4% | — | — |

**E5 v9-200k ties BGE on R@1, slight edge on R@5/R@20 — at 1/3 the embedding dim.** Gated on #949 (model abstraction) to make the BGE→E5 default switch low-friction.

Pre-2026-04-14 numbers (44.9% R@1 etc.) were measured against the dirty (81% worktree-dup) index; see GC prune_all suffix-match bug fixed in wave 1. Roadmap flags the alpha-refit need.

### Residual puzzles

- **Classifier accuracy** — 4.5pp oracle gap entirely in `classify_query()`, not alpha picks. Today: negation 100%, identifier 84%, structural 19%, behavioral 5%, conceptual 3%, cross_language 0%. Wave D #964 (Aho-Corasick) is a pre-req; actual investigation + centroid-matching-on-BGE-embeddings proposed in ROADMAP.
- **Alpha defaults on clean index** — today's defaults were fit on dirty data. Re-sweep on clean infra to find real optima; probably shifts slightly. Roadmap CPU Lane.

## PR status

- All merged. No open PRs.
- Main at `ea7c72b` (audit PR) → `ee0ccae` (atexit) → `4b93e8b` (notes bypass).

## Open Issues (40, tiered)

- **Tier 1 (11)** — fix-worthy near-term: #946-#950 refactors, #953 migration backup, #961 WSL mmap, #962 CAGRA itopk, #963 reranker batch, #972 daemon tests
- **Tier 2 (10)** — real impact, harder: #956 Metal/ROCm, #957 SPLADE/reranker presets, #964 Aho-Corasick, #968 shared runtime, #973 dispatch_search tests, #63 paste RUSTSEC, #916 mmap SPLADE, #917 streaming SPLADE, #921 WSL SPLADE save, #923 INDEX_DB_FILENAME
- **Tier 3 (19)** — low urgency/blocked upstream: #106 ort RC, #389 CAGRA CPU retain, #717 HNSW RAM, #255 reference packages, plus 15 minor perf/refactor/test items

Filter: `gh issue list --state open --label tier-N`

## Architecture notes
- Deterministic search path + deterministic eval pipeline (end-to-end)
- SPLADE always-on, α controls fusion weight only
- HNSW dirty flag self-heals via checksum verification (per-kind after wave 2 AC-V1.25-8)
- cuVS 26.4 + patched with `search_with_filter` (upstream rapidsai/cuvs#2019)
- Eval results write to `~/.cache/cqs/evals/` (outside watched project dir)
- Daemon (`cqs watch --serve`), thread-per-connection capped at 64 (SEC-V1.25-1)
