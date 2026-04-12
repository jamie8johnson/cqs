# Persistent Daemon (`cqs serve`)

**Issue:** #912 (PF-1)
**Date:** 2026-04-12
**Status:** Design (reviewed)

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
 cqs impact baz ────┤──────────────>│  Socket listener (nonblock) │
 cqs gather "q" ────┘  JSON req/    │    │                        │
                       resp         │    ▼                        │
                                    │  BatchContext dispatch       │
                                    │    │                        │
                                    │    ├─> Store (read-only)    │
                                    │    ├─> HNSW index (shared)  │
                                    │    ├─> SPLADE index (shared)│
                                    │    └─> Embedder (Arc shared)│
                                    │                             │
                                    │  File watcher loop          │
                                    │  (existing, uses write Store│
                                    │   + shared Arc<Embedder>)   │
                                    └─────────────────────────────┘
```

### Resource sharing (Phase 0 prerequisite)

Watch and BatchContext both need an Embedder (~500MB). Two instances = 1GB.

**Embedder:** Refactor from `OnceCell<Embedder>` to `Arc<Embedder>`. Pass the same Arc into both WatchConfig and BatchContext. The Embedder is already internally synchronized (`session: Mutex<Session>`).

**Store:** BatchContext opens a **separate read-only Store** for queries. Watch keeps the existing write Store for indexing. Read-only Store skips quick_check and uses a smaller connection pool — cheap to keep alongside the write Store.

**HNSW/SPLADE:** BatchContext loads its own copies (same as current batch mode). Future optimization: share via `Arc<RwLock<HnswIndex>>` and `Arc<RwLock<SpladeIndex>>`.

### Protocol

Socket path: `$CQS_DIR/cqs.sock` (Unix domain socket, permissions `0o600`).

Request (one JSON object per line, max 1MB):
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

This is the same format as `cqs batch` stdin/stdout. The `BatchContext` + `batch/handlers/*` dispatch logic is reused verbatim. Output is routed to the socket stream via `write_json_line(&mut socket, ...)` (the function already takes `&mut impl Write`).

### Client mode

In `cli/dispatch.rs`, before the normal dispatch path:

```rust
if let Some(response) = try_daemon_query(&cli) {
    print!("{}", response);
    return Ok(());
}
```

`try_daemon_query`:
1. If `CQS_NO_DAEMON=1`, return `None`
2. If command is not batch-dispatchable (`index`, `watch`, `gc`, `init`), return `None`
3. Check if `$CQS_DIR/cqs.sock` exists
4. Try to connect (non-blocking, timeout from `CQS_DAEMON_TIMEOUT_MS`, default 30s)
5. If connected: serialize CLI args → send → read response → return `Some`
6. If not: return `None` (fall back to CLI)

### Socket listener integration

The watch loop is synchronous (`recv_timeout(100ms)`). The socket listener uses **non-blocking accept** in the timeout branch:

```rust
// In the timeout branch, after file-change processing:
if let Some(ref listener) = socket_listener {
    listener.set_nonblocking(true)?;
    match listener.accept() {
        Ok((stream, _)) => handle_socket_query(&batch_ctx, stream),
        Err(ref e) if e.kind() == WouldBlock => {} // no pending connection
        Err(e) => warn!(error = %e, "Socket accept failed"),
    }
}
```

Long queries (e.g., `cqs gather` ~500ms) block the file event loop. The channel buffers events — they're processed on the next cycle. Acceptable for agent burst patterns (serial queries).

### Concurrency model

Single-threaded. Queries processed sequentially. The Store is not `Send` (block_on runtime is per-Store). For agent burst patterns this is fine — queries are serial within a turn.

If parallel queries become needed later: spawn per-connection tasks with `Arc<Store>` (requires making Store's block_on pattern thread-safe).

### Index freshness

No new mechanism needed:
1. **File watcher** — detects changes, reindexes
2. **DS-W5 inode check** — detects `cqs index --force` DB replacement
3. **DS-9 cache clearing** — Store reopened after each reindex cycle
4. **SPLADE generation** — tracks invalidation
5. **BatchContext mtime check** — detects concurrent index updates

### Startup / shutdown

**Start:**
1. Try bind socket. If `EADDRINUSE`: try connect to check liveness
   - If alive: bail with "daemon already running on this cqs_dir"
   - If stale: remove socket file, rebind
2. Set socket permissions to `0o600`
3. Log `info!(socket = %path, pid = std::process::id(), "Daemon listening")`

**Shutdown (SIGTERM/SIGINT):**
1. Close socket listener
2. Remove socket file (RAII guard)
3. Flush Store
4. Log `info!("Daemon shutting down")`

**Crash recovery:**
- Stale socket file: client detects (connect fails) → falls back to CLI
- Next `watch --serve` start cleans up stale socket (step 1 above)

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
| 0 | Refactor Embedder to `Arc<Embedder>`, share between watch + batch | ~40 | — |
| 1 | `--serve` flag + non-blocking Unix socket accept in watch loop | ~80 | Phase 0 |
| 2 | Request parsing + BatchContext dispatch for socket clients | ~50 | Phase 1 |
| 3 | `try_daemon_query` client in `dispatch.rs` | ~60 | Phase 1 |
| 4 | Socket cleanup on shutdown (RAII guard) + stale detection on start | ~30 | Phase 1 |
| 5 | systemd unit rename + docs | ~10 | Phase 3 |

**Total: ~270 lines of new code.**

## What we reuse (zero new logic)

- `BatchContext` — command dispatch, cache management, staleness detection
- `batch/handlers/*` — all command implementations (search, callers, impact, etc.)
- `batch/commands.rs` — arg parsing for batch-format commands
- `write_json_line` — already takes `&mut impl Write` (route to socket instead of stdout)
- `is_batch_dispatchable()` — already classifies which commands the batch handler supports
- Watch file-change loop — runs alongside socket listener
- DS-W5 inode check — DB replacement detection
- DS-9 cache clearing — post-reindex Store reopen

## Tracing

| Where | Level | Content |
|---|---|---|
| Socket accept | `info_span!("daemon_query")` | command name, peer |
| Request parse | `debug!` | raw command |
| Dispatch complete | `info!` | command, latency_ms |
| Client connect | `debug!` | socket path |
| Client fallback | `debug!` | "daemon unavailable" |
| Daemon start | `info!` | socket path, pid |
| Daemon shutdown | `info!` | "removing socket" |
| Accept error | `warn!` | error |
| Dispatch panic | `error!` | panic payload |

## Error handling

| Case | Response |
|---|---|
| Malformed request JSON | `{"status":"error","message":"invalid JSON: ..."}` |
| Unknown/non-dispatchable command | `{"status":"error","message":"command not supported in daemon mode"}` |
| Query dispatch panics | `catch_unwind` → `{"status":"error","message":"internal error"}`, daemon survives |
| Socket write fails mid-response | `warn!`, drop connection, daemon continues |
| Oversized request (>1MB) | `{"status":"error","message":"request too large"}` |
| Client timeout | Configurable `CQS_DAEMON_TIMEOUT_MS` (default 30s) |
| Socket bind fails (EADDRINUSE) | Check liveness, remove stale or bail |
| Socket permissions | `chmod 0o600` on create |

## Test plan

### Unit tests (no socket needed)
- Request JSON parse: valid, malformed, unknown command, oversized (>1MB)
- Response serialization: ok response, error response
- `is_batch_dispatchable()` returns false for `index`/`watch`/`gc`/`init`
- Socket path derivation from cqs_dir

### Integration tests (tempdir + real socket)
- Start listener → send search query → get valid JSON response
- Send malformed JSON → get error response, daemon survives
- Two sequential queries on same connection
- `CQS_NO_DAEMON=1` → skips socket check, falls back to CLI
- Stale socket file → cleaned up on start
- `SIGTERM` → socket file removed

### Negative / adversarial tests
- Connect when no daemon running → `try_daemon_query` returns `None`, CLI works
- Daemon busy with long query → client waits up to timeout
- Dispatch panics → error response returned, daemon stays alive for next query
- Non-dispatchable command (`cqs index`) → client skips daemon, runs CLI directly

## Risks

| Risk | Mitigation |
|---|---|
| Two Embedder instances (1GB) | Phase 0 — `Arc<Embedder>` shared between watch + batch |
| Long query blocks file events | Channel buffers events; max query time ~500ms (gather BFS) |
| Socket file leaked on crash | Client fallback + stale detection on restart |
| Memory growth over long uptime | Same as current watch — idle cleanup after 5min |
| Store not `Send` across threads | Sequential dispatch; future: `Arc<Store>` if needed |

## Not in scope

- TCP/HTTP server (Unix socket sufficient for local agents)
- gRPC or complex protocol (JSON lines matches existing batch format)
- Multi-tenant / auth (single-user tool)
- Windows named pipes (WSL uses Unix sockets)
- Parallel query dispatch (serial is fine for agent burst patterns)
