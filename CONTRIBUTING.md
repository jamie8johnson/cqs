# Contributing to cqs

Thank you for your interest in contributing to cqs!

## Development Setup

**Requires Rust 1.93+** (check with `rustc --version`)

1. Clone the repository:
   ```bash
   git clone https://github.com/jamie8johnson/cqs
   cd cqs
   ```

2. Build:
   ```bash
   cargo build                        # CPU-only
   cargo build --features gpu-index   # with GPU acceleration (requires CUDA)
   ```

3. Run tests:
   ```bash
   cargo test                         # CPU-only
   cargo test --features gpu-index    # with GPU acceleration
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

## Pull Request Process

1. Fork the repository and create a feature branch
2. Make your changes
3. Ensure all checks pass:
   ```bash
   cargo test --features gpu-index
   cargo clippy --features gpu-index -- -D warnings
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

- Additional language support (see `src/language/` for current list — 51 languages supported)
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
    dispatch.rs - Command dispatch (match on command, call handlers)
    commands/   - Command implementations
      mod.rs, query.rs, index.rs, stats.rs, graph.rs, init.rs, doctor.rs, notes.rs, reference.rs, similar.rs, explain.rs, diff.rs, drift.rs, trace.rs, impact.rs, impact_diff.rs, test_map.rs, context.rs, resolve.rs, dead.rs, gc.rs, gather.rs, project.rs, audit_mode.rs, read.rs, stale.rs, related.rs, where_cmd.rs, scout.rs, onboard.rs, convert.rs, review.rs, ci.rs, health.rs, suggest.rs, deps.rs, task.rs, blame.rs, plan.rs, train_data.rs, export_model.rs
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
    pipeline.rs - Multi-threaded indexing pipeline
    signal.rs   - Signal handling (Ctrl+C)
    staleness.rs - Proactive staleness warnings for search results
    telemetry.rs - Optional command usage logging (CQS_TELEMETRY=1)
    watch.rs    - File watcher for incremental reindexing
  language/     - Tree-sitter language support
    mod.rs      - Language enum, LanguageRegistry, LanguageDef, ChunkType
    rust.rs, python.rs, typescript.rs, javascript.rs, go.rs, c.rs, cpp.rs, java.rs, csharp.rs, fsharp.rs, powershell.rs, scala.rs, ruby.rs, bash.rs, hcl.rs, kotlin.rs, swift.rs, objc.rs, sql.rs, protobuf.rs, graphql.rs, php.rs, lua.rs, zig.rs, r.rs, yaml.rs, toml_lang.rs, elixir.rs, erlang.rs, gleam.rs, haskell.rs, julia.rs, ocaml.rs, css.rs, perl.rs, html.rs, json.rs, xml.rs, ini.rs, nix.rs, make.rs, latex.rs, solidity.rs, cuda.rs, glsl.rs, svelte.rs, razor.rs, vbnet.rs, vue.rs, aspx.rs, markdown.rs
  test_helpers.rs - Shared test fixtures module
  store/        - SQLite storage layer (Schema v16, WAL mode)
    mod.rs      - Store struct, open/init, FTS5
    metadata.rs - Chunk metadata queries, file-level operations
    search.rs   - RRF fusion, search_filtered, search_unified_with_index
    chunks/     - Chunk storage and retrieval
      mod.rs, crud.rs, staleness.rs, embeddings.rs, query.rs, async_helpers.rs
    notes.rs    - Note CRUD, note_embeddings(), brute-force search
    calls/      - Call graph storage and queries
      mod.rs, crud.rs, dead_code.rs, query.rs, related.rs, test_map.rs
    types.rs    - Type edge storage and queries
    helpers.rs  - Types, embedding conversion functions
    migrations.rs - Schema migration framework
  parser/       - Code parsing (tree-sitter + custom parsers, delegates to language/ registry)
    mod.rs      - Parser struct, parse_file(), parse_file_all(), supported_extensions()
    types.rs    - Chunk (incl. parent_type_name), CallSite, FunctionCalls, TypeRef, ParserError
    chunk.rs    - Chunk extraction, signatures, doc comments, parent type extraction
    calls.rs    - Call graph extraction, callee filtering
    injection.rs - Multi-grammar injection (HTML→JS/CSS via set_included_ranges)
    markdown.rs - Heading-based markdown parser, cross-reference extraction
  embedder/      - ONNX embedding models (configurable: E5-base-v2 default, BGE-large preset, custom ONNX)
    mod.rs      - Embedder struct, embed(), batch embedding, runtime dimension detection
    models.rs   - ModelConfig struct, built-in presets (e5-base, bge-large), resolution logic, EmbeddingConfig
    provider.rs - ORT execution provider selection (CUDA/TensorRT/CPU)
  reranker.rs   - Cross-encoder re-ranking (ms-marco-MiniLM-L-6-v2)
  search/       - Search algorithms, name matching, HNSW-guided search
    mod.rs      - search_filtered(), search_unified_with_index(), hybrid RRF
    scoring/    - ScoringConfig, score normalization, RRF fusion constants
      mod.rs, candidate.rs, config.rs, filter.rs, name_match.rs, note_boost.rs
    query.rs    - Query parsing, filter extraction
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
  cagra.rs      - GPU-accelerated CAGRA index (optional)
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
    bfs.rs      - Reverse BFS, reverse_bfs_multi_attributed, test_reachability
    format.rs   - JSON/Mermaid formatting
    hints.rs    - compute_hints, compute_hints_batch, compute_risk_batch, risk scoring
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
  lib.rs        - Public API
.claude/
  skills/       - Claude Code skills (auto-discovered)
    groom-notes/  - Interactive note review and cleanup
    update-tears/ - Session state capture for context persistence
    release/      - Version bump, changelog, publish workflow
    audit/        - 14-category code audit with parallel agents
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
- Configurable embeddings (BGE-large 1024-dim default, E5-base 768-dim preset, custom ONNX)
- HNSW index is chunk-only; notes use brute-force SQLite search (always fresh)
- Streaming HNSW build via `build_batched()` for memory efficiency
- Large chunks split by windowing (480 tokens, 64 overlap); notes capped at 10k entries
- Schema migrations allow upgrading indexes without full rebuild
- Skills in `.claude/skills/*/SKILL.md` are auto-discovered by Claude Code

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

**Key files:** `src/language/mod.rs` (InjectionRule struct), `src/parser/injection.rs` (parsing logic), `src/language/html.rs` (reference implementation).

## Adding a New Language

Adding a language is a data-entry task, not a coding task. The `LanguageDef` system handles everything — you fill in fields.

### Prerequisites

- A tree-sitter grammar published on crates.io (search `tree-sitter-<lang>`)
- A sample source file to test with
- `tree-sitter parse sample.ext` to see node types (install: `cargo install tree-sitter-cli`)

### Steps

**1. Add the dependency to `Cargo.toml`:**

```toml
tree-sitter-dart = { version = "0.X", optional = true }
```

And the feature flag:
```toml
lang-dart = ["dep:tree-sitter-dart"]
```

Add `"lang-dart"` to the `default` and `lang-all` feature lists.

**2. Create `src/language/dart.rs`:**

Copy `src/language/bash.rs` as your starting template — it's the simplest language file (~65 lines). Then fill in:

```rust
//! Dart language definition

use super::{FieldStyle, LanguageDef, SignatureStyle};

// === STEP A: Write the chunk query ===
// Run `tree-sitter parse sample.dart` and look for function-like nodes.
// Common patterns: function_declaration, method_declaration, class_declaration
const CHUNK_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_declaration
  name: (identifier) @name) @function

(class_declaration
  name: (identifier) @name) @class
"#;

// === STEP B: Write the call query ===
// Look for call-like nodes in the AST dump.
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (selector) @callee)
"#;

// === STEP C: Fill in the rest (data entry) ===
const DOC_NODES: &[&str] = &["comment", "documentation_comment"];

const STOPWORDS: &[&str] = &[
    "if", "else", "for", "while", "do", "return", "class", "extends",
    "implements", "import", "void", "var", "final", "const", "static",
    "this", "super", "new", "null", "true", "false", "async", "await",
];

const COMMON_TYPES: &[&str] = &[
    "String", "int", "double", "bool", "List", "Map", "Set", "Future",
    "Stream", "void", "dynamic", "Object", "Iterable", "Function",
];

static DEFINITION: LanguageDef = LanguageDef {
    name: "dart",
    grammar: Some(|| tree_sitter_dart::LANGUAGE.into()),
    extensions: &["dart"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &["method_declaration"],
    method_containers: &["class_body"],
    stopwords: STOPWORDS,
    extract_return_nl: |sig| {
        // Dart: ReturnType functionName(params) { ... }
        // Type is before the function name
        None // Start simple, add later
    },
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: None,       // Add later for type edges
    common_types: COMMON_TYPES,
    container_body_kinds: &["class_body"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &["@test", "test("],
    test_path_patterns: &["%_test.dart", "%/test/%"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[],
    injections: &[],
    doc_format: "default",
    doc_convention: "Use /// for documentation comments. Follow Effective Dart documentation guidelines.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "final late var static const",
    },
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
```

**3. Register in `src/language/mod.rs`:**

Add one line to `define_languages!`:
```rust
Dart => "dart", feature = "lang-dart", module = dart;
```

**4. Write tests:**

Add a `#[cfg(test)] mod tests` section in your file. Minimum 3 tests:
- Parse a function → verify name and ChunkType::Function
- Parse a class → verify name and ChunkType::Class
- Parse function calls → verify callee names extracted

See `src/language/bash.rs` tests for the pattern.

**5. Build and test:**

```bash
cargo test --features gpu-index,lang-dart -- dart
```

### How to discover node types

Run `tree-sitter parse` on a sample file:

```bash
tree-sitter parse sample.dart 2>/dev/null | head -50
```

Output shows the AST. Look for:
- **Function nodes**: usually `function_declaration`, `method_declaration`, `function_expression`
- **Class nodes**: `class_declaration`, `interface_declaration`, `enum_declaration`
- **Call nodes**: `call_expression`, `method_invocation`
- **Name fields**: `name:` or `(identifier)`

The chunk query captures `@name` (the function/class name) and `@function` / `@class` / `@property` etc. (the full node for content extraction).

### Fields Reference

Most fields have sensible defaults (`None`, `&[]`, empty string). The important ones:

| Field | Required? | How to fill |
|-------|-----------|-------------|
| `grammar` | Yes | `Some(\|\| tree_sitter_<lang>::LANGUAGE.into())` |
| `extensions` | Yes | File extensions without dot |
| `chunk_query` | Yes | Tree-sitter S-expression query |
| `call_query` | Recommended | Tree-sitter query for function calls |
| `signature_style` | Yes | `UntilBrace` for C-like, `UntilNewline` for Python-like |
| `doc_nodes` | Recommended | Node kinds that contain doc comments |
| `stopwords` | Recommended | Language keywords to filter from NL |
| `common_types` | Recommended | Stdlib types to exclude from type edges |
| `field_style` | Recommended | `NameFirst`/`TypeFirst`/`None` for struct field extraction |
| Everything else | Optional | `None`, `&[]`, or `""` — add later as needed |

### Ecosystem updates (after the language works)

- Add `"lang-dart"` to the default features list in `Cargo.toml`
- Add to `CLAUDE.md` agent instructions (key commands block in agent prompts)
- Add to `README.md` language count
- Update `CHANGELOG.md`

## Questions?

Open an issue for questions or discussions.
