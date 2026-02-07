# Contributing to cqs

Thank you for your interest in contributing to cqs!

## Development Setup

**Requires Rust 1.88+** (check with `rustc --version`)

1. Clone the repository:
   ```bash
   git clone https://github.com/jamie8johnson/cqs
   cd cqs
   ```

2. Build:
   ```bash
   cargo build
   ```

3. Run tests:
   ```bash
   cargo test
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

## Pull Request Process

1. Fork the repository and create a feature branch
2. Make your changes
3. Ensure all checks pass:
   ```bash
   cargo test
   cargo clippy -- -D warnings
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

- Additional language support (tree-sitter grammars: C++, Ruby, and more)
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
    mod.rs      - Argument parsing, command dispatch
    commands/   - Command implementations
      mod.rs, query.rs, index.rs, stats.rs, graph.rs, serve.rs, init.rs, doctor.rs, notes.rs, reference.rs, similar.rs, explain.rs, diff.rs, trace.rs, impact.rs, test_map.rs, context.rs, resolve.rs, dead.rs, gc.rs, gather.rs, project.rs
    config.rs   - Configuration file loading
    display.rs  - Output formatting, result display
    files.rs    - File enumeration, lock files, path utilities
    pipeline.rs - Multi-threaded indexing pipeline
    signal.rs   - Signal handling (Ctrl+C)
    watch.rs    - File watcher for incremental reindexing
  language/     - Tree-sitter language support
    mod.rs      - Language enum, LanguageRegistry, LanguageDef, ChunkType
    rust.rs, python.rs, typescript.rs, javascript.rs, go.rs, c.rs, java.rs
  source/       - Source abstraction layer
    mod.rs      - Source trait
    filesystem.rs - File-based source implementation
  store/        - SQLite storage layer (Schema v10, WAL mode)
    mod.rs      - Store struct, open/init, FTS5, RRF fusion
    chunks.rs   - Chunk CRUD, embedding_batches() for streaming
    notes.rs    - Note CRUD, note_embeddings(), brute-force search
    calls.rs    - Call graph storage and queries
    helpers.rs  - Types, embedding conversion functions
    migrations.rs - Schema migration framework
  mcp/          - MCP server implementation
    mod.rs      - McpServer, JSON-RPC handling
    server.rs   - Request routing, error sanitization
    types.rs    - JSON-RPC types
    validation.rs - Input validation, path checks
    audit_mode.rs - Audit mode state
    tools/      - MCP tool implementations
      mod.rs, search.rs, read.rs, notes.rs, stats.rs, call_graph.rs, audit.rs, similar.rs, explain.rs, diff.rs, trace.rs, impact.rs, test_map.rs, batch.rs, context.rs, resolve.rs, dead.rs, gc.rs, gather.rs
    transports/ - stdio.rs, http.rs transport implementations
  parser/       - Tree-sitter code parsing (delegates to language/ registry)
    mod.rs      - Parser struct, parse_file(), supported_extensions()
    types.rs    - Chunk, CallSite, FunctionCalls, ParserError
    chunk.rs    - Chunk extraction, signatures, doc comments
    calls.rs    - Call graph extraction, callee filtering
  embedder.rs   - ONNX model (E5-base-v2), 769-dim embeddings
  search.rs     - Search algorithms, name matching, HNSW-guided search
  math.rs       - Vector math utilities (cosine similarity, SIMD)
  hnsw/         - HNSW index with batched build, atomic writes
    mod.rs      - HnswIndex, HnswInner, HnswError, VectorIndex impl
    build.rs    - build(), build_batched() construction
    search.rs   - Nearest-neighbor search
    persist.rs  - save(), load(), checksum verification
    safety.rs   - Send/Sync and loaded-index safety tests
  cagra.rs      - GPU-accelerated CAGRA index (optional)
  nl.rs         - NL description generation, JSDoc parsing
  note.rs       - Developer notes with sentiment, rewrite_notes_file()
  diff.rs       - Semantic diff between indexed snapshots
  reference.rs  - Multi-index: ReferenceIndex, load, search, merge
  gather.rs     - Smart context assembly (BFS call graph expansion)
  structural.rs - Structural pattern matching on code chunks
  project.rs    - Cross-project search registry
  config.rs     - Configuration file support
  index.rs      - VectorIndex trait (HNSW, CAGRA)
  lib.rs        - Public API
.claude/
  skills/       - Claude Code skills (auto-discovered)
    groom-notes/  - Interactive note review and cleanup
    update-tears/ - Session state capture for context persistence
    release/      - Version bump, changelog, publish workflow
    audit/        - 14-category code audit with parallel agents
    pr/           - WSL-safe PR creation
    cqs-bootstrap/ - New project setup with tears infrastructure
    reindex/      - Rebuild index with before/after stats
    docs-review/  - Check project docs for staleness
    migrate/      - Schema version upgrades
    troubleshoot/ - Diagnose common cqs issues
    cqs-*/        - MCP tool skill wrappers (search, read, callers, etc.)
```

**Key design notes:**
- 769-dim embeddings (768 from E5-base-v2 + 1 sentiment dimension)
- HNSW index is chunk-only; notes use brute-force SQLite search (always fresh)
- Streaming HNSW build via `build_batched()` for memory efficiency
- Chunks capped at 100 lines, notes capped at 10k entries
- Schema migrations allow upgrading indexes without full rebuild
- Skills in `.claude/skills/*/SKILL.md` are auto-discovered by Claude Code

## Questions?

Open an issue for questions or discussions.
