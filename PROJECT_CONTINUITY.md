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
source /home/user001/miniconda3/etc/profile.d/conda.sh
conda activate cuvs
export LD_LIBRARY_PATH="/home/user001/.cache/ort.pyke.io/dfbin/x86_64-unknown-linux-gnu/d3c01924b801c77ff17d300b24e6dcd46d378348a921a48d96f115f87074fbb1:/home/user001/miniconda3/envs/cuvs/lib:$LD_LIBRARY_PATH"
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
