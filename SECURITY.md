# Security

## Architecture

cqs runs entirely locally. There are no external API calls during normal operation.

## Network Requests

The only network activity is:

- **Model download** (`cqs init`): Downloads ~547MB model from HuggingFace Hub
  - Source: `huggingface.co/nomic-ai/nomic-embed-text-v1.5`
  - One-time download, cached in `~/.cache/huggingface/`

No other network requests are made. Search, indexing, and all other operations are offline.

## HTTP Transport

When using `cqs serve --transport http`:

- Server binds to `127.0.0.1` (localhost only) by default
- Origin header validation (rejects non-localhost origins)
- CORS is permissive for local development
- No authentication built-in - use a reverse proxy for production
- Follows MCP Streamable HTTP spec 2025-11-25

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

## CI/CD Security

- **Dependabot**: Automated weekly checks for crate updates
- **CI workflow**: Runs clippy with `-D warnings` to catch issues
- **No secrets in CI**: Build and test only, no publish credentials exposed

## Reporting Vulnerabilities

Report security issues to: https://github.com/jamie8johnson/cqs/issues

Use a private security advisory for sensitive issues.
