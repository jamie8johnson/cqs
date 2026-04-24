# Audit Fix Prompts — v1.29.0

Source triage: `docs/audit-triage.md` (P1 + P2 only)

Generated: 2026-04-23

---

## P1

### SEC-1: `cqs serve` accepts any `Host` header (DNS-rebinding)

**File**: `/mnt/c/Projects/cqs/src/serve/mod.rs`

**Current code** (lines 55-114, re-read at generation time — matches triage):
```rust
pub fn run_server(store: Store<ReadOnly>, bind_addr: SocketAddr, quiet: bool) -> Result<()> {
    let _span = tracing::info_span!("serve", addr = %bind_addr).entered();

    let state = AppState {
        store: Arc::new(store),
    };
    let app = build_router(state);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime for cqs serve")?;

    runtime.block_on(async move {
        let listener = TcpListener::bind(bind_addr)
            .await
            .with_context(|| format!("Failed to bind {bind_addr}"))?;
        let actual = listener
            .local_addr()
            .with_context(|| format!("Failed to read local_addr after bind {bind_addr}"))?;

        if !quiet {
            println!("cqs serve listening on http://{actual}");
            println!("press Ctrl-C to stop");
        }
        tracing::info!(addr = %actual, "cqs serve started");

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("axum server failed")?;

        tracing::info!("cqs serve shut down cleanly");
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

/// Build the axum router. Public-in-crate so integration tests can
/// exercise the full handler tree against an in-memory store without
/// binding a TCP port.
pub(crate) fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/api/stats", get(handlers::stats))
        .route("/api/graph", get(handlers::graph))
        .route("/api/chunk/{id}", get(handlers::chunk_detail))
        .route("/api/hierarchy/{id}", get(handlers::hierarchy))
        .route("/api/embed/2d", get(handlers::cluster_2d))
        .route("/api/search", get(handlers::search))
        .route("/", get(assets::index_html))
        .route("/static/{*path}", get(assets::static_asset))
        .with_state(state)
        // Gzip every response axum sends. The graph + cluster JSON
        // payloads compress ~5-10× (1-2 MB → 150-300 KB on the cqs
        // corpus); vendor JS bundles compress ~3×. Negligible CPU on
        // the server side, big win on parse/transfer time at the browser.
        .layer(CompressionLayer::new())
}
```

**Fix**: Add a Host-header allowlist middleware seeded from `bind_addr`. Accept only loopback hostnames plus the exact `host:port` the server is bound to. DNS-rebinding attacks send `Host: attacker.com` to `127.0.0.1:8080` from a victim's browser; rejecting any Host that isn't on the allowlist closes the class.

Replace `run_server` and `build_router`, and add two helpers:

```rust
pub fn run_server(store: Store<ReadOnly>, bind_addr: SocketAddr, quiet: bool) -> Result<()> {
    let _span = tracing::info_span!("serve", addr = %bind_addr).entered();

    let state = AppState {
        store: Arc::new(store),
    };
    let allowed_hosts = allowed_host_set(&bind_addr);
    let app = build_router(state, allowed_hosts);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime for cqs serve")?;

    runtime.block_on(async move {
        let listener = TcpListener::bind(bind_addr)
            .await
            .with_context(|| format!("Failed to bind {bind_addr}"))?;
        let actual = listener
            .local_addr()
            .with_context(|| format!("Failed to read local_addr after bind {bind_addr}"))?;

        if !quiet {
            println!("cqs serve listening on http://{actual}");
            println!("press Ctrl-C to stop");
        }
        tracing::info!(addr = %actual, "cqs serve started");

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("axum server failed")?;

        tracing::info!("cqs serve shut down cleanly");
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

/// Build the allowed-Host set for DNS-rebinding protection. Accepts:
/// - `localhost` / `127.0.0.1` / `[::1]` (with and without port)
/// - The exact `host:port` the server is bound to
/// - The bind IP on its own (supports explicit LAN binds like `192.168.x.x:8080`)
/// Any other `Host` header is rejected with HTTP 403.
fn allowed_host_set(bind_addr: &SocketAddr) -> std::sync::Arc<std::collections::HashSet<String>> {
    let port = bind_addr.port();
    let mut set = std::collections::HashSet::new();
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        set.insert(host.to_string());
        set.insert(format!("{host}:{port}"));
    }
    set.insert(bind_addr.to_string());
    set.insert(bind_addr.ip().to_string());
    std::sync::Arc::new(set)
}

/// axum middleware: reject requests whose `Host` header isn't on the allowlist.
/// SEC-1: closes the DNS-rebinding class (attacker page sends
/// `Host: evil.com` to `127.0.0.1:8080` — we refuse to serve it).
async fn enforce_host_allowlist(
    axum::extract::State(allowed): axum::extract::State<
        std::sync::Arc<std::collections::HashSet<String>>,
    >,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if allowed.contains(host) {
        Ok(next.run(req).await)
    } else {
        tracing::warn!(host = %host, "serve: rejected request with disallowed Host header");
        Err(axum::http::StatusCode::FORBIDDEN)
    }
}

/// Build the axum router. Public-in-crate so integration tests can
/// exercise the full handler tree against an in-memory store without
/// binding a TCP port.
pub(crate) fn build_router(
    state: AppState,
    allowed_hosts: std::sync::Arc<std::collections::HashSet<String>>,
) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/api/stats", get(handlers::stats))
        .route("/api/graph", get(handlers::graph))
        .route("/api/chunk/{id}", get(handlers::chunk_detail))
        .route("/api/hierarchy/{id}", get(handlers::hierarchy))
        .route("/api/embed/2d", get(handlers::cluster_2d))
        .route("/api/search", get(handlers::search))
        .route("/", get(assets::index_html))
        .route("/static/{*path}", get(assets::static_asset))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            allowed_hosts,
            enforce_host_allowlist,
        ))
        // Gzip every response axum sends. The graph + cluster JSON
        // payloads compress ~5-10× (1-2 MB → 150-300 KB on the cqs
        // corpus); vendor JS bundles compress ~3×. Negligible CPU on
        // the server side, big win on parse/transfer time at the browser.
        .layer(CompressionLayer::new())
}
```

Update every `build_router(state)` call in `src/serve/tests/` (and anywhere else) to pass the allowlist arg — easiest via a small test helper that wraps `build_router` with an allowlist built from a synthetic `"127.0.0.1:0".parse::<SocketAddr>().unwrap()`.

**Why**: Axum has no built-in Host validation; without it, a malicious DNS-rebinding page can make the victim's browser fetch `http://127.0.0.1:8080/api/search?q=...` from an attacker origin and exfiltrate the whole corpus. Allowlisting loopback + the actual bind host defangs the class without breaking local-only use.

**Verify**: `cargo build --features gpu-index,serve` and `cargo test --features gpu-index,serve serve::`. Manual: `cqs serve &` then `curl -H 'Host: evil.com' http://127.0.0.1:8080/api/stats` must return 403; `curl -H 'Host: localhost:8080' http://127.0.0.1:8080/api/stats` must return 200.

---

### SEC-2: XSS via unescaped error body in hierarchy-3d / cluster-3d

**File**: `/mnt/c/Projects/cqs/src/serve/assets/views/hierarchy-3d.js`

**Current code** (lines 103-114):
```javascript
      try {
        const resp = await fetch(apiUrl);
        if (!resp.ok) {
          const body = await resp.text();
          this.container.innerHTML = `<div class="error" style="margin:24px">hierarchy HTTP ${resp.status}: ${body.slice(0, 200)}</div>`;
          return null;
        }
        return await resp.json();
      } catch (e) {
        this.container.innerHTML = `<div class="error" style="margin:24px">hierarchy fetch error: ${e.message}</div>`;
        return null;
      }
```

Plus the earlier `init` error paths in the same file at lines 57-64 (the `3D bundle failed to load: ${e.message}` interpolation is the same class).

**Fix**: Add a local `escapeHtml` helper near the top of the IIFE (after `"use strict";` on line 15, before the `TYPE_COLORS` const), because the one in `app.js` is scoped inside a separate IIFE and not reachable:

```javascript
  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#039;");
  }
```

Then wrap every server-derived interpolation inside an `innerHTML` template literal:
- Line 64 (`3D bundle failed to load`): `${e.message}` → `${escapeHtml(e.message)}`
- Line 107 (hierarchy HTTP error): `${body.slice(0, 200)}` → `${escapeHtml(body.slice(0, 200))}`
- Line 112 (hierarchy fetch error): `${e.message}` → `${escapeHtml(e.message)}`

Leave pure number interpolations (`${resp.status}`) alone.

**File**: `/mnt/c/Projects/cqs/src/serve/assets/views/cluster-3d.js`

**Current code** (lines 84-128):
```javascript
      if (typeof window.cqsEnsureThreeBundle === "function") {
        try {
          await window.cqsEnsureThreeBundle();
        } catch (e) {
          container.innerHTML = `<div class="error" style="margin:24px">3D bundle failed to load: ${e.message}</div>`;
          throw e;
        }
      }

      container.innerHTML = "";
      if (typeof ForceGraph3D === "undefined") {
        container.innerHTML =
          '<div class="error" style="margin:24px">3D renderer not loaded — check that ' +
          "<code>/static/vendor/three.min.js</code> and " +
          "<code>/static/vendor/3d-force-graph.min.js</code> served correctly.</div>";
        throw new Error("ForceGraph3D global not present");
      }
    },

    /// Router calls this before render() because cluster has its own data
    /// source (UMAP coords, not the call-graph payload).
    async loadData(context) {
      const url = context.url;
      const maxNodes = context.maxNodes || 1500;
      const params = new URLSearchParams({ max_nodes: String(maxNodes) });
      this.colorMode = url.searchParams.get("color") === "language" ? "language" : "type";

      try {
        const resp = await fetch(`/api/embed/2d?${params}`);
        if (!resp.ok) {
          const body = await resp.text();
          this.container.innerHTML = `<div class="error" style="margin:24px">cluster HTTP ${resp.status}: ${body.slice(0, 200)}</div>`;
          return null;
        }
        const data = await resp.json();
        if (data.nodes.length === 0) {
          this.container.innerHTML =
            `<div class="error" style="margin:24px">No UMAP coordinates in this index ` +
            `(${data.skipped.toLocaleString()} chunks have no projection). ` +
            `Run <code>cqs index --umap</code> from the project root, then refresh.</div>`;
          return null;
        }
        return data;
      } catch (e) {
        this.container.innerHTML = `<div class="error" style="margin:24px">cluster fetch error: ${e.message}</div>`;
        return null;
      }
    },
```

**Fix**: Same as above — add the `escapeHtml` helper at the top of the IIFE (after `"use strict";` on line 17). Then wrap:
- Line 87: `${e.message}` → `${escapeHtml(e.message)}`
- Line 114: `${body.slice(0, 200)}` → `${escapeHtml(body.slice(0, 200))}`
- Line 127: `${e.message}` → `${escapeHtml(e.message)}`

`data.skipped.toLocaleString()` is safe (u64 serialized by server, formatter only emits digits/separators) — leave as-is.

**Why**: `body.slice(0, 200)` and `e.message` can reflect attacker-controlled strings (e.g. a chunk-id embedded in a shared URL that echoes into the error path, or browser-shaped URL fragments). Pasting into `innerHTML` unescaped allows script injection into the same origin as `/api/*`, which gives full corpus read. Standard five-char escape closes the class.

**Verify**: `cargo build --features gpu-index,serve`. Manually: `cqs serve`, visit a URL like `/#view=hierarchy-3d&root=<img src=x onerror=alert(1)>` — expect literal text, no `alert`.

---

### SEC-3: `build_graph` / `build_cluster` unbounded SQL fetches (DoS)

**File**: `/mnt/c/Projects/cqs/src/serve/data.rs`

**Current code** — three places:

Uncapped node SELECT, lines 219-237:
```rust
        let (node_sql, want_n_callers_col) = if max_nodes.is_some() {
            (
                "SELECT c.id, c.name, c.chunk_type, c.language, c.origin, \
                        c.line_start, c.line_end, \
                        COALESCE((SELECT COUNT(*) FROM function_calls fc \
                                  WHERE fc.callee_name = c.name), 0) AS n_callers_global \
                 FROM chunks c \
                 WHERE 1=1"
                    .to_string(),
                true,
            )
        } else {
            (
                "SELECT id, name, chunk_type, language, origin, line_start, line_end \
                 FROM chunks WHERE 1=1"
                    .to_string(),
                false,
            )
        };
```

Uncapped edge SELECT, lines 318-350 (the `else` arm of `let edge_rows = if max_nodes.is_some() { ... } else { ... }`):
```rust
        let edge_rows = if max_nodes.is_some() {
            let mut name_set: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for node in nodes_by_id.values() {
                name_set.insert(node.name.as_str());
            }
            if name_set.is_empty() {
                Vec::new()
            } else {
                let names: Vec<&str> = name_set.into_iter().collect();
                let placeholders = vec!["?"; names.len()].join(",");
                let edge_sql = format!(
                    "SELECT fc.file, fc.caller_name, fc.callee_name \
                     FROM function_calls fc \
                     WHERE fc.callee_name IN ({placeholders}) \
                        OR fc.caller_name IN ({placeholders})"
                );
                let mut eq = sqlx::query(&edge_sql);
                for n in &names {
                    eq = eq.bind(*n);
                }
                for n in &names {
                    eq = eq.bind(*n);
                }
                eq.fetch_all(&store.pool).await?
            }
        } else {
            sqlx::query(
                "SELECT fc.file, fc.caller_name, fc.callee_name \
                 FROM function_calls fc",
            )
            .fetch_all(&store.pool)
            .await?
        };
```

Uncapped cluster SELECT, lines 831-861:
```rust
    store.rt.block_on(async {
        // Chunks that have coords already projected.
        let rows = sqlx::query(
            "SELECT id, name, chunk_type, language, origin, line_start, line_end, umap_x, umap_y \
             FROM chunks WHERE umap_x IS NOT NULL AND umap_y IS NOT NULL ORDER BY id",
        )
        .fetch_all(&store.pool)
        .await?;

        let skipped_row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE umap_x IS NULL OR umap_y IS NULL")
                .fetch_one(&store.pool)
                .await?;
        let skipped = skipped_row.0.max(0) as u64;

        // Per-chunk caller/callee counts. Same name-based join as
        // build_graph; counts only edges whose endpoints both resolve
        // inside the projected set so the n_callers/n_callees on a node
        // accurately describe what the cluster view shows.
        let mut caller_count: HashMap<String, u32> = HashMap::new();
        let mut callee_count: HashMap<String, u32> = HashMap::new();
        let mut name_to_first_id: HashMap<String, String> = HashMap::new();
        for row in &rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            name_to_first_id.entry(name).or_insert(id);
        }

        let edge_rows = sqlx::query("SELECT caller_name, callee_name FROM function_calls")
            .fetch_all(&store.pool)
            .await?;
```

**Fix**: Add module-level hard caps near the top of `data.rs` (with the existing imports):

```rust
/// SEC-3: absolute ceiling on nodes returned by `/api/graph` even when the
/// client doesn't pass `?max_nodes=N`. Prevents a single unauth request from
/// materialising the full chunks table (millions of rows) in process memory.
const ABS_MAX_GRAPH_NODES: usize = 50_000;

/// SEC-3: absolute ceiling on edges returned by `/api/graph`. function_calls
/// typically has ~10× the rows of chunks, so cap higher but still bound.
const ABS_MAX_GRAPH_EDGES: usize = 500_000;

/// SEC-3: absolute ceiling on nodes returned by `/api/embed/2d`.
const ABS_MAX_CLUSTER_NODES: usize = 50_000;
```

Replace the node SELECT block (current lines 219-237) with a single always-capped path:

```rust
        // SEC-3: always bind an effective cap. When the client omits
        // `?max_nodes`, fall back to ABS_MAX_GRAPH_NODES so a single
        // request can't materialise a million chunks into memory.
        let effective_cap = max_nodes
            .unwrap_or(ABS_MAX_GRAPH_NODES)
            .min(ABS_MAX_GRAPH_NODES);
        let (node_sql, want_n_callers_col) = (
            "SELECT c.id, c.name, c.chunk_type, c.language, c.origin, \
                    c.line_start, c.line_end, \
                    COALESCE((SELECT COUNT(*) FROM function_calls fc \
                              WHERE fc.callee_name = c.name), 0) AS n_callers_global \
             FROM chunks c \
             WHERE 1=1"
                .to_string(),
            true,
        );
```

Then the existing `if let Some(cap) = max_nodes { ... } else { ... }` around lines 257-265 becomes unconditional:

```rust
        // Stable tie-break by id so equal-rank chunks don't reshuffle
        // between requests.
        node_query.push_str(" ORDER BY n_callers_global DESC, c.id ASC LIMIT ?");
        binds.push(effective_cap.to_string());
```

Replace the entire edge-fetch block (lines 318-350) with the always-scoped version:

```rust
        // SEC-3: always use the name-scoped edge fetch, even when the
        // client didn't pass `max_nodes`. The previous uncapped
        // `SELECT fc.*` would return the entire function_calls table
        // (tens of millions of rows on a large monorepo). An extra LIMIT
        // provides a hard ceiling in case the IN-list grows unexpectedly.
        let edge_rows = {
            let mut name_set: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for node in nodes_by_id.values() {
                name_set.insert(node.name.as_str());
            }
            if name_set.is_empty() {
                Vec::new()
            } else {
                let names: Vec<&str> = name_set.into_iter().collect();
                let placeholders = vec!["?"; names.len()].join(",");
                let edge_sql = format!(
                    "SELECT fc.file, fc.caller_name, fc.callee_name \
                     FROM function_calls fc \
                     WHERE fc.callee_name IN ({placeholders}) \
                        OR fc.caller_name IN ({placeholders}) \
                     LIMIT ?"
                );
                let mut eq = sqlx::query(&edge_sql);
                for n in &names {
                    eq = eq.bind(*n);
                }
                for n in &names {
                    eq = eq.bind(*n);
                }
                eq = eq.bind(ABS_MAX_GRAPH_EDGES as i64);
                eq.fetch_all(&store.pool).await?
            }
        };
```

In `build_cluster`, replace the initial row fetch (lines 833-838) with:

```rust
        // SEC-3: cap the initial fetch so an unauth request can't
        // materialise the full chunks table. When the client passes a
        // smaller `max_nodes`, the post-fetch truncate (line 909 range)
        // still applies — this limit is a server-side safety net.
        let effective_cap = max_nodes
            .unwrap_or(ABS_MAX_CLUSTER_NODES)
            .min(ABS_MAX_CLUSTER_NODES);
        let rows = sqlx::query(
            "SELECT id, name, chunk_type, language, origin, line_start, line_end, umap_x, umap_y \
             FROM chunks \
             WHERE umap_x IS NOT NULL AND umap_y IS NOT NULL \
             ORDER BY id \
             LIMIT ?",
        )
        .bind(effective_cap as i64)
        .fetch_all(&store.pool)
        .await?;
```

And cap the cluster edge fetch (line 859-861):

```rust
        let edge_rows = sqlx::query(
            "SELECT caller_name, callee_name FROM function_calls LIMIT ?",
        )
        .bind(ABS_MAX_GRAPH_EDGES as i64)
        .fetch_all(&store.pool)
        .await?;
```

**Why**: Today each request to `/api/graph` or `/api/embed/2d` materialises every `chunks` row (on cqs itself that's ~400 MB of Rust `String`s per request); on a large monorepo it's unbounded. The UI's happy path already passes `max_nodes=1500`, so the hard ceiling only kicks in for misuse / tooling.

**Verify**: `cargo build --features gpu-index,serve` and `cargo test --features gpu-index,serve serve::`. Manual: `curl 'http://127.0.0.1:8080/api/graph'` must return `<= 50_000` nodes regardless of corpus size.

---

### PB-V1.29-2: Watch SPLADE encoder silent no-op on Windows

**File**: `/mnt/c/Projects/cqs/src/cli/watch.rs`

**Current code** (lines 1082-1099):
```rust
    let mut batch: Vec<(String, String)> = Vec::new();
    for file in changed_files {
        let origin = file.display().to_string();
        let chunks = match store.get_chunks_by_origin(&origin) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    origin = %origin,
                    error = %e,
                    "SPLADE encode: failed to fetch chunks for file — skipping"
                );
                continue;
            }
        };
        for chunk in chunks {
            batch.push((chunk.id, chunk.content));
        }
    }
```

**Fix**: Use `cqs::normalize_path` so the origin uses forward slashes — matching the form stored at ingest.

```rust
    let mut batch: Vec<(String, String)> = Vec::new();
    for file in changed_files {
        // PB-V1.29-2: `file.display()` emits Windows backslashes, which
        // never match the forward-slash origins stored at ingest (chunks
        // are upserted via `normalize_path`). Using `.display()` here
        // makes SPLADE encoding a silent no-op on Windows.
        let origin = cqs::normalize_path(file);
        let chunks = match store.get_chunks_by_origin(&origin) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    origin = %origin,
                    error = %e,
                    "SPLADE encode: failed to fetch chunks for file — skipping"
                );
                continue;
            }
        };
        for chunk in chunks {
            batch.push((chunk.id, chunk.content));
        }
    }
```

**Why**: `chunks.origin` is persisted through `normalize_path` which forward-slashes every separator (see `src/lib.rs:348`). Looking up with `file.display()` on Windows produces `src\foo.rs`, which never matches `src/foo.rs` in the table. Net effect: SPLADE sparse vectors silently diverge from dense ones on Windows until the next full `cqs index`.

**Verify**: `cargo build --features gpu-index` and `cargo test --features gpu-index watch::`. Linux-only repos see no observable change; Windows/WSL Windows-path runs now re-encode SPLADE after an edit.

---

### DS2-1: `prune_missing` reads origin list outside write tx (TOCTOU)

**File**: `/mnt/c/Projects/cqs/src/store/chunks/staleness.rs`

**Current code** (lines 69-104, showing the open of the function through the `begin_write` call):
```rust
    pub fn prune_missing(
        &self,
        existing_files: &HashSet<PathBuf>,
        root: &Path,
    ) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("prune_missing", existing = existing_files.len()).entered();
        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&self.pool)
            .await?;

            // AC / CQ-V1.25-4 / CQ-V1.25-6 / PB-V1.25-7: reconcile stored origins
            // against current filesystem state via `origin_exists`. The previous
            // `ends_with` heuristic retained 81% of chunks as orphans whenever a
            // worktree or subdirectory tail-matched a root file name.
            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| !origin_exists(origin, existing_files, root))
                .map(|(origin,)| origin)
                .collect();

            if missing.is_empty() {
                return Ok(0);
            }

            // Batch delete in chunks of 100 (SQLite has ~999 param limit).
            // Single transaction wraps ALL batches — partial prune on crash
            // would leave the index inconsistent with disk.
            const BATCH_SIZE: usize = 100;
            let mut deleted = 0u32;

            let (_guard, mut tx) = self.begin_write().await?;
```

**Fix**: Start the write transaction BEFORE reading origins, and run the SELECT against `&mut *tx` so the read and delete are serialisable as one unit.

```rust
    pub fn prune_missing(
        &self,
        existing_files: &HashSet<PathBuf>,
        root: &Path,
    ) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("prune_missing", existing = existing_files.len()).entered();
        self.rt.block_on(async {
            // DS2-1: acquire the write transaction BEFORE reading origins.
            // Reading outside the tx creates a TOCTOU window where a
            // concurrent upsert adds a chunk for an "existing" file; our
            // stale origin list would then mark it missing and wipe the
            // just-added row on DELETE.
            let (_guard, mut tx) = self.begin_write().await?;

            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&mut *tx)
            .await?;

            // AC / CQ-V1.25-4 / CQ-V1.25-6 / PB-V1.25-7: reconcile stored origins
            // against current filesystem state via `origin_exists`. The previous
            // `ends_with` heuristic retained 81% of chunks as orphans whenever a
            // worktree or subdirectory tail-matched a root file name.
            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| !origin_exists(origin, existing_files, root))
                .map(|(origin,)| origin)
                .collect();

            if missing.is_empty() {
                return Ok(0);
            }

            // Batch delete in chunks of 100 (SQLite has ~999 param limit).
            // Single transaction wraps ALL batches — partial prune on crash
            // would leave the index inconsistent with disk.
            const BATCH_SIZE: usize = 100;
            let mut deleted = 0u32;
```

Then DELETE the duplicate `let (_guard, mut tx) = self.begin_write().await?;` that currently sits on line 102 — the transaction is now established at the top of the block.

**Why**: The existing shape reads origins from the connection pool then opens a write tx. Between those two steps, a concurrent writer (watcher upsert, `cqs index`) can add chunks for paths that are now in `existing_files`; those chunks won't appear in the stale origin list yet are committed. Our delete then targets origins based on a snapshot from before the upsert — harmless in that case, but the inverse race (a delete inside the tx removes rows the read didn't yet see) is also possible for other row states. Putting the read under the write lock forces a consistent view. Same fix class as P2 #32 that hardened `prune_all`.

**Verify**: `cargo build --features gpu-index` and `cargo test --features gpu-index prune_missing` plus `cargo test --features gpu-index store::chunks::staleness::`.

---

### DS2-2: `prune_gitignored` reads origin list outside write tx (same TOCTOU class)

**File**: `/mnt/c/Projects/cqs/src/store/chunks/staleness.rs`

**Current code** (lines 323-374):
```rust
    pub fn prune_gitignored(
        &self,
        matcher: &ignore::gitignore::Gitignore,
        root: &Path,
        max_paths: Option<usize>,
    ) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("prune_gitignored", max_paths = ?max_paths).entered();
        self.rt.block_on(async {
            // Phase 1: collect distinct origins (Rust-side filter, outside tx
            // so the matcher walk doesn't hold the write lock).
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&self.pool)
            .await?;

            let cap = max_paths.unwrap_or(usize::MAX);
            let mut ignored: Vec<String> = Vec::new();
            for (origin,) in rows.into_iter() {
                if ignored.len() >= cap {
                    break;
                }
                let origin_path = PathBuf::from(&origin);
                let absolute = if origin_path.is_absolute() {
                    origin_path
                } else {
                    root.join(&origin_path)
                };
                // `matched_path_or_any_parents` walks up the path's parents
                // so that `.claude/worktrees/agent-x/src/lib.rs` is treated
                // as ignored when `.claude/` is in `.gitignore`. The
                // leaf-only `matched()` would miss this case — same logic
                // as `collect_events` in `cli/watch.rs`.
                if matcher
                    .matched_path_or_any_parents(&absolute, false)
                    .is_ignore()
                {
                    ignored.push(origin);
                }
            }

            if ignored.is_empty() {
                return Ok(0);
            }

            // Phase 2: batched delete in a single transaction. Same shape as
            // `prune_missing` so a partial prune on crash leaves the index
            // consistent with the remaining rows in `chunks`.
            const BATCH_SIZE: usize = 100;
            let mut deleted = 0u32;

            let (_guard, mut tx) = self.begin_write().await?;
```

**Fix**: Open the write tx first, then run the SELECT under it. The matcher walk is pure CPU over an already-materialised Vec — safe to hold the lock across it.

```rust
    pub fn prune_gitignored(
        &self,
        matcher: &ignore::gitignore::Gitignore,
        root: &Path,
        max_paths: Option<usize>,
    ) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("prune_gitignored", max_paths = ?max_paths).entered();
        self.rt.block_on(async {
            // DS2-2: acquire the write transaction BEFORE reading origins.
            // Same TOCTOU fix as DS2-1: a concurrent upsert can land
            // between the SELECT and the DELETE, and our stale origin list
            // would then wipe the just-added row. The matcher walk below
            // is pure CPU over the already-fetched `rows` Vec and is safe
            // under the lock (no additional DB calls).
            let (_guard, mut tx) = self.begin_write().await?;

            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&mut *tx)
            .await?;

            let cap = max_paths.unwrap_or(usize::MAX);
            let mut ignored: Vec<String> = Vec::new();
            for (origin,) in rows.into_iter() {
                if ignored.len() >= cap {
                    break;
                }
                let origin_path = PathBuf::from(&origin);
                let absolute = if origin_path.is_absolute() {
                    origin_path
                } else {
                    root.join(&origin_path)
                };
                // `matched_path_or_any_parents` walks up the path's parents
                // so that `.claude/worktrees/agent-x/src/lib.rs` is treated
                // as ignored when `.claude/` is in `.gitignore`. The
                // leaf-only `matched()` would miss this case — same logic
                // as `collect_events` in `cli/watch.rs`.
                if matcher
                    .matched_path_or_any_parents(&absolute, false)
                    .is_ignore()
                {
                    ignored.push(origin);
                }
            }

            if ignored.is_empty() {
                return Ok(0);
            }

            // Phase 2: batched delete in the SAME transaction started above.
            // Same shape as `prune_missing` so a partial prune on crash
            // leaves the index consistent with the remaining rows in `chunks`.
            const BATCH_SIZE: usize = 100;
            let mut deleted = 0u32;
```

Remove the second `let (_guard, mut tx) = self.begin_write().await?;` currently on line 374 — the tx is already active.

**Why**: Identical TOCTOU class as DS2-1 — the idle-time GC reads a snapshot of origins via the pool, walks the matcher, then opens a write tx to delete. A concurrent upsert landing during the matcher walk (non-trivial for large corpora) creates chunks for paths the matcher will later flag as ignored; the stale origin list then points the DELETE at a just-inserted row. Serialising the read under the write lock closes it.

**Verify**: `cargo build --features gpu-index` and `cargo test --features gpu-index prune_gitignored` plus `cargo test --features gpu-index store::chunks::staleness::`.

---

## P2

### SEC-4: IN-list bind overflow in `build_graph` / `build_hierarchy`

**File**: `/mnt/c/Projects/cqs/src/serve/data.rs`

**Current code** (lines 326-341 — `build_graph` capped-edge branch):
```rust
let names: Vec<&str> = name_set.into_iter().collect();
let placeholders = vec!["?"; names.len()].join(",");
let edge_sql = format!(
    "SELECT fc.file, fc.caller_name, fc.callee_name \
     FROM function_calls fc \
     WHERE fc.callee_name IN ({placeholders}) \
        OR fc.caller_name IN ({placeholders})"
);
let mut eq = sqlx::query(&edge_sql);
for n in &names {
    eq = eq.bind(*n);
}
for n in &names {
    eq = eq.bind(*n);
}
eq.fetch_all(&store.pool).await?
```

Also at lines 670-680, 743-754 in `build_hierarchy`:
```rust
let placeholders = vec!["?"; visited_names.len()].join(",");
let sql = format!(
    "SELECT id, name, chunk_type, language, origin, line_start, line_end \
     FROM chunks WHERE name IN ({placeholders}) ORDER BY id"
);
let mut q = sqlx::query(&sql);
for n in &visited_names {
    q = q.bind(n);
}
```
(and the edge-sql below at :743-754 which binds `visited_names` twice — total 2× bind count).

**Fix**:

Chunk the IN-list into batches of `SQLITE_MAX_IN_LIST` (e.g. 16_000 to stay under SQLite's 32_766 `SQLITE_MAX_VARIABLE_NUMBER` even when the list is bound twice). Extract a helper:

```rust
/// SQLite default `SQLITE_MAX_VARIABLE_NUMBER` is 32_766 in modern builds.
/// We chunk at 16k so queries that bind the list twice (e.g. hierarchy edge
/// SQL binds visited_names × 2) still fit under the cap.
const SQLITE_MAX_IN_LIST: usize = 16_000;

async fn fetch_edges_chunked<'a>(
    pool: &sqlx::SqlitePool,
    names: &[&'a str],
    bind_twice: bool,
) -> Result<Vec<sqlx::sqlite::SqliteRow>, sqlx::Error> {
    let chunk_size = if bind_twice {
        SQLITE_MAX_IN_LIST / 2
    } else {
        SQLITE_MAX_IN_LIST
    };
    let mut out = Vec::new();
    for chunk in names.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT fc.file, fc.caller_name, fc.callee_name \
             FROM function_calls fc \
             WHERE fc.callee_name IN ({placeholders}) \
                OR fc.caller_name IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql);
        for n in chunk {
            q = q.bind(*n);
        }
        for n in chunk {
            q = q.bind(*n);
        }
        out.extend(q.fetch_all(pool).await?);
    }
    Ok(out)
}
```

Apply to `build_graph` capped branch (lines 326-341), `build_hierarchy` chunk-fetch (lines 670-680, binds once), and `build_hierarchy` edge-fetch (lines 743-754, binds twice). Deduplicate returned rows with a `HashSet<(caller, callee, file)>` after collection — the chunked split can produce duplicate edges when an edge's caller and callee fall in different chunks.

**Why**: Large corpora (>16k visible chunks under capped-graph path or >32k visited names in deep hierarchy) overflow SQLite's bind limit and return HTTP 500 — trivial DoS without a malformed query.

**Verify**: `cargo build --features gpu-index` + add a unit test (`tests/serve_data_test.rs::build_hierarchy_over_bind_limit`) that seeds >32k chunks and asserts `build_hierarchy(...)` returns `Ok(_)`. `cargo test --features gpu-index --test serve_data_test`.

---

### PB-V1.29-1: `cqs context` / `cqs brief` don't normalize backslash paths

**File**: `/mnt/c/Projects/cqs/src/cli/commands/io/context.rs` + `src/cli/commands/io/brief.rs`

**Current code** (context.rs:25-29):
```rust
pub(crate) fn build_compact_data<Mode>(store: &Store<Mode>, path: &str) -> Result<CompactData> {
    let _span = tracing::info_span!("build_compact_data", path).entered();
    let chunks = store
        .get_chunks_by_origin(path)
        .context("Failed to load chunks for file")?;
```

Same shape at context.rs:108-116 (`build_full_data`) and brief.rs:37-42 (`build_brief_data`).

**Fix**:

At the top of each function, normalize the path so backslash-delimited Windows input matches forward-slash-delimited storage. Use the existing `cqs::normalize_path` helper. It takes `&Path`:

```rust
pub(crate) fn build_compact_data<Mode>(store: &Store<Mode>, path: &str) -> Result<CompactData> {
    let _span = tracing::info_span!("build_compact_data", path).entered();
    // PB-V1.29-1: normalize backslash input from Windows / agent pipelines.
    // `get_chunks_by_origin` matches on the stored `origin` column which is
    // stored forward-slash; unnormalized `src\foo.rs` silently returns empty.
    let normalized = cqs::normalize_path(std::path::Path::new(path));
    let chunks = store
        .get_chunks_by_origin(&normalized)
        .context("Failed to load chunks for file")?;
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }
    // ... rest uses `normalized` downstream where a `&str` path is expected
```

Apply the same transform at `build_full_data` (context.rs:108+) and `build_brief_data` (brief.rs:37+). Update the downstream calls that use `path` as a key (e.g. the `file:` field in output structs) to use `normalized` so cross-platform JSON consumers see the canonical slash form.

**Why**: On Windows, `cqs context src\foo.rs` returns "No indexed chunks found" even when `src/foo.rs` is indexed. Same class as PR #1044 which fixed `cqs impact`.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test cli_brief_test -- backslash`.

---

### PB-V1.29-3: `chunk.id` prefix-strip uses `abs_path.display()`

**File**: `/mnt/c/Projects/cqs/src/cli/watch.rs`

**Current code** (lines 2432-2434):
```rust
if let Some(rest) = chunk.id.strip_prefix(&abs_path.display().to_string()) {
    chunk.id = format!("{}{}", rel_path.display(), rest);
}
```

**Fix**:

Use `cqs::normalize_path` on both sides so the prefix-strip is slash-normalized:

```rust
// PB-V1.29-3: On Windows verbatim paths (`\\?\C:\...`) `abs_path.display()`
// keeps backslashes while chunk.id was built forward-slash by the parser.
// Normalize both sides so the strip actually matches.
let abs_norm = cqs::normalize_path(&abs_path);
let rel_norm = cqs::normalize_path(&rel_path);
if let Some(rest) = chunk.id.strip_prefix(abs_norm.as_str()) {
    chunk.id = format!("{}{}", rel_norm, rest);
}
```

**Why**: `abs_path.display()` returns platform-native separator. On Windows verbatim paths the prefix never matches and `chunk.id` keeps the absolute prefix — breaks cross-index equality, call-graph resolution, and HNSW id_map.

**Verify**: `cargo build --features gpu-index`. Add a unit test in `watch.rs::tests` that calls the prefix-strip with a `\\?\C:\Projects\cqs` style path and asserts the result starts with the rel path.

---

### PB-V1.29-5: `dispatch_drift` / `dispatch_diff` emit backslashes in JSON

**File**: `/mnt/c/Projects/cqs/src/cli/batch/handlers/misc.rs`

**Current code** (lines 274-281, 350-357, 362-369, 373-383):
```rust
.map(|e| {
    serde_json::json!({
        "name": e.name,
        "file": e.file.display().to_string(),
        "chunk_type": e.chunk_type,
        "similarity": e.similarity,
        "drift": e.drift,
    })
})
```

(Same `e.file.display().to_string()` at :353, :365, :377.)

**Fix**:

Replace `e.file.display().to_string()` with `cqs::normalize_path(&e.file)` in all four sites:

```rust
.map(|e| {
    serde_json::json!({
        "name": e.name,
        "file": cqs::normalize_path(&e.file),
        "chunk_type": e.chunk_type,
        "similarity": e.similarity,
        "drift": e.drift,
    })
})
```

Apply at lines 277, 353, 365, 377. Sister handlers already use `normalize_path`; these four are the drift.

**Why**: `cqs --json diff` / `drift` on Windows emit `"file": "src\\foo.rs"` — breaks agent chaining that passes the field to downstream `cqs context --json`.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test cli_drift_diff_test`.

---

### DS2-3: `set_metadata_opt` / `touch_updated_at` bypass WRITE_LOCK

**File**: `/mnt/c/Projects/cqs/src/store/metadata.rs`

**Current code** (lines 409-418, `touch_updated_at`):
```rust
pub fn touch_updated_at(&self) -> Result<(), StoreError> {
    let now = chrono::Utc::now().to_rfc3339();
    self.rt.block_on(async {
        sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES ('updated_at', ?1)")
            .bind(&now)
            .execute(&self.pool)
            .await?;
        Ok(())
    })
}
```

(Same bare `.execute(&self.pool)` pattern at `set_metadata_opt` lines 457-474.)

**Fix**:

Route through `self.begin_write()` so both paths acquire `WRITE_LOCK` before opening the transaction (mirrors `set_hnsw_dirty` at :436-449):

```rust
pub fn touch_updated_at(&self) -> Result<(), StoreError> {
    let now = chrono::Utc::now().to_rfc3339();
    self.rt.block_on(async {
        let (_guard, mut tx) = self.begin_write().await?;
        sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES ('updated_at', ?1)")
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    })
}

pub(crate) fn set_metadata_opt(&self, key: &str, value: Option<&str>) -> Result<(), StoreError> {
    self.rt.block_on(async {
        let (_guard, mut tx) = self.begin_write().await?;
        match value {
            Some(v) => {
                sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)")
                    .bind(key)
                    .bind(v)
                    .execute(&mut *tx)
                    .await?;
            }
            None => {
                sqlx::query("DELETE FROM metadata WHERE key = ?1")
                    .bind(key)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    })
}
```

**Why**: Concurrent reindex + batch-id setter can race → `SQLITE_BUSY` / observable inconsistency. DS-V1.25-3 closed the class for `set_hnsw_dirty`; these two setters missed the wave.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --lib store::metadata`.

---

### DS2-4: Phantom-chunks DELETE separate tx from upsert

**File**: `/mnt/c/Projects/cqs/src/cli/watch.rs`

**Current code** (lines 2568-2579):
```rust
store.upsert_chunks_and_calls(pairs, mtime, &file_calls)?;

// DS-37 / RT-DATA-10: Delete phantom chunks — functions removed from the
// file but still lingering in the index. The upsert above handles updates
// and inserts; this cleans up deletions.
//
// Ideally this would share a transaction with upsert_chunks_and_calls, but
// both methods manage their own internal transactions. A crash between the
// two leaves phantoms that get cleaned on the next reindex. Propagate the
// error rather than silently swallowing it.
let live_ids: Vec<&str> = pairs.iter().map(|(c, _)| c.id.as_str()).collect();
store.delete_phantom_chunks(file, &live_ids)?;
```

**Fix**:

Add a combined API on `Store<ReadWrite>` that does both the upsert and the phantom delete inside a single `begin_write` transaction. Shape:

```rust
// In src/store/chunks/crud.rs (or a new file-level mod):
pub fn upsert_chunks_calls_and_prune<P: AsRef<Path>>(
    &self,
    pairs: Vec<(Chunk, Embedding)>,
    mtime: Option<i64>,
    calls: &[(String, CallSite)],
    file: P,
    live_ids: &[&str],
) -> Result<(), StoreError> {
    let _span = tracing::info_span!("upsert_chunks_calls_and_prune").entered();
    self.rt.block_on(async {
        let (_guard, mut tx) = self.begin_write().await?;
        // Existing upsert_chunks_and_calls inner logic, adapted to run on `&mut tx`
        self.upsert_chunks_and_calls_tx(&mut tx, &pairs, mtime, calls).await?;
        self.delete_phantom_chunks_tx(&mut tx, file.as_ref(), live_ids).await?;
        tx.commit().await?;
        Ok(())
    })
}
```

Watch call site becomes:
```rust
let live_ids: Vec<&str> = pairs.iter().map(|(c, _)| c.id.as_str()).collect();
store.upsert_chunks_calls_and_prune(pairs, mtime, &file_calls, file, &live_ids)?;
```

Refactor the existing `upsert_chunks_and_calls` and `delete_phantom_chunks` to take an optional `&mut tx` (`_tx` inner variant) so the combined function can share the transaction; the pub methods keep their own `begin_write` for other call sites.

**Why**: Mid-batch crash between `upsert_chunks_and_calls` commit and `delete_phantom_chunks` commit serves queries against a half-pruned index (new chunks visible but deleted ones still present). The comment already acknowledges the gap as tech debt.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --lib store::chunks::crud`. Add a crash-simulation test that injects a failure between the two operations and asserts the pre-batch state is visible.

---

### DS2-8: `CQS_MIGRATE_REQUIRE_BACKUP` defaults to off

**File**: `/mnt/c/Projects/cqs/src/store/backup.rs`

**Current code** (lines 117-141):
```rust
Err(e) => {
    let require = std::env::var(REQUIRE_BACKUP_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if require {
        tracing::error!(
            error = %e,
            db = %db_path.display(),
            "Migration backup failed and CQS_MIGRATE_REQUIRE_BACKUP=1 is set; aborting"
        );
        remove_triplet(&backup_db);
        Err(e)
    } else {
        tracing::warn!(
            error = %e,
            db = %db_path.display(),
            "Migration backup failed; proceeding without snapshot \
             (set CQS_MIGRATE_REQUIRE_BACKUP=1 to fail instead)"
        );
        remove_triplet(&backup_db);
        Ok(None)
    }
}
```

**Fix**:

Flip the default: require backup by default, allow opt-out via `CQS_MIGRATE_REQUIRE_BACKUP=0`. Destructive migrations (v18→v19 drops the old sparse_vectors table) without a backup are a data-loss hazard on first failure.

```rust
Err(e) => {
    // DS2-8: Require-backup is now the default. Opt-out via
    // CQS_MIGRATE_REQUIRE_BACKUP=0 for environments where the user
    // accepts the data-loss risk (e.g. CI rebuilding from source).
    let allow_no_backup = std::env::var(REQUIRE_BACKUP_ENV)
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false);
    if allow_no_backup {
        tracing::warn!(
            error = %e,
            db = %db_path.display(),
            "Migration backup failed; proceeding without snapshot \
             (CQS_MIGRATE_REQUIRE_BACKUP=0 is set)"
        );
        remove_triplet(&backup_db);
        Ok(None)
    } else {
        tracing::error!(
            error = %e,
            db = %db_path.display(),
            "Migration backup failed; aborting to protect DB. \
             Set CQS_MIGRATE_REQUIRE_BACKUP=0 to proceed without a snapshot \
             (data loss risk on migration failure)."
        );
        remove_triplet(&backup_db);
        Err(e)
    }
}
```

Update `SECURITY.md` and `CONTRIBUTING.md` threat-model sections to document the new default. Update the docstring at migrations.rs:51-54.

**Why**: v18→v19 drops the `sparse_vectors` table after a rebuild. If the rebuild commits but the post-rebuild `UPDATE metadata` fails mid-flight without a backup, the user has lost their SPLADE data with no recovery path short of `cqs index --force`. Opt-in to destructive behavior is the standard stance; opt-out is inverted for data-safety.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --lib store::backup`. Add a test that confirms backup-failure + missing env var returns `Err`.

---

### AC-V1.29-1: `semantic_diff` sort no tie-break

**File**: `/mnt/c/Projects/cqs/src/diff.rs`

**Current code** (lines 202-207):
```rust
modified.sort_by(|a, b| match (a.similarity, b.similarity) {
    (Some(sa), Some(sb)) => sa.total_cmp(&sb),
    (Some(_), None) => std::cmp::Ordering::Less,
    (None, Some(_)) => std::cmp::Ordering::Greater,
    (None, None) => std::cmp::Ordering::Equal,
});
```

(Same shape in the test at lines 298-303.)

**Fix**:

Add secondary tie-breaks on `(file, name, chunk_type)` so identical similarities produce deterministic output:

```rust
modified.sort_by(|a, b| {
    match (a.similarity, b.similarity) {
        (Some(sa), Some(sb)) => sa.total_cmp(&sb),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
    .then_with(|| a.file.cmp(&b.file))
    .then_with(|| a.name.cmp(&b.name))
    .then_with(|| format!("{:?}", a.chunk_type).cmp(&format!("{:?}", b.chunk_type)))
});
```

Also update the `test_diff_sort_none_similarity_at_end` test to assert tie-broken order, and add a new test that constructs two entries with identical similarity + asserts lexicographic file order.

**Why**: `cqs diff` / `cqs drift` output is non-deterministic when scores tie — CI pipelines diffing output hit spurious failures.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --lib diff`.

---

### AC-V1.29-2: `is_structural_query` misses EOL keyword

**File**: `/mnt/c/Projects/cqs/src/search/router.rs`

**Current code** (lines 787-789):
```rust
STRUCTURAL_KEYWORDS
    .iter()
    .any(|kw| query.contains(&format!(" {} ", kw)) || query.starts_with(&format!("{} ", kw)))
```

**Fix**:

Add the end-of-string variant so `"find all trait"` matches. The current probe requires the keyword to be followed by a trailing space.

```rust
STRUCTURAL_KEYWORDS.iter().any(|kw| {
    query.contains(&format!(" {kw} "))
        || query.starts_with(&format!("{kw} "))
        || query.ends_with(&format!(" {kw}"))
        || query == *kw
})
```

Add a unit test in `router::tests` that asserts `is_structural_query("find all trait") == true` and `is_structural_query("find all traits") == true`.

**Why**: `"find all trait"` and similar trailing-keyword queries misroute to Conceptual α=0.70 instead of Structural α=0.90 — measurable R@1 drop on structural eval.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --lib search::router`.

---

### AC-V1.29-3: `bfs_expand` HashMap seed order

**File**: `/mnt/c/Projects/cqs/src/gather.rs`

**Current code** (lines 317-320):
```rust
let mut queue: VecDeque<(Arc<str>, usize)> = VecDeque::new();
for name in name_scores.keys() {
    queue.push_back((Arc::from(name.as_str()), 0));
}
```

**Fix**:

Sort seeds before enqueuing so BFS expansion is order-independent of `HashMap` iteration seed:

```rust
// AC-V1.29-3: HashMap iteration order is process-seed-dependent.
// Sort seeds by (score desc, name asc) before enqueue so results are
// deterministic when the cap hits mid-expansion.
let mut seeds: Vec<(&str, f32)> = name_scores
    .iter()
    .map(|(k, (s, _))| (k.as_str(), *s))
    .collect();
seeds.sort_by(|(a_name, a_score), (b_name, b_score)| {
    b_score
        .partial_cmp(a_score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a_name.cmp(b_name))
});

let mut queue: VecDeque<(Arc<str>, usize)> = VecDeque::new();
for (name, _) in seeds {
    queue.push_back((Arc::from(name), 0));
}
```

**Why**: When `name_scores.len() >= max_expanded_nodes` hits mid-expansion, which neighbors get enqueued before the cap fires depends on iteration order. Non-determinism is the bug; the cap itself is fine.

**Verify**: `cargo build --features gpu-index`. Add a deterministic-seed test in `gather::tests`: seed 10 names, cap at 15, run twice, assert byte-identical `name_scores` output.

---

### AC-V1.29-5: `--name-boost` accepts out-of-range

**File**: `/mnt/c/Projects/cqs/src/cli/args.rs`

**Current code** (lines 57-58):
```rust
/// Weight for name matching in hybrid search (0.0-1.0)
#[arg(long, default_value = "0.2", value_parser = parse_finite_f32)]
pub name_boost: f32,
```

**Fix**:

Add a range-bounded parser. Introduce `parse_unit_f32` in `src/cli/definitions.rs`:

```rust
// In src/cli/definitions.rs, sibling of parse_finite_f32:
pub(crate) fn parse_unit_f32(s: &str) -> Result<f32, String> {
    let v = parse_finite_f32(s)?;
    if !(0.0..=1.0).contains(&v) {
        return Err(format!("value must be in [0.0, 1.0], got {v}"));
    }
    Ok(v)
}
```

Then in args.rs:
```rust
/// Weight for name matching in hybrid search (0.0-1.0)
#[arg(long, default_value = "0.2", value_parser = crate::cli::definitions::parse_unit_f32)]
pub name_boost: f32,
```

Also apply to `--weight` in ref add (`src/cli/commands/infra/reference.rs:50`, currently uses `parse_finite_f32` with an after-the-fact range check inside `cmd_ref_add`; centralize the check).

**Why**: `--name-boost 1.5` silently works, subtracts > 1.0 from embedding weight — search degrades with no warning. Same class as SEC-V1.25-8 bounded-parser wave.

**Verify**: `cargo build --features gpu-index`. Add test: `cqs search --name-boost 1.5 foo` must return a clap exit code (not silent success).

---

### AC-V1.29-6: `reranker::compute_scores_opt` unchecked multiply

**File**: `/mnt/c/Projects/cqs/src/reranker.rs`

**Current code** (lines 368-387):
```rust
let stride = if shape.len() == 2 {
    shape[1] as usize
} else {
    1
};
if stride == 0 {
    return Err(RerankerError::Inference(
        "Model returned zero-width output tensor".to_string(),
    ));
}
let expected_len = batch_size * stride;
if data.len() < expected_len {
    return Err(RerankerError::Inference(format!(
        "Model output too short: expected {} elements, got {}",
        expected_len,
        data.len()
    )));
}
```

**Fix**:

Validate ORT dim is non-negative before cast, then use `checked_mul`:

```rust
let stride = if shape.len() == 2 {
    let dim = shape[1];
    if dim < 0 {
        return Err(RerankerError::Inference(format!(
            "Model returned negative output dim {dim} (dynamic axis not bound?)"
        )));
    }
    dim as usize
} else {
    1
};
if stride == 0 {
    return Err(RerankerError::Inference(
        "Model returned zero-width output tensor".to_string(),
    ));
}
let expected_len = batch_size.checked_mul(stride).ok_or_else(|| {
    RerankerError::Inference(format!(
        "Reranker output too large: batch_size={batch_size} * stride={stride} overflows usize"
    ))
})?;
if data.len() < expected_len {
    return Err(RerankerError::Inference(format!(
        "Model output too short: expected {} elements, got {}",
        expected_len,
        data.len()
    )));
}
```

**Why**: ORT's dynamic axis returns -1 when unbound; `-1 as usize == usize::MAX` → `batch_size * stride` wraps silently → `data.len() < expected_len` condition flips direction → reads past the buffer end.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --lib reranker`.

---

### API-V1.29-1: `project list/remove` ignore `--json`

**File**: `/mnt/c/Projects/cqs/src/cli/commands/infra/project.rs`

**Current code** (lines 90-118):
```rust
ProjectCommand::List => {
    let registry = ProjectRegistry::load()?;
    if registry.project.is_empty() {
        println!("No projects registered.");
        println!("Use 'cqs project register <name> <path>' to add one.");
    } else {
        println!("Registered projects:");
        for entry in &registry.project {
            let status = if entry.path.join(".cqs/index.db").exists()
                || entry.path.join(".cq/index.db").exists()
            {
                "ok".green().to_string()
            } else {
                "missing index".red().to_string()
            };
            println!("  {} — {} [{}]", entry.name, entry.path.display(), status);
        }
    }
    Ok(())
}
ProjectCommand::Remove { name } => {
    let mut registry = ProjectRegistry::load()?;
    if registry.remove(name)? {
        println!("Removed '{}'", name);
    } else {
        println!("Project '{}' not found", name);
    }
    Ok(())
}
```

**Fix**:

Mirror the `Search` subcommand pattern (which correctly honors `cli.json || output.json`). Add `output: TextJsonArgs` to `List` and `Remove` variants, then branch on `cli.json || output.json`:

```rust
// Enum variant:
List {
    #[command(flatten)]
    output: TextJsonArgs,
},
Remove {
    name: String,
    #[command(flatten)]
    output: TextJsonArgs,
},

// Handler:
ProjectCommand::List { output } => {
    let json = cli.json || output.json;
    let registry = ProjectRegistry::load()?;
    if json {
        #[derive(serde::Serialize)]
        struct ProjectListEntry { name: String, path: String, indexed: bool }
        let entries: Vec<_> = registry.project.iter().map(|e| ProjectListEntry {
            name: e.name.clone(),
            path: cqs::normalize_path(&e.path),
            indexed: e.path.join(".cqs/index.db").exists()
                || e.path.join(".cq/index.db").exists(),
        }).collect();
        crate::cli::json_envelope::emit_json(&entries)?;
    } else {
        // existing text branch
    }
    Ok(())
}

ProjectCommand::Remove { name, output } => {
    let json = cli.json || output.json;
    let mut registry = ProjectRegistry::load()?;
    let removed = registry.remove(name)?;
    if json {
        crate::cli::json_envelope::emit_json(&serde_json::json!({
            "name": name,
            "removed": removed,
        }))?;
    } else if removed {
        println!("Removed '{}'", name);
    } else {
        println!("Project '{}' not found", name);
    }
    Ok(())
}
```

**Why**: `cqs --json project list` emits colored ANSI text; agents consuming JSON crash.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test cli_envelope_test -- project`.

---

### API-V1.29-2: `ref add/remove/update` ignore `--json`

**File**: `/mnt/c/Projects/cqs/src/cli/commands/infra/reference.rs`

**Current code** (lines 42-69 — `RefCommand` variants — and handlers at 88-185, 234-end):
```rust
Add {
    name: String,
    source: PathBuf,
    #[arg(long, default_value = "0.8", value_parser = crate::cli::definitions::parse_finite_f32)]
    weight: f32,
},
// List has output: TextJsonArgs — the others don't.
Remove { name: String },
Update { name: String },
```

**Fix**:

Add `output: TextJsonArgs` to `Add`, `Remove`, `Update`. Handle JSON branch in each handler by emitting `{name, action, result}`:

```rust
Add {
    name: String,
    source: PathBuf,
    #[arg(long, default_value = "0.8", value_parser = crate::cli::definitions::parse_unit_f32)]
    weight: f32,
    #[command(flatten)]
    output: TextJsonArgs,
},
Remove {
    name: String,
    #[command(flatten)]
    output: TextJsonArgs,
},
Update {
    name: String,
    #[command(flatten)]
    output: TextJsonArgs,
},
```

Thread `cli.json || output.json` through `cmd_ref_add` / `cmd_ref_remove` / `cmd_ref_update`, then swap `println!` blocks to `emit_json(&serde_json::json!({"name": name, "action": "added", "chunks": stats.total_embedded}))` style envelopes.

**Why**: Same class as API-V1.29-1 — `cqs --json ref add` emits text, crashes downstream parsers.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test cli_envelope_test -- ref`.

---

### API-V1.29-4: `notes list --check` dropped by daemon

**File**: `/mnt/c/Projects/cqs/src/cli/args.rs` + `src/cli/batch/handlers/misc.rs`

**Current code** (args.rs:532-540):
```rust
#[derive(Args, Debug, Clone)]
pub(crate) struct NotesListArgs {
    /// Show only warnings (negative sentiment)
    #[arg(long)]
    pub warnings: bool,
    /// Show only patterns (positive sentiment)
    #[arg(long)]
    pub patterns: bool,
}
```

(`NotesCommand::List` at notes.rs:51-64 has an extra `check: bool` field.)

**Fix**:

Add `check: bool` to `NotesListArgs` so daemon dispatch receives the same flag as CLI:

```rust
#[derive(Args, Debug, Clone)]
pub(crate) struct NotesListArgs {
    #[arg(long)]
    pub warnings: bool,
    #[arg(long)]
    pub patterns: bool,
    /// Check mentions for staleness (verifies files exist and symbols are in index)
    #[arg(long)]
    pub check: bool,
}
```

Update `dispatch_notes` (batch/handlers/misc.rs:85-118) signature to accept `check: bool`, pass through to a new `staleness_check` code path that mirrors `cmd_notes_list`'s check logic. If the stale-check impl isn't trivially shareable, initially route: when `check` is true, load notes + run the existing `stale_mentions()` helper from `cmd_notes`, emit those in the output.

Also update the parent dispatcher call site in `src/cli/batch/mod.rs` (match arm for `BatchCmd::Notes(NotesListArgs { warnings, patterns, check })`) to pass `check` through.

**Why**: Both EX-V1.29-5 and API-V1.29-4 — drift between CLI and daemon arg structs causes `cqs notes list --check` via daemon to silently drop the check.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test cli_notes_test -- check`.

---

### EH-V1.29-1: `build_brief_data` swallows 3 errors

**File**: `/mnt/c/Projects/cqs/src/cli/commands/io/brief.rs`

**Current code** (lines 59-75):
```rust
let caller_counts = store.get_caller_counts_batch(&names).unwrap_or_else(|e| {
    tracing::warn!(error = %e, "Failed to fetch caller counts");
    HashMap::new()
});

let graph = store.get_call_graph().unwrap_or_else(|e| {
    tracing::warn!(error = %e, "Failed to load call graph for test counts");
    std::sync::Arc::new(cqs::store::CallGraph::from_string_maps(
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    ))
});
let test_chunks = store.find_test_chunks().unwrap_or_else(|e| {
    tracing::warn!(error = %e, "Failed to find test chunks");
    std::sync::Arc::new(Vec::new())
});
```

**Fix**:

Either propagate the errors, or thread them into a `warnings: Vec<String>` field on `BriefData` + the output JSON so the consumer sees that the brief is partially-populated. The propagate approach is simpler:

```rust
let caller_counts = store
    .get_caller_counts_batch(&names)
    .context("Failed to fetch caller counts for brief")?;
let graph = store
    .get_call_graph()
    .context("Failed to load call graph for brief")?;
let test_chunks = store
    .find_test_chunks()
    .context("Failed to find test chunks for brief")?;
```

Remove the default-empty fallbacks. If the user wants a best-effort partial brief, that should be a separate subcommand mode; the current silent zero-fill is strictly worse than a loud failure.

**Why**: On store corruption / permission denied, `cqs brief foo.rs` returns "0 callers, 0 tests" with zero indication that the data is fake.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test cli_brief_test`.

---

### EH-V1.29-2: `run_ci_analysis` silently downgrades dead-code failure

**File**: `/mnt/c/Projects/cqs/src/ci.rs`

**Current code** (lines 100-128):
```rust
let dead_in_diff = match store.find_dead_code(true) {
    Ok((confident, possibly_pub)) => {
        // ... build Vec<DeadInDiff> ...
        dead
    }
    Err(e) => {
        tracing::warn!(error = %e, "Dead code detection failed — CI will report 0 dead code (not 'scan passed')");
        Vec::new()
    }
};
```

**Fix**:

Add a `dead_code_scan_failed: bool` field to the CI report, populate on error, and let the gate evaluation fail on scan failure unless the user explicitly asks for permissive mode:

```rust
let (dead_in_diff, dead_code_scan_failed, dead_scan_error) = match store.find_dead_code(true) {
    Ok((confident, possibly_pub)) => {
        let dead = /* existing mapping */;
        (dead, false, None)
    }
    Err(e) => {
        tracing::error!(error = %e, "Dead code detection failed — CI treating as a gate failure");
        (Vec::new(), true, Some(format!("{e:#}")))
    }
};

// In evaluate_gate:
if dead_code_scan_failed && !allow_scan_failures {
    reasons.push(format!(
        "Dead-code scan failed: {}",
        dead_scan_error.as_deref().unwrap_or("<unknown>")
    ));
}
```

Wire the `allow_scan_failures` knob through as a `--permissive` CLI flag or `CQS_CI_ALLOW_SCAN_FAILURES=1` env var, default false.

**Why**: `cqs ci` currently emits "0 dead code, gate passed" when dead-code detection exploded — CI treats broken tooling as a passing build.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test ci_test`.

---

### EH-V1.29-7: `EmbeddingCache::stats` 5 silent failures

**File**: `/mnt/c/Projects/cqs/src/cache.rs`

**Current code** (lines 408-461, 5 `unwrap_or_else` chains):
```rust
let total_entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_cache")
    .fetch_one(&self.pool)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "cache stats: COUNT failed");
        0
    });
// ... 4 more sibling queries with identical unwrap_or_else pattern
```

**Fix**:

Propagate via `?` so the caller knows the stats are unreliable. The stats API is read-only; there's no good reason to return a silent-zero when the DB is unhealthy:

```rust
pub fn stats(&self) -> Result<CacheStats, CacheError> {
    let _span = tracing::info_span!("cache_stats").entered();
    self.rt.block_on(async {
        let total_entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_cache")
            .fetch_one(&self.pool)
            .await?;
        let total_size: i64 = sqlx::query_scalar(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
        )
        .fetch_one(&self.pool)
        .await?;
        let unique_models: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT model_fingerprint) FROM embedding_cache",
        )
        .fetch_one(&self.pool)
        .await?;
        let oldest: Option<i64> = sqlx::query_scalar("SELECT MIN(created_at) FROM embedding_cache")
            .fetch_one(&self.pool)
            .await?;
        let newest: Option<i64> = sqlx::query_scalar("SELECT MAX(created_at) FROM embedding_cache")
            .fetch_one(&self.pool)
            .await?;
        Ok(CacheStats {
            total_entries: total_entries as u64,
            total_size_bytes: total_size as u64,
            unique_models: unique_models as u64,
            oldest_timestamp: oldest,
            newest_timestamp: newest,
        })
    })
}
```

The `?` converts `sqlx::Error` via `CacheError::From`; verify that impl exists (re-read `src/cache.rs:1-50` for the error enum).

**Why**: Current impl can return `{total_entries: 0, total_size_bytes: 0, ...}` when 3 of 5 queries failed silently — an agent reads a "healthy empty cache" when reality is "broken DB, cache unknown".

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test cli_cache_test`.

---

### EH-V1.29-8: Gitignore RwLock poison silently re-indexes ignored files

**File**: `/mnt/c/Projects/cqs/src/cli/watch.rs`

**Current code** (lines 1737, 1945):
```rust
let matcher_guard = gitignore.read().ok();
let matcher_ref = matcher_guard.as_ref().and_then(|g| g.as_ref());
```

**Fix**:

Recover from poison instead of silently dropping the matcher. A poisoned read lock usually means some writer panicked mid-update — the old value is still valid:

```rust
// EH-V1.29-8: Recover from RwLock poison. The write side re-loads the
// gitignore matcher from disk; if a writer panicked mid-load the reader
// can still safely see the pre-panic matcher. Dropping to "no matcher"
// silently re-indexes ignored files (including .env.secret).
let matcher_guard = match gitignore.read() {
    Ok(g) => Some(g),
    Err(poisoned) => {
        tracing::error!(
            "Gitignore RwLock poisoned — recovering. Previous matcher is still valid; indexing continues with it."
        );
        Some(poisoned.into_inner())
    }
};
let matcher_ref = matcher_guard.as_ref().and_then(|g| g.as_ref());
```

Apply at both sites (1737 startup GC, 1945 periodic GC).

**Why**: Poisoned RwLock on the gitignore matcher silently degrades to "no matcher → index everything", re-ingesting secrets from `.gitignored` files into the index.

**Verify**: `cargo build --features gpu-index`. Add a test in `watch::tests` that poisons the RwLock and asserts the reader still gets `Some`.

---

### CQ-V1.29-3: `cmd_similar` private `resolve_target` diverges

**File**: `/mnt/c/Projects/cqs/src/cli/commands/search/similar.rs`

**Current code** (lines 16-39):
```rust
fn resolve_target<Mode>(store: &Store<Mode>, name: &str) -> Result<(String, String)> {
    let (file_filter, func_name) = parse_target(name);
    let results = store.search_by_name(func_name, 20)?;
    if results.is_empty() {
        bail!("No function found matching '{}'. Check the name and try again.", func_name);
    }
    let matched = if let Some(file) = file_filter {
        results.iter().find(|r| {
            let path = r.chunk.file.to_string_lossy();
            path.ends_with(file) || path.contains(file)
        })
    } else {
        None
    };
    let result = matched.unwrap_or(&results[0]);
    Ok((result.chunk.id.clone(), result.chunk.name.clone()))
}
```

**Fix**:

Replace with a call to `cqs::resolve_target` (re-exported via `crate::cli::commands::resolve::resolve_target`) so CLI and batch share the same resolution logic:

```rust
// Delete the private resolve_target fn.
// In cmd_similar body:
let resolved = crate::cli::commands::resolve::resolve_target(store, name)?;
let chunk_id = resolved.chunk_id;
let chunk_name = resolved.name;
```

Re-read `cqs::ResolvedTarget` shape (`src/lib.rs` has the public type definition) to confirm field names match. Delete the now-dead `fn resolve_target` above.

**Why**: Private `resolve_target` picks `results[0]` (test chunks sort earlier by id); `cqs::resolve_target` in the library picks real chunks — CLI and batch/daemon return different "similar" answers for the same target.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --lib` + `cargo test --features gpu-index --test cli_surface_test`.

---

### CQ-V1.29-6: `cqs doctor` reports compile-time `MODEL_NAME` constant

**File**: `/mnt/c/Projects/cqs/src/cli/commands/infra/doctor.rs`

**Current code** (lines 144-147, 155-156):
```rust
out(json, &format!(
    "  {} Model: {} (metadata: {})",
    "[✓]".green(),
    cqs::embedder::model_repo(),
    cqs::store::MODEL_NAME
));
```

(Same pattern at :155-156 inside `check_records.push(CheckRecord::ok(...))`.)

**Fix**:

Read the actual `embedding_model` / `repo` key from the opened Store's metadata table instead of using the compile-time constant:

```rust
let actual_model = ctx
    .store()
    .metadata("embedding_model")
    .ok()
    .flatten()
    .unwrap_or_else(|| "<unset>".to_string());

out(json, &format!(
    "  {} Model: {} (index metadata: {})",
    "[✓]".green(),
    cqs::embedder::model_repo(),
    actual_model
));
check_records.push(CheckRecord::ok(
    "runtime",
    "model",
    format!("{} (index metadata: {})", cqs::embedder::model_repo(), actual_model),
));
```

Confirm `Store` exposes a readable `metadata(key)` accessor (re-read `src/store/metadata.rs:1-100`); if not, thread through `repo_name()` or the equivalent setter's mirror getter.

**Why**: After `cqs model swap`, the index metadata points to the new model but `cqs doctor` still reports the compile-time `MODEL_NAME` — the user sees "metadata: BAAI/bge-large-en-v1.5" when the index actually holds E5-base embeddings. Doctor is specifically the command you run to detect drift.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test cli_doctor_fix_test`.

---

### DOC-V1.29-1: CONTRIBUTING says "Schema v20" — actual v22

**File**: `/mnt/c/Projects/cqs/CONTRIBUTING.md`

**Current text** (lines 193, 207):
```
  store/        - SQLite storage layer (Schema v20, WAL mode)
    ...
    migrations.rs - Schema migration framework (v10-v20, including v19 FK cascade + v20 trigger)
```

**Fix**:

Re-read the actual latest schema version first (`grep -n 'SCHEMA_VERSION\|schema_version.*const\|CURRENT_SCHEMA' src/store/mod.rs src/store/migrations.rs`), then update to match. Expected: v22 per project memory. Replace:

```
  store/        - SQLite storage layer (Schema v22, WAL mode)
    ...
    migrations.rs - Schema migration framework (v10-v22, including v19 FK cascade, v20 trigger, v21-v22 <describe>)
```

Verify the "v19 FK cascade + v20 trigger" description is still accurate; update the brief description of v21-v22 by reading the migration function bodies.

**Why**: CONTRIBUTING.md is ground truth for new contributors; wrong schema version sends them diagnosing phantom migration bugs.

**Verify**: `grep -n 'Schema v' CONTRIBUTING.md` and `grep -n 'const.*SCHEMA\|CURRENT_SCHEMA\|SCHEMA_VERSION' src/store/mod.rs src/store/migrations.rs` agree.

---

### DOC-V1.29-2: README missing `cqs serve` section

**File**: `/mnt/c/Projects/cqs/README.md`

**Design spec**:

Add a new second-level section titled **`cqs serve` — interactive graph explorer** after the "Daemon mode" paragraph around line 835. Content outline (draft; re-read nearby section voice first):

1. **Intro** (1-2 sentences): What `cqs serve` does — starts a local HTTP server on `127.0.0.1:8321` that renders call-graph / hierarchy / cluster visualizations from the index. Flagship v1.29.0 feature.
2. **Quickstart** block:
   ```bash
   cqs serve
   # Opens http://127.0.0.1:8321 in your browser (disable with --no-open).
   ```
3. **Views table**: enumerate `graph`, `hierarchy`, `cluster-3d`, `hierarchy-3d` with a one-line description of each.
4. **Flags**: `--bind <addr>`, `--port <N>`, `--no-open`, `--max-nodes N`, `--read-only` (if applicable — re-read `src/cli/commands/infra/serve.rs` for actual flag names).
5. **Security callout**: link to `SECURITY.md#cqs-serve`. Two sentences on "localhost trust" model + that SEC-1 Host-header protection is in place (post-P1 wave) but there is no auth layer.
6. **Example workflows**:
   - "Find dead code in a large call graph" → `cqs serve` → click hierarchy view → filter by `dead: true`.
   - "Explore an unfamiliar module" → `cqs serve` → graph view with `file=src/store/` filter.

Cross-reference from the `## Commands` TOC at the top of README. Add a bullet under the "Surfaces" section near line 56 showing `cqs serve` in the command list.

**Why**: Flagship v1.29.0 feature is invisible to new users reading the README.

**Verify**: Render README locally (`mdcat README.md` or GitHub preview) — confirm the new section renders, headings are at the right level, and all code blocks have language fences.

---

### DOC-V1.29-3: README/CONTRIBUTING missing `.cqsignore`

**File**: `/mnt/c/Projects/cqs/README.md` + `/mnt/c/Projects/cqs/CONTRIBUTING.md`

**Design spec**:

In README, find the existing `.gitignore` handling paragraph (`grep -n gitignore README.md`) and add a paragraph after it:

> ### `.cqsignore`
>
> cqs honors `.gitignore` by default. For paths you want indexed but gitignored (e.g. build artifacts containing docs), or gitignored paths you want explicitly excluded from indexing only, add patterns to `.cqsignore` at the project root. Same syntax as `.gitignore`. Takes precedence over `.gitignore` for index-specific overrides.
>
> Example:
> ```
> # .cqsignore
> !vendor/generated-docs/    # include despite .gitignore
> tests/fixtures/huge/       # exclude from index but keep for tests
> ```

In CONTRIBUTING.md, add a bullet under the "Project Conventions" or equivalent section:

> - `.cqsignore` — project-local override file for index include/exclude, syntax mirrors `.gitignore`

**Why**: `.cqsignore` support exists (`grep -rn 'cqsignore' src/` confirms parsing in watch + index pipeline) but is undocumented; users hit "why isn't my file indexed" and can't find the answer.

**Verify**: `grep -n 'cqsignore' README.md CONTRIBUTING.md` returns at least one hit in each.

---

### DOC-V1.29-4: SECURITY.md wrong integrity-check default

**File**: `/mnt/c/Projects/cqs/SECURITY.md`

**Current text** (line 22):
```
3. **Database corruption**: `PRAGMA quick_check(1)` on write-mode opens (opt-out via `CQS_SKIP_INTEGRITY_CHECK=1`). Read-only opens skip the check entirely — reads cannot introduce corruption and the index is rebuildable via `cqs index --force`
```

**Fix**:

Re-read `src/store/mod.rs:960-967` for the actual behavior. Current code: `opt_in = CQS_INTEGRITY_CHECK == "1"`, default is SKIP. Replace:

```
3. **Database corruption**: `PRAGMA quick_check(1)` is opt-in via `CQS_INTEGRITY_CHECK=1`. By default, integrity checks are skipped on all opens because the index is rebuildable via `cqs index --force` and the check takes ~40s on WSL `/mnt/c` (NTFS over 9P). Read-only opens skip the check entirely. Legacy `CQS_SKIP_INTEGRITY_CHECK=1` forces skip even when `CQS_INTEGRITY_CHECK=1` is set.
```

**Why**: Threat-model docs must match behavior. Current SECURITY.md advertises "default on, opt-out" but code is "default off, opt-in" — users relying on the advertised guarantee silently go unchecked.

**Verify**: `grep -n 'CQS_INTEGRITY\|quick_check\|integrity' SECURITY.md src/store/mod.rs` and confirm the wording matches.

---

### SHL-V1.29-2: `MAX_BATCH_LINE_LEN = 1 MB` blocks large diffs

**File**: `/mnt/c/Projects/cqs/src/cli/batch/mod.rs`

**Current code** (line 104):
```rust
const MAX_BATCH_LINE_LEN: usize = 1_048_576;
```

**Fix**:

Raise the cap and make it env-tunable:

```rust
/// Maximum batch stdin line length. Default 16 MB — large enough for a ~5k-line
/// unified diff piped through batch or daemon. Override via `CQS_BATCH_LINE_LIMIT`
/// (bytes; setting 0 disables the cap, not recommended).
const DEFAULT_BATCH_LINE_LIMIT: usize = 16 * 1_048_576;

fn batch_line_limit() -> usize {
    std::env::var("CQS_BATCH_LINE_LIMIT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_BATCH_LINE_LIMIT)
}
```

Replace uses of `MAX_BATCH_LINE_LEN` (grep the const for sites) with `batch_line_limit()`. Also update the CLI path that reads the same line (look for `MAX_LINE_LEN` or similar near the 50MB threshold mentioned in the triage entry — the CLI accepts 50 MB already, so the asymmetry is in this const).

**Why**: `cqs --json diff --stdin` via daemon silently fails on diffs >1 MB; the same input works via CLI.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --lib cli::batch`.

---

### PF-V1.29-1: Daemon shell-join / re-split on hot path

**File**: `/mnt/c/Projects/cqs/src/cli/watch.rs`

**Current code** (lines 315-331):
```rust
let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    let full_line = if args.is_empty() {
        command.to_string()
    } else {
        format!("{} {}", command, shell_words::join(&args))
    };
    let mut output = Vec::new();
    {
        let ctx = batch_ctx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ctx.dispatch_line(&full_line, &mut output);
    }
```

**Fix**:

Add a `dispatch_tokens` entry point on `BatchContext` that accepts `(command, args)` pre-split. The existing `dispatch_line` does `shell_words::split` on the concatenated string, which is wasteful when the daemon already has the tokens. Sketch:

```rust
// In src/cli/batch/mod.rs:
pub(crate) fn dispatch_tokens(
    &self,
    command: &str,
    args: &[String],
    out: &mut impl std::io::Write,
) {
    // Same body as dispatch_line but skips shell_words::split and the NUL-byte
    // check is applied directly on the (command, args) inputs.
    let tokens: Vec<String> = std::iter::once(command.to_string())
        .chain(args.iter().cloned())
        .collect();
    if let Err(msg) = reject_null_tokens(&tokens) {
        // existing error arm
        return;
    }
    self.query_count.fetch_add(1, Ordering::Relaxed);
    // ... rest of dispatch_line body with `tokens` instead of re-split
}
```

Update the daemon call site in watch.rs:315-331 to call `dispatch_tokens(&command, &args, &mut output)` instead of joining then re-splitting.

**Why**: Every daemon query does `shell_words::join(&args)` then `dispatch_line` does `shell_words::split(&full_line)` — round-trip serialization that can fail on edge-case tokens (pointed out as a latent correctness bug) and is pure waste on the hot path.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test daemon_forward_test`. Bench: `cqs ping` loop before/after.

---

### RM-V1.29-1: `load_references` bypasses LRU

**File**: `/mnt/c/Projects/cqs/src/cli/batch/handlers/search.rs` + `src/reference.rs`

**Current code** (batch/handlers/search.rs:283-308):
```rust
let results = if args.include_refs {
    let config = cqs::config::Config::load(&ctx.root);
    let references = cqs::reference::load_references(&config.references);
    // ... rayon par_iter over every reference, fresh Store + fresh HNSW per call
```

And `src/reference.rs:194-224` — `load_references` builds a fresh rayon pool + loads every ref from scratch per call.

**Fix**:

Cache loaded references on `BatchContext` so a daemon session amortizes the ref-load cost. `BatchContext` already has a `refs` RefCell slot per `invalidate_mutable_caches` at batch/mod.rs:504. Audit what populates it — if nothing currently does, wire it here:

```rust
// In src/cli/batch/handlers/search.rs:
let references = ctx.borrow_refs_or_load(|root| {
    let config = cqs::config::Config::load(root);
    cqs::reference::load_references(&config.references)
})?;
```

Add the accessor on `BatchContext` (`borrow_refs_or_load<F>(f: F) -> Result<Ref<'_, Vec<ReferenceIndex>>>`) that lazily populates on first call and returns a borrow. Invalidate on `invalidate_mutable_caches` (already listed at :504).

For `load_references` itself: thread an optional rayon pool argument so the caller can reuse an existing pool. For daemon, build the pool once at BatchContext construction; sequential CLI path keeps the current one-shot behavior.

**Why**: With N references and M queries per daemon session, we do `O(N*M)` Store opens + HNSW loads instead of `O(N+M)`. Reported latency on a 4-reference setup: 2-3s per `--include-refs` query vs sub-200ms when cached.

**Verify**: `cargo build --features gpu-index`. Bench `cqs search foo --include-refs` in a daemon session loop; first call warms, subsequent calls should be 10× faster.

---

### EX-V1.29-5: NotesListArgs / NotesCommand::List drift

**File**: `/mnt/c/Projects/cqs/src/cli/args.rs` + `src/cli/commands/io/notes.rs`

**Current code**: two hand-maintained arg structs (see API-V1.29-4 above).

**Fix**:

Consolidate: have `NotesCommand::List` embed `NotesListArgs` via `#[command(flatten)]` + the extra `output: TextJsonArgs`. Draft:

```rust
// In src/cli/commands/io/notes.rs:
List {
    #[command(flatten)]
    list: crate::cli::args::NotesListArgs,
    #[command(flatten)]
    output: TextJsonArgs,
}
```

Remove the now-duplicate fields. Update `cmd_notes` match arm to `ctx.list.warnings`, `ctx.list.patterns`, `ctx.list.check` (after API-V1.29-4 adds `check` to `NotesListArgs`).

**Why**: API-V1.29-4 is a symptom of this drift. Rather than fix each field addition in two places, unify the source of truth.

**Verify**: `cargo build --features gpu-index`. `cargo test --features gpu-index --test cli_notes_test`.

---

### TC-HAP-1.29-1: Serve endpoints never tested with data

**File** (new): `/mnt/c/Projects/cqs/tests/serve_endpoints_test.rs`

**Design spec**:

Create a new integration test file exercising `build_graph`, `build_hierarchy`, `build_cluster`, and `build_chunk_detail` against a populated `Store<ReadOnly>`.

**Seed fixture shape** (shared helper in `tests/common/`):
- 20 chunks across 5 files (function, struct, trait, const, test kinds mixed).
- 30 function_calls edges forming a small DAG (some diamond patterns, some self-loops, one cycle).
- Deterministic chunk ids + names.
- Optional: 2 chunks with zero callers (dead), 1 chunk with 10 callers (hotspot).

**Test cases to stub** (one per endpoint, pairs of capped + uncapped):
```rust
#[test]
fn build_graph_returns_all_chunks_when_uncapped() { ... }

#[test]
fn build_graph_respects_max_nodes_cap() {
    // assert response.nodes.len() == cap
    // assert the TOP cap nodes by n_callers_global are present
}

#[test]
fn build_graph_file_filter_restricts_to_origin() { ... }

#[test]
fn build_graph_kind_filter_restricts_to_chunk_type() { ... }

#[test]
fn build_hierarchy_callers_direction_from_leaf() { ... }

#[test]
fn build_hierarchy_callees_direction_from_root() { ... }

#[test]
fn build_hierarchy_max_depth_bounds_bfs() { ... }

#[test]
fn build_hierarchy_returns_none_for_unknown_root_id() { ... }

#[test]
fn build_cluster_returns_language_groups() { ... }

#[test]
fn build_chunk_detail_returns_callers_and_callees_lists() { ... }

#[test]
fn build_chunk_detail_returns_none_for_unknown_id() { ... }
```

Use `tests/common/fixtures.rs` patterns: a `TempProject::with_seeded_graph()` builder that populates the store via `store.upsert_chunks_and_calls(...)` directly (skip the parser/embedder path — these tests exercise the serve data layer, not the pipeline).

**Why**: All four `build_*` functions are tested only for empty-store cases in existing suites (verified via `grep -n 'build_graph\|build_hierarchy\|build_cluster\|build_chunk_detail' tests/*.rs`). The SEC-4 IN-list overflow fix and DS2-4 race both regress silently without populated-data tests.

**Verify**: `cargo test --features gpu-index --test serve_endpoints_test`.

---

### TC-HAP-1.29-2: 16 batch dispatch handlers untested

**Files** to touch: existing `tests/cli_surface_test.rs` or new `tests/batch_dispatch_test.rs`

**Design spec**:

Existing `dispatch_drift`, `dispatch_diff`, `dispatch_notes`, etc. in `src/cli/batch/handlers/*.rs` have zero end-to-end tests. Spawn `cqs batch` with a line-oriented stdin script and assert the JSON envelopes on stdout.

**Test file structure**:
```rust
// tests/batch_dispatch_test.rs
mod common;
use common::TempProject;

fn batch_script(project: &TempProject, lines: &[&str]) -> Vec<serde_json::Value> {
    let mut cmd = cqs_binary_command(); // existing helper
    cmd.arg("batch").current_dir(project.path());
    let stdin_input = lines.join("\n");
    let output = cmd
        .stdin_buf(stdin_input)
        .output()
        .expect("batch should run");
    output
        .stdout
        .lines()
        .filter_map(|l| serde_json::from_str(&l.ok()?).ok())
        .collect()
}

#[test]
fn dispatch_gather_returns_envelope() { ... }

#[test]
fn dispatch_scout_returns_envelope() { ... }

#[test]
fn dispatch_task_returns_envelope() { ... }

#[test]
fn dispatch_where_returns_envelope() { ... }

#[test]
fn dispatch_onboard_returns_envelope() { ... }

#[test]
fn dispatch_callers_returns_envelope() { ... }

#[test]
fn dispatch_callees_returns_envelope() { ... }

#[test]
fn dispatch_impact_returns_envelope() { ... }

#[test]
fn dispatch_test_map_returns_envelope() { ... }

#[test]
fn dispatch_explain_returns_envelope() { ... }

#[test]
fn dispatch_related_returns_envelope() { ... }

#[test]
fn dispatch_similar_returns_envelope() { ... }

#[test]
fn dispatch_dead_returns_envelope() { ... }

#[test]
fn dispatch_diff_returns_envelope() { ... }

#[test]
fn dispatch_drift_returns_envelope() { ... }

#[test]
fn dispatch_context_returns_envelope() { ... }
```

Each test: seed a small project (fixture from `tests/common/fixtures.rs`), index it, spawn `cqs batch` with one input line, assert the emitted envelope has the right shape (`data` field present, not null, matches the expected handler's output struct).

**Why**: 16 handlers are reachable only via batch/daemon; CLI variants are well-tested but daemon wiring is frequently where regressions hide (see PR #1047, #1050 history of dispatch bugs).

**Verify**: `cargo test --features gpu-index --test batch_dispatch_test`.

---

### TC-ADV-1.29-3: Daemon socket adversarial tests

**File** (new): `/mnt/c/Projects/cqs/tests/daemon_socket_adversarial_test.rs`

**Design spec**:

Spawn `cqs watch --serve` against a temp project, open a Unix-socket connection, send adversarial payloads, assert the daemon (a) doesn't panic, (b) emits a structured error envelope, (c) keeps serving subsequent legitimate queries.

**Test cases to stub**:
```rust
#[test]
#[cfg(unix)]
fn daemon_rejects_line_over_1mib_boundary() {
    let project = TempProject::seeded();
    let daemon = SpawnDaemon::start(&project);
    let mut stream = daemon.connect();
    let payload = "a".repeat(1_048_577);  // 1 MiB + 1 byte
    stream.write_all(payload.as_bytes()).unwrap();
    let resp = read_envelope(&mut stream);
    assert_eq!(resp["error"]["code"], "INVALID_INPUT");
    // Sanity: daemon still alive for follow-up
    assert!(ping(&daemon).is_ok());
}

#[test]
#[cfg(unix)]
fn daemon_rejects_nul_byte_in_args() {
    let payload = r#"{"command":"stats","args":["foo bar"]}"#;
    // ... assert INVALID_INPUT envelope, daemon still alive
}

#[test]
#[cfg(unix)]
fn daemon_rejects_malformed_json_not_crash() {
    // Send `{ not valid json at all `
    // Assert PARSE_ERROR envelope
}

#[test]
#[cfg(unix)]
fn daemon_rejects_args_array_over_1024_elements() {
    // Send a "command":"stats" with 10_000 args
    // Assert INVALID_INPUT or a reasonable cap
}

#[test]
#[cfg(unix)]
fn daemon_handles_partial_read_and_disconnect() {
    // Write half a line, close socket
    // Assert daemon doesn't panic, next connection works
}

#[test]
#[cfg(unix)]
fn daemon_rejects_ansi_bel_cr_in_command() {
    // Command with \x07 (BEL), \x0D (CR), \x1b[ (ANSI ESC)
    // Assert INVALID_INPUT envelope
}
```

Shared helpers (`tests/common/daemon.rs`):
- `SpawnDaemon::start(project)` — launches `cqs watch --serve` with a temp `XDG_RUNTIME_DIR`, waits for socket bind, returns handle with cleanup Drop.
- `read_envelope(&mut stream)` — reads one line, parses as envelope JSON.
- `ping(daemon)` — sends `{"command":"ping"}`, asserts 200-like envelope.

**Why**: Daemon socket is the first long-lived network-adjacent surface in cqs; zero adversarial coverage = first SEC-1-class bug ships unnoticed.

**Verify**: `cargo test --features gpu-index --test daemon_socket_adversarial_test`.

---

## Execution rules for the agent fixing P2

- Same re-read-then-edit discipline as P1.
- Group by file so a single fix touches one module (e.g., PB-V1.29-1 touches both context.rs and brief.rs — do together).
- Documentation fixes (DOC-V1.29-1, -3, -4) can be batched in one commit.
- Test-coverage items (TC-HAP-1.29-1, -2, TC-ADV-1.29-3) each deserve their own commit; they add new test files and shouldn't be mixed with code fixes.
- `cargo fmt && cargo build --features gpu-index` after each cluster.
- Run the named test-file target after each cluster (avoid the full suite — it's ~90s and most P2s don't need it).
- When all P2 are done: full `cargo test --features gpu-index --lib` + `cargo test --features gpu-index --tests` once for regression catch.
