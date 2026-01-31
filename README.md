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
