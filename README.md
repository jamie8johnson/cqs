# cqs

Semantic code search with local embeddings.

[![Crates.io](https://img.shields.io/crates/v/cqs.svg)](https://crates.io/crates/cqs)

## Install

```bash
cargo install cqs
```

## Quick Start

```bash
# Download model and initialize
cqs init

# Index your project
cd /path/to/project
cqs index

# Search
cqs "retry with exponential backoff"
cqs --lang rust "error handling"
cqs --json "parse config"
```

## MCP Integration

Use with Claude Code as an MCP server:

```json
{
  "mcpServers": {
    "cqs": {
      "command": "cqs",
      "args": ["serve"],
      "cwd": "/path/to/project"
    }
  }
}
```

## Supported Languages

- Rust
- Python
- TypeScript
- JavaScript
- Go

## How It Works

1. Parses code with tree-sitter to extract functions/methods
2. Generates embeddings with nomic-embed-text-v1.5 (runs locally)
3. Stores in SQLite with vector search
4. Uses GPU if available, falls back to CPU

## License

MIT
