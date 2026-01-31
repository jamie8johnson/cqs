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

# Output options
cqs --json "query"           # JSON output
cqs --no-content "query"     # File:line only, no code
cqs -n 10 "query"            # Limit results
cqs -t 0.5 "query"           # Min similarity threshold
```

## MCP Integration

Use with Claude Code as an MCP server:

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

cqs works fine on CPU (~20ms per embedding). For GPU acceleration:

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
- GPU visibility can be intermittent; CPU fallback always works

### Verify

```bash
cqs doctor  # Shows execution provider (CUDA or CPU)
```

## License

MIT
