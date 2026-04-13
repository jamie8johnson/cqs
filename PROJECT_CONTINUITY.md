# Project Continuity

## Right Now

**Daemon shipped + audit fixes + perf features. 2 PRs in CI. (2026-04-12 16:20 CDT)**

Branches: `feat/shared-runtime` (#929), `feat/query-cache-persist` (#928)

### Session PRs (this session)

| PR | Theme | Status |
|---|---|---|
| #910 | AC-1: SPLADE fusion score preservation | **merged** |
| #911 | Audit P2/P3 mega-batch: 28 findings + 10 tests | **merged** |
| #926 | Daemon: `cqs watch --serve` (3ms queries) | **merged** |
| #927 | Daemon follow-up: arg translation + tests + systemd | **merged** |
| #928 | Persistent query embedding cache (#913) | CI |
| #929 | Shared tokio runtime for Store + Cache (#915) + CQ-4 audit fix | CI |

### Daemon is live

`cqs watch --serve` runs via systemd. Socket at `$XDG_RUNTIME_DIR/cqs-{hash}.sock`. Graph queries (callers, callees, impact) in 3-19ms. Search ~500ms warm. Query cache saves ~500ms on repeated queries.

Key implementation details:
- Socket on native Linux fs (WSL 9P doesn't support AF_UNIX)
- Dedicated query thread with own BatchContext (not shared with watch)
- `daemon_socket_path()` shared helper hashes cqs_dir for per-project sockets
- Client in `dispatch.rs` strips global CLI flags before forwarding to batch parser
- RAII guard cleans socket on shutdown, stale detection on startup

### Correctness audit (post-implementation)

Audited all changes since last full audit. Found 1 bug:
- **CQ-4 incomplete persist fallback**: `load_all_sparse_vectors` failure fell back to delta-only persist → silent data loss. Fixed: skip persist entirely on failure.

### What's next

- **SPLADE re-eval** — AC-1 fix means alpha knob now functional. Need fresh numbers.
- **Remaining audit items** (~22): API hygiene (API-2/3/4/5/6/7/13), extensibility (EXT-10/12), happy-path test gaps
- **Daemon optimization**: client-side arg translation handles `=` form, warm embedder on daemon start

## Open Issues
- #909, #912–#925, #856, #717, #389, #255, #106, #63
- #912 (daemon) has plan doc, implementation shipped in #926
- #913 (query cache) in PR #928
- #915 (shared runtime) in PR #929

## Architecture
- Version: 1.22.0
- Schema: v20 (v19 FK CASCADE, v20 AFTER DELETE trigger on chunks)
- Tests: 1361 lib + ~13 ignored
- Daemon: `cqs watch --serve` → Unix socket → BatchContext dispatch (3-19ms)
- Query cache: `~/.cache/cqs/query_cache.db` (disk-backed, 7-day eviction)
- Store::clear_caches() replaces drop+reopen in watch
- Batch/chat opens read-only store
- Integrity check opt-in via CQS_INTEGRITY_CHECK=1
- New env vars: CQS_BUSY_TIMEOUT_MS, CQS_IDLE_TIMEOUT_SECS, CQS_MAX_CONNECTIONS, CQS_MMAP_SIZE, CQS_SPLADE_MAX_CHARS, CQS_MAX_QUERY_BYTES, CQS_HNSW_BATCH_SIZE, CQS_INTEGRITY_CHECK
