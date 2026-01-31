# cq - Archive

Session log and detailed notes.

---

## Session: 2026-01-31

### Bootstrap

Ran bootstrap per CLAUDE.md instructions:
- Created docs/ directory
- Created SESSION_CONTEXT.md, HUNCHES.md from templates
- Created ROADMAP.md from template
- Created tear files (this file and PROJECT_CONTINUITY)
- Scaffolded Cargo.toml per DESIGN.md dependencies section
- Created GitHub repo

Design doc version: 0.6.1-draft

Key architecture decisions from design doc:
- tree-sitter for parsing (not syn) - multi-language support
- ort + tokenizers for embeddings (not fastembed-rs) - GPU control
- nomic-embed-text-v1.5 model (768-dim, 8192 context)
- SQLite with WAL mode for storage
- Brute-force search initially, HNSW in Phase 4

---

## Session: 2026-01-31 (Continued - Design Refinement)

### Audit Rounds

Ran 7 comprehensive audit rounds on DESIGN.md:

1. **v0.6.1 → v0.6.2**: Fixed ort execution provider imports, model size (547MB)
2. **v0.6.2 → v0.7.0**: Security overhaul (path validation, symlinks, file size limits, file lock, UTF-8 handling, SIGINT)
3. **v0.7.0 → v0.8.0**: Fixed compilation errors (mutability, type parsing), added missing types
4. **v0.8.0 → v0.9.0**: Added comprehensive MCP Integration section (~300 lines)
5. **v0.9.0 → v0.10.0**: Added helper functions, FromStr/Display impls, Store struct
6. **v0.10.0 → v0.11.0**: Two-phase search, Parser query caching, API implementations
7. **v0.11.0 → v0.12.0**: Complete Store API (search_filtered, stats, etc.)
8. **v0.12.0 → v0.13.0**: MCP moved to Phase 1, Testing Strategy added

### Key Fixes

- `upsert_chunks_batch`: `&self` → `&mut self` (rusqlite transaction requires mut)
- `needs_reindex`: removed invalid `.flatten()` on `Option<i64>`
- `check_schema_version`: TEXT → parse as i32
- Added `file_mtime INTEGER` column to schema
- Two-phase search: Phase 1 loads id+embedding only, Phase 2 fetches content for top-N
- Parser caches compiled tree-sitter queries

### MCP Integration

Added full MCP server design:
- `cq serve` command with stdio/SSE transports
- Tools: `cq_search`, `cq_similar`, `cq_stats`, `cq_index`
- Full JSON schemas, error handling, type definitions
- Claude Code configuration examples
- Moved to Phase 1 per user request

### Testing Strategy

Added comprehensive testing section:
- Unit tests: Parser, Embedder, Store modules
- Integration tests: Full pipeline
- Eval suite: 10 golden queries per language, 80% recall@5 target

### Decisions

- MCP in Phase 1 (not Phase 3) - user wants Claude Code integration early
- Name `cq` confirmed available on crates.io
- Testing: all three tiers (unit, integration, eval)

Design doc now at v0.13.0, implementation ready.

---

## Session: 2026-01-31 (Implementation & Testing)

### Implementation Complete

Implemented all 6 Phase 1 modules (~1800 lines):
- `src/parser.rs` (~320 lines) - tree-sitter parsing, 5 languages
- `src/store.rs` (~400 lines) - SQLite, two-phase search
- `src/embedder.rs` (~280 lines) - ort + tokenizers, CUDA/CPU
- `src/cli.rs` (~500 lines) - all commands
- `src/mcp.rs` (~300 lines) - JSON-RPC server, stdio transport
- `src/schema.sql` - database schema

### Rename cq → cqs

- Original name `cq` was taken on crates.io
- Renamed throughout: Cargo.toml, imports, MCP tool names
- Published v0.1.0 to crates.io as `cqs`
- Renamed GitHub repo to `jamie8johnson/cqs`
- Made repo public

### Embedder Fixes (Integration Testing)

Original embedder didn't work with actual ONNX model:
1. **i32 → i64**: Model expects int64 inputs, not int32
2. **token_type_ids**: Model requires this input (all zeros)
3. **Mean pooling**: Model outputs `last_hidden_state`, not `sentence_embedding`

After fixes, full pipeline works:
- `cqs init` - downloads model, creates .cq/
- `cqs index` - 121 chunks from cqs codebase
- `cqs "query"` - semantic search returns relevant results (0.65-0.73 scores)

### CUDA/GPU Investigation

Attempted GPU acceleration in WSL2:
- Installed NVIDIA CUDA repo, cuDNN 9
- cuDNN version mismatch (ort needs v9, Ubuntu had v8) - fixed
- WSL2 GPU visibility dropped during testing
- CPU fallback works reliably (~20ms per embedding)
- Documented in README as optional

### MCP Configuration

Added cqs as MCP server for Claude Code:
```bash
claude mcp add cqs -e LD_LIBRARY_PATH="..." -- /path/to/cqs serve
```
Config stored in `~/.claude.json` under project scope.
Needs Claude Code restart to activate.

### Files Added/Changed

- `SECURITY.md`, `PRIVACY.md` - new
- `README.md` - GPU setup, MCP config
- `.mcp.json` - project MCP config
- `.env.example` - credentials template
- `tests/` - parser and store tests (21 total)
- `tests/fixtures/` - sample files for 5 languages

---

## Session: 2026-01-31 (MCP Debugging)

### Problem

MCP tools (`cqs_search`, `cqs_stats`) returned no output when called from Claude Code conversation, but CLI worked fine.

### Investigation

1. Verified index exists: `.cq/index.db` (121 chunks)
2. Verified CLI works: `cqs "parse files"` returns results (0.79 similarity for `parse_files`)
3. Tested MCP server directly with JSON-RPC - works when given correct project path
4. Found root cause: `.mcp.json` had `"args": ["serve"]` without `--project`

### Root Cause

The `serve` command uses `find_project_root()` which walks up from cwd looking for Cargo.toml/.git. But Claude Code starts MCP servers from an unpredictable working directory, so the server couldn't find the project root or index.

### Fix

Updated `.mcp.json`:
```json
"args": ["serve", "--project", "/mnt/c/projects/cq"]
```

### Verification

```bash
echo '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"cqs_stats","arguments":{}},"id":1}' | \
  cqs serve --project /mnt/c/projects/cq 2>/dev/null | grep -E '^\{'
# Returns full stats JSON
```

### Next

Restart Claude Code to activate the fixed MCP config.

---

## Session: 2026-01-31 (GPU Verification)

### CUDA Working

After WSL reboot, verified GPU acceleration:

```
Provider: CUDA (device 0)
Init: 850ms (model load)
Warmup: 450ms (CUDA kernel compilation)

Single query embeddings:
  parse files                    6.76ms
  database connection            6.19ms
  error handling                 6.58ms

Batch embedding:
  10 docs: 22ms (2.2ms/doc)
  50 docs: 17ms (0.3ms/doc)
```

Environment:
- RTX A6000 (48GB VRAM)
- CUDA 13.0 driver (Windows host)
- cuDNN 9.18.1 (Ubuntu package)
- ort 2.0.0-rc.11

Created `examples/bench_embed.rs` for benchmarking.

### Files Changed

- `CLAUDE.md` - added cqs_search usage instructions
- `README.md` - added benchmark table, updated WSL2 section
- `examples/bench_embed.rs` - new benchmark example

---

## Session: 2026-01-31 (Phase 2 Implementation)

### Implemented All Phase 2 Features

1. **New chunk types** (Task #1-3)
   - Extended ChunkType: Class, Struct, Enum, Trait, Interface, Constant
   - Updated tree-sitter queries for all 5 languages
   - Created separate JavaScript query (no type_identifier)
   - Modified extract_chunk() for multi-capture handling

2. **Hybrid search** (Task #4)
   - Added name_match_score() with substring/word overlap
   - Extended SearchFilter with name_boost, query_text
   - Added --name-boost CLI flag (default 0.2)
   - Updated MCP tool schema

3. **Context display** (Task #5)
   - Added -C/--context flag
   - Implemented read_context_lines() for file reading
   - Note: flag must come before query due to trailing_var_arg

4. **Doc comments in embeddings** (Task #6)
   - Added prepare_embedding_input()
   - Prepends doc + signature to content

### Index Stats After Reindex

```
Total chunks: 293 (was 234)

By type:
  struct: 33
  enum: 8
  function: 170
  constant: 15
  class: 2
  method: 65
```

### Files Changed

- `src/parser.rs` - ChunkType enum, tree-sitter queries, extract_chunk
- `src/store.rs` - SearchFilter, name_match_score, hybrid scoring
- `src/cli.rs` - --name-boost, --context, prepare_embedding_input
- `src/mcp.rs` - name_boost in tool schema
- `tests/store_test.rs` - Updated SearchFilter usage
- `tests/eval_test.rs` - Updated SearchFilter usage

---

## Session: 2026-01-31 (v0.1.2 Release)

### Published v0.1.2

Committed and published all Phase 2 features to crates.io:
- New chunk types (Class, Struct, Enum, Trait, Interface, Constant)
- Hybrid search with `--name-boost`
- Context display with `-C N`
- Doc comments in embeddings

### Reindexed with Doc Comments

Ran `cqs index --force` to rebuild embeddings with doc comments included.

Final stats:
```
Total chunks: 296
By type: function 173, method 65, struct 33, constant 15, enum 8, class 2
By lang: rust 181, python 31, go 31, typescript 28, javascript 25
```

### Commits

- `4ce3924` - Add Phase 2 features: new chunk types, hybrid search, context display
- `b3e75cf` - Bump version to 0.1.2

---

## Session: 2026-01-31 (Phase 3 Complete + GitHub Infrastructure)

### v0.1.3 → v0.1.5 Published

Full Phase 3 implementation:
- v0.1.3: Watch mode, HTTP transport, .gitignore support, CLI restructure
- v0.1.4: MCP 2025-11-25 compliance (Origin validation, Protocol-Version header)
- v0.1.5: GET /mcp SSE stream support for full spec compliance

### MCP Spec Updates

Discovered MCP spec had moved to 2025-11-25 during dependency review:
- Origin header validation now mandatory
- MCP-Protocol-Version header required
- Batching removed from spec
- SSE stream via GET /mcp for server-to-client messages

### GitHub Infrastructure

Added:
- **Dependabot**: Weekly crate update PRs
- **dependency-review.yml**: Weekly MCP spec + model checks
- **ci.yml**: Build, test, clippy, fmt on push/PR
- **Issue templates**: Bug report, feature request forms
- **GitHub release**: v0.1.5 with changelog

### Key Decisions

- Implemented Streamable HTTP instead of deprecated SSE transport
- Kept "sse" as alias for "http" for backwards compatibility
- Removed trailing_var_arg from CLI (query now positional Option<String>)
- Replaced walkdir with ignore crate for .gitignore support

### Files Changed

- `src/mcp.rs` - HTTP transport, SSE handler, MCP 2025-11-25 compliance
- `src/cli.rs` - Watch mode, gitignore support, CLI restructure
- `Cargo.toml` - Added axum, tower, notify, ignore, futures, tokio-stream
- `.github/` - Dependabot, workflows, issue templates

---

## Session: 2026-01-31 (16-Category Audit + Branch Protection)

### 16-Category Audit

Conducted comprehensive audit covering all 16 categories:
- Security, Error Handling, Performance, Memory Safety
- Concurrency, API Design, Code Quality, Testing
- Documentation, Dependencies, Configuration, Logging
- Compatibility, Maintainability, User Experience, Deployment

Results: 74 findings (0 critical, 6 high, 29 medium, 39 low)
Full details in `docs/AUDIT_2026-01-31_16CAT.md`

### CI Fixes

1. **dtolnay/rust-action → dtolnay/rust-toolchain**: CI was using wrong action name
2. **Clippy warnings**: Fixed 5 warnings with `-D warnings` flag
   - `needless_range_loop` in embedder.rs (2 locations)
   - `redundant_closure` in mcp.rs
   - `io::Error::other` in store.rs
   - `truncate(true)` in cli.rs lock file
3. **.cargo/config.toml excluded**: WSL-specific config was breaking CI (permission denied for /home/user001/.cargo-target)
   - Removed from git with `git rm --cached`
   - Added `.cargo/` to .gitignore

### Branch Protection

Created GitHub ruleset for main branch via API:
- Pull requests required (0 approvers for solo dev)
- Status checks required: test, clippy, fmt
- Force push blocked (non_fast_forward rule)

Ruleset stored in `/tmp/ruleset.json` for reference.

### Files Changed

- `src/embedder.rs` - Fixed clippy needless_range_loop
- `src/mcp.rs` - Fixed redundant_closure
- `src/store.rs` - Fixed io::Error::other
- `src/cli.rs` - Added truncate(true) to lock file
- `.gitignore` - Added `.cargo/`
- `.github/workflows/ci.yml` - Fixed action name
- `docs/AUDIT_2026-01-31_16CAT.md` - Created

---

## Session: 2026-01-31 (Audit Phase A Remediation)

### Phase A Fixes (PR #7)

Addressed 4 HIGH and 2 MEDIUM audit findings:

1. **A1: SQL Parameterized Queries (S1.1 HIGH)**
   - Replaced string interpolation with `params_from_iter`
   - Language filter now uses placeholders: `language IN (?,?)`
   - File: `src/store.rs:407-430`

2. **A2: Replace glob with globset (D10.2 MEDIUM)**
   - Replaced unmaintained `glob 0.3` (last update 2016)
   - Using `globset 0.4` from ripgrep project
   - File: `Cargo.toml`, `src/store.rs:461`

3. **A3: Replace fs2 with fs4 (D10.3 MEDIUM)**
   - Replaced unmaintained `fs2 0.4` (last update 2017)
   - Using `fs4 0.12` (maintained fork)
   - File: `Cargo.toml`, `src/cli.rs:295`

4. **A4: MCP Protocol Integration Tests (T8.1 HIGH)**
   - Created 8 new tests in `tests/mcp_test.rs`
   - Tests: initialize, tools/list, tools/call, error handling
   - Made JsonRpcRequest/JsonRpcResponse/JsonRpcError fields public

### Community Standards (D5-D6)

Improved GitHub community health from 57% to 100%:
- Added CodeQL badge to README
- Created `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1)
- Created `CONTRIBUTING.md` (development guide)
- Created `.github/PULL_REQUEST_TEMPLATE.md`

### Security Features

- Enabled CodeQL analysis (GitHub security scanning)
- Enabled Secret Protection
- Added Phase 5 (Security) to roadmap with index encryption planned

### Files Changed

- `Cargo.toml` - glob→globset, fs2→fs4
- `src/store.rs` - SQL params, globset usage
- `src/cli.rs` - fs4 usage
- `src/mcp.rs` - Public types for testing, Debug derive
- `tests/mcp_test.rs` - New file, 8 tests
- `README.md` - CodeQL badge
- `CODE_OF_CONDUCT.md` - New file
- `CONTRIBUTING.md` - New file
- `.github/PULL_REQUEST_TEMPLATE.md` - New file
- `ROADMAP.md` - Added Phase 5 (Security)

### Test Results

29 tests passing:
- 13 parser tests
- 8 store tests
- 8 MCP tests (new)

---

## Session: 2026-01-31 (Audit Phase B Implementation)

### Phase B Fixes

Implemented all 4 Phase B items from the audit remediation plan:

1. **B1: RwLock for HTTP Handler (C5.1 HIGH)**
   - Initially couldn't use RwLock because rusqlite::Connection isn't Sync
   - Solution: Added r2d2-sqlite connection pooling (4 max connections)
   - Store methods now take `&self` instead of `&mut self`
   - HttpState now uses `RwLock<McpServer>` for concurrent read access

2. **B2: Secure UUID Generation (S1.3 MEDIUM)**
   - Changed `uuid_simple()` to include random component
   - Now uses: `format!("{:x}-{:08x}", nanos, random)`
   - Random from `rand::thread_rng().gen::<u32>()`

3. **B3: Request Body Limit (S1.4 MEDIUM)**
   - Added `RequestBodyLimitLayer::new(1024 * 1024)` (1MB)
   - Using tower ServiceBuilder to stack middleware

4. **B4: Query Embedding Cache (P3.2 HIGH)**
   - Added LRU cache with 100 entry capacity to Embedder
   - Cache keyed by query text, returns cloned Embedding
   - Uses `Mutex<LruCache<String, Embedding>>`

### Dependencies Added

- `r2d2 = "0.8"` - Generic connection pool
- `r2d2_sqlite = "0.24"` - SQLite adapter for r2d2
- `rand = "0.8"` - Random number generation
- `lru = "0.12"` - LRU cache implementation
- `tower-http` features: added `limit`

### Files Changed

- `Cargo.toml` - New dependencies, tower-http features
- `src/store.rs` - Connection pooling (Pool<SqliteConnectionManager>)
- `src/mcp.rs` - RwLock, uuid_simple(), RequestBodyLimitLayer
- `src/embedder.rs` - LRU cache for query embeddings
- `src/cli.rs` - Removed `mut` from Store variables

### Test Results

29 tests passing (unchanged from Phase A).
Clippy clean with `-D warnings`.

---
