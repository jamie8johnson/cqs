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

---

## 2026-01-31 - Two-phase search trades latency for memory

New two-phase search (id+embedding first, content second) reduces memory but adds a second SQL query. For small indexes this might be slower. Could add threshold: single-phase for <10k chunks, two-phase above.

---

## 2026-01-31 - WSL2 CUDA is fragile

GPU visibility in WSL2 can drop randomly. nvidia-smi works, then doesn't. Installing CUDA packages can disrupt the connection. CPU fallback saves us, but GPU acceleration in WSL2 shouldn't be promised - only "works when it works."

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
