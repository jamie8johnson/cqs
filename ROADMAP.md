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
- More languages (C++, Ruby) [C and Java shipped in Phase 5]
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

## Current Phase: 5 (Multi-index)

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

- Incremental embedding updates (brute-force notes approach from #244 handles note staleness; chunk HNSW rebuild works fine)
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

- [x] Model evaluation (#221)
  - E5-base-v2 confirmed: 100% Recall@5 (50/50 eval queries)
  - CodeSage/Qwen3 evaluation unnecessary — E5 wins

- [x] C and Java language support (#222)
  - tree-sitter-c, tree-sitter-java grammars
  - Language enum + LanguageDef registry entries
  - 7 languages total (Rust, Python, TypeScript, JavaScript, Go, C, Java)

- [x] Parser/registry consolidation (#223)
  - parser.rs: 1469 → 1056 lines (28% reduction)
  - Parser re-exports Language, ChunkType from language module

- [x] Test coverage expansion (#224)
  - 50 new tests across 6 modules (cagra, index, mcp tools, pipeline, CLI)
  - Total: 375 tests (GPU) / 364 (no GPU)

- [x] Template experiments (#226)
  - 5 variants tested: Standard, NoPrefix, BodyKeywords, Compact, DocFirst
  - All scored 100% Recall@5 — ceiling effect from doc comments
  - Baseline stays; infra available for harder eval cases later

- [x] 20-category audit v2 (PRs #227-#229)
  - ~85 actionable findings fixed across P1-P3
  - 13 P4 items tracked in issues #230-#241
  - 15 new search path tests, test count 379 (no GPU)

- [x] Note management tools (PR #244, closes #230)
  - Notes removed from HNSW/CAGRA — always brute-force from SQLite
  - MCP tools: `cqs_update_note`, `cqs_remove_note` (atomic TOML rewrite)
  - `rewrite_notes_file()` helper with header preservation

### Done (cont.)

- [x] Note grooming command/skill (#245)
  - `cqs notes list` — display all notes with sentiment, staleness
  - `/groom-notes` skill — interactive review + batch cleanup

- [x] Multi-index support (reference codebases)
  - `cqs ref add/list/remove/update` CLI commands
  - Search multiple indexes simultaneously (project + refs)
  - MCP `cqs_search` searches across all configured indexes
  - `sources` filter parameter for targeted search
  - Score-based merge with configurable weight multiplier
  - `cqs doctor` validates reference health
  - `[[reference]]` config in `.cqs.toml`

- [x] 20-category audit v3 (PRs #259, #261, #262, #263)
  - 193 raw findings, ~120 unique, P1-P4 triaged
  - P1: 12 fixes (path traversal, glob filter, pipeline mtime, threshold, SSE origin, docs, error messages)
  - P2: 5 fixes (dead code, CAGRA streaming, brute-force notes, call graph errors, config parse)
  - P3: 11 fixes (signal reset, unreachable, glob dedup, empty query, CRLF, permissions, SQL dedup, HNSW accessor, pipeline stats, panic messages, IO context)
  - Remaining items tracked in issues #264-270
  - Test count: 418 (no GPU)

## Phase 6: Discovery & UX

### Status: Complete (v0.7.0)

### Done

- [x] `cqs similar` (CLI + MCP) — find similar functions by example using stored embeddings
- [x] `cqs explain` (CLI + MCP) — function card (signature, callers, callees, similar)
- [x] `cqs diff` (CLI + MCP) — semantic diff between indexed snapshots
- [x] Workspace-aware indexing — detect Cargo workspace root from member crates
- [x] Store prereqs: `get_chunk_with_embedding`, `all_chunk_identities`, `ChunkIdentity`
- [x] 431 tests (no GPU), 12 MCP tools

### Deferred

- Pre-built reference packages (#255) — `cqs ref install tokio`

## Phase 7: Token Efficiency

### Status: Complete (v0.8.0)

### Done

- [x] **`cqs trace`** (CLI + MCP) — BFS shortest path between two functions through the call graph. Replaces 5-10 sequential file reads per code-flow question.
- [x] **`cqs impact`** (CLI + MCP) — "What breaks if I change X?" Returns callers with call-site snippets, plus tests that reference X via reverse BFS. ~5 tool calls → 1.
- [x] **`cqs test-map`** (CLI + MCP) — Map functions to tests that exercise them with full call chains. Saves grep rounds before refactoring.
- [x] **`cqs batch`** (MCP-only) — Execute multiple queries in one tool call. Eliminates round-trip overhead for independent lookups. Max 10 queries per batch.
- [x] **`cqs context`** (CLI + MCP) — Module-level understanding: all chunks, external callers/callees, dependent files, related notes for a file.
- [x] **Focused `cqs_read`** (MCP) — `focus` parameter returns target function + type dependencies instead of whole file. Cuts file-read tokens by 50-80%.
- [x] Shared infrastructure: `CallGraph`, `CallerWithContext`, `get_call_graph()`, `find_test_chunks()`, `get_chunks_by_origin()`, shared `resolve.rs` modules
- [x] 17 MCP tools (up from 12), 4 new CLI subcommands, 431+ tests passing

## Post-v0.8.0: Uncharted Features

### Status: Complete (PR #280)

### Done

- [x] **`--chunk-type` filter** — narrow search to function/method/class/struct/enum/trait/interface/constant (CLI + MCP)
- [x] **`cqs dead`** (CLI + MCP) — find functions/methods never called by indexed code. Excludes main, tests, trait impls. `--include-pub` for full audit.
- [x] **Index staleness warnings** — `cqs stats` and MCP stats report stale/missing file counts
- [x] **`cqs gc`** (CLI + MCP) — prune chunks for deleted files, clean orphan call graph entries, rebuild HNSW
- [x] **`--format mermaid`** on `cqs trace` — generate Mermaid diagrams from call paths
- [x] **`cqs project`** (CLI) — cross-project search via `~/.config/cqs/projects.toml` registry
- [x] **`cqs gather`** (CLI + MCP) — smart context assembly: BFS call graph expansion from semantic seed results
- [x] **`--pattern` filter** — post-search structural matching (builder, error_swallow, async, mutex, unsafe, recursion)
- [x] 20 MCP tools (up from 17), 260 lib tests, 33 new unit tests

## Refactoring

### Done (v0.9.1)

- [x] **Split `parser.rs`** → `src/parser/` directory (mod.rs, types.rs, chunk.rs, calls.rs)
- [x] **Split `hnsw.rs`** → `src/hnsw/` directory (mod.rs, build.rs, search.rs, persist.rs, safety.rs)
- [x] **Fix flaky `test_loaded_index_multiple_searches`** — one-hot embeddings for reliable separation

## Next: New Languages (SQL + VB.NET)

### Priority: High (needed for VS2005 project)

- [ ] **SQL** — `tree-sitter-sql` crate on crates.io. Stored procedures, functions, views, triggers.
- [ ] **VB.NET** — `tree-sitter-vb-dotnet` (git dep, not on crates.io). Subs, Functions, Classes, Modules.
- [x] P3 audit fixes (#264-266) — completed in PR #296

### Notes

- SQL: stored procs and functions are the main chunk types. Views/triggers optional.
- VB.NET: grammar from [CodeAnt-AI/tree-sitter-vb-dotnet](https://github.com/CodeAnt-AI/tree-sitter-vb-dotnet). May need vendored C source if git dep doesn't work cleanly.
- Each language needs: Language enum variant, extension mapping, grammar loading, query patterns, display impl (~5 changes per #268)
- After: 9 languages total

## Agent Experience Improvements

### Planned

- [ ] **Mermaid output for `cqs impact`** — `format: "mermaid"` renders caller graph as flowchart. Agents can visualize blast radius of a change.
- [ ] **Mermaid output for `cqs context`** — `format: "mermaid"` renders module dependency graph (external callers/callees, dependent files).
- [ ] **Mermaid output for `cqs dead`** — `format: "mermaid"` renders orphan clusters. Visualize dead code relationships.
- [ ] **Token cost estimates** — include approximate token count in tool responses so agents can budget context window usage
- [ ] **Proactive hints in cqs_read/cqs_explain** — auto-surface "0 callers" (dead code) and "no tests" flags without requiring separate tool calls
- [ ] **Refactor assistant** — "move function X from A to B" → checklist of import changes, visibility fixes, re-exports needed
- [ ] **Batch UX** — common batch patterns (e.g., "callers for these N functions") as named shortcuts instead of raw JSON construction

## Parked

- **Markdown support** — `tree-sitter-markdown`, headings as chunks, sections as content. Makes docs/READMEs/specs searchable. No call graph/signatures though — half the tools wouldn't apply.

## Phase 8: Security

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

## Future: Agent Memory

Ideas beyond code search — making cqs a knowledge layer across sessions.

- [ ] **Diff-aware impact** — `cqs impact-diff` takes a git diff, returns affected callers + tests that need re-running. CI integration: run only relevant tests. Combines `git diff` parse → function extraction → call graph traversal.
- [ ] **Navigational traces** — record what an agent searched for, read, and edited during a session. Future sessions can replay the trail instead of rediscovering it. "Last time someone asked about auth, they read these 5 files in this order."
- [ ] **Cross-session search** — embed and index past conversation fragments (questions + answers). When an agent asks "how does X work?", surface the answer from last Tuesday's session, not just code.
- [ ] **Session knowledge packages** — export "what the last agent learned" as a reference index. Not just notes — navigational knowledge, frequently-accessed files, decision context. Bootstrap cold starts on unfamiliar codebases.
- [ ] **Auto-detected patterns** — track search→read→edit sequences across sessions. When a pattern repeats (e.g., "searching for error handling always leads to these 3 files"), pre-compute and suggest the path.
