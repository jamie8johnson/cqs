# Project Continuity

## Right Now

**Phase 2 complete: VectorIndex trait + CAGRA implementation**

Branch: `feat/cuvs-gpu-search`

### Done this session:
- **VectorIndex trait** (`src/index.rs`)
  - Abstracts over HNSW and CAGRA
  - `search()`, `len()`, `is_empty()`
  - `Send + Sync` for async contexts
- **Implemented VectorIndex for HnswIndex** (`src/hnsw.rs`)
- **CAGRA GPU implementation** (`src/cagra.rs`, behind `gpu-search` feature)
  - Builds from SQLite embeddings at runtime (no persistence)
  - Interior mutability with `Mutex<Option<Index>>` for consuming `search()` API
  - `build_from_store()` helper for easy initialization
- **Updated CLI and MCP** to use trait object `Box<dyn VectorIndex>`
  - Runtime selection: CAGRA (GPU) > HNSW (CPU) > brute-force
  - Automatic fallback if GPU unavailable

### Previous session:
- Fixed incomplete HNSW integration (was built but never used for search)
- Fixed hnsw_rs lifetime issue properly (no leak)

### cuVS Environment Setup:
```bash
source $HOME/miniconda3/etc/profile.d/conda.sh
conda activate cuvs
export CMAKE_PREFIX_PATH=$CONDA_PREFIX:$CMAKE_PREFIX_PATH
export LD_LIBRARY_PATH=$CONDA_PREFIX/lib:$LD_LIBRARY_PATH
export LIBCLANG_PATH=$CONDA_PREFIX/lib
cargo build --features gpu-search
```

### Next:
- Test with actual GPU (requires cuVS installed)
- Benchmark CAGRA vs HNSW performance
- Consider adding `--gpu` CLI flag to force CAGRA

### Crate Status:
- **Deleted from crates.io** - incomplete HNSW work shipped as "done"
- Repo made private until quality is solid

## Key Architecture

- **769-dim embeddings**: 768 from nomic-embed-text + 1 sentiment
- **Notes**: unified memory (text + sentiment + mentions)
- **Sentiment**: -1.0 to +1.0, baked into similarity search
- **VectorIndex trait**: abstraction over HNSW (CPU) and CAGRA (GPU)
- **HNSW**: O(log n) search, CPU-based, persisted to disk
- **CAGRA**: O(log n) search, GPU-accelerated, rebuilt at runtime

## Parked

- Curator agent architecture (design done)
- Fleet coordination (Git append-only model)
- Republish to crates.io (after GPU testing)
