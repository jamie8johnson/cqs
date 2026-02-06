# Project Continuity

## Right Now

**Brute-force notes + note management tools** (2026-02-06)

Branch: `feat/brute-force-notes-and-note-tools`

### Done
- Notes removed from HNSW/CAGRA index — always brute-force from SQLite (fixes #230)
- `search_notes_by_ids()` removed (dead code)
- `cqs_update_note` MCP tool — find by exact text, update text/sentiment/mentions
- `cqs_remove_note` MCP tool — find by exact text, remove from notes.toml
- `rewrite_notes_file()` helper in note.rs — atomic TOML rewrite with header preservation
- NoteEntry/NoteFile now Serialize+Deserialize for round-trip
- 379 tests pass, 0 warnings, clippy clean

### Needs
- Commit and PR
- Check CLAUDE.md bootstrap section (user asked if still current)
- Update ROADMAP.md (note management tools → done)

### Open PRs
None.

### GPU Build
```bash
bash ~/gpu-test.sh test --features gpu-search  # all env vars set
bash ~/gpu-test.sh build --features gpu-search
```
Needs: CUDA 13.1, conda base env (miniforge3), libcuvs 25.12

## Parked

- **Phase 5**: Multi-index (deferred for audit)
- **Note management tools**: `cqs_update_note`, `cqs_remove_note` (roadmap planned)
- **P4 issues**: #230-#241 (HNSW staleness, file locking, CAGRA guard, etc.)

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### P4 Deferred
- #230: HNSW stale after MCP note additions
- #231: Notes file locking
- #232: CAGRA RAII guard pattern
- #233: Cache parsed notes.toml in MCP server
- #234: search.rs / store::helpers refactor
- #235: Dual tokio runtimes in HTTP mode
- #236: HNSW-SQLite freshness validation
- #237: TOML manual escaping → serializer
- #238: CJK tokenization
- #239: Test coverage gaps (low-priority)
- #240: embedding_batches cursor pagination
- #241: Config permission checks

## Architecture

- Version: 0.5.1
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- Unified HNSW index (chunks + notes with prefix)
- Language enum + LanguageDef registry in language/mod.rs (source of truth)
- Parser re-exports Language, ChunkType from language module
- Store: split into focused modules (7 files including migrations)
- CLI: mod.rs + display.rs + watch.rs + pipeline.rs
- 390 tests with GPU / 379 without (including CLI, server, stress tests)
