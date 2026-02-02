# Project Continuity

## Right Now

**Switched to E5-base-v2 for full CUDA coverage**

Branch: `feat/cuvs-gpu-search`
PR: #46 (open)

### Model Change: nomic → E5-base-v2

Switched from nomic-embed-text-v1.5 to E5-base-v2 because:
- nomic's rotary position embeddings cause 72 Gather ops to fall back to CPU
- E5's 100 Gather ops are all embedding lookups (word/position) - full CUDA coverage expected

**embedder.rs changes:**
```rust
const MODEL_REPO: &str = "intfloat/e5-base-v2";
// Prefixes changed: "passage: " for documents, "query: " for search
// max_length: 512 (was 8192)
```

### Window Parameters (adjusted for E5's 512 limit)

```rust
const MAX_TOKENS_PER_WINDOW: usize = 480;  // Was 2048
const WINDOW_OVERLAP_TOKENS: usize = 64;   // Was 256
```

**Schema v9** - added `parent_id`, `window_idx` columns

### CRITICAL: ort CUDA requires LD_LIBRARY_PATH

ort CUDA libs at `~/.cache/ort.pyke.io/dfbin/.../` need to be in LD_LIBRARY_PATH or CUDA provider silently fails and uses CPU only.

### Build & Run

```bash
source /home/user001/miniconda3/etc/profile.d/conda.sh
conda activate cuvs
export LD_LIBRARY_PATH="/home/user001/.cache/ort.pyke.io/dfbin/x86_64-unknown-linux-gnu/d3c01924b801c77ff17d300b24e6dcd46d378348a921a48d96f115f87074fbb1:/home/user001/miniconda3/envs/cuvs/lib:$LD_LIBRARY_PATH"
cargo build --release --features gpu-search
cd /home/user001/rust && /home/user001/.cargo-target/cq/release/cqs index
```

### Status: E5 + CUDA Working

E5 indexing running on rust-lang/rust (36k files):
- GPU util: 0-70% (bursty but no rotary CPU fallback)
- GPU mem: ~9.6GB allocated
- Only position/shape ops on CPU (fast constant-time lookups)
- DB growing at ~30MB/min
- **Estimated final size: 700MB - 1.5GB** (E5's smaller windows = more chunks)
- Currently at ~385MB, ~50% through

**No rotary_emb ops** - key difference from nomic. E5 uses absolute position embeddings.

### Bug Fixed: Symlink Corruption

`ensure_ort_provider_libs()` was creating circular symlinks when ort cache dir was first in LD_LIBRARY_PATH. Fixed by skipping dirs containing ort_cache path.

### CPU Users Unaffected

- MCP uses CPU embed + HNSW (0.13s/query) - unchanged
- `new_cpu()` constructor for CPU-only systems
- GPU work only accelerates batch indexing

### Hardware

- i9-11900K, 128GB physical / 92GB WSL limit
- RTX A6000 (48GB VRAM), CUDA 12.0/13.0
- WSL2

### Test Repo

`/home/user001/rust` (rust-lang/rust, 36k files)

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- MCP: CPU embed + HNSW (0.13s/query)
- Indexing: Dual GPU+CPU pipeline with windowing + failure requeue

## Parked

- CAGRA persistence (serialize/deserialize) - would fix 1.5s startup
- Curator agent, fleet coordination
- Republish to crates.io (after PR merge)
- Search quality evaluation (decided to skip nomic comparison, spot-check E5 instead)

## Open Questions

- Should we add blake3 checksums for E5 model files?
- MCP query latency with larger index? (estimate ~0.15-0.18s, still fast)
- Try batch_size=64 with E5? (smaller windows = less memory pressure)

## Tools

- **better-dev plugin** installed - use `/fresh-eyes` before commits for quality review

## TODO

- [ ] Try `/fresh-eyes` on next commit, add to CLAUDE.md if useful
