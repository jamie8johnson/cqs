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

- Additional language support (tree-sitter grammars: C, C++, Java, Ruby)
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
    commands/   - Command implementations (serve.rs)
    config.rs   - Configuration file loading
    display.rs  - Output formatting, result display
    pipeline.rs - Multi-threaded indexing pipeline
    watch.rs    - File watcher for incremental reindexing
  language/     - Tree-sitter language support
    mod.rs      - LanguageRegistry, LanguageDef trait
    rust.rs, python.rs, typescript.rs, javascript.rs, go.rs
  source/       - Source abstraction layer
    mod.rs      - Source trait
    filesystem.rs - File-based source implementation
  store/        - SQLite storage layer (Schema v10, WAL mode)
    mod.rs      - Store struct, open/init, FTS5, RRF fusion
    chunks.rs   - Chunk CRUD, embedding_batches() for streaming
    notes.rs    - Note CRUD, note_embeddings(), search_notes_by_ids()
    calls.rs    - Call graph storage and queries
    helpers.rs  - Types, embedding conversion functions
    migrations.rs - Schema migration framework
  mcp/          - MCP server implementation
    mod.rs      - McpServer, JSON-RPC handling
    transports/ - stdio.rs, http.rs transport implementations
  parser.rs     - tree-sitter code parsing (lazy grammar loading)
  embedder.rs   - ONNX model (E5-base-v2), 769-dim embeddings
  search.rs     - Search algorithms, cosine similarity, HNSW-guided search
  hnsw.rs       - HNSW index with batched build, atomic writes
  cagra.rs      - GPU-accelerated CAGRA index (optional)
  nl.rs         - NL description generation, JSDoc parsing
  note.rs       - Developer notes with sentiment
  config.rs     - Configuration file support
  index.rs      - VectorIndex trait (HNSW, CAGRA)
  lib.rs        - Public API
```

**Key design notes:**
- 769-dim embeddings (768 from E5-base-v2 + 1 sentiment dimension)
- Unified HNSW index contains both chunks and notes (notes prefixed with `note:`)
- Streaming HNSW build via `build_batched()` for memory efficiency
- Chunks capped at 500 lines, notes capped at 10k entries
- Schema migrations allow upgrading indexes without full rebuild

## Questions?

Open an issue for questions or discussions.
