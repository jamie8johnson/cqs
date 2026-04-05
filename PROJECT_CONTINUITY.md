# Project Continuity

## Right Now

**Language macro v2 in CI. Band mining running. Margin sweep was null result. (2026-04-05 CDT)**

### PR #815 — language macro v2
Branch: `feat/language-macro-v2`. Clippy fix pushed, CI re-running. Consolidates 52 per-language `.rs` files into `languages.rs` + 106 `.scm` query files. 2320 tests pass, 31 private-function tests dropped.

### Band mining experiment (Exp #1)
Running on A6000: `~/training-data/run_band_mining.sh`. Mining with original v9-200k model, band [20,50), margin=0.05. ETA ~4-5h from 12:56 CDT. Monitor: `tail -f ~/training-data/exp-band/experiment.log`. Resumable.

### Margin sweep (Exp #2) — null result
All margins (0.01-0.10) land in 80-83% pipeline R@1. Margin=0.03 gives +1.8pp raw R@1 (repeatable) but pipeline is training variance. Default 0.05 confirmed correct. v10 designation withdrawn.

**Stale baseline discovered:** 90.5% pipeline R@1 in expanded eval table was from older codebase. On v1.15.2 index, original v9-200k scores 80.1%. Needs re-baselining.

**Rust stress MRR collapsed to 0.046** for ALL E5-base models on current index. Not margin-related. Needs investigation.

### Uncommitted on feat/language-macro-v2
- `ROADMAP.md` — margin sweep + band mining updates
- `docs/notes.toml` — margin sweep note updated

### Session PRs (#810-815)
- #810: 5 misc P3/P4 audit findings — merged
- #811: DS-NEW-4, last audit finding (103/103) — merged
- #812: docs pre-release review — merged
- #813: v1.15.2 release — merged, published crates.io + tagged
- #814: session artifacts — merged
- #815: language macro v2 — in CI

## Parked
- Cross-project call graph — spec ready
- Embedding cache — spec ready
- Wiki system — spec ready (standalone design)
- SSD fine-tuning: band mining running, iterative self-distillation next
- Rust stress MRR collapse — needs investigation
- Re-baseline expanded eval numbers on current index
- Ladder logic (RLL) grammar
- Dart, hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- Reranker V2 experiments

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste — advisory resolved, monitoring)

## Architecture
- Version: 1.15.2, Languages: 52 + L5X/L5K, Commands: 54+, Tests: 2320
- Best model: BGE-large FT (91.6% R@1 fixture, but needs re-eval on current index)
- CI: rust-cache, ~16m test
- CommandContext with lazy reranker + embedder + open_readwrite
- Commands in 7 subdirectories, JSON schema typed Serialize
- 10th audit: 103/103 fixed, 0 remaining
- Language macro v2: `languages.rs` + `queries/*.scm` (PR #815)
