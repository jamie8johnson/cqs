# Contributing to cqs

Thank you for your interest in contributing to cqs!

## Development Setup

**Requires Rust 1.96+** (check with `rustc --version`)

1. Clone the repository:
   ```bash
   git clone https://github.com/jamie8johnson/cqs
   cd cqs
   ```

2. Build:
   ```bash
   cargo build                        # CPU-only
   cargo build --features cuda-index   # with GPU acceleration (requires CUDA)
   ```

3. Run tests:
   ```bash
   cargo test                         # CPU-only
   cargo test --features cuda-index    # with GPU acceleration
   ```

4. Initialize and index (for manual testing):
   ```bash
   cargo run -- init
   cargo run -- index
   cargo run -- "your search query"
   ```

5. Set up pre-commit hook (recommended):
   ```bash
   git config core.hooksPath .githooks
   ```
   This runs `cargo fmt --check` before each commit.

## Code Style

- Run `cargo fmt` before committing
- No clippy warnings: `cargo clippy -- -D warnings`
- Add tests for new features
- Follow existing code patterns

### `_with_*` Function Naming Convention

Functions that accept pre-loaded resources use a `_with_<resource>` suffix:

| Suffix | Meaning | Example |
|--------|---------|---------|
| `_with_graph` | Pre-loaded call graph | `gather_with_graph()` |
| `_with_options` | Config struct parameter | `scout_with_options()` |
| `_with_embedding` | Pre-computed embedding | `get_chunk_with_embedding()` |
| `_with_resources` | Pre-loaded embedder + graph | `task_with_resources()` |

Rules:
- The base function loads its own resources. The `_with_*` variant accepts them.
- Don't stack suffixes (`_with_graph_depth`). Add parameters to the existing `_with_*` function instead.
- If the `_with_*` variant has no external callers, fold it into the base function.

### JSON Output Envelope

JSON output has **two wire shapes**, selected by surface (and, on the CLI direct path, by `CQS_OUTPUT_FORMAT`). The shape is intentionally lean: the default drops redundant keys. Agents still parse a small, predictable set of shapes — but it is not one universal `{data, error, version}` wrapper. See `src/output_format.rs` (`EnvelopeShape`) and `src/cli/json_envelope.rs`. (`EnvelopeShape` is the wire-envelope selector — renamed from the colliding `OutputFormat` in the v1.40 audit; the CLI `--format text|json|mermaid` flag enum keeps the `OutputFormat` name in `src/cli/definitions.rs`.)

**Default lean shapes:**

- **CLI direct (`--json`), success** — `EnvelopeShape::V2Bare` (the default). The handler emits the **bare payload** to stdout, no envelope wrap:
  ```json
  { "name": "search_filtered", "callers": [ ... ] }
  ```
  When the worktree is stale (`EnvelopeMeta::current()` non-default), an object payload gets a `_meta` field spliced in; array/scalar payloads can't carry it and the signal degrades to a `tracing::warn!` on stderr.

- **Batch / daemon line, success** — the slim JSONL shape from `write_json_line` (via `wrap_value`). It carries `data` and drops `error: null` and `version`:
  ```json
  {"data": { ... }, "_meta": {"worktree_stale": true, "stale_origins": ["src/a.rs"]}}
  ```
  `_meta` is emitted **skip-when-empty** — absent when it has no non-default fields. It carries `worktree_stale` / `worktree_name` (process-level) and per-response entries like `stale_origins` from search handlers (merged via `merged_meta_value`).

- **Batch / daemon line, error** — slim error shape from `wrap_error`. Carries `error` and drops `data: null` and `version`:
  ```json
  {"error": {"code": "not_found", "message": "no reference named 'foo'"}}
  ```
  CLI direct error paths emit via `emit_json_error` (see `reference.rs`, which surfaces `not_found` as a structured envelope error in JSON mode).

**`CQS_OUTPUT_FORMAT=v1` opt-in (legacy full envelope):** on the CLI direct success path, set `CQS_OUTPUT_FORMAT=v1` (`EnvelopeShape::V1Envelope`) to restore the wrapped shape:
```json
{ "data": <payload>, "error": null, "version": 1, "_meta": { ... } }
```
The eval harness pins itself to `v1` via env. The batch / daemon JSONL path is **not** affected by this knob — it always uses the slim shape, because self-describing JSONL lines need to stand alone regardless.

**Error codes** (additive taxonomy — `ErrorCode` / `error_codes` in `src/cli/json_envelope.rs`):
- `not_found` — function/file/symbol/reference absent. **Emitted** by production handlers (e.g. `src/cli/commands/infra/reference.rs`).
- `invalid_input` — bad user-supplied argument.
- `parse_error` — failed to parse a query/expression/diff.
- `io_error` — filesystem/network/socket failure. **Emitted** by `redact_error`'s `std::io::Error` downcast on the daemon batch path.
- `internal` — catch-all; carries a redacted summary or a correlation chain-id (`err-<hex>`), with the full anyhow chain logged via `tracing::warn!`.
- `timeout` — operation exceeded its time budget (e.g. `cqs status --wait`).

All six codes can reach a client. The redaction boundary (`redact_error`) downcasts the root cause: sqlx errors → `internal` (query text never leaks), IO errors → `io_error`, unknown → `internal` with a chain-id.

**`version`** (v1 envelope only) is the wire-format version — bump `JSON_OUTPUT_VERSION` on any breaking change to inner `data` payload shapes. The slim default shapes omit it entirely.

**How to emit when adding `--json` to a new command:**
- **CLI direct:** call `crate::cli::json_envelope::emit_json(&output)?` (success) / `emit_json_error(code, message)?` (error) instead of `println!`. `emit_json` resolves `EnvelopeShape` and picks bare-vs-v1 for you, and sanitizes NaN/Infinity to `null`.
- **Batch / daemon:** return a raw `serde_json::Value` from your `dispatch_*` handler — the chokepoint `src/cli/batch/mod.rs::write_json_line` wraps it in the slim shape and splices `_meta`. For per-response meta (like `stale_origins`), build it through `merged_meta_value`.
- **Daemon socket transport:** the outer `{ "status", "output" }` framing in `watch.rs` is transport-level and orthogonal — its `output` field carries the JSONL line as a string.

### JSON Output Field Naming Conventions

All `--json` output uses consistent field names across commands:

| Field | Not | Why |
|-------|-----|-----|
| `line_start` / `line_end` | `line`, `lines` | Separate scalars, not an array or ambiguous singular |
| `name` | `function`, `identifier` | Works for structs, enums, traits, not just functions |
| `score` | `similarity` | Generic — covers RRF, cosine, and risk scores |
| `file` | `origin`, `path` | Matches user mental model; `origin` is too abstract |

Rules:
- **snake_case** for all field names — no camelCase, no kebab-case.
- All output structs use `#[derive(serde::Serialize)]` with serde's default snake_case renaming. Do not use `#[serde(rename = "...")]` unless matching an external schema.
- Use `#[serde(skip_serializing_if = "Option::is_none")]` for optional fields so absent data is omitted (not `null`).
- When adding `--json` to a new command, follow existing output structs (e.g., `ChunkSummary`, `CallerDetail`, `ExplainOutput`) rather than inventing new field names.

## Test Concurrency

`cargo test` runs tests in parallel by default. Most tests are isolated (own
`TempDir`, own `Store`), but three classes of test need explicit serialization
or contention-tolerant assertions. Get this wrong and you ship a flake that
passes in isolation and fails ~1% of the time in CI (#1693).

### 1. GPU / CUDA-context tests → module `GPU_LOCK`

CUDA contexts require serialized access. Every test in `src/cagra.rs` that
builds or searches a `CagraIndex` takes the module-level static:

```rust
static GPU_LOCK: Mutex<()> = Mutex::new(());
// ...
#[test]
fn test_search() {
    let _guard = GPU_LOCK.lock().unwrap();
    if !require_gpu() { return; }
    // ... GPU work ...
}
```

Add `let _guard = GPU_LOCK.lock().unwrap();` as the **first line** of any new
GPU-touching test.

### 2. Env-var-mutating tests → shared lock or `#[serial]`

Env vars are process-global. A test that `set_var`/`remove_var`s a key races
every other test that *reads* that key — even across modules. Two accepted
patterns:

- **`serial_test`** (already a dev-dependency) with a **named group** so only
  tests touching the same env are serialized:
  ```rust
  #[serial_test::serial(cqs_eval_require_fresh_env)]
  ```
  Used in `src/limits.rs`, `src/cli/commands/eval/mod.rs`.
- **A shared module-level mutex** when several tests in one area mutate the same
  keys. `src/hnsw/mod.rs` exposes `pub(crate) static HNSW_ENV_LOCK` for all
  `CQS_HNSW_*` tests; `env_override_tests` and `test_hnsw_for_helpers_pick_tier`
  both lock it. A *single* shared static is required — a per-module mutex does
  not coordinate across modules, which is exactly the gap that caused #1693.

Always restore the env (`remove_var` after `set_var`) before releasing the lock.

### 3. Approximate-index recall assertions → containment + bounded build retry

`HnswIndex` is built with `parallel_insert_data` (rayon) over an
OS-entropy-seeded layer RNG, and `CagraIndex` is an approximate GPU graph.
Under CPU saturation, `parallel_insert` produces a degenerate graph on ~1-2% of
builds where even the cosine-distance-0 self-match vector is unreachable
(measured: 52/3000 parallel vs 0/3000 sequential under 16-core load). Search on
a *fixed* index is deterministic (0/100k misses) — the nondeterminism is purely
in concurrent construction.

Therefore:

- **Never `assert_eq!(results[0].id, expected)`** on an HNSW/CAGRA result.
  Assert **top-k containment** (`results.iter().any(|r| r.id == expected)`).
- For tests whose *intent* is recall (verify the index can find the self-match),
  **retry the build** a bounded number of times so a single degenerate graph
  does not fail the assertion. At 8 retries a transient miss is ~2.5e-14 while a
  systematic recall bug (miss on every build) still fails deterministically. See
  `assert_self_match_reachable` in `src/hnsw/build.rs`.
- For tests whose intent is **lifecycle/soundness** (e.g. `src/hnsw/safety.rs`),
  assert only that results are non-empty and IDs are valid (no corruption) — do
  not assert recall at all.

Use unique temp paths (`TempDir::new()`, never a fixed `temp_dir().join("name")`)
so parallel tests never collide on disk.

## Pull Request Process

1. Fork the repository and create a feature branch
2. Make your changes
3. Ensure all checks pass:
   ```bash
   cargo test --features cuda-index
   cargo clippy --features cuda-index -- -D warnings
   cargo fmt --check
   ```
4. Update documentation if needed (README, CLAUDE.md)
5. Submit PR against `main`

## What to Contribute

### Good First Issues

- Look for issues labeled `good-first-issue`
- Documentation improvements
- Test coverage improvements

### Feature Ideas

- Additional language support (see `src/language/languages.rs` for current list — 54 languages + L5X/L5K PLC exports)
- Non-CUDA GPU support (ROCm for AMD, Metal for Apple Silicon)
- VS Code extension
- Performance improvements
- CLI enhancements

### Bug Reports

When reporting bugs, please include:
- cqs version (`cqs --version`)
- OS and architecture
- Steps to reproduce
- Expected vs actual behavior

## Architecture Overview

```
src/
  cli/          - Command-line interface (clap)
    mod.rs      - Top-level CLI module, re-exports
    definitions.rs - Clap argument definitions, `Commands` enum, and `#[derive(cqs_macros::CqsCommands)]` (PR #1495) which generates dispatch + variant_name + batch_support from per-variant `#[cqs(...)]` attributes
    dispatch.rs - Command dispatch helpers (entry points; per-command arms generated by the proc-macro derive)
    commands/   - Command implementations (organized by category)
      mod.rs      - Top-level re-exports
      resolve.rs  - Target resolution (function name → chunk)
      dispatch_shims.rs - Hand-written `cmd_<variant>_dispatch` shims the `CqsCommands` derive binds by naming convention; each destructures one `Commands` variant via `must_be!` and calls its `cmd_<name>`
      search/     - query, gather, similar, related, where_cmd, scout, onboard, neighbors; search_ctx.rs (SearchCtx trait — the surface-agnostic search context implemented by both CommandContext and the daemon's BatchView, so query_core runs unchanged on either)
      graph/      - callers, deps, explain, impact, impact_diff, test_map, trace; notes_text.rs (shared kind-fallback/redirect note + text consts referenced by both CLI and daemon surfaces)
      review/     - diff_review, ci, dead, health, suggest, affected
      index/      - build, gc, stale, stats, umap
      io/         - blame, brief, context, diff, drift, notes, read, reconstruct
      infra/      - audit_mode, cache_cmd, convert, doctor, hook, init, model, ping, project, reference, slot, status, telemetry_cmd
      eval/       - `cqs eval` A/B harness: mod.rs (cmd_eval, require_fresh_gate), runner.rs (load query set, run search, score against gold — reuses production search path), baseline.rs (diff R@K against a saved EvalReport, regression gate). Shared on-disk types live in `src/eval/` (not here).
      serve.rs   - cmd_serve (auth-gated read-only HTTP UI launcher)
      train/      - export_model, plan, task, train_data, train_pairs
    chat.rs     - Interactive REPL (wraps batch mode with rustyline)
    batch/      - Batch mode: persistent Store + Embedder, stdin commands, JSONL output, pipeline syntax
      mod.rs      - Module root: wire/identity types (DbFileIdentity, CachedReload), vector index builder, evict + JSON-serialization helpers, re-exports, main test suite (split #1691)
      context.rs  - BatchContext struct + impl: shared Store/Embedder/index, per-name reference cache, staleness invalidation (split #1691)
      view.rs     - BatchView snapshot (owned-Arc clones), checkout_view_from_arc/dispatch_via_view glue, SearchCtx impl, refs-LRU helpers (split #1691)
      session.rs  - Session entry points: create_context, cmd_batch stdin line-loop, create_test_context (split #1691)
      commands.rs - BatchInput/BatchCmd parsing, dispatch router
      handlers/ - Daemon dispatch adapters (one per command); each `dispatch_*` is a thin wrapper that parses the wire request into the command's typed `*Args` and calls the shared `*_core`
        mod.rs, analysis.rs, graph.rs, info.rs, misc.rs, search.rs, dispatch_tests.rs (cross-surface parity tests: daemon dispatch == direct core)
      pipeline.rs - Pipeline execution (pipe chaining via `|`)
    args.rs     - Shared CLI/batch arg structs via #[command(flatten)]
    config.rs   - Configuration file loading
    display.rs  - Output formatting, result display
    enrichment.rs - Enrichment pass (extracted from pipeline.rs)
    files.rs    - File enumeration, lock files, path utilities
    json_envelope.rs - JSON output emission helpers: emit_json/emit_json_error (CLI direct, bare-vs-v1 via EnvelopeShape), wrap_value/wrap_error (slim batch/daemon JSONL), ErrorCode taxonomy + error_codes consts, redact_error (daemon error redaction), EnvelopeMeta (_meta worktree-stale + per-response stale_origins merge), NaN/Infinity sanitization
    limits.rs   - Shared clamp ceilings + env-overridable size limits for the CLI and batch/daemon dispatchers (keeps the two paths from drifting on `--limit`). Library-layer counterpart is `src/limits.rs`.
    pipeline/   - Multi-threaded indexing pipeline
      mod.rs, embedding.rs, parsing.rs, types.rs, upsert.rs, windowing.rs
      reuse.rs    - Shared embedding-reuse resolver: global cache → per-slot store cache → split chunks into reuse-cached vs embed-fresh; used by both the bulk pipeline and the watch/daemon incremental path
    signal.rs   - Signal handling (Ctrl+C)
    staleness.rs - Proactive staleness warnings for search results
    telemetry.rs - Optional command usage logging (CQS_TELEMETRY=1)
    store.rs    - Store opening utilities, CommandContext, vector index building
    watch/      - File watcher + daemon (split from watch.rs in PR #1147)
      mod.rs    - WatchConfig/WatchState, cmd_watch entry point, gitignore + WSL helpers
      socket.rs - Unix-socket daemon client handler (handle_socket_client)
      runtime.rs - SIGTERM flag, build_shared_runtime
      rebuild.rs - HNSW background rebuild orchestration + EmbedderBackoff
      gc.rs     - Daemon startup/periodic GC sweeps + last_indexed_mtime prune
      events.rs - collect_events + process_file_changes + process_note_changes
      reindex.rs - reindex_files + reindex_notes + SPLADE encoder helpers
      reconcile.rs - Layer 2 periodic full-tree reconciliation (#1182)
      siblings.rs - Slot-parallel reindex: delta propagation + drains for sibling slots (#1717)
      daemon.rs - spawn_daemon_thread (the --serve accept-loop closure body)
      tests.rs  - watch unit-test bench (#[cfg(test)])
      adversarial_socket_tests.rs - adversarial coverage for handle_socket_client (#[cfg(all(test, unix))])
  watch_status.rs - WatchSnapshot state machine + ops-stats block (`cqs status --watch`) + Arc<RwLock<...>> shared between watch writer and daemon reader (#1182, #1208, #1715)
  daemon_translate.rs - Daemon RPC client + wait_for_fresh helper + DaemonRpcError typed enum (#1211)
  fs.rs        - atomic_replace and other FS helpers
  limits.rs    - Library-side env-overridable size limits (counterpart to `src/cli/limits.rs`; readable from parser/, store/, convert/, nl/fts.rs)
  serde_helpers.rs - Tiny serde predicates for `#[serde(skip_serializing_if = ...)]` (e.g. `is_false`), centralized so envelope-shaped structs don't redeclare them
  ort_helpers.rs - Shared ORT error mapping (`ort_err`) wrapping `ort::Error` into the embedder/reranker/SPLADE `*Error` variants
  aux_model.rs - Auxiliary (small) model paths used by the LLM-summary pipeline
  kind.rs      - Polymorphic-routing Kind enum (Function | Type | Const | Module | Other | Ambiguous | Multiple | NotFound) + classify_chunk_type + classify_hits + detect_kind_for_store (#1610). Every function-or-type-specialized command consults this before its happy-path query.
  output_format.rs - Wire-envelope selector: EnvelopeShape (V1Envelope | V2Bare; gated by CQS_OUTPUT_FORMAT, process-lifetime cached). Default V2Bare emits the bare payload on the CLI direct success path; v1 restores the full envelope. Consumed by emission helpers in src/cli/json_envelope.rs. Distinct from the CLI `--format` flag enum `OutputFormat` in src/cli/definitions.rs. (The CQS_ULTRASECURITY posture knob was removed in #1690 — security signals always emit when meaningful.)
  language/     - Tree-sitter language support (54 languages + L5X/L5K)
    mod.rs      - Language enum (define_languages! macro), LanguageRegistry, LanguageDef, ChunkType
    languages.rs - All 54 language definitions (LanguageDef statics with ..DEFAULTS) + custom functions
    queries/    - Tree-sitter queries (.scm files, loaded via include_str!())
      <lang>.chunks.scm, <lang>.calls.scm, <lang>.types.scm
  test_helpers.rs - Shared test fixtures module
  store/        - SQLite storage layer (Schema v28, WAL mode)
    mod.rs      - Store struct, open/init, FTS5, split_sql_statements (BEGIN/END-aware)
    metadata.rs - Chunk metadata queries, file-level operations
    search.rs   - Store-owned SQL search: search_fts, fts_match_ids (v27 needs_embedding gate), search_by_name (imports nothing from search/ — scoring lives there)
    serve_queries.rs - Typed-row SQL for the `cqs serve` `/api/*` endpoints; serve/data.rs wire builders call these instead of running raw sqlx against the pool
    sparse.rs   - Sparse vector CRUD (SPLADE), upsert_sparse_vectors, prune_orphan,
                  bump_splade_generation_tx, splade_generation()
    chunks/     - Chunk storage and retrieval
      mod.rs, crud.rs, staleness.rs, embeddings.rs, query.rs, async_helpers.rs
    notes.rs    - Note CRUD, note_embeddings(), brute-force search
    calls/      - Call graph storage and queries
      mod.rs, crud.rs, cross_project.rs, dead_code.rs, query.rs, related.rs, test_map.rs
    types.rs    - Type edge storage and queries
    backup.rs   - Filesystem snapshots of `index.db` taken before schema migrations run (covers commit-time I/O failures the migration transaction's rollback can't)
    summary_queue.rs - Write-coalescing queue for streamed LLM-summary inserts (routes through WRITE_LOCK instead of bypassing it with a raw INSERT OR IGNORE)
    helpers/    - Types, embedding conversion, scoring, SQL utilities
      mod.rs, embeddings.rs, error.rs, rows.rs, scoring.rs, search_filter.rs, sql.rs, types.rs
    migrations.rs - Schema migration framework (v10-v28, including v19 FK cascade, v20 trigger, v21 splade tokens, v22 chunks.umap_x/y, v23 reconcile fingerprint, v24 vendored-code trust, v25 notes.kind, v26 composite (source_type, origin) index on chunks, v27 chunks.needs_embedding for skip-first-pass embed under --llm-summaries, v28 chunks.canonical_hash for comment-canonical embedding reuse)
  parser/       - Code parsing (tree-sitter + custom parsers, delegates to language/ registry)
    mod.rs      - Parser struct, parse_file(), parse_file_all(), supported_extensions()
    types.rs    - Chunk (incl. parent_type_name), CallSite, FunctionCalls, TypeRef, ParserError
    chunk.rs    - Chunk extraction, signatures, doc comments, parent type extraction
    calls.rs    - Call graph extraction, callee filtering
    injection.rs - Multi-grammar injection (HTML→JS/CSS via set_included_ranges)
    aspx.rs     - ASP.NET Web Forms (.aspx/.ascx/.asmx) custom parser
    l5x.rs      - Rockwell PLC exports (L5X XML + L5K ASCII) → Structured Text extraction
    markdown/   - Heading-based markdown parser
      mod.rs, headings.rs, code_blocks.rs, tables.rs
  embedder/      - ONNX embedding models (configurable: embeddinggemma-300m default since v1.35.0; bge-large, bge-large-ft, E5-base, v9-200k, nomic-coderank-137M, qwen3-embedding-4b, qwen3-embedding-8b, custom ONNX presets)
    mod.rs      - Module root: error/value types (EmbedderError, Embedding, EmbeddingDimensionError, ExecutionProvider enum — CUDA/TensorRT/CPU; CoreML/ROCm cfg-gated per #956 Phase A), shared consts, re-exports, unit tests (split #1691)
    core.rs     - Embedder struct + impl: session lifecycle, embed()/embed_query()/embed_batch(), runtime dimension detection, model_fingerprint() (split #1691)
    download.rs - Model + tokenizer download / fingerprint helpers (ensure_model, verify_checksum) (split #1691)
    pooling.rs  - Tensor padding (pad_2d_i64*), L2 normalize, mean/cls/last-token pooling, truncate_at_char_boundary (split #1691)
    models.rs   - ModelConfig struct, built-in presets (embeddinggemma-300m default, bge-large, bge-large-ft, e5-base, v9-200k, nomic-coderank, qwen3-embedding-4b, qwen3-embedding-8b), resolution logic, EmbeddingConfig
    provider.rs - ORT execution provider selection — per-backend cfg-blocks; CUDA/TensorRT always-on, CoreML/ROCm scaffolded via `ep-coreml`/`ep-rocm` features (#956 Phase A)
  reranker.rs   - Cross-encoder re-ranking (Reranker trait + OnnxReranker / NoopReranker / LlmReranker impls; default ms-marco-MiniLM-L-6-v2)
  search/       - Search algorithms, name matching, HNSW-guided search
    mod.rs      - search_filtered(), search_unified_with_index(), hybrid RRF
    scoring/    - ScoringConfig, score normalization, RRF fusion constants
      mod.rs, candidate.rs, config.rs, filter.rs, name_match.rs, note_boost.rs
      fusion.rs - RRF reciprocal-rank fusion (rrf_fuse, rrf_fuse_n, rrf_k, set_rrf_k_from_config); search owns fusion (moved out of store/search.rs)
      knob.rs   - Shared resolver for f32 scoring knobs (SCORING_KNOBS table: name, env var, default, range, cache contract — one row per knob)
    mmr.rs      - Maximum Marginal Relevance re-ranking: diversifies the top-K pool to break near-duplicate crowding (same-file/same-name) surfaced by the R@5 audit
    query.rs    - Query parsing, filter extraction
    router.rs   - Query classifier (QueryCategory + SearchStrategy), adaptive routing for
                  identifier/structural/behavioral/conceptual/multi_step/negation/type_filtered/cross_language/unknown;
                  resolve_splade_alpha() for per-category SPLADE fusion weights (env override precedence)
    synonyms.rs - Query synonym expansion
  eval/         - Evaluation surface: shared on-disk types for the `cqs eval` runner and the integration tests in `tests/`
    mod.rs      - Query-set / gold-chunk / report-row types (single source of truth for the eval JSON shape)
    schema.rs   - Deserialization types for the v3 eval query format (`evals/queries/v3_*.json`)
  splade/       - SPLADE sparse encoder + persisted inverted index
    mod.rs      - SpladeEncoder, SparseVector type, encode()/encode_batch(), resolve_splade_model_dir()
    index.rs    - SpladeIndex with persist/load (splade.index.bin + metadata.splade_generation)
  math.rs       - Vector math utilities (cosine similarity, SIMD)
  hnsw/         - HNSW index with batched build, atomic writes
    mod.rs      - HnswIndex, LoadedHnsw (self_cell), HnswError, VectorIndex impl
    build.rs    - build(), build_batched() construction
    search.rs   - Nearest-neighbor search
    persist.rs  - save(), load(), checksum verification
    safety.rs   - Send/Sync and loaded-index safety tests
  convert/      - Document-to-Markdown conversion (optional, "convert" feature)
    mod.rs      - ConvertOptions, convert_path(), format detection
    html.rs     - HTML → Markdown via fast_html2md
    pdf.rs      - PDF → Markdown via Python pymupdf4llm (shell out)
    chm.rs      - CHM → 7z extract → HTML → Markdown
    naming.rs   - Title extraction, kebab-case filename generation
    cleaning.rs - Extensible tag-based cleaning rules (7 rules)
    webhelp.rs  - Web help site detection and multi-page merge
  cache/        - Per-project embedding cache `.cqs/embeddings_cache.db` (SQLite, keyed by content_hash + model_id; #1105) (split #1691)
    mod.rs            - Shared types (CacheError, CacheStats, PerModelStats, CachePurpose), evict locks, perms/WAL helpers, re-exports, shared-runtime tests
    embedding_cache.rs - EmbeddingCache struct + impl (read/write batch, evict, checkpoint_wal)
    query_cache.rs    - QueryCache struct + impl (persistent query-embedding cache)
  slot/         - Named slots — side-by-side full indexes under `.cqs/slots/<name>/` (#1105)
    mod.rs      - slot_dir(), resolve_slot_name() (CQS_SLOT > .cqs/active_slot > "default"), one-shot legacy migration
  cagra.rs      - GPU-accelerated CAGRA index (optional), save/load via cuvsCagraSerialize
  tiered.rs     - cuVS tiered index backend (optional, `tiered-index` feature + fork pin). Brute-force tier absorbs incremental `extend`s; CAGRA ANN tier compacts internally — no periodic rebuild. No persistence (no cuVS serialize); rebuilt from store on daemon restart. Opt-in via `CQS_TIERED_INDEX=1`. See "Tiered-index fork pin" below.
  nl/           - NL description generation, JSDoc parsing
    mod.rs      - Core NL generation, type-aware embeddings, call context
    fts.rs      - FTS5 normalization, tokenization
    fields.rs   - Field/keyword extraction from code bodies
    markdown.rs - Markdown-specific NL generation
  note.rs       - Developer notes with sentiment, rewrite_notes_file()
  diff.rs       - Semantic diff between indexed snapshots
  drift.rs      - Drift detection (semantic change magnitude between snapshots)
  reference.rs  - Multi-index: ReferenceIndex, load, search, merge
  gather.rs     - Smart context assembly (BFS call graph expansion)
  structural.rs - Structural pattern matching on code chunks
  project.rs    - Cross-project search registry
  audit.rs    - Audit mode persistence and duration parsing
  focused_read.rs - Focused read logic (extract type dependencies)
  impact/         - Impact analysis (callers + affected tests + diff-aware)
    mod.rs      - Public API, re-exports
    types.rs    - Impact types (CallerDetail, RiskScore, etc.)
    analysis.rs - suggest_tests, find_transitive_callers, extract_call_snippet_from_cache
    diff.rs     - analyze_diff_impact, map_hunks_to_functions
    cross_project.rs - Cross-project impact analysis and trace
    bfs.rs      - reverse_bfs, reverse_bfs_multi_attributed, test_reachability, forward_bfs_multi (used by suggest_tests, #1115)
    format.rs   - JSON/Mermaid formatting
    hints.rs    - compute_hints, compute_hints_batch, compute_risk_batch, risk scoring
    test_map.rs - Shared test-map algorithm (reverse BFS from function to test chunks)
  related.rs      - Co-occurrence analysis (shared callers, callees, types)
  scout.rs        - Pre-investigation dashboard (search + callers/tests + staleness + notes)
  task.rs         - Single-call implementation brief (scout + gather + impact + placement + notes)
  onboard.rs      - Guided codebase tour (entry point + call chain + callers + types + tests)
  review.rs       - Diff review (impact-diff + notes + risk scoring)
  ci.rs           - CI pipeline (review + dead code + gate logic)
  where_to_add.rs - Placement suggestion (semantic search + pattern extraction)
  plan.rs         - Task planning with 11 task-type templates
  diff_parse.rs   - Unified diff parser for impact-diff
  health.rs     - Codebase quality snapshot (dead code, staleness, hotspots)
  suggest.rs    - Auto-suggest notes from code patterns
  config.rs     - Configuration file support
  vendored.rs   - Vendored-content detection (#1221): default prefix list (`vendor`, `node_modules`, `third_party`, `.cargo`, `target`, `dist`, `build`) + path-segment matcher used at index time to flag chunks for the `trust_level: "vendored-code"` downgrade. Override via `[index].vendored_paths` in `.cqs.toml`.
  worktree.rs   - Git-worktree → main-project-`.cqs/` discovery (#1254). When `cqs` runs from inside a worktree without its own `.cqs/`, `resolve_index_dir` parses the worktree's `.git` file, follows `commondir` to the main project, and serves queries from main's index. Every JSON envelope from that process gets `_meta.worktree_stale: true` so consuming agents know the served snapshot is from main's branch.
  index.rs      - VectorIndex trait (HNSW, CAGRA)
  llm/          - LLM summary generation, HyDE query predictions via Anthropic Batches API
    mod.rs, batch.rs (BatchPhase2, submit_batch_prebuilt), doc_comments.rs, hyde.rs, prompts.rs (build_contrastive_prompt), provider.rs (BatchProvider trait, BatchSubmitItem, MockBatchProvider for tests), summary.rs (find_contrastive_neighbors)
    local.rs    - Local / OpenAI-compat batch provider (`/v1/chat/completions`: llama.cpp, vLLM, Ollama, LMStudio) — fans out a worker pool for synchronous per-item inference behind the async batch interface
    redirect.rs - Redirect policy for bearer-bearing HTTP clients (`same_origin_redirect_policy`): refuses cross-origin redirects with a loud fail-fast so the `Authorization: Bearer` header can't leak to a redirect target, and caps same-origin hops
    validation.rs - Validates LLM summary output before caching (indirect-prompt-injection defence; modes via `CQS_SUMMARY_VALIDATION`)
  doc_writer/   - Doc comment generation and source file rewriting (SQ-8, optional "llm-summaries" feature)
    mod.rs      - DocCommentResult, module exports
    formats.rs  - Per-language doc comment formatting (prefix, position, wrapping)
    rewriter.rs - Source file rewriter: find insertion point, apply edits bottom-up, atomic write
  train_data/   - Fine-tuning training data generation from git history
    mod.rs      - TrainDataConfig, generate_training_data(), Triplet types
    bm25.rs     - BM25 index for hard negative mining
    checkpoint.rs - Resume support for long generation runs
    diff.rs     - Git diff parsing for function-level changes
    git.rs      - Git history traversal (log, show, diff-tree)
    query.rs    - Query normalization for training pairs
  serve/        - `cqs serve` web UI (gated on `serve` feature; axum + tower)
    mod.rs      - run_server, build_router, server wiring
    handlers.rs - axum route handlers (search, graph, hierarchy, cluster, chunk detail); each emits a tracing event and wraps sync Store calls in spawn_blocking
    data.rs     - Wire-format types + builders for `/api/*` (Node/Edge shapes matching Cytoscape.js element-data convention); wire-shaping only — SQL lives in store/serve_queries.rs
    error.rs    - HTTP-side error type wrapping StoreError → 4xx/5xx responses
    assets.rs   - Static assets baked into the binary via `include_str!` (index.html + app.css + app.js; no request-time filesystem reads)
    auth.rs     - Per-launch auth token: 256-bit URL-safe base64, constant-time compare, Bearer/cookie/?token= surfaces (#1118 / SEC-7)
    tests.rs    - Router + auth integration tests (test_router_with_auth helper, host allowlist, gzip)
  main.rs       - Binary entry point: clap parse, tracing-subscriber init, dispatch into `cli`
  lib.rs        - Public API
.claude/
  skills/       - Claude Code skills (auto-discovered)
    groom-notes/  - Interactive note review and cleanup
    update-tears/ - Session state capture for context persistence
    release/      - Version bump, changelog, publish workflow
    audit/        - 16-category code audit with parallel agents
    red-team/     - Adversarial security audit (attacker mindset, PoC-required)
    pr/           - WSL-safe PR creation
    cqs-bootstrap/ - New project setup with tears infrastructure
    cqs/          - Unified CLI dispatcher (search, graph, quality, notes, infrastructure)
    reindex/      - Rebuild index with before/after stats
    docs-review/  - Check project docs for staleness
    migrate/      - Schema version upgrades
    troubleshoot/ - Diagnose common cqs issues
    cqs-batch/    - Batch mode with pipeline syntax
    cqs-plan/     - Task planning with templates
    before-edit/  - Pre-edit workflow: snapshot state before changes
    investigate/  - Investigation workflow: structured code exploration
    check-my-work/ - Post-implementation verification checklist
    cqs-verify/   - Exercise all command categories, catch regressions
```

**Key design notes:**
- Configurable embeddings (embeddinggemma-300m 768-dim default since v1.35.0; bge-large 1024-dim, bge-large-ft 1024-dim, E5-base 768-dim, nomic-coderank-137M 768-dim, qwen3-embedding-4b 2560-dim, qwen3-embedding-8b 4096-dim, custom ONNX presets)
- HNSW index is chunk-only; notes use brute-force SQLite search (always fresh)
- Streaming HNSW build via `build_batched()` for memory efficiency
- Large chunks split by windowing (480 tokens, 64 overlap); notes capped at 10k entries
- Schema migrations allow upgrading indexes without full rebuild
- Skills in `.claude/skills/*/SKILL.md` are auto-discovered by Claude Code

## Tiered-index fork pin

The optional `tiered-index` feature builds the cuVS **tiered index** backend
(`src/tiered.rs`): a brute-force tier that absorbs incremental `extend`s coupled
with a CAGRA ANN tier that cuVS compacts internally. It exists to retire the
watch loop's periodic full HNSW rebuild — incremental adds flow into the
brute-force tier and stay searchable immediately, and there is no separate
"rebuild every N inserts" pass.

**Why a fork.** The Rust `cuvs::tiered_index` bindings are not in an official
cuVS release yet — they live on our upstream PR branch (rapidsai/cuvs#2235).
cqs consumes them through a Cargo `[patch.crates-io]` pointing at the fork
branch:

- Fork: `jamie8johnson/cuvs`, branch `cqs-tiered-26.6`
  (commit `5081e4eb592a57b665c01f2e0af3230ccf07dd87`).
- That branch is the PR branch (`rust-tiered-index`) plus a one-commit
  version-pin: the cuvs workspace version is set `26.8.0 → 26.6.0` so the
  patched crate matches cqs's `cuvs = "=26.6"` (strict coupling with conda
  libcuvs 26.06, which already exports the tiered C API). The PR branch itself
  is never moved.

**How the gate keeps `cuda-index` honest.** The `[patch.crates-io]` block in the
root `Cargo.toml` is **commented out by default**. A plain `--features
cuda-index` build (and every crates.io consumer, who never sees `[patch]`) thus
resolves the *official* cuvs 26.6 and never references the tiered module
(`src/tiered.rs` is `#[cfg(feature = "tiered-index")]`-gated, so no tiered
symbols are required). To build the tiered backend, uncomment the two patch
lines and build `--features tiered-index`.

**Using it.** Even with `tiered-index` compiled in, the backend is **opt-in at
runtime** via `CQS_TIERED_INDEX=1`; unset, selection falls through to CAGRA then
HNSW exactly as before. When opted in and the corpus clears the CAGRA threshold
on a GPU, the tiered backend (priority 150) shadows CAGRA (100). The tiered
index has **no persistence** (the cuVS C API offers no serialize/deserialize),
so the daemon rebuilds it from the store on restart — this still removes the
*periodic* rebuild; only the one-time cold-start build remains (same cost as the
CAGRA build it replaces).

**Retirement.** When rapidsai/cuvs#2235 merges and the tiered bindings ship in
an official cuvs release, drop the `tiered-index` feature's fork dependency:
delete the `[patch.crates-io]` block, bump `cuvs` to the release that carries
the bindings, and the module compiles unchanged against the official crate.
This is the same playbook used to retire the previous cuvs fork (#1679).

## Adding a New CLI Command

Checklist for every new command:

1. **Implementation** — `src/cli/commands/<category>/<name>.rs` with the core logic (pick category: search/, graph/, review/, index/, io/, infra/, train/). Follow the surface-agnostic core pattern established by the command-core unification (#1688–#1698): a typed `<Name>Args` (input, derives `Deserialize`), a typed `<Name>Output` (the single JSON schema, derives `Serialize`), and a `<name>_core(ctx, args) -> Result<<Name>Output>` holding all logic and never printing or reading env posture. The CLI `cmd_<name>` and, where one exists, the daemon `dispatch_<name>` are thin adapters that build `Args`, call the core, and render. Logic lives in the core, not the adapters.
2. **Category mod.rs** — add `mod <name>;` + `pub(crate) use <name>::*;` in `src/cli/commands/<category>/mod.rs`
3. **CLI definition** — `Commands` enum variant in `src/cli/definitions.rs` with clap args
4. **Derive surfaces** — `Commands` enum variants pick up `variant_name()`, `batch_support()`, and dispatch routing from `#[derive(cqs_macros::CqsCommands)]` automatically (PR #1495 / #1500). Every variant needs a `#[cqs_cmd(...)]` attribute with two required keys: `group = "a"` (no-store / lifecycle / mutation) or `group = "b"` (store-using), and `batch = "cli"` (in-process only), `"daemon"` (answerable by the daemon), or `"runtime"` (defers to a `<variant_snake>_batch_support` helper — used when support depends on the inner subcommand). An optional `name = "..."` overrides the telemetry label when the kebab-case default doesn't match. See existing variants in `src/cli/definitions.rs` and the macro in `cqs-macros/src/lib.rs`.
5. **Dispatch shim** — handlers bind by naming convention, not attribute: the derive calls `cmd_<variant_snake>_dispatch`, which you write by hand in `src/cli/commands/dispatch_shims.rs` (standardized signature; destructure the variant with `must_be!` and call your `cmd_<name>`). A missing or mis-shaped shim is a single compile error from the derive's const existence guard.
6. **Daemon wiring** (only for `batch = "daemon"`) — three more edit sites: a `BatchCmd` variant in `src/cli/batch/commands.rs` (reuse the shared `*Args` struct so CLI and batch share one source of flags), a `dispatch_<name>` adapter in the matching `src/cli/batch/handlers/` module (thin: parse wire args into the core's `*Args`, call the core, serialize the typed output), and a CLI==daemon parity test following the `src/cli/batch/handlers/dispatch_tests.rs` pattern (dispatch one line, assert the envelope shape).
7. **`--json` support** — serde serialization for programmatic output
8. **Tracing** — `tracing::info_span!` at entry, `tracing::warn!` on error fallback
9. **Error handling** — `Result` propagation, no bare `.unwrap_or_default()` in production
10. **Tests** — happy path + empty input + error path + edge cases
11. **CLAUDE.md** — add to the command reference section
12. **Skills** — add to `.claude/skills/cqs/SKILL.md` and `.claude/skills/cqs-bootstrap/SKILL.md`
13. **CHANGELOG** — entry in the next release section

Pattern to follow: look at `src/cli/commands/io/blame.rs` or `src/cli/commands/review/dead.rs` for a minimal example.

### Dry-Run vs Apply

Commands that touch the filesystem split into two families with opposite
defaults. Pick the one that matches the command's purpose:

- **Side-effect commands** (`index`, `convert`) exist *to* mutate — writing the
  index or the converted `.md` files is the point. They **default to mutating**
  and expose an opt-out `--dry-run` flag that previews the work without writing.
- **Analyser commands** (`doctor`, `suggest`) exist *to* report — their primary
  output is the analysis, and any mutation is a follow-up the user asks for.
  They **default to read-only** and require an explicit opt-in (`--fix`,
  `--apply`) to mutate.

The rule keeps the dangerous default safe: a command whose name promises a
mutation may perform it unprompted, but a command whose name promises a report
never surprises the caller by editing their tree. When adding a new
filesystem-touching command, classify it first, then wire the matching default
+ flag (`--dry-run` to opt out of mutation, `--fix`/`--apply` to opt in).

## Adding Injection Rules (Multi-Grammar)

Files like HTML contain embedded languages (`<script>` → JS, `<style>` → CSS). cqs handles this via injection rules on `LanguageDef`.

**To add injection rules for a new host language:**

1. Define `InjectionRule` entries in the language's `LanguageDef` (in `src/language/languages.rs`, where all `LanguageDef` statics live):
   ```rust
   injections: &[
       InjectionRule {
           container_kind: "script_element",  // outer tree node kind
           content_kind: "raw_text",          // child node with embedded content
           target_language: "javascript",     // must match a Language variant name
           detect_language: Some(detect_fn),  // optional: inspect attributes for lang override
       },
   ],
   ```

2. `container_kind` / `content_kind` must match the host grammar's node kinds (inspect with `tree-sitter parse`).

3. `target_language` must be a valid `Language` name with a grammar (validated at runtime in `find_injection_ranges`).

4. `detect_language` receives the container node and source — return `Some("typescript")` to override the default, `Some("_skip")` to skip the container entirely, or `None` for the default.

5. Injection is single-level only. Inner languages are not re-scanned for their own injections.

6. The two-phase flow in `parse_file` and `parse_file_relationships` automatically handles injection when `injections` is non-empty. No changes needed outside the language definition.

**Key files:** `src/language/mod.rs` (InjectionRule struct), `src/parser/injection.rs` (parsing logic), `src/language/languages.rs` (HTML definition with injection rules as reference).

## Adding a New Language

Adding a language is a data-entry task. Write query files, add a `LanguageDef` static, register it.

### Prerequisites

- A tree-sitter grammar published on crates.io (search `tree-sitter-<lang>`)
- A sample source file to test with
- The grammar's `node-types.json` (in `~/.cargo/registry/src/*/tree-sitter-<lang>-*/src/node-types.json` after `cargo check`)

### Steps

**1. Add the dependency to `Cargo.toml`:**

```toml
tree-sitter-newlang = { version = "0.X", optional = true }
```

And the feature flag:
```toml
lang-newlang = ["dep:tree-sitter-newlang"]
```

Add `"lang-newlang"` to the `default` and `lang-all` feature lists.

**2. Create query files:**

Create `src/language/queries/newlang.chunks.scm` with tree-sitter patterns:
```scheme
(function_declaration
  name: (identifier) @name) @function

(class_declaration
  name: (identifier) @name) @class
```

Optionally create `newlang.calls.scm` (call extraction) and `newlang.types.scm` (type edges).

Discover node types from the grammar's `node-types.json` or `tree-sitter parse sample.ext`.

**3. Add definition to `src/language/languages.rs`:**

Add a `LanguageDef` static using `..DEFAULTS` for all optional fields. Only specify fields that differ from defaults:

```rust
#[cfg(feature = "lang-newlang")]
static LANG_NEWLANG: LanguageDef = LanguageDef {
    name: "newlang",
    grammar: Some(|| tree_sitter_newlang::LANGUAGE.into()),
    extensions: &["nl"],
    chunk_query: include_str!("queries/newlang.chunks.scm"),
    call_query: Some(include_str!("queries/newlang.calls.scm")),
    doc_nodes: &["comment"],
    stopwords: &["if", "else", "for", "while", "return"],
    entry_point_names: &["main"],
    ..DEFAULTS
};

#[cfg(feature = "lang-newlang")]
pub fn definition_newlang() -> &'static LanguageDef {
    &LANG_NEWLANG
}
```

See Bash (simplest) or Rust/HTML (complex, with custom functions and injections) in `languages.rs` for reference.

**4. Register in `src/language/mod.rs`:**

Add one line to `define_languages!`:
```rust
NewLang => "newlang", feature = "lang-newlang", def = languages::definition_newlang;
```

**5. Write tests in `tests/language_test.rs`:**

Minimum 3 tests: parse a function, parse a class/struct, parse doc comments.

```rust
#[test]
fn test_newlang_parse_function() {
    let content = r#"func hello() { print("hi") }"#;
    let file = write_temp_file(content, "nl");
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_file(file.path()).unwrap();
    assert!(chunks.iter().any(|c| c.name == "hello" && c.chunk_type == ChunkType::Function));
}
```

**6. Build and test:**

```bash
cargo test --features cuda-index -- newlang
```

### Fields Reference

All fields except `name`, `grammar`, `extensions`, `chunk_query` have defaults via `..DEFAULTS`. Important optional fields:

| Field | Default | When to set |
|-------|---------|-------------|
| `call_query` | `None` | If the grammar has call/invocation nodes |
| `type_query` | `None` | For type dependency edges |
| `signature_style` | `UntilBrace` | `UntilColon` for Python-like, `FirstLine` for Ruby-like |
| `doc_nodes` | `&[]` | Node kinds containing doc comments |
| `stopwords` | `&[]` | Language keywords to filter from NL descriptions |
| `common_types` | `&[]` | Stdlib types to exclude from type edges |
| `field_style` | `None` | `NameFirst` or `TypeFirst` for struct field extraction |
| `post_process_chunk` | `None` | Custom logic to rename/retype/filter chunks |
| `extract_return_nl` | `\|_\| None` | Return type extraction for NL descriptions |
| `injections` | `&[]` | Multi-grammar rules (e.g., HTML→JS/CSS) |

### Required updates (the tests enforce these)

- Add `#[cfg(feature = "lang-newlang")] { expected += 1; }` to `test_registry_all_languages` in `src/language/mod.rs`
- Add `"newlang" => Some("newlang")` to `normalize_lang()` in `src/parser/markdown/code_blocks.rs`

### Ecosystem updates (after the language works)

- Update language count in README.md (Supported Languages section + TL;DR), lib.rs, Cargo.toml
- Update `CHANGELOG.md`

## Adding a New Chunk Type

Chunk types are defined in a single macro invocation. Adding one is a data-entry task.

### Steps

**1. Add the variant to `define_chunk_types!` in `src/language/mod.rs`:**

```rust
/// Brief description of what this chunk type represents
MyType => "mytype";
```

Use `capture = "alt"` if the tree-sitter capture name differs from the display name (e.g., `Constant => "constant", capture = "const"`).

**2. Classify in `is_callable()` and `is_code()`** (same file):

- **Callable + code**: appears in call graphs and default search (functions, methods, endpoints, etc.)
- **Code but not callable**: in search but not call graphs (structs, enums, constants, etc.)
- **Not code**: excluded from default search (sections, config keys, etc.)

The `test_all_chunk_types_classified` test enforces exhaustive classification — it won't compile if you skip this.

**3. Add tree-sitter query captures** in the relevant `.chunks.scm` files, or handle via `post_process_chunk` on the language's `LanguageDef`.

**4. Add tests** — at minimum, verify a chunk with the new type is parsed from a sample file and classified correctly.

**5. Update docs** — chunk type count in README.md (How It Works section) and CHANGELOG.md.

## Questions?

Open an issue for questions or discussions.
