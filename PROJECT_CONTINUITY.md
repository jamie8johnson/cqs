# Project Continuity

## Right Now

**HNSW wired into search. CAGRA next.**

Branch: `feat/cuvs-gpu-search`

### Done this session:
- **Fixed incomplete HNSW integration** (was built but never used for search)
  - `search_filtered_with_index()` and `search_unified_with_index()` in store.rs
  - CLI loads HNSW at query time, passes to search
  - MCP loads HNSW at startup, keeps in memory
  - `search_by_candidate_ids()` now has callers (was dead code)
- **Fixed hnsw_rs lifetime issue properly** (no leak)
  - `LoadedHnsw` struct with `ManuallyDrop` for controlled drop order
  - 5 unsafe blocks, all localized and documented
  - Memory freed when HnswIndex dropped

### cuVS Environment Setup (for future use):
```bash
source $HOME/miniconda3/etc/profile.d/conda.sh
conda activate cuvs
export CMAKE_PREFIX_PATH=$CONDA_PREFIX:$CMAKE_PREFIX_PATH
export LD_LIBRARY_PATH=$CONDA_PREFIX/lib:$LD_LIBRARY_PATH
export LIBCLANG_PATH=$CONDA_PREFIX/lib
cargo build --features gpu-search
```

### Next:
- Phase 2: VectorIndex trait + CAGRA implementation
  - `src/index.rs` - trait definition
  - `src/cagra.rs` - cuVS CAGRA backend (behind `gpu-search` feature)
  - Runtime GPU detection and fallback

### Crate Status:
- **Deleted from crates.io** - incomplete HNSW work shipped as "done"
- Repo made private until quality is solid

## Key Architecture

- **769-dim embeddings**: 768 from nomic-embed-text + 1 sentiment
- **Notes**: unified memory (text + sentiment + mentions)
- **Sentiment**: -1.0 to +1.0, baked into similarity search
- **HNSW**: O(log n) search, CPU-based, now actually wired in

## Parked

- Curator agent architecture (design done)
- Fleet coordination (Git append-only model)
- Republish to crates.io (after CAGRA done)
