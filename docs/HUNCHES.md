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
