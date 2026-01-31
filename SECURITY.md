# Security

## Architecture

cqs runs entirely locally. There are no external API calls during normal operation.

## Network Requests

The only network activity is:

- **Model download** (`cqs init`): Downloads ~547MB model from HuggingFace Hub
  - Source: `huggingface.co/nomic-ai/nomic-embed-text-v1.5`
  - One-time download, cached in `~/.cache/huggingface/`

No other network requests are made. Search, indexing, and all other operations are offline.

## File Access

cqs accesses:

- **Project files**: Read-only, to parse and embed code
- **Index directory**: `.cq/` in project root (created by `cqs init`)
- **Model cache**: `~/.cache/huggingface/` (HuggingFace default)
- **Cargo credentials**: `~/.cargo/credentials.toml` (only for `cargo publish`)

## Index Storage

- Stored in `.cq/index.db` (SQLite)
- Contains: code chunks, embeddings, file metadata
- Add `.cq/` to `.gitignore` to avoid committing

## Reporting Vulnerabilities

Report security issues to: https://github.com/jamie8johnson/cq/issues

Use a private security advisory for sensitive issues.
