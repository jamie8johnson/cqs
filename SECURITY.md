# Security

## Threat Model

### What cqs Is

cqs is a **local code search tool** for developers. It runs on your machine, indexes your code, and answers semantic queries. It's designed to be used with Claude Code as an MCP server.

### Trust Boundaries

| Boundary | Trust Level | Notes |
|----------|-------------|-------|
| **Local user** | Trusted | You run cqs, you control it |
| **Project files** | Trusted | Your code, indexed by your choice |
| **MCP client** | Semi-trusted | Claude Code or other MCP clients |
| **Network** | Untrusted | Only relevant with `--transport http` |

### What We Protect Against

1. **Path traversal**: Queries cannot read files outside project root
2. **Network exposure**: Localhost-only by default, requires explicit flag for network binding
3. **DNS rebinding**: Origin validation rejects `localhost.evil.com` style attacks
4. **Timing attacks**: API key validation uses constant-time comparison
5. **Resource exhaustion**: Query length limits, request body limits

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

No other network requests are made. Search, indexing, and all other operations are offline.

## HTTP Transport Security

When using `cqs serve --transport http`:

| Control | Default | Override |
|---------|---------|----------|
| **Bind address** | `127.0.0.1` (localhost) | `--bind` + `--dangerously-allow-network-bind` |
| **API key** | None required | `--api-key`, `--api-key-file`, or `CQS_API_KEY` env var |
| **Origin validation** | Localhost only | Rejects external origins |
| **Body limit** | 1MB | Prevents oversized payloads |
| **Protocol version** | 2025-11-25 | MCP Streamable HTTP spec |

**API key options:**
- `--api-key SECRET` - Direct value (visible in process list)
- `--api-key-file /path/to/file` - Read from file (recommended, keeps secret out of `ps aux`)
- `CQS_API_KEY=SECRET` - Environment variable (visible in `/proc/*/environ`)

The `--api-key-file` option uses `zeroize` to clear the key from memory when dropped.

**When binding to network (`0.0.0.0`):**
- API key becomes **required**
- Use HTTPS via reverse proxy for production
- Consider firewall rules to limit access

**Origin validation accepts:**
- `http://localhost`, `http://localhost:*`
- `http://127.0.0.1`, `http://127.0.0.1:*`
- `http://[::1]`, `http://[::1]:*` (IPv6 localhost)
- HTTPS variants of above

## Filesystem Access

### Read Access

| Path | Purpose | When |
|------|---------|------|
| Project source files | Parsing and embedding | `cqs index`, `cqs watch` |
| `.cq/index.db` | SQLite database | All operations |
| `.cq/hnsw.*` | Vector index files | Search operations |
| `docs/notes.toml` | Developer notes | Search, `cqs_read` |
| `~/.cache/huggingface/` | ML model cache | Embedding operations |
| `~/.config/cqs/` | User config (future) | Not yet implemented |

### Write Access

| Path | Purpose | When |
|------|---------|------|
| `.cq/` directory | Index storage | `cqs init` |
| `.cq/index.db` | SQLite database | `cqs index`, note operations |
| `.cq/hnsw.*` | Vector index | `cqs index` |
| `.cq/checksums.bin` | File change detection | `cqs index` |
| `.cq/cqs.pid` | Process lock file | `cqs watch` |
| `docs/notes.toml` | Developer notes | `cqs_add_note`, `cqs_update_note`, `cqs_remove_note` |

### Process Operations

| Operation | Purpose |
|-----------|---------|
| `libc::kill(pid, 0)` | Check if watch process is running (signal 0 = existence check only) |

### Path Traversal Protection

The `cqs_read` MCP tool validates paths:

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

- Stored in `.cq/index.db` (SQLite with WAL mode)
- Contains: code chunks, embeddings (769-dim vectors), file metadata
- Add `.cq/` to `.gitignore` to avoid committing
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
