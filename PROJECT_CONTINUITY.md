# Project Continuity

## Right Now

**v1.15.2 released. Starting language codegen macro. Margin sweep running. (2026-04-05 CDT)**

### Active work
- Language code generation macro — spec at `docs/superpowers/specs/2026-04-03-language-macro-design.md`
- Margin sweep: 4/6 done (0.01, 0.03, 0.05, 0.08), 0.10 running, 0.15 queued. ETA ~1:15 PM.
- Band mining experiment ready to launch after sweep: `~/training-data/run_band_mining.sh`

### Margin sweep results (raw R@1, 55-query eval)
| Margin | R@1 | MRR | Eval Loss |
|--------|-----|-----|-----------|
| 0.01 | 69.1% | 0.785 | 0.084 |
| **0.03** | **72.7%** | **0.811** | 0.024 |
| 0.05 | 70.9% | 0.792 | 0.005 |
| 0.08 | 69.1% | 0.797 | 0.002 |
Peak at 0.03 — +1.8pp over baseline. Pipeline eval pending.

### Uncommitted on main
- `PROJECT_CONTINUITY.md` — this file
- `docs/audit-triage.md` — final fix status
- `.claude/agents/implementer.md` — targeted-test-only rule
- `docs/audit-findings-v1.15.1.md`, `docs/audit-triage-v1.15.1.md` — archived audit
- `docs/superpowers/specs/2026-04-04-wiki-system-design.md` — spec
- `docs/superpowers/specs/2026-04-04-ssd-fine-tuning-roadmap.md` — spec
- `docs/superpowers/plans/2026-04-04-wiki-system.md` — plan

### Design decisions this session
- Wiki system: standalone (own git repo, `~/wiki/`), not colocated
- Implementer agents: targeted tests only (`-- test_name`), never full suite
- GIST margin 0.03 beats 0.05 baseline for CG-filtered data

### Training results (BGE-large FT)
91.6% R@1 fixture, 50% R@1 real (100q), 57.5 CoIR. Published: jamie8johnson/bge-large-v1.5-code-search

## Parked
- Cross-project call graph — spec ready
- Embedding cache — spec ready
- Wiki system — spec ready (standalone design)
- SSD fine-tuning experiments — sweep running, band mining prepped
- Ladder logic (RLL) grammar
- Dart, hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- Reranker V2 experiments

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste — advisory resolved, monitoring)

## Architecture
- Version: 1.15.2, Languages: 52 + L5X/L5K, Commands: 54+, Tests: 2351
- Best model: BGE-large FT (91.6% R@1, 57.5 CoIR)
- CI: rust-cache, ~16m test
- CommandContext with lazy reranker + embedder + open_readwrite
- Commands in 7 subdirectories, JSON schema typed Serialize
- 10th audit: 103/103 fixed, 0 remaining
