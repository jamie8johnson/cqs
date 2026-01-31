# Hunches

Soft observations, gut feelings, latent risks. Append new entries as they arise.

---

## 2026-01-31 - tree-sitter version gap

Grammar crates (0.23.x) have dev-dep on tree-sitter ^0.23, but we're using tree-sitter 0.26. Works via `tree-sitter-language` abstraction layer, but feels fragile. If parsing breaks mysteriously, check this first.

---

## 2026-01-31 - ort 2.x is still RC

Using `ort = "2.0.0-rc.11"` - no stable 2.0 release yet. API could change. Pin exact version and watch for breaking changes on upgrade.

---

## 2026-01-31 - WSL /mnt/c/ permission hell

Building Rust on Windows paths from WSL causes random permission errors (libsqlite3-sys, git config). Workaround in place (.cargo/config.toml), but might bite us elsewhere.

---

## 2026-01-31 - glob crate is abandoned

`glob 0.3` hasn't been updated since 2016. Works fine for basic patterns but if we need anything fancier, `globset` from ripgrep is the modern replacement.

**RESOLVED 2026-01-31:** Replaced glob with globset in Audit Phase A.

---

## 2026-01-31 - Brute-force search will hit a wall

O(n) search with 100k chunks = 300ms. Users will notice. HNSW in Phase 4 is non-negotiable for serious use. Consider adding a warning in `cq stats` when chunk count exceeds 50k.

---

## 2026-01-31 - Symlinks are a landmine

No policy defined. Could follow into `/etc/`, could loop forever, could index vendor code outside project. Default should be "skip symlinks" - safer and predictable.

---

## 2026-01-31 - Model versioning time bomb

If nomic releases v2.0 with different embeddings, old indexes become garbage. Need to check model_name on every query and warn loudly if mismatched. Re-index isn't optional in that case.

---

## 2026-01-31 - MCP server resource consumption

`cq_index` tool in MCP allows remote triggering of reindex. On large codebases this is CPU/GPU intensive. Could be DoS vector if SSE transport exposed to network. Keep SSE localhost-only or add rate limiting.

---

## 2026-01-31 - Embedding model checksums still empty

Model verification skeleton exists but SHA256 constants are empty TODOs. Need to actually download the model, compute checksums, and fill them in. First person to run `cq init` should do this.

**RESOLVED 2026-01-31:** Filled blake3 checksums in embedder.rs:15-16. Verified with `cqs doctor`.

---

## 2026-01-31 - Two-phase search trades latency for memory

New two-phase search (id+embedding first, content second) reduces memory but adds a second SQL query. For small indexes this might be slower. Could add threshold: single-phase for <10k chunks, two-phase above.

---

## 2026-01-31 - WSL2 CUDA is fragile (UPDATE: working reliably after reboot)

Original concern: GPU visibility in WSL2 can drop randomly during package installation.

**Update 2026-01-31:** After WSL reboot, CUDA working reliably. RTX A6000, CUDA 13.0 driver, cuDNN 9.18.1. ort detects CUDA automatically. Getting 6ms single queries, 0.3ms/doc in batches. The fragility may have been during initial setup - once stable, it stays stable.

---

## 2026-01-31 - ONNX model input/output assumptions

nomic-embed-text-v1.5 ONNX model needs: i64 inputs (not i32), token_type_ids (all zeros), and outputs last_hidden_state (not sentence_embedding). These aren't obvious from docs. If switching models, verify inputs/outputs with the model directly - don't assume they're standard.

---

## 2026-01-31 - MCP server working directory is unpredictable

Claude Code starts MCP servers from an unknown cwd. The `find_project_root()` approach (walk up looking for Cargo.toml/.git) fails because the server isn't started from the project directory. Solution: always require `--project` in MCP config. This is a footgun for any MCP server that relies on cwd inference.

---

## 2026-01-31 - Claude Code MCP config lives in ~/.claude.json, not .mcp.json

The `.mcp.json` file in the project root isn't used by Claude Code. Config set via `claude mcp add` goes to `~/.claude.json` under `projects["/path/to/project"].mcpServers`. Editing `.mcp.json` does nothing. Also: Claude Code caches config in memory - changes to `~/.claude.json` require restart to take effect.

---

## 2026-01-31 - ort logs to stdout by default

The `ort` crate's internal logging goes to stdout unless you configure the subscriber otherwise. For stdio-based JSON-RPC (like MCP), this pollutes the response stream. Fix: `tracing_subscriber::fmt().with_writer(std::io::stderr).init()`.

---

## 2026-01-30 - MCP tools/call response format is easy to get wrong

MCP `tools/call` responses MUST wrap results in `{"content":[{"type":"text","text":"..."}]}`. Returning raw JSON works when tested standalone but fails silently in Claude Code - the tool runs but results appear empty. Not obvious from error messages.

---

## 2026-01-31 - Absolute vs relative paths in index

Storing absolute paths in chunk IDs breaks path pattern filtering and makes indexes non-portable. The fix was straightforward but required touching multiple places: enumerate_files returns relative paths, parse_files rewrites chunk paths, callers join with root for filesystem ops. Test with glob patterns early - they're a good canary for path issues.

---

## 2026-01-31 - WSL cargo config breaks CI

`.cargo/config.toml` with custom `target-dir` is needed locally (avoids /mnt/c/ permission issues) but breaks CI (GitHub Actions can't access /home/user001/.cargo-target). Solution: gitignore `.cargo/` entirely. CI uses default target-dir, local dev uses custom config.

**RESOLVED 2026-01-31:** Added `.cargo/` to .gitignore, removed from tracking.

---

## 2026-01-31 - clap trailing_var_arg eats flags after query

With `#[arg(trailing_var_arg = true)]` on the query field, any flags that appear AFTER the query get consumed as part of the query text. This means `cqs "foo" -C 3` doesn't work (context=None) but `cqs -C 3 "foo"` does. Documented in README but could surprise users. Might need to restructure CLI parsing or use `--` separator.

**RESOLVED 2026-01-31:** Removed trailing_var_arg. Query is now positional `Option<String>`. Users must quote multi-word queries but flags work anywhere.

---

## 2026-01-31 - MCP SSE transport deprecated

The MCP spec (2025-03-26) deprecated HTTP+SSE transport in favor of "Streamable HTTP". Key differences:
- Single `/mcp` endpoint instead of separate `/sse` and `/messages`
- POST for requests, optional GET for server-initiated SSE stream
- Session management via `Mcp-Session-Id` header

Implemented Streamable HTTP transport. Kept "sse" as alias mapping to "http" for backwards compat with existing configs.

---

## 2026-01-31 - Review MCP docs periodically

MCP spec evolves. The SSE deprecation caught us by surprise. Periodically check:
- https://modelcontextprotocol.io/specification (official spec)
- https://mcp-framework.com/docs (framework docs)
- https://github.com/anthropics/anthropic-cookbook (examples)

Look for: new transport options, deprecations, new capabilities, security advisories.

**UPDATE 2026-01-31:** Found spec moved to 2025-11-25. Changes: MCP-Protocol-Version header required, Origin validation mandatory, batching removed. Updated HTTP transport.

---

## 2026-01-31 - Dependency drift is silent risk

We depend on fast-moving projects:
- **ort 2.0.0-rc.11** - still RC, API could change
- **tree-sitter grammars** - version gap (0.23 vs 0.26)
- **nomic-embed-text** - model updates break index compatibility
- **MCP spec** - deprecations happen (SSE → Streamable HTTP)

**AUTOMATED 2026-01-31:** Added Dependabot for crate PRs + GitHub Action for MCP/model checks. Runs weekly on Mondays. CI workflow (build, test, clippy) catches breaking changes early.

---

## 2026-01-31 - r2d2 pool size may need tuning

Added r2d2-sqlite with max 4 connections. This is arbitrary. For CPU-bound embedding work, more connections don't help (bottleneck is GPU/CPU, not DB). For pure search workloads (parallel queries), more connections could help. Monitor if users report connection pool exhaustion errors.

---

## 2026-01-31 - Brute-force search will hit a wall

**RESOLVED 2026-01-31:** HNSW index added in v0.1.8. O(log n) search with hnsw_rs. Falls back to brute-force when filters are active (can't pre-filter HNSW candidates efficiently yet).

---

## 2026-01-31 - hnsw_rs lifetime design forces reload on search

The hnsw_rs crate returns `Hnsw<'a>` with lifetime tied to `HnswIo`. Can't store loaded index and search later without lifetime issues. Workaround: store path info in `HnswInner::Loaded`, reload on each search. Works but adds ~1-2ms overhead. Watch for library updates that fix this.

---

## 2026-01-31 - HNSW filtered search is still O(n)

When filters are active (language, path pattern), we fall back to brute-force. Could optimize: run HNSW to get top-k*10 candidates, then filter in Rust. Trades recall for speed. Not implemented yet - brute-force is fine for <50k chunks.

---

## 2026-01-31 - MCP Registry is npm-focused

Official MCP Registry (registry.modelcontextprotocol.io) only supports npm packages via their `mcp-publisher` CLI. Rust crates on crates.io aren't first-class citizens. Submitted to awesome-mcp-servers and mcpservers.org instead. Watch for registry updates adding cargo support.

---

## 2026-01-31 - gh pr checks --watch gets stale after force push

When you force-push to a PR branch, `gh pr checks --watch` continues watching the old CI run. Need to Ctrl+C and restart the watch to pick up the new run. Minor annoyance but caused confusion during rapid iteration.

---
