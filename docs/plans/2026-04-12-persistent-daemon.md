# Persistent Daemon (`cqs serve`)

**Issue:** #912 (PF-1)
**Date:** 2026-04-12
**Status:** Design

## Problem

Every `cqs` CLI invocation pays ~2-3s startup:
- tokio runtime: ~15ms
- ONNX model load: ~500ms
- HNSW index load: ~200ms (24k vectors)
- SPLADE index load: ~100ms (from disk) or ~45s (SQLite rebuild)
- Store open + quick_check: ~40ms (SSD) or ~40s (WSL /mnt/c)

Agents burst 5-20 queries per turn. At 2s each, that's 10-40s of pure startup overhead. The daemon eliminates this entirely — queries hit warm state in < 50ms.

## Design: extend `cqs watch` with a query socket

### Why not a separate daemon?

`cqs watch` already runs 24/7 via systemd with:
- Store (open, re-opened each cycle for cache clearing)
- HNSW index (loaded, incrementally updated)
- Embedder (lazy-loaded on first file change)
- File watcher loop

Adding a socket listener to watch means one process, one set of resources, one systemd unit. No coordination protocol between watcher and server.

### Architecture

```
                                    ┌─────────────────────────────┐
 cqs search "foo" ──┐               │      cqs watch+serve        │
 cqs callers bar ───┤  Unix socket  │                             │
 cqs impact baz ────┤──────────────>│  Socket listener (async)    │
 cqs gather "q" ────┘  JSON req/    │    │                        │
                       resp         │    ▼                        │
                                    │  BatchContext dispatch       │
                                    │    │                        │
                                    │    ├─> Store (shared)       │
                                    │    ├─> HNSW index (shared)  │
                                    │    ├─> SPLADE index (shared)│
                                    │    └─> Embedder (shared)    │
                                    │                             │
                                    │  File watcher loop          │
                                    │  (existing, unchanged)      │
                                    └─────────────────────────────┘
```

### Protocol

Socket path: `$CQS_DIR/cqs.sock` (Unix domain socket).

Request (one JSON object per line):
```json
{"command": "search", "args": ["query text", "-n", "5", "--json"]}
```

Response (one JSON object per line):
```json
{"status": "ok", "output": "...serialized JSON output..."}
```

Error:
```json
{"status": "error", "message": "..."}
```

This is the same format as `cqs batch` stdin/stdout. The `BatchContext` + `batch/handlers/*` dispatch logic is reused verbatim.

### Client mode

In `cli/dispatch.rs`, before the normal dispatch path:

```rust
if let Some(response) = try_daemon_query(&cli) {
    // Daemon handled it — print response and exit
    print!("{}", response);
    return Ok(());
}
// Fall through to normal CLI path
```

`try_daemon_query`:
1. Check if `$CQS_DIR/cqs.sock` exists
2. Try to connect (non-blocking, 100ms timeout)
3. If connected: serialize CLI args → send → read response → return `Some`
4. If not: return `None` (fall back to CLI)

Env var `CQS_NO_DAEMON=1` skips the check entirely.

### Concurrency model

Single-threaded async (tokio). The socket listener accepts connections on the same runtime as the file watcher. Queries are processed sequentially — the Store is not `Send` across threads (SQLite pool is thread-safe, but the block_on runtime is per-Store).

For agent burst patterns (serial queries), sequential processing is fine. If parallel queries become needed later, the daemon can spawn per-connection tasks with a shared `Arc<Store>`.

### Index freshness

The daemon holds resources long-term. Index freshness is handled by:

1. **File watcher** — already detects changes and reindexes
2. **DS-W5 inode check** — already detects `cqs index --force` DB replacement
3. **DS-9 cache clearing** — Store reopened after each reindex cycle
4. **SPLADE generation** — already tracks invalidation

No new freshness mechanism needed.

### Shutdown

- `SIGTERM` / `SIGINT` — clean shutdown (close socket, flush Store)
- Socket file removed on clean exit
- Stale socket detected by client (connect fails → fall back to CLI)

### systemd integration

Rename `cqs-watch.service` to `cqs.service`. The `--serve` flag enables the socket listener:

```ini
[Service]
ExecStart=/home/user001/.cargo/bin/cqs watch --serve
```

Without `--serve`, watch behaves exactly as today (backward compatible).

## Implementation phases

| Phase | What | Lines | Depends on |
|---|---|---|---|
| 1 | `--serve` flag + Unix socket accept loop in `watch.rs` | ~80 | — |
| 2 | Request parsing + BatchContext dispatch for socket clients | ~40 | Phase 1 |
| 3 | `try_daemon_query` client in `dispatch.rs` | ~50 | Phase 1 |
| 4 | Socket cleanup on shutdown (RAII guard) | ~20 | Phase 1 |
| 5 | Embedder sharing (watch lazy-loads, queries need it immediately) | ~30 | Phase 2 |
| 6 | systemd unit rename + docs | ~10 | Phase 3 |

**Total: ~230 lines of new code.**

## What we reuse (zero new logic)

- `BatchContext` — command dispatch, cache management, staleness detection
- `batch/handlers/*` — all command implementations (search, callers, impact, etc.)
- `batch/commands.rs` — arg parsing for batch-format commands
- Watch file-change loop — runs alongside socket listener
- DS-W5 inode check — DB replacement detection
- DS-9 cache clearing — post-reindex Store reopen

## Risks

| Risk | Mitigation |
|---|---|
| Store not `Send` — can't share across threads | Sequential dispatch on main thread; async yields between queries |
| Embedder lazy-load blocks first query ~500ms | Pre-load on daemon start if `CQS_SPLADE_MODEL` is set |
| Socket file leaked on crash | Client detects stale socket (connect fails); `watch --serve` cleans up on start |
| Memory growth over long uptime | Same as current `cqs watch` — already handles idle cleanup after 5min |

## Not in scope

- TCP/HTTP server (Unix socket is sufficient for local agents)
- gRPC or complex protocol (JSON lines matches existing batch format)
- Multi-tenant / auth (single-user tool)
- Windows named pipes (WSL uses Unix sockets)
