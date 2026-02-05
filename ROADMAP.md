# Roadmap

## Phase 1: MVP

### Status: Complete

### Done

- [x] Design document (v0.13.0) - architecture, API, all implementations specified
- [x] Audits - 7 rounds, 0 critical/high issues remaining
- [x] Parser - tree-sitter extraction, all 5 languages (13 tests passing)
- [x] Embedder - ort + tokenizers, CUDA/CPU detection, model download
- [x] Store - sqlite with WAL, BLOB embeddings, two-phase search (8 tests passing)
- [x] CLI - init, doctor, index, query, stats, serve, --lang filter, --path filter
- [x] MCP - cqs serve with stdio, cqs_search + cqs_stats tools
- [x] Published to crates.io as `cqs` v0.1.0, v0.1.1
- [x] End-to-end testing - init, index, search all working
- [x] MCP integration tested with Claude Code
- [x] Path pattern filtering fixed (relative paths)
- [x] Invalid language error handling
- [x] Eval suite - 50 queries (10/lang), Recall@5: 98% (49/50)

### Exit Criteria

- [x] `cargo install cqs` works (published v0.1.0)
- [x] CPU fallback works (~20ms per embedding)
- [x] GPU works when available (CUDA tested)
- [x] 8/10 eval queries return relevant result in top-5 per language (actual: 9.8/10)
- [x] Index survives Ctrl+C during indexing
- [x] MCP works with Claude Code

## Phase 2: Polish

### Status: Complete (v0.1.2)

### Done

- [x] GPU acceleration verified (6ms query, 0.3ms/doc batched)
- [x] More chunk types (Class, Struct, Enum, Trait, Interface, Constant)
- [x] Hybrid search (embedding + name match, --name-boost flag)
- [x] Doc comments in embeddings (prepend to content)
- [x] --context N for surrounding lines
- [x] Published v0.1.2 to crates.io

### Deferred

- Signature-aware search (name boost covers most cases)
- More languages (C, C++, Java, Ruby)
- MCP extras: cqs_similar, cqs_index, progress notifications

## Phase 3: Integration

### Status: Complete (v0.1.3 → v0.1.5)

### Done

- [x] .gitignore support (ignore crate replaces walkdir)
- [x] Watch mode (`cqs watch` with debounce)
- [x] HTTP transport (MCP Streamable HTTP spec)
- [x] CLI restructured (query as positional arg, flags work anywhere)
- [x] Compiler warnings fixed
- [x] Model checksums renamed (SHA256 → BLAKE3)
- [x] MCP 2025-11-25 compliance (Origin validation, Protocol-Version header)
- [x] SSE stream support (GET /mcp)
- [x] Automated dependency reviews (Dependabot + GitHub Actions)
- [x] CI workflow (build, test, clippy, fmt)
- [x] Issue templates (bug report, feature request)
- [x] GitHub releases with changelogs
- [x] Published v0.1.3, v0.1.4, v0.1.5 to crates.io

### Deferred

- VS Code extension (can use MCP directly)

## Current Phase: 4 (Scale)

### Status: Complete (v0.1.8)

### Done

- [x] HNSW index for >50k chunks (hnsw_rs v0.3.3)
  - O(log n) search, sub-10ms for 100k chunks
  - Auto-builds after indexing, persists to disk
  - HNSW-guided filtered search (no longer falls back to brute-force)
- [x] P0 audit fixes:
  - RwLock poison recovery in HTTP handler
  - LRU cache poison recovery in embedder
  - Query length validation (8KB max)
  - Embedding byte validation
- [x] Published v0.1.8 to crates.io
- [x] Submitted to awesome-mcp-servers

### Deferred

- Incremental embedding updates (rebuild works fine for now)
- Index sharing (team sync)

### Done (Post-Release)

- [x] Branch protection ruleset (require CI, block force push)
- [x] 16-category audit (74 findings documented)
- [x] Audit Phase A fixes (SQL params, globset, fs4, MCP tests)
- [x] Audit Phase B fixes:
  - Connection pooling (r2d2-sqlite) for concurrent reads
  - RwLock for HTTP handler (enabled by pooling)
  - Secure UUID generation (timestamp + random)
  - Request body limit (1MB via tower middleware)
  - Query embedding LRU cache (100 entries)
- [x] Pre-commit hook (.githooks/pre-commit - cargo fmt check)
- [x] Audit Phase C fixes (v0.1.7):
  - Removed Parser::default() panic risk
  - Added logging for silent search errors
  - Clarified embedder unwrap with expect()
  - Added parse error logging in watch mode
  - Added 100KB chunk byte limit (handles minified files)
  - Graceful HTTP shutdown (Ctrl+C handler)
  - Protocol version constant consistency
- [x] v0.1.9 (published):
  - HNSW-guided filtered search (10-100x faster)
  - SIMD cosine similarity (simsimd crate)
  - Shell completions (bash/zsh/fish/powershell)
  - Config file support (.cqs.toml)
  - Lock file with PID (stale lock detection)
  - Error messages with actionable hints
  - Rustdoc for public API
  - CHANGELOG.md

### Optional (Enable as Needed)

- GitHub Discussions (community Q&A)
- GitHub Wiki (end-user documentation)
- Security advisories (for vulnerability reports)

## Phase 5: Quality

### Status: In Progress

### Done

- [x] Chunk-level incremental indexing
  - Use content_hash to skip re-embedding unchanged chunks
  - `Store::get_embeddings_by_hashes()` for batch lookup
  - 80-90% cache hit rate on re-index verified

- [x] RRF hybrid search (Reciprocal Rank Fusion)
  - FTS5 virtual table for keyword search
  - Preprocess identifiers (split camelCase/snake_case)
  - Fuse semantic + keyword with `1/(k + rank)` scoring (k=60)
  - Enabled by default for better recall

- [x] v0.1.10 release (includes incremental indexing + RRF)
  - Published to crates.io 2026-01-31
  - Schema version bumped to 2 (FTS5 support)

- [x] MCP tool polish (post v0.1.10)
  - Added `semantic_only` parameter to disable RRF when needed
  - Added HNSW index status to cqs_stats output
  - Updated tree-sitter grammars (rust 0.24, python 0.25)

### Done

- [x] Code→NL→Embed pipeline (Greptile insight)
  - Embeds NL descriptions instead of raw code
  - Template-based: "A function named X. Takes parameters Y. Returns Z."
  - Doc comments prioritized as human-written NL

- [x] NL module extraction (v0.1.13)
  - src/nl.rs with generate_nl_description, tokenize_identifier
  - JSDoc parsing for JavaScript (@param, @returns)
  - Eval suite uses NL pipeline (matches production)
  - Eval runs in CI on tagged releases

- [x] MCP integration tests (8 tests)

- [x] Call graph analysis (v0.1.14)
  - Extract function call relationships via tree-sitter
  - `cqs callers` / `cqs callees` commands
  - MCP tools: `cqs_callers`, `cqs_callees`

- [x] Full call graph coverage (v0.1.15)
  - Separate call extraction from chunk extraction
  - Large functions (>100 lines) now captured
  - 1889 calls indexed (CLI handlers included)

- [x] E5-base-v2 model switch (v0.1.16)
  - Full CUDA coverage (no rotary CPU fallback)
  - Windowing for long functions (schema v9: parent_id, window_idx)
  - VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
  - 9-layer fresh-eyes audit completed

- [x] GPU query embedding for MCP server
  - `cqs serve --gpu` flag for GPU-accelerated queries
  - CPU: cold 0.52s, warm 22ms
  - GPU: cold 1.15s, warm 12ms (~45% faster warm)

- [x] 20-category audit (v0.4.4)
  - ~243 findings across 20 categories
  - P1-P3 fixed (PRs #199, #200, #201)
  - P4 tracked in issues #202-208
  - Audit mode added (`cqs_audit_mode` tool)
  - `note_weight` parameter for search

- [x] Sprint improvements (v0.4.6)
  - Schema migration framework (#188)
  - CLI integration tests - 12 end-to-end tests (#206)
  - Server transport tests - 3 stdio tests (#205)
  - Stress tests - 5 ignored heavy-load tests (#207)
  - `--api-key-file` with zeroize for secure key loading (#202)
  - Lazy grammar loading - 50-200ms startup improvement (#208)
  - Pipeline resource sharing via `Arc<Store>` (#204)
  - Atomic HNSW writes for crash safety (#186)
  - Note search warning at WARN level (#203)

### Planned

- [ ] C and Java language support (tree-sitter-c, tree-sitter-java)
- [ ] Code-specific embedding model (CodeSage, Qwen3-Embedding vs E5)
- [ ] Template experiments (no prefix, body keywords) - run eval to compare
- [ ] Multi-index support (reference codebases)
  - Search multiple indexes simultaneously (project + stdlib + deps)
  - Index popular crates as reference (tokio, serde, axum)
  - Index rust-lang/rust stdlib as language reference
  - MCP searches across all configured indexes
  - Use case: "How does stdlib implement X?" while coding your project

## Phase 6: Security

### Done

- [x] Rate limiting for HTTP transport (RequestBodyLimitLayer - 1MB)

### Planned

- Index encryption (SQLCipher behind cargo feature flag)
  - Protect code snippets and embeddings at rest
  - Password/key required on operations
  - Optional: integrate with system keyring
- Request rate limiting (requests per second, not just body size)
- Audit log for MCP operations

## 1.0 Release Criteria

Ship 1.0 when:

- [ ] Schema stable for 1+ week of daily use (currently v10)
- [ ] Used on 2+ different codebases without issues
- [ ] MCP integration solid in daily Claude Code use
- [ ] No known correctness bugs

1.0 means: API stable, semver enforced, breaking changes = major bump.
