# Roadmap

## Current: v1.16.0

v1.16.0: Language macro v2 (52 files → `languages.rs` + `.scm` queries). Dart (53rd language). 53 languages + L5X/L5K PLC exports. ~2325 tests.

### Eval Baselines (296 queries, 7 languages, re-baselined 2026-04-06)

Config A (Cosine-only). A6000. Current v1.16.0 codebase.

| Model | Params | R@1 | MRR |
|-------|--------|-----|-----|
| **BGE-large FT** | 335M | **91.9%** | **0.955** |
| BGE-large | 335M | 91.2% | 0.951 |
| v9-200k | 110M | 81.4% | 0.898 |
| E5-base | 110M | ~75% | ~0.87 |

BGE-large FT is the best model. Fine-tuning adds +0.7pp. 10pp gap to E5-base models is real and stable.

---

## Active & Next

### Training Experiments — E5-base ceiling reached

Three experiments all null. E5-base + CG-filtered 200K + GIST 0.05 + 1 epoch is the ceiling.

| Experiment | Result |
|-----------|--------|
| Margin sweep (Exp #2) | Null — all margins 80-83% pipeline |
| Band mining (Exp #1) | Null — 81.1% vs 81.4% baseline |
| Iterative distillation (Exp #3) | Null — 70.9% raw, exact fixed point |

Further embedding improvement requires BGE-large (already 91.2%) or fundamentally different training signal (enrichment-mismatch mining Exp #4, lock/fork-aware weights Exp #5).

- [x] Re-baseline expanded eval on current codebase (2026-04-06)
- [ ] Enrichment-mismatch mining (Exp #4) — mine from raw, train with enriched
- [ ] Lock/fork-aware training weights (Exp #5) — entropy-weighted loss
- [x] Find BGE-large FT ONNX path and re-baseline — 91.9% R@1 (path: `bge-large-lora-v1/onnx`)
- [ ] Paper v0.7

### Features

- [ ] **Cross-project call graph** — spec ready (`docs/superpowers/specs/2026-04-03-cross-project-call-graph-design.md`)
- [ ] **Embedding cache** — spec ready (`docs/superpowers/specs/2026-04-03-embedding-cache-design.md`)
- [ ] **Ladder logic (RLL) parser** — L5X CDATA extraction, JSR call graph
- [ ] **Migrate HNSW to hnswlib-rs** — eliminates unsafe blocks. Blocked: nightly-only dep, needs fork.
- [ ] **Reranker V2** — hard negative reranker (V1 was catastrophic failure)

### Languages

53 shipped. Remaining:
- [ ] **Clojure** — blocked: tree-sitter-clojure requires tree-sitter ^0.25, incompatible with 0.26
- [ ] **ArchestrA QuickScript** — needs custom grammar from scratch
- [ ] Astro, ERB, EEx/HEEx — need tree-sitter grammars
- DXF, Openclaw PLC, hnswlib-rs

### Infrastructure

- [x] **Fix --force summary cache invalidation** — ATTACH backup DB, copy summaries. PR #820.
- [ ] **Unify capture name lists** — `chunk.rs` capture_types, `mod.rs` DEF_CAPTURES, and `define_chunk_types!` all maintain separate lists of chunk type names. Adding a ChunkType requires updating all three. Replace with single source of truth via `ChunkType::capture_name_to_chunk_type()`.
- [x] **RPC/Service chunk type** — protobuf `service` → Service type. RPCs stay Function. #833.
- [x] **SQL view/trigger chunk types** — procedures/views/triggers → StoredProc. Functions stay Function. #833.
- [x] **Test/Variable chunk types** — Rust #[test], Python test_, Go Test prefix → Test. static mut, let/var, module-level assignments → Variable. #833.
- [ ] **Endpoint chunk type queries** — capture name registered but no .scm queries yet. Phase 2: Flask @app.route, Express app.get, Spring @GetMapping.
- [ ] **JS/TS test block detection** — Jest describe/it/test call expressions. Needs tree-sitter query for call expressions with string args.
- [ ] **--exclude-type search filter** — `--chunk-type` includes, but no way to exclude (e.g., "callable but not test"). Useful for impact analysis.
- [ ] **Audit weak chunk type tests** — post_process overrides can mask wrong query captures. Haskell instance test was asserting Object while query said Struct — passed because post_process silently fixed it. 34 languages have post_process; scan for tests where the assertion matches the post_process output, not the query capture.
- [ ] **Refactor batch --rrf opt-in** — `semantic_only` field suppressed with `#[allow(dead_code)]`. Rename to `rrf: bool`, wire as opt-in `--rrf` flag. Cosine-only is the default since RRF degrades quality by 17pp.
- [ ] **Agent adoption** — fewer commands in prompts (only `scout`/`task`)
- [ ] **Reranker eval config** — add Config G (Cosine + rerank) to pipeline_eval

---

## Parked

- Wiki system — spec ready (standalone design, `~/wiki/`)
- SSD fine-tuning experiments — spec ready, 5 experiments prioritized
- MCP server — re-add as slim read-only wrapper when CLI is rock solid
- Pre-built reference packages (#255)
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- KD-LoRA distillation (CodeSage→E5)
- ColBERT late interaction
- SPLADE sparse-dense hybrid — shipped in v1.17.0 behind `--splade` flag. V2 eval ablation: 0pp on BGE-large, -1.4pp on E5-LoRA. Off-the-shelf naver/splade doesn't help code search. **Code-trained SPLADE** is the next step: fine-tune on cqs training data with code-specific vocabulary expansion. Reuse E5-LoRA data pipeline.
- **Fine-tune SPLADE for code search** — train naver/splade-cocondenser on code pairs from Stack repos. The model should learn to expand "sort" → {quicksort, heapsort, merge_sort}, "retry" → {backoff, exponential, jitter}. Training data exists (200k pairs from v9-200k). Priority: after eval reaches 300q.
- Solidity modifier chunk type — `modifier onlyOwner()` is access control, not a function
- Rust impl block chunk type — `impl<T: Hash> Cache<T>` for type dependency analysis
- Test suite/describe chunk type — Jest `describe()`, RSpec `context`, pytest fixtures
- CSS @media/@keyframes chunk types — scoping rules and animation definitions

---

## Done (Summary)

| Version | Highlights |
|---------|-----------|
| v1.16.0 | Language macro v2 (52→2 files), Dart (53rd), ConfigKey + Impl chunk types, HNSW traversal filtering, batch code-only filter, eval cleanup |
| v1.15.2 | 10th audit 103/103 fixed, typed JSON output structs, 35 PRs |
| v1.15.1 | JSON schema migration, batch/CLI unification, 4 file splits |
| v1.15.0 | L5X/L5K PLC, telemetry, CommandContext, custom agents, BGE-large FT |
| v1.14.0 | `--format text\|json` on all commands, `ImpactOptions`, scoring config |
| v1.13.0 | 296-query expanded eval, 9th audit, 16 commands added |
| v1.12.0 | Pre-edit hooks, query expansion, diff impact cap |
| v1.11.0 | Synonym expansion, f32→f64 cosine, 80/88 audit fixes, 19 OpenClaw PRs |
| v1.9.0 | BGE-large default, v9-200k LoRA preset, red team (23 findings) |
| v1.0.x | 52 languages, multi-grammar injection, LLM summaries, HNSW, call graphs |

**Training:** BGE-large is the production model (91.2% pipeline R@1). v9-200k (81.4%, 110M) is the E5-base ceiling — 10 experiments confirmed. BGE-large FT (91.9%, 335M) published. CoIR: 57.5 (BGE-large FT), 52.7 (v9-200k).

**Enrichment stack (shipped):** Type-aware signatures (+3.6pp) → call graph context (63% enriched) → LLM summaries (opt-in) → HyDE predictions (opt-in). Ablation: doc +6.8pp > file context +4.1pp > signatures +1.4pp > call graph ~0.4pp.

---

## Open Issues

- #717: HNSW mmap
- #389: CAGRA memory (blocked on upstream)
- #255: Pre-built reference packages
- #106: ort stable (currently rc.12)
- #63: paste dep (RUSTSEC-2024-0436, transitive via tokenizers)

---

## Red Team — Accepted/Deferred

- RT-DATA-2: Enrichment no idempotency marker (needs schema change)
- RT-DATA-3: HNSW orphan accumulation in watch mode (no deletion API)
- RT-DATA-5: Batch OnceLock stale cache (by design)
- RT-DATA-6: SQLite/HNSW crash desync (needs generation counter)
- RT-DATA-4: Notes file lock vs rename race (low)
