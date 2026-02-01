# Project Continuity

## Right Now

**Audit complete. Found and fixed incomplete work.**

Branch: `feat/cuvs-gpu-search`

### Fixed this session:
- **`cqs_add_note` didn't embed immediately** - Notes written to TOML but not indexed until `cqs index`. Now embeds on add.
- **`cqs watch` ignored notes.toml** - Only watched code files. Now monitors `docs/notes.toml` too.
- **CAGRA ndarray version conflict** - ort needs 0.17, cuVS needs 0.15. Added separate `ndarray_015` dep.

### Previous session:
- VectorIndex trait + CAGRA GPU implementation
- Fixed HNSW integration (was built but never used for search)

### Audit results:
- No other dead code found (cargo warnings clean)
- No TODOs/FIXMEs in codebase
- All public functions have callers
- RRF hybrid search is wired in and working

### cuVS Environment Setup:
```bash
source /home/user001/miniconda3/etc/profile.d/conda.sh
conda activate cuvs
export CMAKE_PREFIX_PATH=$CONDA_PREFIX:$CMAKE_PREFIX_PATH
export LD_LIBRARY_PATH=$CONDA_PREFIX/lib:$LD_LIBRARY_PATH
cargo build --features gpu-search
```

### Next:
- Test CAGRA with actual GPU workload
- Consider PR to merge feat/cuvs-gpu-search to main

### Crate Status:
- **Deleted from crates.io** - incomplete work shipped as "done"
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
- Republish to crates.io (after more testing)
