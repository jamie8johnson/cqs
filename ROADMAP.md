# Roadmap

## Current: v1.16.0

v1.16.0: Language macro v2 (52 files → `languages.rs` + `.scm` queries). Dart (53rd language). 53 languages + L5X/L5K PLC exports. ~2325 tests.

### Eval Baselines (296 queries, 7 languages)

Config A (Cosine-only). **⚠️ Numbers are relative rankings — absolute values shift with codebase changes.** On v1.15.2 index, original v9-200k scores ~80% (was 90.5% on older index).

| Model | Params | R@1 | R@5 | MRR | Raw R@1 | CoIR |
|-------|--------|-----|-----|-----|---------|------|
| **BGE-large FT** | 335M | **91.6%** | 99.3% | **0.9521** | 66.2% | **57.5** |
| **BGE-large** | 335M | 90.9% | 99.3% | 0.9493 | 61.8% | 55.7 |
| **v9-200k** | 110M | 90.5% | 99.3% | 0.9482 | **70.9%** | 52.7 |
| E5-base | 110M | 75.3% | 99.0% | 0.8688 | 49.1% | 0.627 |

Enrichment stack contributes ~15pp. RRF hurts at scale (74.7% vs 90.9% cosine-only).

---

## Active & Next

### Training Experiments

**Band mining (Exp #1)** — mine negatives from similarity rank [20,50) instead of top-k. Uses original v9-200k as mining model, margin=0.05. Script: `~/training-data/run_band_mining.sh`.

**GIST Margin Sweep (Exp #2) — null result.** All margins 0.01-0.10 land in 80-83% pipeline. Default 0.05 confirmed correct. +1.8pp raw at margin=0.03 doesn't translate to pipeline.

**Open training items:**
- [ ] Band mining experiment
- [ ] Investigate Rust stress MRR collapse (0.046 for all E5-base models on v1.15.2 index)
- [ ] Re-baseline expanded eval on current codebase
- [ ] Iterative self-distillation (Exp #3 from SSD roadmap)
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

- [ ] **Unify capture name lists** — `chunk.rs` capture_types, `mod.rs` DEF_CAPTURES, and `define_chunk_types!` all maintain separate lists of chunk type names. Adding a ChunkType requires updating all three. Replace with single source of truth via `ChunkType::capture_name_to_chunk_type()`.
- [ ] **RPC/Service chunk type** — protobuf `service`/`rpc`, GraphQL `type Query`/`mutation`, gRPC definitions. Currently Function. Distinct concept: contract definitions, not implementations.
- [ ] **SQL view/trigger chunk types** — views, triggers, and stored procedures are all Function today. Views are computed data (closer to Property), triggers are event handlers. Matters for impact analysis on schema changes.
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
- SPLADE sparse retrieval
- Solidity modifier chunk type — `modifier onlyOwner()` is access control, not a function
- Rust impl block chunk type — `impl<T: Hash> Cache<T>` for type dependency analysis
- Test suite/describe chunk type — Jest `describe()`, RSpec `context`, pytest fixtures
- CSS @media/@keyframes chunk types — scoping rules and animation definitions

---

## Done (Summary)

| Version | Highlights |
|---------|-----------|
| v1.16.0 | Language macro v2 (52→2 files), Dart (53rd language) |
| v1.15.2 | 10th audit 103/103 fixed, typed JSON output structs, 35 PRs |
| v1.15.1 | JSON schema migration, batch/CLI unification, 4 file splits |
| v1.15.0 | L5X/L5K PLC, telemetry, CommandContext, custom agents, BGE-large FT |
| v1.14.0 | `--format text\|json` on all commands, `ImpactOptions`, scoring config |
| v1.13.0 | 296-query expanded eval, 9th audit, 16 commands added |
| v1.12.0 | Pre-edit hooks, query expansion, diff impact cap |
| v1.11.0 | Synonym expansion, f32→f64 cosine, 80/88 audit fixes, 19 OpenClaw PRs |
| v1.9.0 | BGE-large default, v9-200k LoRA preset, red team (23 findings) |
| v1.0.x | 52 languages, multi-grammar injection, LLM summaries, HNSW, call graphs |

**Training:** v9-200k (90.5% pipeline R@1, 110M) is production model. BGE-large FT (91.6%, 335M) published. 7 basin experiments, margin sweep — all confirmed v9-200k recipe optimal. CoIR: 57.5 (BGE-large FT), 52.7 (v9-200k).

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
