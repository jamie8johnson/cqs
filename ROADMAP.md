# Roadmap

## Current: v1.19.0

54 languages. 27 chunk types. 265-query v2 eval. BGE-large + LLM summaries = best config.

### Eval Baselines

| Eval | Model | R@1 | Notes |
|------|-------|-----|-------|
| Fixture (296q) | BGE-large FT | 91.9% | Synthetic fixtures, high ceiling |
| Fixture (296q) | BGE-large | 91.2% | Production model |
| V2 (265q, live) | BGE-large | 68.0% | Real codebase, diverse categories |
| V2 (265q, live) | BGE-large + LLM summaries | 69.3% | Best production config |
| V2 (265q, live) | E5-LoRA v9-200k | 54.7% | 13pp gap to BGE-large |

---

## Active

### GPU Lane
- [x] ~~SPLADE v1~~ — **NULL**. 0.0pp R@1. Failure: reg_weight 5e-4 too weak (198 tokens vs 40), BERT vocab can't represent code identifiers, dense-mined hard negatives don't teach token discrimination.
- [ ] **SPLADE v2: token-overlap hard negatives** — mine by token Jaccard, not dense similarity. Foundation — without the right training signal, reg and vocab won't help. New mining pass.
- [ ] **SPLADE v3: reg sweep** — reg_weight 1e-3, 3e-3, 5e-3 on v2 data. 3h each.
- [ ] **SPLADE v4: CodeBERT base** — SPLADE on microsoft/codebert-base (50k code BPE vocab). Only if v2+v3 show promise.
- [ ] **Reranker V2** — code-trained cross-encoder (ms-marco-MiniLM was -15pp)

### CPU Lane — ready to pick up
- [ ] **Paper v0.7** — research writeup
- [ ] **Cross-project call graph** — spec ready. CPU-only (graph algorithms, no embeddings)
- [ ] **Extern chunk type** — FFI declarations without implementation: Rust `extern fn`, TS `declare function`, C prototypes, Java `native`, C# `extern`. Distinguishes "contract" from "has code" for impact analysis.
- [ ] **Namespace chunk type** — C++ `namespace`, C# `namespace`. Currently Module or uncaptured.
- [ ] **Middleware chunk type** — Express `app.use()`, Django middleware. Framework-specific but common pattern.
- [ ] **Solidity modifier chunk type** — `modifier onlyOwner()` → new type. ~30 min.
- [ ] **Rust impl block chunk type** — `impl<T: Hash> Cache<T>`. ~30 min.
- [ ] **CSS @media/@keyframes** — scoping rules, animations. ~30 min.
- [ ] **Agent adoption** — fewer commands in agent prompts (only `scout`/`task`)
- [ ] **Move language** — blocked: no tree-sitter grammar on crates.io. Needs git dep or custom.

---

## Blocked

- **Clojure** — tree-sitter-clojure requires tree-sitter ^0.25, incompatible with 0.26
- **Astro, ERB, EEx/HEEx** — need tree-sitter grammars
- **Migrate HNSW to hnswlib-rs** — nightly-only dep, needs fork
- **ArchestrA QuickScript** — needs custom grammar from scratch

---

## Parked

- Wiki system — spec revised (agent-first), parked for review
- SSD fine-tuning experiments — spec ready, 5 experiments
- MCP server — re-add when CLI solid
- Pre-built reference packages (#255)
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- KD-LoRA distillation (CodeSage→E5)
- ColBERT late interaction
- Enrichment-mismatch mining (Exp #4) — mine from raw, train with enriched
- Lock/fork-aware training weights (Exp #5) — entropy-weighted loss
- Ladder logic (RLL) parser — L5X CDATA extraction, JSR call graph
- DXF, Openclaw PLC

---

## Done (Summary)

| Version | Highlights |
|---------|-----------|
| v1.19.0 | `--include-type`/`--exclude-type`, Java/C# test+endpoint, batch `--rrf`, capture list unification, Phase 2 chunks, 265q eval, store dim check |
| v1.18.0 | Embedding cache, 5 chunk types, v2 eval harness, batch query logging |
| v1.17.0 | SPLADE sparse-dense hybrid, schema v17, HNSW traversal filtering, ConfigKey, CAGRA itopk fix |
| v1.16.0 | Language macro v2, Dart (53rd), Impl chunk type |
| v1.15.2 | 10th audit 103/103, typed JSON output structs, 35 PRs |
| v1.15.1 | JSON schema migration, batch/CLI unification |
| v1.15.0 | L5X/L5K PLC, telemetry, CommandContext, custom agents, BGE-large FT |
| v1.14.0 | `--format text|json`, ImpactOptions, scoring config |
| v1.13.0 | 296-query eval, 9th audit, 16 commands |
| v1.12.0 | Pre-edit hooks, query expansion, diff impact cap |
| v1.11.0 | Synonym expansion, f32→f64 cosine, 80/88 audit fixes |
