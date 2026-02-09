# Privacy

## Data Stays Local

cqs processes your code entirely on your machine. Nothing is transmitted externally.

- **No telemetry**: We collect no usage data
- **No analytics**: No tracking of any kind
- **No cloud sync**: Index stays in your project directory

## What Gets Stored

When you run `cqs index`, the following is stored in `.cqs/index.db`:

- Code chunks (functions, methods, documentation sections)
- Embedding vectors (769-dimensional floats: 768 from E5-base-v2 + 1 sentiment dimension)
- File paths and line numbers
- File modification times

This data never leaves your machine.

## Model Download

The embedding model is downloaded once from HuggingFace:

- Model: `intfloat/e5-base-v2`
- Size: ~440MB
- Cached in: `~/.cache/huggingface/`

HuggingFace may log download requests per their privacy policy. After download, the model runs offline.

## MCP Server

When using `cqs serve` with Claude Code:

- **stdio transport** (default): Communicates via local stdin/stdout
- **HTTP transport**: Binds to localhost (127.0.0.1) only
- Search queries and results pass through the MCP protocol
- All communication is local - no external network traffic

## CI/CD

If you fork or contribute to the cqs repository:

- GitHub Actions runs tests on push/PR
- Code is processed on GitHub-hosted runners
- No index data is uploaded (only source code)
- See GitHub's privacy policy for runner data handling

## Deleting Your Data

To remove all cqs data:

```bash
rm -rf .cqs/                          # Project index
rm -rf ~/.local/share/cqs/refs/       # Reference indexes
rm -rf ~/.config/cqs/projects.toml    # Project registry
rm -f .cqs.toml                       # Project config
rm -f docs/notes.toml                 # Project notes
rm -rf ~/.cache/huggingface/          # Downloaded model
```
