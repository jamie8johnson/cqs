# cqs - Project Continuity

Updated: 2026-01-31T21:00Z

## Current State

**v0.1.9 RELEASED - All planned fixes shipped.**

- Published to crates.io: `cargo install cqs`
- GitHub release: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.9
- ~4400 lines across 9 modules
- 38 tests passing, 5 doctests, clippy clean

## This Session - v0.1.9 Release

### PR #16: Performance and UX improvements (MERGED)

**Performance:**
- HNSW-guided filtered search (10-100x faster)
- SIMD cosine similarity via simsimd (2-4x faster)

**UX:**
- Shell completions (bash/zsh/fish/powershell)
- Config file support (.cqs.toml)
- Lock file with PID for stale detection
- Error messages with actionable hints

**Documentation:**
- CHANGELOG.md with full version history
- Rustdoc for public API

### PR #17: Cargo.lock fix (MERGED)

- Updated Cargo.lock version to 0.1.9 (was missed in PR #16)

## Files Changed

| File | Changes |
|------|---------|
| Cargo.toml | +simsimd, +clap_complete, +libc, version=0.1.9 |
| src/cli.rs | +128 lines (lock PID, config, completions, error hints) |
| src/config.rs | NEW - 55 lines |
| src/store.rs | +174 lines (search_by_candidate_ids, SIMD cosine, rustdoc) |
| src/lib.rs | +53 lines (rustdoc) |
| src/parser.rs | +57 lines (rustdoc) |
| src/embedder.rs | +27 lines (rustdoc) |
| src/mcp.rs | +5 lines (error hints) |
| CHANGELOG.md | NEW - full version history |
| tests/*.rs | Fixed mut warnings from Store API change |

## Next Steps

No immediate work required. Possible future improvements:

1. **HNSW filtered search optimization** - Currently falls back to O(n) for path patterns
2. **More languages** - C, C++, Java, Ruby
3. **VS Code extension**
4. **Index sharing** - Team sync for large codebases

## Hunches Resolved

- ort logs to stdout: Already using stderr (main.rs:5-11)
- Model versioning: check_model_version() exists (store.rs:318-335)
- Symlinks: follow_links(false) already set (cli.rs:259)

## Remaining Hunches

- hnsw_rs lifetime forces reload (~1-2ms overhead per search) - library limitation
- HNSW filtered search uses candidates but path_pattern filter still O(n) scan

## Dependencies

```toml
simsimd = "6"        # SIMD cosine similarity
clap_complete = "4"  # Shell completions
libc = "0.2"         # Unix PID checking
```
