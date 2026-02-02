# Project Continuity

## Right Now

**Clean slate.** Session work:
- GPU MCP flag merged (`cqs serve --gpu`)
- Markdown review: model refs → E5-base-v2, docs paths fixed
- Removed superseded files: hunches.toml, scars.toml, HUNCHES.md
- Archived DESIGN.md → `docs/DESIGN_SPEC_27k_tokens.md`

Convention: large files get token count in filename (e.g., `_27k_tokens.md`).

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- E5-base-v2 model with "passage: " / "query: " prefixes
- Schema v9 with windowing (parent_id, window_idx)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- MCP: CPU (22ms warm) or GPU (12ms warm) + HNSW

## Build & Run

```bash
source /home/user001/miniconda3/etc/profile.d/conda.sh
conda activate cuvs
export LD_LIBRARY_PATH="/home/user001/.cache/ort.pyke.io/dfbin/x86_64-unknown-linux-gnu/d3c01924b801c77ff17d300b24e6dcd46d378348a921a48d96f115f87074fbb1:/home/user001/miniconda3/envs/cuvs/lib:$LD_LIBRARY_PATH"
cargo build --release --features gpu-search
```

## Parked

- CAGRA persistence (serialize/deserialize) - would fix 1.5s startup
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
