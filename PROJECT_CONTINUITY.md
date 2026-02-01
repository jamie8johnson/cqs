# Project Continuity

## Right Now

**CAGRA GPU search working!**

Branch: `feat/cuvs-gpu-search`

### Benchmark Results (474 vectors):

| Config | Time | Notes |
|--------|------|-------|
| HNSW (CPU) | ~1.0s | Fastest - index persisted |
| CAGRA + CPU embed | ~2.3s | CAGRA rebuild each query |
| CAGRA + CUDA embed | ~2.7s | GPU context overhead |

HNSW wins for small indexes. CAGRA shines at scale (10k+) or when index stays in memory (MCP server).

### ort CUDA provider:
- Fix: add `~/.cache/ort.pyke.io/dfbin/.../` to LD_LIBRARY_PATH
- Not worth it for single queries (slower than CPU due to setup)
- Documented in notes.toml

### Hardware:
- i9-11900K, 62GB RAM, RTX A6000 (49GB), CUDA 12.0, WSL2

### Build command:
```bash
source /home/user001/miniconda3/etc/profile.d/conda.sh
conda activate cuvs
export LD_LIBRARY_PATH=/home/user001/miniconda3/envs/cuvs/lib:$LD_LIBRARY_PATH
cargo build --release --features gpu-search
```

### Binary location:
`/home/user001/.cargo-target/cq/release/cqs`

### This session:
- Benchmarked CAGRA vs HNSW
- Investigated ort CUDA (not worth it for single queries)
- Security review passed (no issues)
- Added completion checklist to CLAUDE.md
- Fixed notes immediate indexing (cqs_add_note)
- Fixed cqs watch to monitor notes.toml

### Next:
- PR to main

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- Notes: unified memory with sentiment, indexed by cqs

## Parked

- Curator agent, fleet coordination
- Republish to crates.io
