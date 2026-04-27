# Contributing to cqs

Thank you for your interest in contributing to cqs!

## Development Setup

**Requires Rust 1.95+** (check with `rustc --version`)

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
| `_with_embedding` | Pre-computed embedding | `suggest_placement_with_embedding()` |
| `_with_resources` | Pre-loaded embedder + graph | `task_with_resources()` |

Rules:
- The base function loads its own resources. The `_with_*` variant accepts them.
- Don't stack suffixes (`_with_graph_depth`). Add parameters to the existing `_with_*` function instead.
- If the `_with_*` variant has no external callers, fold it into the base function.

### JSON Output Envelope

Every JSON-emitting command (CLI `--json`, batch line, daemon socket response) wraps its payload in a uniform envelope so agents parse one shape across all commands.

**Success:**
```json
{ "data": <payload>, "error": null, "version": 1 }
```

**Failure (batch / daemon):**
```json
{ "data": null, "error": { "code": "...", "message": "..." }, "version": 1 }
```

CLI command failures still propagate via `anyhow → stderr` for now; the envelope error path is reserved for batch and future CLI migration.

**Error codes** (small, additive — see `src/cli/json_envelope.rs::error_codes`):
- `not_found` — function/file/symbol absent
- `invalid_input` — bad user-supplied argument
- `parse_error` — failed to parse a query/expression/diff
- `io_error` — filesystem/network failure
- `internal` — anything else (carries the full anyhow chain in `message`)

Today only `internal`, `invalid_input`, and `parse_error` are emitted by production handlers; `not_found` and `io_error` are reserved for future use and currently collapse into `internal`.

**`version`** is the wire-format version. Bump on any breaking change to inner `data` payload shapes; the envelope itself stays stable across versions.

**How to emit:**
- CLI handlers call `crate::cli::json_envelope::emit_json(&output)?` instead of `println!("{}", serde_json::to_string_pretty(&output)?)`.
- Batch handlers return raw `serde_json::Value` from `dispatch()` — the chokepoint at `src/cli/batch/mod.rs::write_json_line` wraps every line.
- The daemon socket `{ "status", "output" }` framing is transport-level and orthogonal — its `output` field carries this envelope as a string.

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
    definitions.rs - Clap argument definitions and command enum
    registry.rs - `for_each_command!` table; single source of truth for dispatch + variant_name + batch_support
    dispatch.rs - Command dispatch helpers (entry points; per-command arms generated from `registry.rs`)
    commands/   - Command implementations (organized by category)
      mod.rs      - Top-level re-exports
      resolve.rs  - Target resolution (function name → chunk)
      search/     - query, gather, similar, related, where_cmd, scout, onboard, neighbors
      graph/      - callers, deps, explain, impact, impact_diff, test_map, trace
      review/     - diff_review, ci, dead, health, suggest, affected
      index/      - build, gc, stale, stats
      io/         - blame, brief, context, diff, drift, notes, read, reconstruct
      infra/      - audit_mode, cache_cmd, convert, doctor, init, project, reference, telemetry_cmd
      train/      - export_model, plan, task, train_data, train_pairs
    chat.rs     - Interactive REPL (wraps batch mode with rustyline)
    batch/      - Batch mode: persistent Store + Embedder, stdin commands, JSONL output, pipeline syntax
      mod.rs      - BatchContext, vector index builder, main loop
      commands.rs - BatchInput/BatchCmd parsing, dispatch router
      handlers/ - Handler functions (one per command)
        mod.rs, analysis.rs, graph.rs, info.rs, misc.rs, search.rs
      pipeline.rs - Pipeline execution (pipe chaining via `|`)
      types.rs    - Output types (ChunkOutput, normalize_path)
    args.rs     - Shared CLI/batch arg structs via #[command(flatten)]
    config.rs   - Configuration file loading
    display.rs  - Output formatting, result display
    enrichment.rs - Enrichment pass (extracted from pipeline.rs)
    files.rs    - File enumeration, lock files, path utilities
    pipeline/   - Multi-threaded indexing pipeline
      mod.rs, embedding.rs, parsing.rs, types.rs, upsert.rs, windowing.rs
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
      daemon.rs - spawn_daemon_thread (the --serve accept-loop closure body)
      tests.rs  - watch unit-test bench (#[cfg(test)])
      adversarial_socket_tests.rs - adversarial coverage for handle_socket_client (#[cfg(all(test, unix))])
  language/     - Tree-sitter language support (54 languages + L5X/L5K)
    mod.rs      - Language enum (define_languages! macro), LanguageRegistry, LanguageDef, ChunkType
    languages.rs - All 54 language definitions (LanguageDef statics with ..DEFAULTS) + custom functions
    queries/    - Tree-sitter queries (.scm files, loaded via include_str!())
      <lang>.chunks.scm, <lang>.calls.scm, <lang>.types.scm
  test_helpers.rs - Shared test fixtures module
  store/        - SQLite storage layer (Schema v22, WAL mode)
    mod.rs      - Store struct, open/init, FTS5, split_sql_statements (BEGIN/END-aware)
    metadata.rs - Chunk metadata queries, file-level operations
    search.rs   - RRF fusion, search_filtered, search_unified_with_index
    sparse.rs   - Sparse vector CRUD (SPLADE), upsert_sparse_vectors, prune_orphan,
                  bump_splade_generation_tx, splade_generation()
    chunks/     - Chunk storage and retrieval
      mod.rs, crud.rs, staleness.rs, embeddings.rs, query.rs, async_helpers.rs
    notes.rs    - Note CRUD, note_embeddings(), brute-force search
    calls/      - Call graph storage and queries
      mod.rs, crud.rs, cross_project.rs, dead_code.rs, query.rs, related.rs, test_map.rs
    types.rs    - Type edge storage and queries
    helpers/    - Types, embedding conversion, scoring, SQL utilities
      mod.rs, embeddings.rs, error.rs, rows.rs, scoring.rs, search_filter.rs, sql.rs, types.rs
    migrations.rs - Schema migration framework (v10-v22, including v19 FK cascade, v20 trigger, v21 splade tokens, v22 chunks.umap_x/y)
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
  embedder/      - ONNX embedding models (configurable: BGE-large-en-v1.5 default; E5-base, nomic-coderank-137M, custom ONNX presets)
    mod.rs      - Embedder struct, embed(), batch embedding, runtime dimension detection, ExecutionProvider enum (CUDA/TensorRT/CPU; CoreML/ROCm cfg-gated per #956 Phase A)
    models.rs   - ModelConfig struct, built-in presets (e5-base, bge-large, nomic-coderank), resolution logic, EmbeddingConfig
    provider.rs - ORT execution provider selection — per-backend cfg-blocks; CUDA/TensorRT always-on, CoreML/ROCm scaffolded via `ep-coreml`/`ep-rocm` features (#956 Phase A)
  reranker.rs   - Cross-encoder re-ranking (ms-marco-MiniLM-L-6-v2)
  search/       - Search algorithms, name matching, HNSW-guided search
    mod.rs      - search_filtered(), search_unified_with_index(), hybrid RRF
    scoring/    - ScoringConfig, score normalization, RRF fusion constants
      mod.rs, candidate.rs, config.rs, filter.rs, name_match.rs, note_boost.rs
    query.rs    - Query parsing, filter extraction
    router.rs   - Query classifier (QueryCategory + SearchStrategy), adaptive routing for
                  identifier/structural/behavioral/conceptual/multi_step/negation/type_filtered/cross_language/unknown;
                  resolve_splade_alpha() for per-category SPLADE fusion weights (env override precedence)
    synonyms.rs - Query synonym expansion
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
  cache.rs      - Per-project embedding cache `.cqs/embeddings_cache.db` (SQLite, keyed by content_hash + model_id; #1105)
  slot/         - Named slots — side-by-side full indexes under `.cqs/slots/<name>/` (#1105)
    mod.rs      - slot_dir(), resolve_slot_name() (CQS_SLOT > .cqs/active_slot > "default"), one-shot legacy migration
  cagra.rs      - GPU-accelerated CAGRA index (optional), save/load via cuvsCagraSerialize
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
  index.rs      - VectorIndex trait (HNSW, CAGRA)
  llm/          - LLM summary generation, HyDE query predictions via Anthropic Batches API
    mod.rs, batch.rs (BatchPhase2, submit_batch_prebuilt), doc_comments.rs, hyde.rs, prompts.rs (build_contrastive_prompt), provider.rs (BatchProvider trait, BatchSubmitItem, LlmProvider), summary.rs (find_contrastive_neighbors)
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
    mod.rs      - run_server, build_router, route handlers (search, graph, hierarchy, cluster, chunk detail)
    auth.rs     - Per-launch auth token: 256-bit URL-safe base64, constant-time compare, Bearer/cookie/?token= surfaces (#1118 / SEC-7)
    tests.rs    - Router + auth integration tests (test_router_with_auth helper, host allowlist, gzip)
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
- Configurable embeddings (BGE-large 1024-dim default; E5-base 768-dim, nomic-coderank-137M 768-dim, custom ONNX presets)
- HNSW index is chunk-only; notes use brute-force SQLite search (always fresh)
- Streaming HNSW build via `build_batched()` for memory efficiency
- Large chunks split by windowing (480 tokens, 64 overlap); notes capped at 10k entries
- Schema migrations allow upgrading indexes without full rebuild
- Skills in `.claude/skills/*/SKILL.md` are auto-discovered by Claude Code

## Adding a New CLI Command

Checklist for every new command:

1. **Implementation** — `src/cli/commands/<category>/<name>.rs` with the core logic (pick category: search/, graph/, review/, index/, io/, infra/, train/)
2. **Category mod.rs** — add `mod <name>;` + `pub(crate) use <name>::*;` in `src/cli/commands/<category>/mod.rs`
3. **CLI definition** — `Commands` enum variant in `src/cli/definitions.rs` with clap args
4. **Registry row** — add a `(bind, wild, name, batch_support, body)` row to `group_a` or `group_b` in `src/cli/registry.rs`. The `for_each_command!` macro generates dispatch + variant_name + batch_support from this single row; a missing row is a compile error.
5. **`--json` support** — serde serialization for programmatic output
6. **Tracing** — `tracing::info_span!` at entry, `tracing::warn!` on error fallback
7. **Error handling** — `Result` propagation, no bare `.unwrap_or_default()` in production
8. **Tests** — happy path + empty input + error path + edge cases
9. **CLAUDE.md** — add to the command reference section
10. **Skills** — add to `.claude/skills/cqs/SKILL.md` and `.claude/skills/cqs-bootstrap/SKILL.md`
11. **CHANGELOG** — entry in the next release section

Pattern to follow: look at `src/cli/commands/io/blame.rs` or `src/cli/commands/review/dead.rs` for a minimal example.

## Adding Injection Rules (Multi-Grammar)

Files like HTML contain embedded languages (`<script>` → JS, `<style>` → CSS). cqs handles this via injection rules on `LanguageDef`.

**To add injection rules for a new host language:**

1. Define `InjectionRule` entries in the language's `LanguageDef` (`src/language/<lang>.rs`):
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
