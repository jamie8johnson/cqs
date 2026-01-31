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

### Planned

- HNSW index for >50k chunks
- Incremental embedding updates
- Index sharing (team sync)

### Done (Post-Release)

- [x] Branch protection ruleset (require CI, block force push)
- [x] 16-category audit (74 findings documented)

### Optional (Enable as Needed)

- GitHub Discussions (community Q&A)
- GitHub Wiki (end-user documentation)
- Security advisories (for vulnerability reports)
