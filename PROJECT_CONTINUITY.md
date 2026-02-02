# Project Continuity

## Right Now

**Hybrid CAGRA complete.** MCP starts instantly (HNSW), upgrades to GPU in background.

Pronunciation: cqs = "seeks" (it seeks code semantically).

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- E5-base-v2 model with "passage: " / "query: " prefixes
- Schema v9 with windowing (parent_id, window_idx)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- MCP: hybrid startup (HNSW 30ms, CAGRA upgrades in background)

## Build & Run

```bash
conda activate cuvs  # LD_LIBRARY_PATH set automatically via conda env vars
cargo build --release --features gpu-search
```

## Parked

- CAGRA persistence (serialize/deserialize) - hybrid startup approach used instead
- Republish to crates.io
- Curator agent, fleet coordination

## Open Questions

None active.

## Hardware

- i9-11900K, 128GB physical / 92GB WSL limit
- RTX A6000 (48GB VRAM), CUDA 12.0/13.0
- WSL2

## Test Repo

`/home/user001/rust` (rust-lang/rust, 36k files) - indexed with E5-base-v2

## Timeline

Project started: 2026-01-30
