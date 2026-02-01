# Contributing to cqs

Thank you for your interest in contributing to cqs!

## Development Setup

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
  cli.rs      - Command-line interface (clap)
  parser.rs   - tree-sitter code parsing
  embedder.rs - ONNX model embedding generation
  store.rs    - SQLite storage, FTS5 keyword search, RRF hybrid fusion
  hnsw.rs     - HNSW index for fast O(log n) vector search
  mcp.rs      - MCP server implementation
  nl.rs       - NL description generation, JSDoc parsing
  lib.rs      - Public API
```

## Questions?

Open an issue for questions or discussions.
