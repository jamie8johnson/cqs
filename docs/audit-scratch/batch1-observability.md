## Observability

#### `Reranker::rerank()` lacks entry span — the `rerank_with_passages()` wrapper has one but the shortcut path doesn't
- **Difficulty:** easy
- **Location:** `src/reranker.rs:160`
- **Description:** `Reranker::rerank` is the canonical public reranker API. It does NOT call `rerank_with_passages` — instead it fetches passages from results and calls `compute_scores` directly (see lines 177-181). Consequently the hot cross-encoder path has no span, while the seldom-used `rerank_with_passages` (line 197) has a `tracing::info_span!("rerank", count, limit, query_len)`. If a reranker regression shows up in journal logs (non-finite scores, timeout, session poison), the primary entry path is untagged and hard to correlate with the caller. A single tracing journal line "reranker took 850ms" can't be attributed to a specific search invocation without a parent span.
- **Suggested fix:** Add `let _span = tracing::info_span!("rerank", count = results.len(), limit, query_len = query.len()).entered();` at the top of `pub fn rerank` (line 165-166). Mirrors the span already present in `rerank_with_passages`.

#### `serve::build_chunk_detail` and `build_stats` lack `tracing::info_span!` — the other three `build_*` have them
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:452` (`build_chunk_detail`), `src/serve/data.rs:933` (`build_stats`)
- **Description:** `serve/data.rs` exposes five public `build_*` functions invoked from axum handlers via `spawn_blocking`. Three of them (`build_graph:198`, `build_hierarchy:592`, `build_cluster:829`) open a `tracing::info_span!` at entry with useful fields. The remaining two — `build_chunk_detail` and `build_stats` — open no span and emit no tracing at all. `build_chunk_detail` runs 5+ distinct SQL queries including a blocking `rt.block_on(...)`. If any one fails, the `ServeError::Store` warn in `error.rs:50` fires without the chunk_id that triggered it, and `build_stats` has no trace at all if the 4 COUNT queries stall. Latency debugging for `/api/chunk/:id` and `/api/stats` is therefore blind.
- **Suggested fix:** In `build_chunk_detail`, add `let _span = tracing::info_span!("build_chunk_detail", chunk_id = %chunk_id).entered();` after the signature. In `build_stats`, add `let _span = tracing::info_span!("build_stats").entered();`. Matches the pattern used by the other three `build_*` functions.

#### `cmd_project` span doesn't record the subcommand dispatched (register / list / remove / search)
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/project.rs:75`
- **Description:** The entry span is `tracing::info_span!("cmd_project")` — no fields. All four subcommands (`Register`, `List`, `Remove`, `Search`) run inside the same undifferentiated span, so journal output for `cqs project search foo` and `cqs project list` is indistinguishable after the entry line. `Search` additionally initializes an `Embedder` and calls `search_across_projects` — the span should record which action was taken and, for Search, the query + limit. Currently nothing distinguishes a no-op `List` from a 5-second cross-project search in the journal.
- **Suggested fix:** Replace the single entry span with a match-scoped span, e.g. `let _span = match subcmd { ProjectCommand::Register { name, .. } => tracing::info_span!("cmd_project_register", name = %name).entered(), ProjectCommand::List => tracing::info_span!("cmd_project_list").entered(), ProjectCommand::Remove { name } => tracing::info_span!("cmd_project_remove", name = %name).entered(), ProjectCommand::Search { query, limit, .. } => tracing::info_span!("cmd_project_search", query = %query, limit).entered(), };` Alternatively keep one `cmd_project` span but record `action` and subcommand-specific fields via `span.record(...)`.

#### `hnsw::persist::verify_hnsw_checksums` uses format-interpolated `tracing::warn!` instead of structured field
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:136`
- **Description:** `tracing::warn!("Ignoring unknown extension in checksum file: {}", ext)` interpolates `ext` into the message. Every other warn in the file uses the structured `field = value` form. Structured fields enable filtering (`journalctl ... | jq 'select(.ext == "hnsw.foo")'`) — the interpolated form forces regex. This function is on the self-heal checksum verify path; when an attacker (or bit rot) manages to drop an unexpected extension in the checksum file, the operator has to grep the message text rather than query a field.
- **Suggested fix:** Change to `tracing::warn!(ext = %ext, "Ignoring unknown extension in checksum file");`.

#### `serve` axum handlers log entry but never log completion — no latency trace for API requests
- **Difficulty:** medium
- **Location:** `src/serve/handlers.rs:77-242` (`stats`, `graph`, `chunk_detail`, `search`, `hierarchy`, `cluster_2d`)
- **Description:** Each handler emits a single `tracing::info!` at entry (e.g. `"serve::stats"`, `"serve::graph"`). None wrap the body in a span and none log completion. Latency and error-path diagnosis for the web UI requires either (a) an external reverse proxy's access log, or (b) reading axum's tower middleware output, neither of which is configured by default in `serve::mod.rs`. When a user reports "the cluster view takes 10s to load", the journal shows no signal — not even whether the request hit `/api/cluster/2d` at all, let alone how long `build_cluster` took. Contrast with `cli/watch.rs:62` where `daemon_query` wraps the whole span and emits `cmd_duration_ms` on exit.
- **Suggested fix:** Wrap each handler body in a `tracing::info_span!("serve_<name>", <params>)` so the downstream `build_*` spans nest under it, and add an `.instrument(span)` on the `spawn_blocking` await if preserving span across tokio boundaries. Alternatively add `tower_http::trace::TraceLayer` to `build_router` in `serve/mod.rs:97` which gives latency + status code per request for free.

#### `classify_query` / `reclassify_with_centroid` lack entry span — routing decisions not traceable for a given query
- **Difficulty:** easy
- **Location:** `src/search/router.rs:561` (`classify_query`), `src/search/router.rs:1093` (`reclassify_with_centroid`)
- **Description:** `classify_query` (1549 lines of routing logic) decides which embedder path a query takes (DenseDefault vs DenseBase vs NameOnly vs DenseWithTypeHints). It's called from every search entry point. It has `tracing::info!(centroid_category, margin, "centroid filled Unknown gap")` at line 1116 but no entry span — so when a user asks "why did my query route to DenseBase?", the operator has to grep for the message text and has no correlation id back to the originating `cmd_search`/`cmd_scout`/etc. `resolve_splade_alpha` (P3 OB-NEW-1, triaged but still open — triage shows ✅ wave-1 which may be fixed already) was the sibling issue.
- **Suggested fix:** Add `let _span = tracing::info_span!("classify_query", query_len = query.len()).entered();` at the top of `classify_query`. At exit add `tracing::debug!(category = %classification.category, confidence = ?classification.confidence, strategy = ?classification.strategy, "Query classified");` so the full routing decision is one journal line. Same treatment for `reclassify_with_centroid` with `tracing::info_span!("reclassify_with_centroid").entered();`.

#### `verify_hnsw_checksums` silently returns errors on IO failure — no tracing line before wrapping
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:120-156` (`verify_hnsw_checksums`)
- **Description:** Every IO failure in this function (read checksum file, open data file, read through hasher) flattens `std::io::Error` into `HnswError::Internal(format!("Failed to read {}: {}", ...))` via `.map_err`. None of these emit a `tracing::warn!` before returning. When a real user hits a permission or FD-exhaustion problem during daemon startup self-heal (this function is called from `build_vector_index_with_config` line 447 on the hot path), the only signal reaching the journal is the eventual `tracing::warn!(error = %e, ...)` at the caller in `cli/store.rs:455` — which shows the flattened String, stripping the `io::ErrorKind`. For transient IO failures (NFS stall, filesystem readonly remount) operators can't tell the kind from the log alone.
- **Suggested fix:** Add `tracing::warn!(error = %e, path = %path.display(), kind = ?e.kind(), "verify_hnsw_checksums IO failure");` inline before each `.map_err(|e| HnswError::Internal(...))` so the `ErrorKind` reaches the journal even though the wrapped `HnswError` is a plain string.
