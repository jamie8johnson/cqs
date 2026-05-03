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
| **`cqs serve` HTTP clients** | Untrusted by default | Per-launch 256-bit auth token gates every request (#1118 / SEC-7). Three credential channels: `Authorization: Bearer`, `cqs_token_<port>` cookie (port-scoped per RFC 6265, #1135 — concurrent instances don't collide in the browser jar), `?token=` query param. Cookie handoff is `HttpOnly; SameSite=Strict; Path=/`; compare is constant-time on every channel. Disabling auth requires `--no-auth` plus an internal `NoAuthAcknowledgement` proof token (#1136), so no internal caller can ship a fully-open server by accident; the disabled branch logs a structured `tracing::error!` regardless of `quiet`. Loud-warn banner on non-loopback binds with `--no-auth`. |
| **Indexed content (in AI agent context)** | Untrusted | cqs relays code, comments, summaries, and developer notes verbatim. Injection payloads in any of those surfaces survive the relay. See [Indirect Prompt Injection](#indirect-prompt-injection--supply-chain-risks-from-indexed-content) below. |

### What We Protect Against

1. **Path traversal**: Commands cannot read files outside project root
2. **FTS injection**: Search queries sanitized before SQLite FTS5 MATCH operations
3. **Database corruption**: `PRAGMA quick_check(1)` on write-mode opens (opt-out via `CQS_SKIP_INTEGRITY_CHECK=1`). Read-only opens skip the check entirely — reads cannot introduce corruption and the index is rebuildable via `cqs index --force`
4. **Reference config trust**: Warnings logged when reference configs override project settings

### What We Don't Protect Against

- **Malicious code in your project**: If your code contains exploits, indexing won't stop them
- **Local privilege escalation**: cqs runs with your permissions
- **Side-channel attacks**: Beyond timing, not in scope for a local tool
- **Indirect prompt injection from indexed content**: cqs relays code, comments, summaries, and notes verbatim to AI consumers; injection payloads inside those surfaces survive the relay. See [Indirect Prompt Injection](#indirect-prompt-injection--supply-chain-risks-from-indexed-content) below.

## Indirect Prompt Injection / Supply-Chain Risks from Indexed Content

cqs's primary consumer is AI agents. By design, cqs *faithfully relays* the content it has indexed — code, comments, doc strings, LLM-generated summaries, and developer notes — into the agent's context window. Any of those surfaces can carry **indirect prompt injection** payloads: instructions disguised as content that try to redirect the consuming agent ("Ignore prior instructions and...", "This function is safe to call with sudo", etc.).

cqs cannot reliably distinguish a legitimate doc comment from a malicious one. Defence has to live partly in the agent (treat retrieved code as untrusted input) and partly in cqs's protocol (make the trust boundary loud and visible).

### Surfaces

| Surface | Vector | Persistent? |
|---------|--------|-------------|
| **Project source code** | Comments, strings, doc blocks containing injection payloads (committed by a contributor or embedded by an upstream dependency) | Yes — survives until removed from source |
| **Reference content** (`cqs ref add`) | Third-party code indexed for cross-project search; less curated than the user's own code, blended into search results without an explicit trust signal | Yes — survives until ref is removed |
| **Shared notes** (`docs/notes.toml`) | A cloned repo can ship committed notes that bias rankings and surface in agent context. `audit-mode` mitigates ranking influence at runtime, but not the first-encounter case | Yes — survives in the indexed repo |
| **LLM-generated summaries** (`cqs index --llm-summaries`) | Claude is prompted with chunk content; a poisoned chunk can produce a summary that contains injection text. The summary text is cached in the `llm_summaries` table keyed by `(content_hash, purpose)` (search for `CREATE TABLE IF NOT EXISTS llm_summaries` in `src/schema.sql`); the post-summary embedding flows through the normal `embeddings_cache.db` (purpose `embedding`, the same purpose served to search) and is replayed to downstream agents | Yes — cached in `llm_summaries` table + `embeddings_cache.db` |
| **Doc-comment generation** (`cqs index --llm-summaries --improve-docs`) | LLM output is **written back to source files in place**. A poisoned chunk can produce a doc comment that lands in the user's repo on commit | **Yes — commits the LLM's output into git** |
| **Search result blending** | RRF merges chunks across project + references; the consuming agent sees a single ranked list with no in-protocol trust signal distinguishing user code from third-party content | Yes — every query |

### Current mitigations

- **`audit-mode`**: `cqs audit-mode on` excludes notes from rankings and forces direct code examination. Mitigates the runtime side of shared-notes injection.
- **No automatic execution**: cqs never executes indexed code; the threat is purely textual relay into agent context.
- **`--improve-docs` review gate (since v1.30.1)**: by default, `cqs index --improve-docs` writes proposed doc comments as unified-diff patches to `.cqs/proposed-docs/<rel>.patch` instead of mutating source files in place. Review with `git diff` and apply with `git apply .cqs/proposed-docs/**/*.patch`. Pass `--apply` to opt back into direct write-back; the run prints a warning when it does.
- **First-encounter shared-notes gate (since v1.30.1)**: on the first `cqs index` against a repo containing `docs/notes.toml`, cqs prompts to confirm before indexing the notes — committed notes affect search rankings and surface in agent context. Acceptance is persisted to `.cqs/.accepted-shared-notes` so the prompt doesn't repeat. Pass `--accept-shared-notes` to bypass for CI / scripted use; non-TTY stdin auto-skips the notes pass with a warning so CI never hangs. (#1168)
- **`trust_level` + `reference_name` on chunk JSON (since v1.30.1, three-tier as of #1221)**: every chunk-returning JSON output (`search`, `gather`, `task`, `scout`, `onboard`, `read`, `read --focus`, `context`, `similar`) carries one of:
  - `"user-code"` — chunk lives in the user's project store and did not match any vendored-path prefix at index time.
  - `"vendored-code"` — chunk lives in the user's project store **but** its origin passed through a configured vendored-path segment (default list: `vendor`, `third_party`, `node_modules`, `.cargo`, `target`, `dist`, `build`; override via `[index].vendored_paths` in `.cqs.toml`). Treat as third-party content for indirect-injection purposes — same threat profile as `reference-code`. (#1221, schema v24)
  - `"reference-code"` (with `reference_name`) — chunk lives in a `cqs ref` reference index. Wins over `vendored-code` when both apply: the per-reference name is the more useful agent-facing signal.

  **Scope of `user-code` after #1221.** `trust_level: "user-code"` now means *"from the user's project store AND not under a configured vendored-path prefix"*. Vendored upstream content (`vendor/`, `third_party/`, `node_modules/`, etc.) is structurally distinguished as `"vendored-code"` at index time — the structural fix the SEC-V1.30.1-5 doc-only stop-gap acknowledged was needed. **Caveats that still apply**: (1) `user-code` does not mean "authored by the user" — generated code under `src/` is still `user-code`. (2) Developer notes (`docs/notes.toml` content surfaced via `cqs notes mention`) still ride the user-code label since they're stored separately and don't pass through the chunk pipeline. (3) Reindex required to retroactively flag pre-v24 chunks: the v23→v24 migration adds the column with default `0`; only the next `cqs index` (or `cqs watch` re-emit) populates it.
- **`CQS_TRUST_DELIMITERS` is on by default (since v1.30.2)**: every chunk's `content` is wrapped in `<<<chunk:{id}>>> ... <<</chunk:{id}>>>` markers so prompt-injection guards downstream of cqs can detect content boundaries even when the agent inlines the rendered string into a larger prompt. Set `CQS_TRUST_DELIMITERS=0` to opt out (raw text). Was opt-in in v1.30.1. (#1167, #1181)
- **LLM summary validation (since v1.30.1)**: every summary headed for the `llm_summaries` cache passes through `cqs::llm::validation::validate_summary` before insertion. Catches lazy injections (leading "Ignore prior" / "Disregard"; embedded code fences; embedded URLs) and enforces a 1500-char hard length cap. Configurable via `CQS_SUMMARY_VALIDATION=strict|loose|off` (default `loose`: log + keep on pattern match, truncate over-long; strict drops pattern-matched summaries entirely). Doc-comment generation is intentionally exempt — its prose is imperative by design and would false-positive; it has its own review gate (#1166). (#1170)
- **`_meta.handling_advice` on every JSON envelope (since v1.30.2)**: every JSON-emitting command surface (`emit_json`, batch `write_json_line`, daemon socket) carries a constant `_meta.handling_advice` string framing the response as untrusted-by-default for any consuming agent — `trust_level` signals origin, not safety; `injection_flags` lists which heuristics fired but cqs never refuses to relay. Free to ignore; ~80 bytes once per response. (#1181)
- **`_meta.worktree_stale` + `_meta.worktree_name` (#1254)**: when a cqs command runs from inside a `git worktree` that has no `.cqs/` of its own, cqs auto-discovers the main project's index via the worktree's `.git/commondir` link and serves queries against main's `.cqs/`. Every JSON envelope from that process carries `worktree_stale: true` plus the worktree's directory name in `worktree_name`. Consuming agents should treat the served snapshot as reflecting **main's branch**, not uncommitted edits in the worktree — fall back to reading absolute worktree paths for any chunk that's about to be edited. The fields are absent (skipped) for the non-worktree happy path so the wire shape only grows when the redirect actually fires. Closes the worktree-leakage class of bugs documented in `feedback_agent_worktrees.md`.
- **Per-chunk `injection_flags` array (since v1.30.2)**: every chunk-returning JSON output additionally surfaces an `injection_flags: []` field listing which injection heuristics fired on the chunk's raw content (e.g. `["leading-directive", "code-fence", "embedded-url"]`). Empty array when nothing matched, always present so the schema stays stable. cqs labels — never refuses to relay. Agents that want a stricter posture can refuse to act on chunks with non-empty `injection_flags`. (#1181)

These are defence in depth, not absolute protection. Subtle injections (a summary that is superficially correct but biased) will still get through. The agent-side defence — treat retrieved code as untrusted input, sandbox tool calls, never execute payload-shaped output — remains the load-bearing layer.

## Architecture

cqs runs locally by default. No network telemetry. Optional local command logging to `.cqs/telemetry.jsonl` — active when `CQS_TELEMETRY=1` is set OR when the telemetry file already exists (persists across shells/subprocesses). Never transmitted. Delete the file to opt out. The optional `--llm-summaries` flag sends function code to the Anthropic API (see below).

## Network Requests

The only network activity is:

- **Model download** (`cqs init`): Downloads embedding model from HuggingFace Hub
  - Default since v1.35.0: `huggingface.co/onnx-community/embeddinggemma-300m-ONNX` (~1.2GB FP32 ONNX bundle + ~20MB tokenizer)
  - Preset: `bge-large` (`BAAI/bge-large-en-v1.5`, ~1.2GB) — former default; opt-in via `CQS_EMBEDDING_MODEL=bge-large`
  - Preset: `e5-base` (`intfloat/e5-base-v2`, ~438MB)
  - Preset: `nomic-coderank` (`jamie8johnson/CodeRankEmbed-onnx`, ~547MB) — code-specialised, opt-in via `CQS_EMBEDDING_MODEL=nomic-coderank` (#1110)
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
| `.cqs/slots/<name>/index.db` | SQLite database (per-slot, PR #1105). Pre-migration projects may still see the legacy `.cqs/index.db`. | All operations |
| `.cqs/slots/<name>/index.hnsw.*` | HNSW vector index files (per-slot) | Search operations |
| `.cqs/slots/<name>/index_base.hnsw.*` | Base (non-enriched) HNSW index (per-slot) | Search operations (Phase 5 dual routing) |
| `.cqs/splade.index.bin` | SPLADE sparse inverted index | Search operations (`--splade` or routed cross-language) |
| `docs/notes.toml` | Developer notes | Search, `cqs read` |
| `~/.cache/huggingface/` | ML model cache | Embedding operations |
| `<project>/.cqs/embeddings_cache.db` | Per-project embedding cache (PR #1105, primary; legacy global cache at `~/.cache/cqs/embeddings.db` is fallback) | `cqs index`, search |
| `~/.cache/cqs/embeddings.db` | Global embedding cache (content-addressed, capped at 1 GB). Linux path; on macOS resolves to `~/Library/Caches/cqs/embeddings.db`, on Windows to `%LOCALAPPDATA%\cqs\embeddings.db` | Index and search |
| `~/.cache/cqs/query_cache.db` | Recent query embedding cache (size-capped at `CQS_QUERY_CACHE_MAX_SIZE`, 100 MiB default). Linux path; macOS `~/Library/Caches/cqs/query_cache.db`; Windows `%LOCALAPPDATA%\cqs\query_cache.db` | Search |
| `~/.config/cqs/` | Config file (user-level defaults) | All operations |
| `$CQS_ONNX_DIR/` | Local ONNX model directory | When `CQS_ONNX_DIR` is set |
| `~/.local/share/cqs/refs/*/` | Reference indexes (read-only copies) | Search operations |

### Write Access

| Path | Purpose | When |
|------|---------|------|
| `.cqs/` directory | Index storage | `cqs init` |
| `.cqs/slots/<name>/index.db` | SQLite database (per-slot, PR #1105). Pre-migration projects may still see the legacy `.cqs/index.db`. | `cqs index`, note operations |
| `.cqs/slots/<name>/index.hnsw.*` | HNSW vector index + checksums (per-slot) | `cqs index` |
| `.cqs/slots/<name>/index_base.hnsw.*` | Base HNSW index + checksums (per-slot) | `cqs index` |
| `.cqs/splade.index.bin` | SPLADE sparse inverted index | `cqs index` (with `CQS_SPLADE_MODEL` set), lazy rebuild on first `--splade` query |
| `.cqs/index.lock` | Process lock file | `cqs watch` |
| `.cqs/audit-mode.json` | Audit mode state (on/off, expiry) | `cqs audit-mode on`, `cqs audit-mode off` |
| `.cqs/telemetry*.jsonl` | Command usage logs (opt-in, persists via file presence) | `CQS_TELEMETRY=1` or file exists, delete to opt out |
| `docs/notes.toml` | Developer notes | `cqs notes add`, `cqs notes update`, `cqs notes remove` |
| `.cqs.toml` | Reference configuration | `cqs ref add`, `cqs ref remove` |
| `~/.config/cqs/projects.toml` | Project registry | `cqs project register`, `cqs project remove` |
| `~/.local/share/cqs/refs/*/` | Reference index creation and updates (write) | `cqs ref add`, `cqs ref update` |
| `<project>/.cqs/embeddings_cache.db` | Per-project embedding cache writes (primary; PR #1105) | `cqs index`, search |
| `~/.cache/cqs/embeddings.db` | Global embedding cache writes (legacy fallback). Linux path; macOS `~/Library/Caches/cqs/embeddings.db`; Windows `%LOCALAPPDATA%\cqs\embeddings.db` | `cqs index` |
| `~/.cache/cqs/query_cache.db` | Recent query embedding cache writes. Linux path; macOS `~/Library/Caches/cqs/query_cache.db`; Windows `%LOCALAPPDATA%\cqs\query_cache.db` | Search (cache miss) |
| `~/.cache/cqs/query_log.jsonl` | Local query log (append-only). Linux path; macOS `~/Library/Caches/cqs/query_log.jsonl`; Windows `%LOCALAPPDATA%\cqs\query_log.jsonl` | `cqs chat` / `cqs batch` (search, gather, onboard, scout, where, task) |
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

cqs has **two** symlink-handling regimes, depending on the entry point.

### Directory walks (`cqs index`, `cqs ref add`, `cqs watch` reconcile, `cqs convert`)

Symlinks are **skipped** entirely — `enumerate_files` (`src/lib.rs:601`) sets `WalkBuilder::follow_links(false)` and `cqs convert`'s archive extraction skips them in extract paths. The walker never opens the link's target.

| Scenario | Behavior |
|----------|----------|
| `project/link → project/src/file.rs` | Skipped (symlink, regardless of target) |
| `project/link → /etc/passwd` | Skipped |
| `project/link → ../sibling/file` | Skipped |

This is conservative: a monorepo workspace that uses in-tree symlinks to share common code will silently miss those files. Workaround: replace the symlinks with the actual files (or use a `[references]` config block to index the shared tree as a separate slot).

### Explicit-path canonicalization (`cqs read <path>`, `cqs ref add --source <path>`)

When the user passes a path on the command line, cqs canonicalizes it (`dunce::canonicalize`), then validates the resolved path against the project root.

| Scenario | Behavior |
|----------|----------|
| `cqs read link` where `link → project/src/file.rs` | Allowed (target inside project, canonicalised path reads `project/src/file.rs`) |
| `cqs read link` where `link → /etc/passwd` | Blocked (target outside project) |
| `cqs read link` where `link → ../sibling/file` | Blocked (target outside project) |

**`cqs ref add --source <path>` redirect surfacing (since v1.30.2, [#1222](https://github.com/jamie8johnson/cqs/issues/1222))**: when the user-supplied `--source` path resolves through a symlink to a different filesystem location, `cmd_ref_add` emits a `tracing::warn!` (and a `WARN:` line on stderr in non-`--quiet` text mode) naming both the user-supplied and resolved paths. JSON output gains a `warnings: ["source path '<input>' resolved via symlink to '<target>'"]` field. The reference is still indexed at the resolved path — the warning exists so an operator who ran `cqs ref add foo vendored-monorepo-pull/` against a symlink to `~/work/customer-A-private/` can see what they actually pulled in. Lexical normalization (`..`, `.`, repeated separators) is applied before comparison so purely syntactic differences in the input don't trigger false positives.

**TOCTOU consideration**: A symlink could theoretically be changed between canonicalization and read. This is a standard filesystem race condition that affects all programs. Mitigation would require `O_NOFOLLOW` or similar, which would break legitimate symlink use cases on `cqs read`.

**Recommendation**: If you don't trust symlinks in your project, remove them. The directory-walk path is already conservative.

## Windows-Specific Asymmetries

Several `cfg(unix)` paths apply file-mode hardening that the `cfg(not(unix))` arms can't replicate without per-file ACL programming. Documented here so the SEC promises don't read as universal when they're actually Linux/macOS-specific.

### Audit-mode marker (`src/audit.rs::write_state`)

- **Linux/macOS**: file is created with `mode(0o600)` so it's never world-readable; the audit token can't be read by other accounts on a shared host.
- **Windows**: file is created via `std::fs::write` and inherits the parent directory's DACL. The default `%LOCALAPPDATA%\cqs\` location inherits user-only ACLs from the user profile, which is fine — but operators with roaming profiles, or who've relocated `%LOCALAPPDATA%` to a shared mount (some enterprise deployments), can end up with the audit-mode file readable by other authenticated users on the same machine.
- **Recommendation**: on Windows, verify the parent directory's ACL doesn't grant read to other users before treating the audit-mode marker as a confidentiality boundary. Audit-mode is a debugging aid, not a credential store; the file contains a timestamp + opt-in flag, not a token.

### Embedding cache file mode (`src/cache.rs::apply_db_file_perms`)

- **Linux/macOS**: `apply_db_file_perms` sets `mode(0o600)` after creation.
- **Windows**: `apply_db_file_perms(_path: &Path) {}` is a no-op. Same DACL inheritance story as above. The cache contains chunk content_hashes + embedding vectors — not directly sensitive, but the same operator-environment caveat applies.

### Database file backup discovery (`src/store/migrations::db_file_identity`)

- **Linux/macOS**: identity is `(dev, inode)` — durable across `mtime` changes from `--force` reindexes.
- **Windows / non-Unix**: identity falls back to `mtime`, which can collide if two `--force` operations run in the same second. The legacy backup discovery path (used only for the v18→v19 migration on existing indexes) may misidentify a fresh DB as the pre-migration one. Mitigated in practice because the v18→v19 migration shipped over a year ago and operators on current schemas don't hit this code path.

### Hook scripts on Windows-native (`src/cli/hook.rs`)

- The `cqs hook install` script body assumes `cqs` is on PATH from a POSIX-style shell. On Windows-native Git (cmd.exe / PowerShell), the inherited PATH from MSYS shells doesn't always carry over. Operators on native-Windows git should use Git for Windows' bundled bash for the hook scripts to fire reliably.

These limitations are tracked as Windows-specific issues in the v1.33.0 audit batch (#1353, #1354, #1355).

## Index Storage

- Stored in `.cqs/slots/<name>/index.db` (SQLite with WAL mode; PR #1105 introduced per-slot layout, pre-migration projects may still see the legacy `.cqs/index.db`)
- Contains: code chunks, embeddings (768-dim vectors for default embeddinggemma-300m since v1.35.0; 1024-dim for bge-large preset), file metadata
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
