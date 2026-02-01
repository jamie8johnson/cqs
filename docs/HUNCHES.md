# Hunches

Soft observations, gut feelings, latent risks. Append new entries as they arise.

---

## 2026-01-31 - tree-sitter version gap

Grammar crates (0.23.x) have dev-dep on tree-sitter ^0.23, but we're using tree-sitter 0.26. Works via `tree-sitter-language` abstraction layer, but feels fragile. If parsing breaks mysteriously, check this first.

**UPDATE 2026-01-31:** Updated grammars to reduce gap:
- tree-sitter-rust: 0.23 → 0.24
- tree-sitter-python: 0.23 → 0.25
Still not fully aligned with tree-sitter 0.26 but closer. All tests pass.

---

## 2026-01-31 - ort 2.x is still RC

Using `ort = "2.0.0-rc.11"` - no stable 2.0 release yet. API could change. Pin exact version and watch for breaking changes on upgrade.

**UPDATE 2026-01-31:** Still on rc.11 as of this date. Dependabot will notify when stable releases. No API issues encountered so far.

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

**RESOLVED 2026-01-31:** HNSW index added in v0.1.8. See later entry for details.

---

## 2026-01-31 - Symlinks are a landmine

No policy defined. Could follow into `/etc/`, could loop forever, could index vendor code outside project. Default should be "skip symlinks" - safer and predictable.

**RESOLVED 2026-01-31:** `.follow_links(false)` set in cli.rs:272. Symlinks are skipped during enumeration.

---

## 2026-01-31 - Model versioning time bomb

If nomic releases v2.0 with different embeddings, old indexes become garbage. Need to check model_name on every query and warn loudly if mismatched. Re-index isn't optional in that case.

**RESOLVED 2026-01-31:** `check_model_version()` called on `Store::open()`. Returns `StoreError::ModelMismatch` with helpful message if stored model differs from current.

---

## 2026-01-31 - MCP server resource consumption

`cq_index` tool in MCP allows remote triggering of reindex. On large codebases this is CPU/GPU intensive. Could be DoS vector if SSE transport exposed to network. Keep SSE localhost-only or add rate limiting.

**RESOLVED 2026-01-31:** No `cq_index` tool exists. MCP only exposes `cqs_search` (read-only) and `cqs_stats` (metadata). HTTP transport binds to localhost only with 1MB body limit.

---

## 2026-01-31 - Embedding model checksums still empty

Model verification skeleton exists but SHA256 constants are empty TODOs. Need to actually download the model, compute checksums, and fill them in. First person to run `cq init` should do this.

**RESOLVED 2026-01-31:** Filled blake3 checksums in embedder.rs:15-16. Verified with `cqs doctor`.

---

## 2026-01-31 - Two-phase search trades latency for memory

New two-phase search (id+embedding first, content second) reduces memory but adds a second SQL query. For small indexes this might be slower. Could add threshold: single-phase for <10k chunks, two-phase above.

**ACCEPTED 2026-01-31:** Trade-off is acceptable. Second query is fast (fetches by PK). HNSW search path bypasses this for unfiltered queries. Memory savings matter more for large indexes.

---

## 2026-01-31 - WSL2 CUDA is fragile (UPDATE: working reliably after reboot)

Original concern: GPU visibility in WSL2 can drop randomly during package installation.

**RESOLVED 2026-01-31:** After WSL reboot, CUDA working reliably. RTX A6000, CUDA 13.0 driver, cuDNN 9.18.1. ort detects CUDA automatically. Getting 6ms single queries, 0.3ms/doc in batches. The fragility was during initial setup - once stable, it stays stable.

---

## 2026-01-31 - ONNX model input/output assumptions

nomic-embed-text-v1.5 ONNX model needs: i64 inputs (not i32), token_type_ids (all zeros), and outputs last_hidden_state (not sentence_embedding). These aren't obvious from docs. If switching models, verify inputs/outputs with the model directly - don't assume they're standard.

---

## 2026-01-31 - MCP server working directory is unpredictable

Claude Code starts MCP servers from an unknown cwd. The `find_project_root()` approach (walk up looking for Cargo.toml/.git) fails because the server isn't started from the project directory. Solution: always require `--project` in MCP config. This is a footgun for any MCP server that relies on cwd inference.

**RESOLVED 2026-01-31:** `--project` is now required for `cqs serve`. Documented in README.

---

## 2026-01-31 - Claude Code MCP config lives in ~/.claude.json, not .mcp.json

The `.mcp.json` file in the project root isn't used by Claude Code. Config set via `claude mcp add` goes to `~/.claude.json` under `projects["/path/to/project"].mcpServers`. Editing `.mcp.json` does nothing. Also: Claude Code caches config in memory - changes to `~/.claude.json` require restart to take effect.

---

## 2026-01-31 - ort logs to stdout by default

The `ort` crate's internal logging goes to stdout unless you configure the subscriber otherwise. For stdio-based JSON-RPC (like MCP), this pollutes the response stream. Fix: `tracing_subscriber::fmt().with_writer(std::io::stderr).init()`.

**RESOLVED 2026-01-31:** All logging goes to stderr. MCP stdio transport works cleanly.

---

## 2026-01-30 - MCP tools/call response format is easy to get wrong

MCP `tools/call` responses MUST wrap results in `{"content":[{"type":"text","text":"..."}]}`. Returning raw JSON works when tested standalone but fails silently in Claude Code - the tool runs but results appear empty. Not obvious from error messages.

**RESOLVED 2026-01-31:** Fixed in mcp.rs. All tool responses properly wrapped.

---

## 2026-01-31 - Absolute vs relative paths in index

Storing absolute paths in chunk IDs breaks path pattern filtering and makes indexes non-portable. The fix was straightforward but required touching multiple places: enumerate_files returns relative paths, parse_files rewrites chunk paths, callers join with root for filesystem ops. Test with glob patterns early - they're a good canary for path issues.

**RESOLVED 2026-01-31:** All paths stored as relative. Path pattern filtering works correctly.

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

**RESOLVED 2026-01-31:** Implemented Streamable HTTP transport. Kept "sse" as alias mapping to "http" for backwards compat with existing configs.

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

**RESOLVED 2026-01-31:** v0.1.9 added `search_by_candidate_ids()` - fetches only HNSW candidate chunks from DB, filters in Rust. 10-100x faster for filtered queries on large indexes. Path pattern filter is still O(candidates) but that's typically <1000.

---

## 2026-01-31 - MCP Registry is npm-focused

Official MCP Registry (registry.modelcontextprotocol.io) only supports npm packages via their `mcp-publisher` CLI. Rust crates on crates.io aren't first-class citizens. Submitted to awesome-mcp-servers and mcpservers.org instead. Watch for registry updates adding cargo support.

---

## 2026-01-31 - gh pr checks --watch gets stale after force push

When you force-push to a PR branch, `gh pr checks --watch` continues watching the old CI run. Need to Ctrl+C and restart the watch to pick up the new run. Minor annoyance but caused confusion during rapid iteration.

---

## 2026-01-31 - simsimd returns f64, not f32

simsimd v6 returns `f64` from `SpatialSimilarity::dot()`, not `f32`. Had to cast result back to f32 for our API. Minor but surprising API difference from earlier versions.

---

## 2026-01-31 - Config file can't detect explicit CLI defaults

When using config files, we can't distinguish "user passed -n 5" from "user didn't pass -n, using default 5". Current workaround: only apply config if CLI value equals the hardcoded default. If user explicitly passes the default value, config won't override. Minor edge case.

---

## 2026-01-31 - Cargo.lock not auto-updated on version bump

When bumping version in Cargo.toml, Cargo.lock isn't automatically regenerated unless you run a cargo command (build, check, etc.). Easy to forget to commit the updated lock file. Required a separate PR (#17) after v0.1.9 release. Consider adding a pre-commit hook to verify Cargo.lock is in sync.

**RESOLVED 2026-01-31:** Pre-commit hook (.githooks/pre-commit) runs `cargo check` when Cargo.toml is staged and warns if Cargo.lock changes.

---

## 2026-01-31 - PowerShell can't access WSL paths

When calling `gh` or other Windows commands from WSL via `powershell.exe`, they can't read `/tmp/` or other WSL-native paths. Must copy files to `/mnt/c/` first. Example: `gh release create --notes-file /tmp/notes.md` fails silently. Workaround: write to Windows-accessible path or inline the content.

---

## 2026-01-31 - FTS5 tokenization needs preprocessing for code

When implementing RRF hybrid search with SQLite FTS5, default tokenizer won't work well for code:
- `parse_config_file` is one token (underscore not a separator by default)
- `parseConfigFile` is one token (camelCase not split)

Solution: preprocess before indexing:
```rust
fn normalize_for_fts(name: &str) -> String {
    // snake_case -> "snake case"
    // camelCase -> "camel case"
    name.replace('_', " ")
        .chars()
        .fold(String::new(), |mut s, c| {
            if c.is_uppercase() && !s.is_empty() {
                s.push(' ');
            }
            s.push(c.to_lowercase().next().unwrap_or(c));
            s
        })
}
```

Store normalized text in FTS5, query with same normalization.

**RESOLVED 2026-01-31:** Implemented `normalize_for_fts()` in store.rs. FTS5 table `chunks_fts` stores normalized text. RRF hybrid search (PR #24) combines semantic + keyword results.

---

## 2026-01-31 - MCP tool schema is the source of truth for params

The MCP `tools/list` response defines all available parameters for each tool. When adding new params like `semantic_only`, update the schema in `handle_tools_list()` in mcp.rs. The README and CLAUDE.md should reflect these but the schema is authoritative. Claude Code reads the schema directly.

---

## 2026-01-31 - Greptile insight: code→NL→embed

Greptile found that "semantic search on codebases works better if you first translate the code to natural language, before generating embedding vectors." Naive chunking by file/function yields poor results. They translate code→NL→embed. This could significantly improve our search quality. Competitors: SeaGOAT (Python/ChromaDB), CodeGrok MCP, grepai (call graphs).

**IMPLEMENTED 2026-01-31:** v0.1.12 adds template-based NL generation. Schema v3.

---

## 2026-01-31 - NL descriptions are shorter than raw code

The NL template produces ~50-100 chars vs 500+ for raw code. nomic-embed-text was trained on longer texts. Might affect embedding quality. Monitor recall@5 on eval suite - if it drops, consider adding body tokens back.

---

## 2026-01-31 - Template repetition might create noise

Every chunk now starts with "A function named..." or "A method named...". This repetitive prefix might dilute the semantic signal. Could experiment with dropping the prefix for embedding but keeping it for display.

---

## 2026-01-31 - JavaScript has no type annotations

`extract_return_nl` returns `None` for JavaScript because there's no type syntax in signatures. This creates asymmetry: Rust/TS/Python get richer descriptions than JS. May need JSDoc parsing for parity.

---
