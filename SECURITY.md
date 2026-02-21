# Security

## Threat Model

### What cqs Is

cqs is a **local code search tool** for developers. It runs on your machine, indexes your code, and answers semantic queries.

### Trust Boundaries

| Boundary | Trust Level | Notes |
|----------|-------------|-------|
| **Local user** | Trusted | You run cqs, you control it |
| **Project files** | Trusted | Your code, indexed by your choice |

### What We Protect Against

1. **Path traversal**: Commands cannot read files outside project root
2. **FTS injection**: Search queries sanitized before SQLite FTS5 MATCH operations
3. **Database corruption**: `PRAGMA quick_check` on every database open
4. **Reference config trust**: Warnings logged when reference configs override project settings

### What We Don't Protect Against

- **Malicious code in your project**: If your code contains exploits, indexing won't stop them
- **Local privilege escalation**: cqs runs with your permissions
- **Side-channel attacks**: Beyond timing, not in scope for a local tool

## Architecture

cqs runs entirely locally. No telemetry, no external API calls during operation.

## Network Requests

The only network activity is:

- **Model download** (`cqs init`): Downloads ~440MB model from HuggingFace Hub
  - Source: `huggingface.co/intfloat/e5-base-v2`
  - One-time download, cached in `~/.cache/huggingface/`

- **Reranker model download** (first `--rerank` use): Downloads cross-encoder model from HuggingFace Hub
  - Model: `ms-marco-MiniLM-L-6-v2` (cross-encoder)
  - One-time download, cached in `~/.cache/huggingface/`

No other network requests are made. Search, indexing, and all other operations are offline.

## Filesystem Access

### Read Access

| Path | Purpose | When |
|------|---------|------|
| Project source files | Parsing and embedding | `cqs index`, `cqs watch` |
| `.cqs/index.db` | SQLite database | All operations |
| `.cqs/index.hnsw.*` | Vector index files | Search operations |
| `docs/notes.toml` | Developer notes | Search, `cqs read` |
| `~/.cache/huggingface/` | ML model cache | Embedding operations |
| `~/.config/cqs/` | Config file (user-level defaults) | All operations |
| `~/.local/share/cqs/refs/*/` | Reference indexes (read-only copies) | Search operations |

### Write Access

| Path | Purpose | When |
|------|---------|------|
| `.cqs/` directory | Index storage | `cqs init` |
| `.cqs/index.db` | SQLite database | `cqs index`, note operations |
| `.cqs/index.hnsw.*` | Vector index + checksums | `cqs index` |
| `.cqs/cqs.pid` | Process lock file | `cqs watch` |
| `docs/notes.toml` | Developer notes | `cqs notes add`, `cqs notes update`, `cqs notes remove` |
| `~/.local/share/cqs/refs/*/` | Reference index creation and updates (write) | `cqs ref add`, `cqs ref update` |

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

- Symlink filtering: Symlinks in extracted archives are skipped
- Zip-slip containment: Extracted paths are validated to stay within the output directory
- Page count limits: PDF conversion enforces a maximum page count to bound processing time

### Path Traversal Protection

The `cqs read` command validates paths:

```rust
let canonical = file_path.canonicalize()?;
if !canonical.starts_with(&project_root) {
    return Err("Path traversal not allowed");
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
- Contains: code chunks, embeddings (769-dim vectors), file metadata
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
