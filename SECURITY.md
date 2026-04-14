# Security

## Threat Model

### What cqs Is

cqs is a **local code search tool** for developers. It runs on your machine, indexes your code, and answers semantic queries.

### Trust Boundaries

| Boundary | Trust Level | Notes |
|----------|-------------|-------|
| **Local user** | Trusted | You run cqs, you control it |
| **Project files** | Trusted | Your code, indexed by your choice |
| **External documents** | Semi-trusted | PDF/HTML/CHM files converted via `cqs convert` — parsed but not executed |
| **Reference sources** | Semi-trusted | Indexed via `cqs ref add` — search results blended with project code |

### What We Protect Against

1. **Path traversal**: Commands cannot read files outside project root
2. **FTS injection**: Search queries sanitized before SQLite FTS5 MATCH operations
3. **Database corruption**: `PRAGMA quick_check(1)` on write-mode opens (opt-out via `CQS_SKIP_INTEGRITY_CHECK=1`). Read-only opens skip the check entirely — reads cannot introduce corruption and the index is rebuildable via `cqs index --force`
4. **Reference config trust**: Warnings logged when reference configs override project settings

### What We Don't Protect Against

- **Malicious code in your project**: If your code contains exploits, indexing won't stop them
- **Local privilege escalation**: cqs runs with your permissions
- **Side-channel attacks**: Beyond timing, not in scope for a local tool

## Architecture

cqs runs locally by default. No network telemetry. Optional local command logging to `.cqs/telemetry.jsonl` — active when `CQS_TELEMETRY=1` is set OR when the telemetry file already exists (persists across shells/subprocesses). Never transmitted. Delete the file to opt out. The optional `--llm-summaries` flag sends function code to the Anthropic API (see below).

## Network Requests

The only network activity is:

- **Model download** (`cqs init`): Downloads embedding model from HuggingFace Hub
  - Default: `huggingface.co/BAAI/bge-large-en-v1.5` (~1.2GB)
  - Preset: `e5-base` (`intfloat/e5-base-v2`, ~438MB)
  - Preset: `v9-200k` (`jamie8johnson/e5-base-v2-code-search`, ~417MB) — fine-tuned E5-base LoRA
  - Custom: any HuggingFace repo via `[embedding]` config or `CQS_EMBEDDING_MODEL` env var. Custom model configs download ONNX files from the specified repo — only configure repos you trust.
  - One-time download per model, cached in `~/.cache/huggingface/`

- **Reranker model download** (first `--rerank` use): Downloads cross-encoder model from HuggingFace Hub
  - Model: `ms-marco-MiniLM-L-6-v2` (cross-encoder)
  - One-time download, cached in `~/.cache/huggingface/`

- **LLM summaries** (`cqs index --llm-summaries`): Sends function code to the Anthropic API
- **HyDE queries** (`cqs index --llm-summaries --hyde-queries`): Sends function descriptions to the Anthropic API for synthetic query generation

| Flag | Endpoint | Data Sent | Notes |
|------|----------|-----------|-------|
| `--llm-summaries` | api.anthropic.com | Function bodies (up to 8000 chars), chunk type, language | Requires `ANTHROPIC_API_KEY`. Opt-in via `cqs index --llm-summaries` |
| `--hyde-queries` | api.anthropic.com | Function NL descriptions, signatures | Requires `--llm-summaries`. Generates synthetic search queries per function |
| `--improve-docs` | api.anthropic.com | Function bodies (for doc generation) | Requires `--llm-summaries`. Writes doc comments back to source files |

- **Model export** (`cqs export-model`): Spawns Python `optimum.exporters.onnx` which downloads the specified HuggingFace model and converts to ONNX format

No other network requests are made. Without `--llm-summaries` or `export-model`, all operations are offline.

## Filesystem Access

### Read Access

| Path | Purpose | When |
|------|---------|------|
| Project source files | Parsing and embedding | `cqs index`, `cqs watch` |
| `.cqs/index.db` | SQLite database | All operations |
| `.cqs/index.hnsw.*` | HNSW vector index files | Search operations |
| `.cqs/index_base.hnsw.*` | Base (non-enriched) HNSW index | Search operations (Phase 5 dual routing) |
| `.cqs/splade.index.bin` | SPLADE sparse inverted index | Search operations (`--splade` or routed cross-language) |
| `docs/notes.toml` | Developer notes | Search, `cqs read` |
| `~/.cache/huggingface/` | ML model cache | Embedding operations |
| `~/.cache/cqs/embeddings.db` | Global embedding cache (content-addressed, capped at 1 GB) | Index and search |
| `~/.cache/cqs/query_cache.db` | Recent query embedding cache (7-day TTL) | Search |
| `~/.config/cqs/` | Config file (user-level defaults) | All operations |
| `$CQS_ONNX_DIR/` | Local ONNX model directory | When `CQS_ONNX_DIR` is set |
| `~/.local/share/cqs/refs/*/` | Reference indexes (read-only copies) | Search operations |

### Write Access

| Path | Purpose | When |
|------|---------|------|
| `.cqs/` directory | Index storage | `cqs init` |
| `.cqs/index.db` | SQLite database | `cqs index`, note operations |
| `.cqs/index.hnsw.*` | HNSW vector index + checksums | `cqs index` |
| `.cqs/index_base.hnsw.*` | Base HNSW index + checksums | `cqs index` |
| `.cqs/splade.index.bin` | SPLADE sparse inverted index | `cqs index` (with `CQS_SPLADE_MODEL` set), lazy rebuild on first `--splade` query |
| `.cqs/index.lock` | Process lock file | `cqs watch` |
| `.cqs/audit-mode.json` | Audit mode state (on/off, expiry) | `cqs audit-mode on`, `cqs audit-mode off` |
| `.cqs/telemetry*.jsonl` | Command usage logs (opt-in, persists via file presence) | `CQS_TELEMETRY=1` or file exists, delete to opt out |
| `docs/notes.toml` | Developer notes | `cqs notes add`, `cqs notes update`, `cqs notes remove` |
| `.cqs.toml` | Reference configuration | `cqs ref add`, `cqs ref remove` |
| `~/.config/cqs/projects.toml` | Project registry | `cqs project register`, `cqs project remove` |
| `~/.local/share/cqs/refs/*/` | Reference index creation and updates (write) | `cqs ref add`, `cqs ref update` |
| `~/.cache/cqs/embeddings.db` | Global embedding cache writes | `cqs index` |
| `~/.cache/cqs/query_cache.db` | Recent query embedding cache writes | Search (cache miss) |
| `~/.cache/cqs/query_log.jsonl` | Opt-in local query log | `CQS_TELEMETRY=1` or file exists |
| Project source files | Doc comment insertion | `cqs index --llm-summaries --improve-docs` |
| `<output>/` directory | ONNX model files + model.toml | `cqs export-model` |

### Process Operations

| Operation | Purpose |
|-----------|---------|
| `libc::kill(pid, 0)` | Check if watch process is running (signal 0 = existence check only) |

### Document Conversion (`cqs convert`)

The convert module spawns external processes for format conversion:

| Subprocess | Purpose | When |
|------------|---------|------|
| `python3` / `python` | PDF-to-Markdown via pymupdf4llm | `cqs convert *.pdf` |
| `7z` | CHM archive extraction | `cqs convert *.chm` |

**Attack surface:**

- **`CQS_PDF_SCRIPT` env var**: If set, the convert module executes the specified script instead of the default PDF conversion logic. This allows arbitrary script execution under the user's permissions.
- **Output directory**: Generated Markdown files are written to the `--output` directory. The output path is not sandboxed beyond normal filesystem permissions.

**Mitigations:**

- Symlink filtering: Symlinks are skipped during directory walks and archive extraction
- Zip-slip containment: Extracted paths are validated to stay within the output directory
- Page count limits: PDF conversion enforces a maximum page count to bound processing time

### Model Export (`cqs export-model`)

The export-model command spawns Python to convert HuggingFace models to ONNX format:

| Subprocess | Purpose | When |
|------------|---------|------|
| `python3` / `python` / `py` | ONNX export via `optimum.exporters.onnx` | `cqs export-model --repo org/model` |

**Attack surface:**

- **Repo ID**: Passed to `python -m optimum.exporters.onnx --model <repo>`. Validated to contain `/` and reject `"`, `\n`, `\` characters (SEC-18).
- **Output directory**: Model files and `model.toml` written to `--output` path. Not sandboxed beyond filesystem permissions.
- **Python execution**: Spawns Python with user permissions to run optimum library code.

**Mitigations:**

- Repo ID format validation prevents injection (SEC-18)
- Output path canonicalized via `dunce::canonicalize` (PB-30)
- `model.toml` restricted to 0o600 permissions on Unix (SEC-19)

### Path Traversal Protection

The `cqs read` command validates paths:

```rust
let canonical = dunce::canonicalize(&file_path)?;
let project_canonical = dunce::canonicalize(root)?;
if !canonical.starts_with(&project_canonical) {
    bail!("Path traversal not allowed: {}", path);
}
```

This blocks:
- `../../../etc/passwd` - resolved and rejected
- Absolute paths outside project - rejected
- Symlinks pointing outside - resolved then rejected

## Symlink Behavior

**Current behavior**: Symlinks are followed, then the resolved path is validated.

| Scenario | Behavior |
|----------|----------|
| `project/link → project/src/file.rs` | ✅ Allowed (target inside project) |
| `project/link → /etc/passwd` | ❌ Blocked (target outside project) |
| `project/link → ../sibling/file` | ❌ Blocked (target outside project) |

**TOCTOU consideration**: A symlink could theoretically be changed between validation and read. This is a standard filesystem race condition that affects all programs. Mitigation would require `O_NOFOLLOW` or similar, which would break legitimate symlink use cases.

**Recommendation**: If you don't trust symlinks in your project, remove them or use `--no-ignore` to skip gitignored paths where symlinks might hide.

## Index Storage

- Stored in `.cqs/index.db` (SQLite with WAL mode)
- Contains: code chunks, embeddings (1024-dim vectors for default BGE-large), file metadata
- Add `.cqs/` to `.gitignore` to avoid committing
- Database is **not encrypted** - it contains your code

## CI/CD Security

- **Dependabot**: Automated weekly checks for crate updates
- **CI workflow**: Runs clippy with `-D warnings` to catch issues
- **cargo audit**: Runs in CI, allowed warnings documented in `audit.toml`
- **No secrets in CI**: Build and test only, no publish credentials exposed

## Branch Protection

The `main` branch is protected by a GitHub ruleset:

- **Pull requests required**: All changes go through PR
- **Status checks required**: `test`, `clippy`, `fmt` must pass
- **Force push blocked**: History cannot be rewritten

## Dependency Auditing

Known advisories and mitigations:

| Crate | Advisory | Status |
|-------|----------|--------|
| `bincode` | RUSTSEC-2025-0141 | Mitigated: checksums validate data before deserialization |
| `paste` | RUSTSEC-2024-0436 | Accepted: proc-macro, no runtime impact, transitive via tokenizers |

Run `cargo audit` to check current status.

## Reporting Vulnerabilities

Report security issues to: https://github.com/jamie8johnson/cqs/issues

Use a private security advisory for sensitive issues.
