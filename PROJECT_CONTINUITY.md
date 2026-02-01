# Project Continuity

## Right Now

**Optimizing GPU search performance**

Branch: `feat/cuvs-gpu-search`
PR: #46 (open)

### Key Finding: HNSW beats CAGRA for MCP queries

| Config | 10 queries (8k vectors) |
|--------|------------------------|
| CPU embed + HNSW | **1.3s** (0.13s/query) |
| CPU embed + CAGRA | 13s (1.3s/query) |

**Why:** CAGRA rebuilds from SQLite at startup (~1.5s). HNSW loads from disk (~50ms).

### Optimizations committed:
- `Embedder::new_cpu()` - CPU embedding for single queries (faster than GPU)
- MCP server uses CPU embedding, GPU for batch indexing only
- `CAGRA_THRESHOLD = 5000` - use HNSW for smaller indexes
- Auto-heal symlinks for ort CUDA provider libs

### Testing large index (in progress):
- Indexing rust-lang/rust compiler (35k files, ~350k chunks expected)
- Index at `/tmp/rust-compiler/.cq/index.db`
- Testing if CAGRA wins at scale

### Hardware:
- i9-11900K, 62GB RAM, RTX A6000 (49GB), CUDA 12.0, WSL2

### Build:
```bash
source /home/user001/miniconda3/etc/profile.d/conda.sh
conda activate cuvs
export LD_LIBRARY_PATH=/home/user001/miniconda3/envs/cuvs/lib:$LD_LIBRARY_PATH
cargo build --release --features gpu-search
```

### Binary: `/home/user001/.cargo-target/cq/release/cqs`

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- MCP: CPU embed + HNSW (fast) or CAGRA (large indexes)
- Indexing: GPU batch embedding

## Parked

- Curator agent, fleet coordination
- Republish to crates.io
