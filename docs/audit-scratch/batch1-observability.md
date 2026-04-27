## Observability

Coverage is broadly excellent in v1.30.0 — the v0.12.1 lesson has been applied across all post-v0.9.7 modules and v1.30.0 additions (`slot`, `cache`, `serve`, embedder/provider). Previously-flagged OB-V1.29-1 through OB-V1.29-7 are now fixed. Below are NEW observability gaps not in `audit-triage-v1.30.0.md`.

#### OB-V1.30-1: Default subscriber drops EVERY `info_span!` event — no spans render at default log level
- **Difficulty:** easy
- **Location:** `src/main.rs:14-32`
- **Description:** The default `EnvFilter` is `"warn,ort=error"`, but every span in the codebase (~150 sites) is `tracing::info_span!` (INFO) or `tracing::debug_span!`. Under default settings none of these match the filter, so the subscriber emits nothing for them. `fmt::init()` also never calls `.with_span_events(FmtSpan::CLOSE)` (or `NEW | CLOSE`), so even if INFO were enabled, span boundaries would not produce log lines on entry/exit — only events emitted *inside* the span would. Consequence: a user running `cqs index` with default config sees no per-batch progress, no SPLADE timing, no UMAP rows-projected count from spans alone — the heavy investment in span instrumentation across `scout`, `gather`, `serve`, `cache`, `slot`, parser, store, and embedder is invisible until someone discovers `--verbose` or `RUST_LOG=info`. There is no runtime way to trace one `cqs serve` request end-to-end without rebuilding the process. Operators of the daemon (`cqs watch --serve`) hit this hardest because the systemd unit inherits empty `RUST_LOG`.
- **Suggested fix:** (a) Bump default to `"cqs=info,warn"` (cqs's own crate at INFO, world at WARN) so existing spans render without third-party noise. (b) Configure span events: `tracing_subscriber::fmt().with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE).with_env_filter(filter)…` so close events emit `latency_ms` automatically. (c) Add a `--log-format=json` flag (and `CQS_LOG_FORMAT=json`) wired to `.json()` on the fmt builder, so daemon journals are structurally consumable.

#### OB-V1.30-2: `auth::enforce_auth` rejects 401 silently — no `tracing::warn!` on auth failure
- **Difficulty:** easy
- **Location:** `src/serve/auth.rs:194-232` (specifically `AuthOutcome::Unauthorized` branch at lines 224-230)
- **Description:** The new per-launch token middleware (#1118, SEC-7) emits zero log output when a request fails authentication. A brute-force scan, expired bookmark, or misconfigured client gets a generic `401 Unauthorized` body and the operator has no journal trail showing it happened, the path attempted, or the source. Asymmetric with `enforce_host_allowlist` at `src/serve/mod.rs:246`, which DOES emit `tracing::warn!(host = %host, "serve: rejected request with disallowed Host header")`. Auth failures are the higher-value signal — host-allowlist failures are usually misconfiguration, while auth failures are the canonical "someone is probing" event.
- **Suggested fix:** Inside the `AuthOutcome::Unauthorized` arm at `src/serve/auth.rs:224`, before returning the 401 response, emit:
  ```rust
  tracing::warn!(
      method = %req.method(),
      path = %req.uri().path(),
      "serve: rejected unauthenticated request",
  );
  ```
  Do NOT log token candidates — even truncated.

#### OB-V1.30-3: Per-request span (TraceLayer) and `build_*` spans are disconnected because `tokio::task::spawn_blocking` doesn't propagate span context
- **Difficulty:** medium
- **Location:** `src/serve/handlers.rs:86, 111, 131, 160, 210, 236` (every `spawn_blocking` call)
- **Description:** The serve router wires `TraceLayer::new_for_http()` (`src/serve/mod.rs:195`) which generates a per-request span (fixes OB-V1.29-5 at the request level). Each handler then spawns its blocking work via `tokio::task::spawn_blocking(move || super::data::build_graph(…))`. The closure runs on a fresh blocking-pool thread which has NO span stack inherited from the caller — `tokio` does not propagate `tracing::Span::current()` into spawn_blocking by default. Inside `build_graph`/`build_chunk_detail`/`build_hierarchy`/`build_cluster`/`build_stats`, the `info_span!` becomes a fresh root span. A JSON capture of one request shows two unrelated trees: TraceLayer's `http_request{method=GET path=/api/graph}` and a separately-rooted `build_graph{file_filter=…}`. Once OB-V1.30-1 is fixed and INFO spans render, the noise will be doubled with no parent linkage.
- **Suggested fix:** Capture `tracing::Span::current()` before the spawn and `instrument` the closure:
  ```rust
  use tracing::Instrument;
  let span = tracing::Span::current();
  let stats = tokio::task::spawn_blocking({
      let store = state.store.clone();
      move || {
          let _entered = span.enter();
          super::data::build_stats(&store)
      }
  }).await…
  ```
  Apply at all six handlers (stats/graph/chunk_detail/search/hierarchy/cluster_2d). Alternative: drop the per-handler `tracing::info!` lines (80, 100, 126, 149, 175, 201, 231) and rely solely on the inner `build_*` span entry — they're redundant once the inner span emits CLOSE events.

#### OB-V1.30-4: `cqs eval` runner uses `eprintln!` for progress instead of `tracing::info!`
- **Difficulty:** easy
- **Location:** `src/cli/commands/eval/runner.rs:163-168`
- **Description:** The eval runner emits progress (`[eval] 50/109 queries (12.3 q/s)`) via `eprintln!` at line 167. Every other progress signal in the codebase (`build.rs` SPLADE batches at 546, UMAP at `umap.rs:142`, daemon GC at `watch.rs:1217`) uses `tracing::info!` with structured fields. Using `eprintln!`: (a) prevents JSON-redirect of eval output for downstream comparison tooling, (b) fires unconditionally even with `RUST_LOG=error` (no quiet mode), (c) the q/s number can't be filtered or summed from journal logs.
- **Suggested fix:** Replace line 167 with:
  ```rust
  tracing::info!(done, total = total_queries, qps, "eval progress");
  ```
  Keep `eprintln!` only behind a `--quiet=false` CLI gate if interactive feedback is required, or rely on the `cqs=info` default after OB-V1.30-1.

#### OB-V1.30-5: `nl/mod.rs` public NL generators have zero spans — generated text shapes the embedding for every chunk
- **Difficulty:** easy
- **Location:** `src/nl/mod.rs:43, 65, 189, 209` (`generate_nl_with_call_context`, `generate_nl_with_call_context_and_summary`, `generate_nl_description`, `generate_nl_with_template`)
- **Description:** None of the four `generate_nl_*` public functions have entry spans, despite being the canonical text-shaping path that determines what every chunk's embedding sees. When eval drops 5pp recall after a model swap, there is no way to bisect from "which NL template rendered which chunk" — an operator has to add spans by hand and rebuild. (The fts/markdown helpers running in tight indexing loops are reasonable to skip; the 4 NL generators are NOT inner-loop, they run once per chunk during enrichment.)
- **Suggested fix:** Add a single `tracing::debug_span!("generate_nl", template = ?template, chunk_kind = ?chunk.chunk_type, len = chunk.content.len())` at the top of `generate_nl_with_template` (line 209). The other three call into it transitively, so one span covers all four entry points. `debug_span!` (not info) keeps it off by default.

#### OB-V1.30-6: `embed_documents` / `embed_query` lack completion fields — entry span has `count`/`text` but no result.len, dim, time
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:683, 722`
- **Description:** Both entry spans are minimal: `info_span!("embed_documents", count = texts.len())` and `info_span!("embed_query")`. There is no "embed_documents complete" event with the produced batch size, embedding dim, or tokenization stats. `FmtSpan::CLOSE` (OB-V1.30-1) would partially fix this, but even with CLOSE events the only structured field would be the entry-time `count`, not output sizes / dim / tokenization stats. Indexing 100k chunks today produces ~200 `embed_batch` enter events and zero "I produced N embeddings of dim D in T ms" events.
- **Suggested fix:** At the bottom of `embed_documents` (after the loop completes), add:
  ```rust
  tracing::info!(
      total = embeddings.len(),
      dim = self.embedding_dim(),
      input_count = texts.len(),
      "embed_documents complete"
  );
  ```
  Same pattern in `embed_query` at `tracing::debug!` level.

#### OB-V1.30-7: `Reranker::rerank_with_passages` swallows `passages.len() != results.len()` mismatch with no log — silent semantic corruption
- **Difficulty:** easy
- **Location:** `src/reranker.rs:200-220`
- **Description:** `rerank_with_passages` accepts `passages: &[&str]` independent of `results: &mut Vec<SearchResult>`. The doc says "passages must have the same length as results" but the implementation does not assert, log, or take a tracing event when the lengths diverge. If a caller mis-zips (e.g. filters results post-fetch but forgets to re-trim passages), `compute_scores` either panics on out-of-bounds slicing or scores arbitrarily-paired text, and the operator sees nothing in the journal. Semantically silent: ranks shift, neither error nor warning fires.
- **Suggested fix:** At line 213 (after the entry span, before the early-return), add:
  ```rust
  if passages.len() != results.len() {
      tracing::warn!(
          passages = passages.len(),
          results = results.len(),
          "rerank_with_passages: length mismatch — caller bug, results will be corrupted",
      );
      return Err(RerankerError::InvalidArguments(format!(
          "passages.len()={} != results.len()={}",
          passages.len(),
          results.len()
      )));
  }
  ```
  Add the `InvalidArguments` variant if it doesn't exist; warn-only also acceptable but less safe.

#### OB-V1.30-8: `train_data` git subprocess wrappers don't log non-zero exit codes — silent failure on shallow clones / missing SHAs
- **Difficulty:** easy
- **Location:** `src/train_data/git.rs:65-242` (`git_log`, `git_diff_tree`, `git_show`)
- **Description:** Each function has an entry `tracing::info_span!` (good), but on `output.status.success()` failure the exit code and stderr are bundled into a `TrainDataError` and returned — no structured log. When `cqs train-data --max-commits 1000` walks a shallow repo and 50% of `git_diff_tree` calls fail with `fatal: bad revision`, the user sees a single aggregated count at the end and has no way to reconstruct WHICH SHAs failed without re-running with `RUST_LOG=trace`. The `is_shallow` probe at line 241 is the only one that handles a missing-SHA case gracefully.
- **Suggested fix:** In each subprocess wrapper, on `!output.status.success()` add before the error return:
  ```rust
  tracing::warn!(
      sha,
      exit = output.status.code(),
      stderr = %String::from_utf8_lossy(&output.stderr).trim(),
      "git_diff_tree failed",
  );
  ```
  Apply consistently to `git_log` (line 65), `git_diff_tree` (line 131), `git_show` (line 173).

#### OB-V1.30-9: Format-string-interpolated `tracing::info!` calls — structural fields are lost
- **Difficulty:** easy
- **Location:** `src/hnsw/build.rs:78, 236`; `src/hnsw/persist.rs:210, 638, 771`; `src/reference.rs:220`; `src/cli/commands/train/export_model.rs:76`; `src/audit.rs:85, 93`; `src/embedder/provider.rs:149`
- **Description:** OB-V1.29-4 specifically called out `verify_hnsw_checksums` for this pattern; the audit fixed that one site but the broader pattern persists across HNSW build/persist and several other modules. Lines like `tracing::info!("Building HNSW index with {} vectors", nb_elem)` produce a single rendered string instead of structured `count = nb_elem` fields. Once OB-V1.30-1 lands JSON formatting, these lines remain un-queryable: jq-extracting the vector count from "Building HNSW index with 178432 vectors" needs a regex per line, while `count: 178432` is a JSON field. Friction with no offsetting benefit — `tracing` natively supports both forms.
- **Suggested fix:** Convert each site to structured form. Examples:
  ```rust
  // src/hnsw/build.rs:78
  tracing::info!(count = nb_elem, "Building HNSW index");
  // src/hnsw/persist.rs:210
  tracing::info!(dir = %dir.display(), basename, "Saving HNSW index");
  ```
  Sweep the 9 sites in one pass. Pure-mechanical change, no behavior delta.

#### OB-V1.30-10: `cqs serve` cluster_2d emits no warn when corpus has chunks but zero UMAP rows — operators see only the empty payload
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:901, 1020` (`build_cluster`); handler at `src/serve/handlers.rs:227-242`
- **Description:** When the user runs `cqs serve` against a corpus indexed without `cqs index --umap` (the v1.30.0 schema-v22 default — UMAP is opt-in), the cluster-3d view returns `{nodes: [], skipped: N}` with N = total chunks. The frontend renders a "run cqs index --umap" hint (per the doc comment at handlers.rs:226), but the backend emits no `tracing::warn!` to surface this state in the journal — the operator who runs `cqs serve` over SSH and gets a blank cluster view has no log to point at. Neighboring `build_hierarchy` at `data.rs:638` DOES log `tracing::info!(root_id, "build_hierarchy: root chunk not found")` for its empty-result case.
- **Suggested fix:** At the point inside `build_cluster` where `coords` is empty but `total_chunks > 0`, add:
  ```rust
  if response.nodes.is_empty() && response.skipped > 0 {
      tracing::warn!(
          skipped = response.skipped,
          "build_cluster: corpus has chunks but no UMAP coordinates — run `cqs index --umap`",
      );
  }
  ```

---

## Triage notes

- **OB-V1.30-1** is the highest-leverage finding — fixing it unlocks the value of every span already in the codebase. P1 by impact, easy by effort.
- **OB-V1.30-2** is the only one tied directly to the new auth surface (#1118); SEC-7 shipped without the warn-on-reject side, so the security event is silent.
- **OB-V1.30-3** is the only "medium" — it requires understanding tokio's blocking-pool span propagation. The other nine are all easy.
- **OB-V1.30-9** is bundled because the prior audit (OB-V1.29-4) only patched one site of an idiomatic-but-stale pattern that persists across ~9 lines.
- I did NOT report module-wide gaps in `scout`, `where_to_add`, `gather`, `staleness`, `impact`, or `slot`/`cache`/`serve` — every public function in those modules now has an entry span, confirming the v0.12.1 lesson has been applied. The current observability bar in cqs is high; the remaining gaps are at the rendering / correlation / completeness edges, not the "no spans exist" tier.
