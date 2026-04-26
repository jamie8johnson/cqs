# Privacy

## Data Stays Local

cqs processes your code locally by default. With `--llm-summaries`, function code is sent to Anthropic's API for one-sentence summary generation. With `--improve-docs`, LLM-generated doc comments are written back to your source files. With `--hyde-queries`, function descriptions are sent to Anthropic's API for synthetic search query generation. See [Anthropic's privacy policy](https://www.anthropic.com/privacy). Without these flags, nothing is transmitted externally and no source files are modified.

- **No telemetry by default**: Optional local-only command logging when `CQS_TELEMETRY=1` is set OR when `.cqs/telemetry.jsonl` already exists (persists opt-in across shells/subprocesses). Stored in `.cqs/telemetry.jsonl`, never transmitted. Delete the file and unset the env var to opt out
- **No analytics**: No tracking of any kind
- **No cloud sync**: Index stays in your project directory

## What Gets Stored

When you run `cqs index`, the following is stored under `.cqs/`:

- `.cqs/slots/<name>/index.db` — code chunks, embedding vectors (dim depends on configured model; 1024 for BGE-large default, 768 for E5-base / nomic-coderank presets), file paths, line numbers, modification times. Per-named-slot, side-by-side (#1105). Pre-migration projects may still see a legacy single-slot path at `.cqs/index.db`.
- `.cqs/embeddings_cache.db` — per-project embedding cache, keyed by `(content_hash, model_id)` (#1105). Skips re-embedding chunks that haven't changed across reindexes / model swaps.

A legacy global cache may also exist from older versions:

- `~/.cache/cqs/embeddings.db` — pre-#1105 cross-project embedding cache, capped at 1 GB by default (`CQS_CACHE_MAX_SIZE`). Still consulted when the per-project cache misses.
- `~/.cache/cqs/query_cache.db` — recent query embeddings with a 7-day TTL. Speeds up repeated searches.
- `~/.cache/cqs/query_log.jsonl` — opt-in query log, written only when `CQS_TELEMETRY=1` or the file already exists. Stays local.

This data never leaves your machine.

## Model Download

The embedding model is downloaded once from HuggingFace:

- Default: `BAAI/bge-large-en-v1.5` (BGE-large, ~1.2GB, 1024-dim)
- Preset: `intfloat/e5-base-v2` (E5-base, ~438MB, 768-dim)
- Preset: `nomic-ai/CodeRankEmbed` (nomic-coderank, ~547MB, 768-dim) — code-specialised, opt-in via `CQS_EMBEDDING_MODEL=nomic-coderank` (#1110)
- Custom: any HuggingFace repo via `[embedding]` config section, `--model` CLI flag, or `CQS_EMBEDDING_MODEL` env var
- Size varies by model
- Cached in: `~/.cache/huggingface/`

HuggingFace may log download requests per their privacy policy. Custom model configurations cause downloads from the specified HuggingFace repository. After download, the model runs offline.

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
rm -rf ~/.cache/cqs/                  # Embedding + query caches, query log
rm -rf ~/.cache/huggingface/          # Downloaded model
```
