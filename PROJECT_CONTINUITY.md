# Project Continuity

## Right Now

**Waiting on:**
- `cargo publish` - crates.io cooldown ends 2026-02-02 05:18 UTC (run `cargo publish`)
- CodeQL scan - verify suppression comment works (runs daily ~04:44 UTC)
- awesome-mcp-servers PR #1783 - in queue (91 PRs, position 78)

**1.0 progress:**
- Schema v9 stable since 2026-02-01 (need 1 week = Feb 8)
- Used on 2+ codebases (cqs + rust-lang/rust)
- Zero open PRs/issues

**Last session (2026-02-02):**
- Published v0.1.17 to GitHub
- Merged 5 dependabot PRs (dirs, insta, rand 0.9, tower, notify)
- Added `--bind` flag with `--dangerously-allow-network-bind` safety
- CodeQL suppression for allocation size alert
- Simplified README claude block

Pronunciation: cqs = "seeks" (it seeks code semantically).

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- E5-base-v2 model with "passage: " / "query: " prefixes
- Schema v9 with windowing (parent_id, window_idx)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- MCP: hybrid startup (HNSW 30ms, CAGRA upgrades in background)

## Build & Run

```bash
conda activate cuvs  # LD_LIBRARY_PATH set automatically via conda env vars
cargo build --release --features gpu-search
```

## Parked

- CAGRA persistence (serialize/deserialize) - hybrid startup approach used instead
- API key auth for HTTP transport (for network exposure use cases)
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
