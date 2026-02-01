# Project Continuity

## Right Now

**CAGRA GPU search working!**

Branch: `feat/cuvs-gpu-search`

### Just completed:
- Fixed `itopk_size` param (was 64, needed 128+ for our k=100)
- CAGRA builds index in ~1.2s, search works
- ort embedding still on CPU (missing libonnxruntime_providers_shared.so)

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

### This session also:
- Added completion checklist to CLAUDE.md
- Fixed notes immediate indexing (cqs_add_note)
- Fixed cqs watch to monitor notes.toml
- Updated hunches.toml → notes.toml throughout

### Next:
- Benchmark CAGRA vs HNSW
- Fix ort CUDA provider (optional - embeddings work on CPU)
- PR to main

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- Notes: unified memory with sentiment, indexed by cqs

## Parked

- Curator agent, fleet coordination
- Republish to crates.io
