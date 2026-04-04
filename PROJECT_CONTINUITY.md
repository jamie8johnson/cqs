# Project Continuity

## Right Now

**Audit complete. 103 findings triaged. Fixing P1s next. (2026-04-04 19:30 CDT)**

### Audit results (v1.15.1, 2026-04-04)
103 findings across 16 categories. 10 P1, 13 P2, 33 P3, ~47 P4.
Triage: `docs/audit-triage.md`. Findings: `docs/audit-findings.md`.
No regressions from JSON schema migration. P1s are pre-existing bugs or normalization misses.

### Fix ordering (by file cluster, not priority)
1. telemetry_cmd.rs (8 findings) — RB-7, RB-9, SHL-20, SEC-7, RM-10, RM-11, DS-NEW-2, PB-10
2. store/search.rs (3 findings) — SEC-10, PF-2, AC-9
3. Normalization one-liners (6 findings) — AD-11, AD-13, AD-15, AD-12, PB-8, PB-9
4. README + CONTRIBUTING docs (9 findings) — DOC-11 to DOC-17, EXT-42
5. display.rs + similar.rs (4 findings) — CQ-NEW-3/5/7, HP-7
6. context.rs + batch (5 findings) — AD-14, EH-8/11, HP-1, OB-9
7. commands/mod.rs (4 findings) — TC-6, AC-11, HP-2/9
8. impact + trace (4 findings) — AC-7, CQ-NEW-6/PF-3, SHL-16
Then: onboard/scout, L5X, spans, wrapper cleanup, tests, remaining P3/P4.

### Session summary (2026-04-02 to 2026-04-04)
30 PRs (#753-784). Two releases (v1.15.0, v1.15.1). Major work:
- BGE-large fine-tuned (91.6% R@1, 57.5 CoIR), published to HuggingFace
- L5X + L5K PLC parsers, telemetry dashboard, custom agents
- CommandContext + lazy embedder/reranker, commands subdirectories, 4 file splits
- Batch/CLI unification (3 phases), CI rust-cache
- JSON schema migration — all 7 command groups + lib-level types
- Full 16-category audit: 103 findings, 10 P1, 13 P2
- Data integrity fixes (eval script, CoIR averaging, paper corrections)
- 4 design specs: cross-project call graph, embedding cache, JSON schema, language code gen
- Reranker re-eval: ms-marco confirmed useless for code, 4 V2 angles identified

### PR #784 (roadmap update) — CI re-running (flaky HNSW test)

## Parked
- Cross-project call graph — spec: `docs/superpowers/specs/2026-04-03-cross-project-call-graph-design.md`
- Embedding cache — spec: `docs/superpowers/specs/2026-04-03-embedding-cache-design.md`
- Language code generation — spec: `docs/superpowers/specs/2026-04-03-language-macro-design.md`
- Ladder logic (RLL) grammar
- Dart, hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB)
- L5X files from plant (waiting on access)
- Reranker V2 experiments (CG hard negs, BGE-reranker-v2-m3, distillation, ColBERT)

## Open Issues
- #717, #389, #255, #106, #63

## Architecture
- Version: 1.15.1, Languages: 52 + L5X/L5K, Commands: 54+, Tests: ~2196
- Best model: BGE-large FT (91.6% R@1, 57.5 CoIR)
- Published: jamie8johnson/bge-large-v1.5-code-search, jamie8johnson/e5-base-v2-code-search
- CI: rust-cache (clippy 41s, msrv 39s, test ~14m)
- CommandContext with lazy reranker + embedder (OnceLock)
- Commands in 7 subdirectories, 4 file splits, batch/CLI unified
- JSON schema: typed Serialize structs for all commands, field normalization
- Schema v16, crates.io v1.15.1 current
