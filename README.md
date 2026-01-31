# cqs

Semantic code search with local ML embeddings. Find functions by concept, not name. GPU-accelerated. MCP server included.

**TL;DR:** Semantic code search for Claude Code. Instead of grep, it understands what code *does* - so Claude finds relevant functions without knowing exact names. Means Claude finds the right code faster and doesn't miss stuff buried in unfamiliar codebases.

[![Crates.io](https://img.shields.io/crates/v/cqs.svg)](https://crates.io/crates/cqs)
[![CI](https://github.com/jamie8johnson/cqs/actions/workflows/ci.yml/badge.svg)](https://github.com/jamie8johnson/cqs/actions/workflows/ci.yml)
[![CodeQL](https://github.com/jamie8johnson/cqs/actions/workflows/codeql.yml/badge.svg)](https://github.com/jamie8johnson/cqs/actions/workflows/codeql.yml)

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
cqs "validate email with regex"
cqs "database connection pool"
```

## Filters

```bash
# By language
cqs --lang rust "error handling"
cqs --lang python "parse json"

# By path pattern
cqs --path "src/*" "config"
cqs --path "tests/**" "mock"
cqs --path "**/*.go" "interface"

# Combined
cqs --lang typescript --path "src/api/*" "authentication"

# Hybrid search (boost name matches)
cqs --name-boost 0.5 "parse"    # Names containing "parse" ranked higher

# Show surrounding context
cqs -C 3 "error handling"       # 3 lines before/after each result

# Output options
cqs --json "query"           # JSON output
cqs --no-content "query"     # File:line only, no code
cqs -n 10 "query"            # Limit results
cqs -t 0.5 "query"           # Min similarity threshold
```

## Watch Mode

Keep your index up to date automatically:

```bash
cqs watch              # Watch for changes and reindex
cqs watch --debounce 1000  # Custom debounce (ms)
```

Watch mode respects `.gitignore` by default. Use `--no-ignore` to index ignored files.

## Claude Code Integration

### Why use cqs?

Without cqs, Claude Code uses grep/glob to find code - which only works if you know the exact names. With cqs, Claude can:

- **Find code by behavior**: "function that retries with backoff" finds retry logic even if it's named `doWithAttempts`
- **Navigate unfamiliar codebases**: Claude finds relevant code without knowing the project structure
- **Catch related code**: Semantic search surfaces similar patterns across the codebase that text search misses

### Setup

**Step 1:** Add cqs as an MCP server:

```bash
claude mcp add cqs -- cqs serve --project /path/to/project
```

Or manually in `~/.claude.json`:

```json
{
  "projects": {
    "/path/to/project": {
      "mcpServers": {
        "cqs": {
          "command": "cqs",
          "args": ["serve", "--project", "/path/to/project"]
        }
      }
    }
  }
}
```

**Note:** The `--project` argument is required because MCP servers run from an unpredictable working directory.

**Step 2:** Add to your project's `CLAUDE.md` so Claude uses it automatically:

```markdown
## Code Search

Use `cqs_search` for semantic code search instead of grep/glob when looking for:
- Functions by behavior ("retry with backoff", "parse config")
- Implementation patterns ("error handling", "database connection")
- Code where you don't know the exact name

Available tools:
- `cqs_search` - semantic search, supports `language`, `path_pattern`, `threshold`, `limit`
- `cqs_stats` - index stats and chunk counts

Keep index fresh: run `cqs watch` in a background terminal, or `cqs index` after significant changes.
```

### HTTP Transport

For web integrations, use the HTTP transport:

```bash
cqs serve --transport http --port 3000 --project /path/to/project
```

Endpoints:
- `POST /mcp` - JSON-RPC requests
- `GET /mcp` - SSE stream for server-to-client messages
- `GET /health` - Health check

Implements MCP Streamable HTTP spec 2025-11-25 with Origin validation and protocol version headers.

## Supported Languages

- Rust
- Python
- TypeScript
- JavaScript
- Go

## Indexing

By default, `cqs index` respects `.gitignore` rules:

```bash
cqs index              # Respects .gitignore
cqs index --no-ignore  # Index everything
cqs index --force      # Re-index all files
cqs index --dry-run    # Show what would be indexed
```

## How It Works

1. Parses code with tree-sitter to extract:
   - Functions and methods
   - Classes and structs
   - Enums, traits, interfaces
   - Constants
2. Generates embeddings with nomic-embed-text-v1.5 (runs locally)
   - Includes doc comments for better semantic matching
3. Stores in SQLite with vector search
4. Uses GPU if available, falls back to CPU

## Search Quality

Semantic search finds conceptually related code:

| Query | Top Match | Score |
|-------|-----------|-------|
| "cosine similarity" | `cosine_similarity` | 0.85 |
| "validate email regex" | `validateEmail` | 0.73 |
| "check if adult age 18" | `isAdult` | 0.71 |
| "pop from stack" | `Stack.Pop` | 0.70 |
| "generate random id" | `generateId` | 0.70 |

## GPU Acceleration (Optional)

cqs works on CPU (~20ms per embedding). GPU provides 3x+ speedup:

| Mode | Single Query | Batch (50 docs) |
|------|--------------|-----------------|
| CPU  | ~20ms        | ~15ms/doc       |
| CUDA | ~6ms         | ~0.3ms/doc      |

For GPU acceleration:

### Linux

```bash
# Add NVIDIA CUDA repo
wget https://developer.download.nvidia.com/compute/cuda/repos/ubuntu2404/x86_64/cuda-keyring_1.1-1_all.deb
sudo dpkg -i cuda-keyring_1.1-1_all.deb
sudo apt update

# Install CUDA runtime and cuDNN 9
sudo apt install cuda-cudart-12-6 libcublas-12-6 libcudnn9-cuda-12
```

Set library path:
```bash
export LD_LIBRARY_PATH=/usr/local/cuda-12.6/lib64:/usr/lib/x86_64-linux-gnu:$LD_LIBRARY_PATH
```

### WSL2

Same as Linux, plus:
- Requires NVIDIA GPU driver on Windows host
- Add `/usr/lib/wsl/lib` to `LD_LIBRARY_PATH`
- Tested working with RTX A6000, CUDA 13.0 driver, cuDNN 9.18

### Verify

```bash
cqs doctor  # Shows execution provider (CUDA or CPU)
```

## Contributing

Issues and PRs welcome at [GitHub](https://github.com/jamie8johnson/cqs).

## License

MIT
