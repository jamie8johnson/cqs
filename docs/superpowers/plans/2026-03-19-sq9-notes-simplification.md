# SQ-9: Notes Simplification + Dimension Reduction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove notes from search results, drop note embeddings, remove sentiment dimension (769→768-dim). Requires schema v15 + reindex.

**Architecture:** Three-phase approach. Phase 1: remove notes from search pipeline. Phase 2: drop sentiment dimension. Phase 3: schema migration + docs. Each phase is independently testable — the codebase compiles and passes tests after each phase.

**Tech Stack:** Rust, SQLite, sqlx, serde

**Risk:** This is the largest single change since the HNSW migration. 20 production files, 28+ test files. Must be executed carefully with build verification after each task.

---

## Phase 1: Remove Notes from Search Results (7 tasks)

### Task 1: Remove note_weight/note_only from SearchFilter + Config

**Files:**
- Modify: `src/store/helpers.rs` — remove `note_weight` and `note_only` fields from `SearchFilter`, their defaults, and validation
- Modify: `src/config.rs` — remove `note_weight: Option<f32>` and `note_only: Option<bool>` from `Config` struct, clamping, merge logic
- Modify: `src/cli/config.rs` — remove `DEFAULT_NOTE_WEIGHT`, config application for both fields
- Modify: `src/cli/mod.rs` — remove `--note-weight` and `--note-only` CLI flags

Build must pass after this task. Downstream code that sets `note_weight:` or `note_only:` on SearchFilter will fail to compile — that's intentional, it reveals all sites to clean up.

Commit: `refactor(search): remove note_weight/note_only from SearchFilter and Config`

---

### Task 2: Remove NoteSearchResult and UnifiedResult::Note

**Files:**
- Modify: `src/store/helpers.rs` — delete `NoteSearchResult` struct, delete `UnifiedResult::Note` variant, remove Note arms from `score()`, `to_json()`, `to_json_relative()`
- Modify: `src/search.rs` — remove `min_code_slot_count()`, gut note paths from `search_unified_with_index()` (note_only, skip_notes, slot allocation, note score attenuation, note merge)

After this, `search_unified_with_index()` becomes code-only. Consider whether it's still needed or if callers can use `search_filtered_with_index()` directly.

Commit: `refactor(search): remove UnifiedResult::Note and note slot allocation`

---

### Task 3: Fix all downstream UnifiedResult::Note match arms

**Files:** Every file that matches on `UnifiedResult::Note(_)`:
- `src/cli/commands/query.rs` — ~6 match arms (pattern filter, parent context, token packing, re-ranking)
- `src/cli/display.rs` — ~4 display paths
- `src/cli/batch/handlers.rs` — ~5 match arms (re-ranking, token packing, JSON output)
- `src/reference.rs` — dedup retain + test data
- `src/cli/commands/similar.rs` — remove `note_weight: 0.0`
- `src/cli/commands/explain.rs` — remove `note_weight: 0.0`

All of these will be compile errors from Tasks 1-2. Fix each by removing the Note arm.

Commit: `refactor: remove all UnifiedResult::Note match arms from CLI and display`

---

### Task 4: Remove search_notes() and note_embeddings()

**Files:**
- Modify: `src/store/notes.rs` — delete `search_notes()`, `score_note_row()`, `note_embeddings()`
- Modify: `src/store/mod.rs` — remove any re-exports of deleted functions

Commit: `refactor(notes): remove search_notes and note_embeddings`

---

### Task 5: Remove embedding parameter from note store functions

**Files:**
- Modify: `src/store/notes.rs` — change `upsert_notes_batch(&[(Note, Embedding)])` to `upsert_notes_batch(&[Note])`, change `replace_notes_for_file(&[(Note, Embedding)])` to `replace_notes_for_file(&[Note])`. Stop storing embeddings in the `embedding` column (write empty blob or NULL).
- Modify: `src/lib.rs` — simplify `index_notes()`: remove `embedder.embed_documents()` call, remove `with_sentiment()`, pass `&[Note]` directly

Commit: `refactor(notes): remove embedding from note storage and indexing`

---

### Task 6: Fix note-related tests

**Files:**
- Modify: `tests/search_test.rs` — remove note weight/search tests (~lines 272-389)
- Modify: `tests/store_notes_test.rs` — remove note embedding tests, update others for new signatures
- Modify: `src/store/notes.rs` tests — update for new signatures (no Embedding param)
- Modify: `src/store/mod.rs` tests — update `cached_notes_summaries` test if it uses Embedding

Commit: `test: update note tests for embedding-free architecture`

---

### Task 7: Build + full test verification for Phase 1

Run `cargo build --features gpu-index` and `cargo test --features gpu-index`. Fix any remaining issues. Verify note boost still works (NoteBoostIndex untouched). Verify `cqs read` note injection still works. Verify `cqs notes list/add/remove` still works.

Commit (if fixes needed): `fix: phase 1 cleanup`

---

## Phase 2: Drop Sentiment Dimension 769→768 (4 tasks)

### Task 8: Change EMBEDDING_DIM and remove with_sentiment/sentiment

**Files:**
- Modify: `src/lib.rs` — change `EMBEDDING_DIM` from 769 to 768
- Modify: `src/embedder.rs` — delete `with_sentiment()` method, delete `sentiment()` method, delete `MODEL_DIM` constant, simplify `Embedding::new()` dimension check
- Modify: `src/cli/watch.rs` — remove `.with_sentiment(0.0)` from chunk embedding
- Modify: `src/cli/pipeline.rs` — remove `.with_sentiment(0.0)` from chunk embedding (2 sites)
- Modify: `src/store/helpers.rs` — update `ModelInfo::default()` dimensions from 769 to 768

This will break tests with hardcoded 769-dim vectors and `with_sentiment()` calls. That's Phase 2 Task 10.

Commit: `refactor(embeddings): drop sentiment dimension — EMBEDDING_DIM 769→768`

---

### Task 9: Remove CAGRA distance conversion workaround (AC-8)

**Files:**
- Modify: `src/cagra.rs` — the `score = 1.0 - dist / 2.0` formula is now correct for truly unit-norm vectors. Update the comment to remove the AC-8 caveat. The formula stays the same but the assumption is now valid.

Commit: `docs(cagra): distance conversion now correct — vectors are unit-norm after sentiment removal`

---

### Task 10: Fix all tests with 769-dim vectors

**Files (28+ files):** Every test file creating `vec![0.0; 769]`, calling `with_sentiment()`, or using `mock_embedding()` that appends a 769th dim.

Key changes:
- `tests/common/mod.rs` — `mock_embedding()`: remove 769th dim push, return 768-dim
- All `vec![0.0; 769]` → `vec![0.0; 768]`
- All `vec![val; 769]` → `vec![val; 768]`
- Remove all `.with_sentiment(0.0)` calls in tests
- `src/store/mod.rs` — delete `mock_embedding_769()`, update to `mock_embedding()` returning 768
- Update all test helpers in `src/search.rs`, `src/store/chunks.rs`, `src/store/calls.rs`, `src/suggest.rs`, `src/review.rs`, `src/health.rs`, etc.
- Update integration tests in `tests/` directory

This is mechanical but touches many files. Use `grep -rn "769\|with_sentiment" src/ tests/` to find all sites.

Commit: `test: update all test vectors from 769-dim to 768-dim`

---

### Task 11: Build + full test verification for Phase 2

Run `cargo build --features gpu-index` and `cargo test --features gpu-index`. Every test should pass with 768-dim vectors.

Commit (if fixes needed): `fix: phase 2 cleanup`

---

## Phase 3: Schema Migration + Docs (3 tasks)

### Task 12: Schema v15 migration

**Files:**
- Modify: `src/store/helpers.rs` — bump `CURRENT_SCHEMA_VERSION` from 14 to 15
- Modify: `src/store/migrations.rs` — add `migrate_v14_to_v15()`: update dimensions metadata to "768", make notes.embedding nullable or drop it, mark index as needing rebuild
- Modify: `src/schema.sql` — update notes table (embedding column), update version comment, update 769→768 references

The migration should:
1. `UPDATE metadata SET value = '768' WHERE key = 'dimensions'`
2. Handle the notes.embedding column (SQLite ≥3.35 supports DROP COLUMN; alternatively recreate table without it)
3. Set `hnsw_dirty = '1'` to trigger HNSW rebuild on next load
4. Delete HNSW/CAGRA files if possible, or let the dirty flag handle it

Commit: `feat(store): schema v15 — 768-dim embeddings, drop notes.embedding`

---

### Task 13: Update all docs (769→768)

**Files:**
- Modify: `README.md` — update dimension references
- Modify: `PRIVACY.md` — update embedding description
- Modify: `SECURITY.md` — update dimension references
- Modify: `CONTRIBUTING.md` — update architecture notes
- Modify: `CLAUDE.md` — update dimension in any references
- Modify: source code comments — grep for "769" and update

Commit: `docs: update all dimension references 769→768`

---

### Task 14: Add migration test + final verification

**Files:**
- Modify: `src/store/migrations.rs` — add `test_migrate_v14_to_v15`
- Full build + test suite

Commit: `test: add v14→v15 migration test`

---

## Task Parallelism Map

**Sequential chains (file conflicts):**
- Tasks 1→2→3 (SearchFilter, UnifiedResult, downstream — cascading compile errors)
- Tasks 4→5→6 (notes store removal)
- Tasks 8→10→11 (dimension change + tests)

**Independent groups:**
- Phase 1 chain (1-7) and Phase 2 chain (8-11) are sequential within phase but Phase 2 depends on Phase 1
- Phase 3 (12-14) depends on Phase 2
- Task 9 (CAGRA comment) is independent, can run anytime after Task 8

**Recommended execution:** Sequential within phases. Tasks 1-3 as one agent, Tasks 4-6 as another (after 1-3 land). Tasks 8+10 as one agent. Tasks 12-14 as one agent.

---

## Risk Mitigation

1. **Build after every task.** This change cascades — a missing match arm will block everything.
2. **Don't parallelize within phases.** The compile-error cascade is the signal that finds all sites.
3. **Run full test suite after each phase.** Phase boundaries are the natural verification points.
4. **Reindex required.** Users upgrading from v1.0.13 must run `cqs index --force`. The migration handles metadata; the dirty flag triggers HNSW rebuild; but chunk embeddings are 769-dim blobs that simply won't load until reindexed.
