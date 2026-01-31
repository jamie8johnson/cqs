# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.11] - 2026-01-31

### Added
- MCP: `semantic_only` parameter to disable RRF hybrid search when needed
- MCP: HNSW index status in `cqs_stats` output

### Changed
- tree-sitter-rust: 0.23 -> 0.24
- tree-sitter-python: 0.23 -> 0.25
- Raised brute-force warning threshold from 50k to 100k chunks

### Documentation
- Simplified CLAUDE.md and tears system
- Added docs/SCARS.md for failed approaches
- Consolidated PROJECT_CONTINUITY.md (removed dated files)

## [0.1.10] - 2026-01-31

### Added
- RRF (Reciprocal Rank Fusion) hybrid search combining semantic + FTS5 keyword search
- FTS5 virtual table for full-text keyword search
- `normalize_for_fts()` for splitting camelCase/snake_case identifiers into searchable words
- Chunk-level incremental indexing (skip re-embedding unchanged chunks via content_hash)
- `Store::get_embeddings_by_hashes()` for batch embedding lookup

### Changed
- Schema version bumped from 1 to 2 (FTS5 support)
- RRF enabled by default in CLI and MCP for improved recall

## [0.1.9] - 2026-01-31

### Added
- HNSW-guided filtered search (10-100x faster for filtered queries)
- SIMD-accelerated cosine similarity via simsimd crate
- Shell completion generation (`cqs completions bash/zsh/fish/powershell`)
- Config file support (`.cqs.toml` in project, `~/.config/cqs/config.toml` for user)
- Lock file with PID for stale lock detection
- Rustdoc documentation for public API

### Changed
- Error messages now include actionable hints
- Improved unknown language/tool error messages

## [0.1.8] - 2026-01-31

### Added
- HNSW index for O(log n) search on large codebases (>50k chunks)
- Automatic HNSW index build after indexing
- Query embedding LRU cache (100 entries)

### Fixed
- RwLock poison recovery in HTTP handler
- LRU cache poison recovery in embedder
- Query length validation (8KB max)
- Embedding byte validation with warning

## [0.1.7] - 2026-01-31

### Fixed
- Removed `Parser::default()` panic risk
- Added logging for silent search errors
- Clarified embedder unwrap with expect()
- Added parse error logging in watch mode
- Added 100KB chunk byte limit (handles minified files)
- Graceful HTTP shutdown with Ctrl+C handler
- Protocol version constant consistency

## [0.1.6] - 2026-01-31

### Added
- Connection pooling with r2d2-sqlite (4 max connections)
- Request body limit (1MB) via tower middleware
- Secure UUID generation (timestamp + random)

### Fixed
- lru crate vulnerability (0.12 -> 0.16, GHSA-rhfx-m35p-ff5j)

### Changed
- Store methods now take `&self` instead of `&mut self`

## [0.1.5] - 2026-01-31

### Added
- SSE stream support via GET /mcp
- GitHub Actions CI workflow (build, test, clippy, fmt)
- Issue templates for bug reports and feature requests
- GitHub releases with changelogs

## [0.1.4] - 2026-01-31

### Changed
- MCP 2025-11-25 compliance (Origin validation, Protocol-Version header)
- Batching removed per MCP spec update

## [0.1.3] - 2026-01-31

### Added
- Watch mode (`cqs watch`) with debounce
- HTTP transport (MCP Streamable HTTP spec)
- .gitignore support via ignore crate

### Changed
- CLI restructured (query as positional arg, flags work anywhere)
- Replaced walkdir with ignore crate

### Fixed
- Compiler warnings

## [0.1.2] - 2026-01-31

### Added
- New chunk types: Class, Struct, Enum, Trait, Interface, Constant
- Hybrid search with `--name-boost` flag
- Context display with `-C N` flag
- Doc comments included in embeddings

## [0.1.1] - 2026-01-31

### Fixed
- Path pattern filtering (relative paths)
- Invalid language error handling

## [0.1.0] - 2026-01-31

### Added
- Initial release
- Semantic code search for 5 languages (Rust, Python, TypeScript, JavaScript, Go)
- tree-sitter parsing for function/method extraction
- nomic-embed-text-v1.5 embeddings (768-dim)
- GPU acceleration (CUDA/TensorRT) with CPU fallback
- SQLite storage with WAL mode
- MCP server (stdio transport)
- CLI commands: init, doctor, index, stats, serve
- Filter by language (`-l`) and path pattern (`-p`)

[Unreleased]: https://github.com/jamie8johnson/cqs/compare/v0.1.10...HEAD
[0.1.10]: https://github.com/jamie8johnson/cqs/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/jamie8johnson/cqs/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/jamie8johnson/cqs/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/jamie8johnson/cqs/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/jamie8johnson/cqs/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/jamie8johnson/cqs/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/jamie8johnson/cqs/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/jamie8johnson/cqs/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/jamie8johnson/cqs/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/jamie8johnson/cqs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.0
