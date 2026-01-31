# cqs - Project Continuity

Updated: 2026-01-31T06:15Z

## Current State

**Phase 1 MVP complete. All major features working.**

- All 6 modules implemented (~1900 lines)
- 21 tests passing (13 parser, 8 store)
- End-to-end pipeline verified: init -> index -> search
- Published to crates.io as `cqs` v0.1.0
- GitHub repo public at github.com/jamie8johnson/cqs
- Index has 121 chunks (100 Rust, 6 Python, 6 TypeScript, 5 Go, 4 JavaScript)
- CLI search works with excellent semantic matching
- MCP integration tested and working

## This Session

### Fixed: Path Pattern Filtering

**Bug:** Glob patterns like `--path "src/*"` returned no results.

**Root cause:** Chunk IDs were stored with absolute paths (`/mnt/c/projects/cq/src/cli.rs`) but glob patterns were relative (`src/*`).

**Fix:** Modified `enumerate_files` to return relative paths, updated `parse_files` to rewrite chunk paths before storage. All filesystem operations (metadata, needs_reindex) now join with root.

Files changed:
- `src/cli.rs`: enumerate_files, parse_files, cmd_index

### Fixed: Invalid Language Error

**Bug:** `--lang invalid` silently defaulted to Rust.

**Fix:** Changed `unwrap_or(Language::Rust)` to proper error handling with context message.

### Added: Scale Warning

Added warning in `cqs stats` (CLI and MCP) when index exceeds 50k chunks. Brute-force O(n) search will be slow at that scale - warns users to split projects or wait for HNSW support.

### Published v0.1.1

Released to crates.io with all fixes from this session.

### Extensive CLI Testing

Verified all functionality:
- All 5 language filters work
- Path patterns work: `src/*`, `tests/*.rs`, `**/*.go`, exact paths
- Combined filters (lang + path) work
- Threshold and limit parameters work
- JSON and no-content output modes work
- Empty query errors correctly
- Invalid language errors correctly
- Semantic matching quality excellent (see README)

## MCP Status

**Working.** Tested via Claude Code after restart.

Tools available:
- `cqs_search` - semantic code search with filters
- `cqs_stats` - index statistics

### Created Eval Suite

50 test cases (10 per language), measuring Recall@5:
- Rust: 9/10 (90%)
- Python: 10/10 (100%)
- TypeScript: 10/10 (100%)
- JavaScript: 10/10 (100%)
- Go: 10/10 (100%)
- **Overall: 49/50 (98%)**

Files created:
- `tests/fixtures/eval_rust.rs` (20+ functions)
- `tests/fixtures/eval_python.py`
- `tests/fixtures/eval_typescript.ts`
- `tests/fixtures/eval_javascript.js`
- `tests/fixtures/eval_go.go`
- `tests/eval_test.rs` (test harness)

Run with: `cargo test eval -- --ignored --nocapture`

### Verified: GPU/CUDA Working

After WSL reboot, GPU acceleration confirmed working:

```
Provider: CUDA (device 0)
Single query: 6-7ms (vs ~20ms CPU)
Batch 50 docs: 0.3ms/doc
```

- RTX A6000, CUDA 13.0 driver, cuDNN 9.18.1
- ort 2.x detects CUDA automatically
- ~450ms warmup (CUDA kernel compilation), then fast
- Created `examples/bench_embed.rs` for benchmarking

Updated README with benchmark table, removed "intermittent" warning for WSL2.
Updated CLAUDE.md with cqs_search usage instructions.

## Next Steps

1. Implement hybrid search (embedding + name match) - Task #3
2. Consider publishing v0.1.2 with eval suite + GPU benchmarks

## Blockers

None.

## Decisions Made

- **Relative paths in index**: Makes indexes portable, fixes glob matching
- **Error on invalid language**: Fail fast, don't silently default
- **MCP project path required**: Working directory unpredictable for MCP servers
- **Scale warning at 50k**: Inform users before search becomes slow
- **Keep threshold at 0.3**: Reasonable default, users can adjust with `-t`
- **Incremental indexing exists**: Already had `needs_reindex` + `prune_missing`
