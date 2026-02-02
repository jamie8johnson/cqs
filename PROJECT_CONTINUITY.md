# Project Continuity

## Right Now

**GPU MCP flag merged** - `cqs serve --gpu` for GPU query embedding

Benchmarks:
- CPU MCP: cold 0.52s, warm 22ms
- GPU MCP: cold 1.15s, warm 12ms (~45% faster warm)

Default is CPU for lower cold-start latency.

### Index Rebuild

rust-lang/rust indexing with E5-base-v2 complete (~2h).

### Full Audit Complete

9-layer fresh-eyes audit found:
- 1 bug: schema version mismatch (fixed)
- 1 dead code warning (fixed)
- Test model names outdated (fixed)
- 5 unmaintained deps (transitive, low risk, ignored)
- Test coverage gaps: embedder.rs, cagra.rs, cli.rs (low risk)

Codebase is solid.

### Notes Simplified

Migrated sentiment scale from 21-point (0.1 increments) to 5-point:
- -1 (serious pain), -0.5 (notable pain), 0 (neutral), 0.5 (notable gain), 1 (major win)
- Clearer signal, no false precision
- Also added small positive notes (+0.5) to balance negativity bias

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
