# Audit P4 Test Coverage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fill 9 test coverage gaps (TC-8 through TC-16) from the v1.0.13 audit — ~45 new tests covering LLM store functions, schema migrations, HNSW dirty flag, pagination, notes, cache invalidation, readonly store, watch event filtering, and extract_first_sentence edge cases.

**Architecture:** Grouped by file to enable parallel agents with zero overlap. 6 independent tasks.

**Tech Stack:** Rust, `#[cfg(test)]`, tempfile, sqlx, notify

---

### Task 1: LLM summary store tests (TC-8)

**Files:**
- Modify: `src/store/chunks.rs` (add tests to `#[cfg(test)]` module)
- Modify: `src/store/mod.rs` (add pending batch tests to `#[cfg(test)]` module)

**Tests to add in store/chunks.rs:**

```rust
// get_summaries_by_hashes
#[test]
fn get_summaries_by_hashes_empty_input() // returns empty HashMap
#[test]
fn get_summaries_by_hashes_roundtrip() // upsert 3, get all 3 back
#[test]
fn get_summaries_by_hashes_missing_keys() // query nonexistent hashes, empty result
#[test]
fn get_summaries_by_hashes_mixed() // 3 inserted, query 5 (2 missing), get 3

// upsert_summaries_batch
#[test]
fn upsert_summaries_batch_empty() // returns Ok(0)
#[test]
fn upsert_summaries_batch_overwrites() // upsert same hash twice, second summary wins

// get_all_summaries
#[test]
fn get_all_summaries_empty() // empty table returns empty map
#[test]
fn get_all_summaries_returns_all() // upsert 3, get_all returns 3

// prune_orphan_summaries
#[test]
fn prune_orphan_summaries_no_orphans() // summaries with matching chunks survive
#[test]
fn prune_orphan_summaries_removes_unmatched() // summaries without chunks deleted
```

**Tests to add in store/mod.rs:**

```rust
// set_pending_batch_id / get_pending_batch_id
#[test]
fn pending_batch_id_roundtrip() // set Some, get returns Some
#[test]
fn pending_batch_id_clear() // set Some, set None, get returns None
#[test]
fn pending_batch_id_default_none() // fresh store, get returns None
#[test]
fn pending_batch_id_overwrite() // set "a", set "b", get returns "b"
```

**Infrastructure:** Use existing `setup_store()` pattern. For `prune_orphan_summaries`, insert chunks via `upsert_chunks_batch` to create matching content_hashes.

Commit: `test: add LLM summary store + pending batch tests (TC-8)`

---

### Task 2: Schema migration tests (TC-9)

**Files:**
- Modify: `src/store/migrations.rs` (add tests following v10→v11 template)

**Tests to add:**

```rust
#[test]
fn test_migrate_v12_to_v13_adds_enrichment_hash_and_dirty() {
    // 1. Create v12 schema: chunks table WITHOUT enrichment_hash, metadata table
    // 2. Set schema_version = 12
    // 3. Run migrate(&pool, 12, 13)
    // 4. Verify: enrichment_hash column exists (INSERT with it succeeds)
    // 5. Verify: hnsw_dirty metadata key = '0'
    // 6. Verify: schema_version = 13
}

#[test]
fn test_migrate_v13_to_v14_creates_llm_summaries() {
    // 1. Create v13 schema: chunks with enrichment_hash, metadata with hnsw_dirty
    // 2. Set schema_version = 13
    // 3. Run migrate(&pool, 13, 14)
    // 4. Verify: llm_summaries table exists (SELECT from sqlite_master)
    // 5. Verify: can INSERT into llm_summaries
    // 6. Verify: schema_version = 14
}

#[test]
fn test_migrate_v12_to_v14_full_chain() {
    // Multi-step migration. Verify both enrichment_hash AND llm_summaries exist.
}
```

**Infrastructure:** Follow `test_migrate_v10_to_v11_creates_type_edges` pattern exactly: `tokio::runtime::Builder::new_current_thread()`, `tempfile`, raw `SqlitePoolOptions`, manual DDL for base schema, then `migrate()`.

The v12 base schema needs the chunks table with all columns EXCEPT `enrichment_hash`. Copy the CREATE TABLE from `schema.sql` and remove that column.

Commit: `test: add schema migration tests v12→v13, v13→v14 (TC-9)`

---

### Task 3: HNSW dirty flag + chunks_paged + open_readonly + cache invalidation (TC-10/11/13/16)

**Files:**
- Modify: `src/store/mod.rs` (add tests to `#[cfg(test)]` module)

**Note:** TC-8 pending_batch tests also go in mod.rs, but Task 1 owns those. This task ONLY adds the TC-10/13/16 tests. Task 1 agent must not touch TC-10/13/16 tests, and this agent must not touch TC-8 tests.

Actually, to avoid conflict: **merge TC-10/13/16 into Task 1** since they're the same file. Move chunks_paged to Task 1 too since it's store/chunks.rs.

**Revised: This task is only TC-13 (open_readonly) — the only test that needs a separate store lifecycle.**

```rust
#[test]
fn open_readonly_on_initialized_store() {
    // Create + init store, drop, reopen readonly, verify stats() works
}

#[test]
fn open_readonly_on_missing_file_fails() {
    // open_readonly on nonexistent path should error
}

#[test]
fn open_readonly_rejects_writes() {
    // open_readonly, attempt upsert_chunks_batch, expect error
}
```

Commit: `test: add Store::open_readonly tests (TC-13)`

---

### Task 4: Notes store tests (TC-15)

**Files:**
- Modify: `src/store/notes.rs` (add tests to `#[cfg(test)]` module)

**Tests to add:**

```rust
#[test]
fn replace_notes_for_file_replaces_not_appends() {
    // Insert 2 notes for file A, replace with 1 note, verify count is 1
}

#[test]
fn replace_notes_for_file_with_empty_deletes_all() {
    // Insert 2 notes, replace with empty list, verify count is 0
}

#[test]
fn notes_need_reindex_returns_some_when_stale() {
    // Create real tempfile, insert notes with old mtime, verify returns Some
}

#[test]
fn notes_need_reindex_returns_none_when_current() {
    // Insert notes with current mtime, verify returns None
}

#[test]
fn note_embeddings_roundtrip() {
    // Insert notes, verify note_embeddings returns correct IDs with "note:" prefix
}

#[test]
fn note_count_matches_inserts() {
    // Insert 3 notes, verify note_count() == 3
}

#[test]
fn note_stats_classifies_sentiment() {
    // Insert notes with -1, 0, 0.5 sentiment, verify stats.warnings/patterns/neutral counts
}

#[test]
fn search_notes_returns_sorted_by_score() {
    // Insert notes with distinct embeddings, search, verify top result is closest
}
```

**Infrastructure:** Needs `Note` struct construction and 769-dim embeddings. Use `mock_embedding(seed)` pattern. Notes need a real `source_file` path (use tempdir path). Create helper:

```rust
fn make_note(id: &str, text: &str, sentiment: f64) -> Note {
    Note { id: id.to_string(), text: text.to_string(), sentiment, mentions: vec![] }
}
```

Commit: `test: add notes store tests — replace, reindex, embeddings, search (TC-15)`

---

### Task 5: extract_first_sentence edge cases (TC-12)

**Files:**
- Modify: `src/llm.rs` (add tests to existing `#[cfg(test)]` module)

**Tests to add:**

```rust
#[test]
fn extract_first_sentence_url_with_period() {
    // "See https://example.com. Usage:" — cuts at first period
    let result = extract_first_sentence("See https://example.com. Usage:");
    assert_eq!(result, "See https://example.com.");
}

#[test]
fn extract_first_sentence_short_sentence_falls_to_line() {
    // "Short. More text here." — "Short." is 6 chars (<=10), falls to first line
    let result = extract_first_sentence("Short. More text here.");
    assert_eq!(result, "Short. More text here.");
}

#[test]
fn extract_first_sentence_exclamation() {
    let result = extract_first_sentence("This is great! More text.");
    assert_eq!(result, "This is great!");
}

#[test]
fn extract_first_sentence_question() {
    let result = extract_first_sentence("Is this working? Yes it is.");
    assert_eq!(result, "Is this working?");
}

#[test]
fn extract_first_sentence_whitespace_only() {
    assert_eq!(extract_first_sentence("   \n  \t  "), "");
}

#[test]
fn extract_first_sentence_empty() {
    assert_eq!(extract_first_sentence(""), "");
}

#[test]
fn extract_first_sentence_boundary_10_chars() {
    // Exactly 11 chars with period at end
    assert_eq!(extract_first_sentence("1234567890."), "1234567890.");
}

#[test]
fn extract_first_sentence_multiline_short_first() {
    // First line too short, no period — returns empty
    assert_eq!(extract_first_sentence("OK.\nThis is longer"), "");
}
```

Commit: `test: add extract_first_sentence edge cases (TC-12)`

---

### Task 6: Watch event filtering (TC-14) + HNSW dirty flag (TC-10) + chunks_paged (TC-11) + cache invalidation (TC-16)

**Revised grouping:** TC-14 is cli/watch.rs, TC-10/TC-16 are store/mod.rs, TC-11 is store/chunks.rs. These are 3 different files — safe to combine since no other task touches them (Task 1 only touches TC-8 pending batch + summaries tests).

**Wait — Task 1 touches store/chunks.rs (TC-8 summaries) and store/mod.rs (TC-8 pending batch). This task also touches store/mod.rs (TC-10, TC-16) and store/chunks.rs (TC-11). CONFLICT.**

**Resolution: Move TC-10, TC-11, TC-16 into Task 1.** Task 6 becomes TC-14 only.

**Files:** `src/cli/watch.rs`

**Note:** `collect_events` is private. Make it `pub(crate)` to enable testing within the crate, or add `#[cfg(test)] mod tests` directly in watch.rs.

**Tests to add:**

```rust
#[test]
fn collect_events_skips_cqs_dir() {
    // Event path inside .cqs/ — not added to pending
}

#[test]
fn collect_events_detects_notes() {
    // Event for notes.toml path — sets pending_notes = true
}

#[test]
fn collect_events_filters_by_extension() {
    // .rs accepted, .xyz rejected
}

#[test]
fn collect_events_deduplicates_by_mtime() {
    // File with unchanged mtime since last index — skipped
}
```

**Infrastructure:** Construct `notify::Event` directly. Need `notify::EventKind::Modify(ModifyKind::Data(DataChange::Content))` and `vec![path]`. Use `tempfile::TempDir` for real paths (canonicalization needs them to exist).

Commit: `test: add watch collect_events tests (TC-14)`

---

### Revised Task 1 (expanded): LLM summaries + pending batch + dirty flag + chunks_paged + cache (TC-8/10/11/16)

**Files:** `src/store/chunks.rs`, `src/store/mod.rs`

All TC-8 tests plus:

**TC-10 (dirty flag) in store/mod.rs:**
```rust
#[test]
fn hnsw_dirty_roundtrip()
#[test]
fn hnsw_dirty_default_false()
#[test]
fn hnsw_dirty_toggle()
```

**TC-11 (chunks_paged) in store/chunks.rs:**
```rust
#[test]
fn chunks_paged_empty_store()
#[test]
fn chunks_paged_single_page()
#[test]
fn chunks_paged_multi_page()
#[test]
fn chunks_paged_exact_boundary()
```

**TC-16 (cache invalidation) in store/mod.rs:**
```rust
#[test]
fn cached_notes_summaries_reflects_inserts()
#[test]
fn cached_notes_summaries_invalidated_on_replace()
```

Commit: `test: add store tests — summaries, pending batch, dirty flag, pagination, cache (TC-8/10/11/16)`

---

### Task Summary

| Task | Findings | File(s) | ~Tests |
|------|----------|---------|--------|
| 1 | TC-8/10/11/16 | store/chunks.rs, store/mod.rs | ~20 |
| 2 | TC-9 | store/migrations.rs | 3 |
| 3 | TC-13 | store/mod.rs (separate lifecycle) | 3 |
| 4 | TC-15 | store/notes.rs | 8 |
| 5 | TC-12 | llm.rs | 8 |
| 6 | TC-14 | cli/watch.rs | 4 |

**Parallelism:** Tasks 2, 4, 5, 6 are fully independent. Task 1 and Task 3 both touch store/mod.rs — run Task 1 first, Task 3 after. Or have Task 3 add tests to `tests/store_test.rs` instead.

**Total: ~46 new tests.**
