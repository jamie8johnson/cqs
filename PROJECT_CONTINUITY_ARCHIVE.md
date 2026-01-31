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
