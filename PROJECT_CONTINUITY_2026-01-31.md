# cqs - Project Continuity

Updated: 2026-01-31T20:40Z

## Current State

**v0.1.8 published. All planned fixes implemented, ready for v0.1.9 release.**

- ~4400 lines across 9 modules (added config.rs)
- 38 tests passing
- All clippy warnings resolved

## This Session - Major Changes

### PR 1: HNSW Filtered Search Fix (CRITICAL)
- Added `Store::search_by_candidate_ids()` in store.rs
- Modified cli.rs to use HNSW candidates instead of falling back to brute-force
- **Impact:** 10-100x faster filtered queries on large indexes

### PR 2: SIMD Cosine Similarity
- Added `simsimd = "6"` dependency
- Updated `cosine_similarity()` to use SIMD-accelerated dot product
- **Impact:** 2-4x faster similarity calculations

### PR 3: UX Improvements
- **Lock file with PID:** Writes PID, detects stale locks, auto-cleanup (cli.rs:357-400)
- **Error messages:** Added hints to all user-facing errors
- **Shell completions:** `cqs completions bash/zsh/fish/powershell`
- **Config file:** `.cqs.toml` (project) and `~/.config/cqs/config.toml` (user)
- Added `libc` and `clap_complete` dependencies

### PR 4: Documentation
- Created `CHANGELOG.md` with all releases v0.1.0-v0.1.8
- Added rustdoc to public API: lib.rs, parser.rs, store.rs, embedder.rs
- 5 doctests now passing

## Files Changed

| File | Changes |
|------|---------|
| Cargo.toml | +simsimd, +clap_complete, +libc |
| src/cli.rs | +128 lines (lock PID, config, completions, error hints) |
| src/config.rs | NEW - 55 lines |
| src/store.rs | +174 lines (search_by_candidate_ids, SIMD cosine, rustdoc) |
| src/lib.rs | +53 lines (rustdoc) |
| src/parser.rs | +57 lines (rustdoc) |
| src/embedder.rs | +27 lines (rustdoc) |
| src/mcp.rs | +5 lines (error hints) |
| CHANGELOG.md | NEW - full version history |
| tests/*.rs | Fixed mut warnings from Store API change |

## Tests Verified

- 38 unit/integration tests passing
- 5 doctests passing
- Clippy clean with -D warnings
- Shell completions generate correctly (bash/zsh/fish)
- Config file loading works
- Stale lock detection works
- CUDA benchmark unchanged (~6ms single, 0.3ms/doc batch)

## Next Steps

1. **Commit changes** - All PRs can be combined into one
2. **Bump version to 0.1.9** - Cargo.toml
3. **Create PR and merge**
4. **Publish to crates.io**
5. **Update README if needed** (CUDA benchmarks still accurate)

## Hunches Resolved This Session

- ort logs to stdout: Already using stderr (main.rs:5-11)
- Model versioning: check_model_version() exists (store.rs:318-335)
- Symlinks: follow_links(false) already set (cli.rs:259)

## Remaining Hunches

- hnsw_rs lifetime forces reload (~1-2ms overhead per search) - library limitation
- HNSW filtered search now optimized but still O(n) for path pattern (could add SQL LIKE)

## Dependencies Added

```toml
simsimd = "6"        # SIMD cosine similarity
clap_complete = "4"  # Shell completions
libc = "0.2"         # Unix PID checking
```
