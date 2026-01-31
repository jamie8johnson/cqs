# Privacy

## Data Stays Local

cqs processes your code entirely on your machine. Nothing is transmitted externally.

- **No telemetry**: We collect no usage data
- **No analytics**: No tracking of any kind
- **No cloud sync**: Index stays in your project directory

## What Gets Stored

When you run `cqs index`, the following is stored in `.cq/index.db`:

- Code chunks (functions, methods)
- Embedding vectors (768-dimensional floats)
- File paths and line numbers
- File modification times

This data never leaves your machine.

## Model Download

The embedding model is downloaded once from HuggingFace:

- Model: `nomic-ai/nomic-embed-text-v1.5`
- Size: ~547MB
- Cached in: `~/.cache/huggingface/`

HuggingFace may log download requests per their privacy policy. After download, the model runs offline.

## MCP Server

When using `cqs serve` with Claude Code:

- cqs communicates with Claude Code via local stdio
- Search queries and results pass through the MCP protocol
- This is local IPC, not network traffic

## Deleting Your Data

To remove all cqs data:

```bash
rm -rf .cq/                    # Project index
rm -rf ~/.cache/huggingface/   # Downloaded model
```
