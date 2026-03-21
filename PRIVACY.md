# Privacy

## Data Stays Local

cqs processes your code locally by default. With `--llm-summaries`, function code is sent to Anthropic's API for one-sentence summary generation. With `--improve-docs`, LLM-generated doc comments are written back to your source files. With `--hyde-queries`, function descriptions are sent to Anthropic's API for synthetic search query generation. See [Anthropic's privacy policy](https://www.anthropic.com/privacy). Without these flags, nothing is transmitted externally and no source files are modified.

- **No telemetry**: We collect no usage data
- **No analytics**: No tracking of any kind
- **No cloud sync**: Index stays in your project directory

## What Gets Stored

When you run `cqs index`, the following is stored in `.cqs/index.db`:

- Code chunks (functions, methods, documentation sections)
- Embedding vectors (768-dimensional floats from E5-base-v2)
- File paths and line numbers
- File modification times

This data never leaves your machine.

## Model Download

The embedding model is downloaded once from HuggingFace:

- Model: `intfloat/e5-base-v2`
- Size: ~547MB
- Cached in: `~/.cache/huggingface/`

HuggingFace may log download requests per their privacy policy. After download, the model runs offline.

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
rm -f ~/.config/cqs/config.toml       # User configuration
rm -f .cqs.toml                       # Project config
rm -f docs/notes.toml                 # Project notes
rm -rf ~/.cache/huggingface/          # Downloaded model
```
