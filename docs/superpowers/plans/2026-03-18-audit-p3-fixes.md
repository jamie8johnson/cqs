# Audit P3 Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all 39 P3 findings from the v1.0.13 audit — easy fixes with low individual impact but high cumulative quality improvement.

**Architecture:** Grouped by theme to maximize parallelism. 9 groups, all independent files (no conflicts). Each group is one agent dispatch.

**Tech Stack:** Rust, serde, tracing, sqlx

---

### Task 1: API Design — Type fixes (AD-13, AD-15, AD-17, AD-18)

**Files:** `src/onboard.rs`, `src/impact/types.rs`, `src/store/helpers.rs`

- [ ] **AD-13:** `OnboardEntry.language: String` → `Language` enum. Update `gathered_to_onboard()` to pass `c.language` directly.
- [ ] **AD-15:** `TestSuggestion.suggested_file: String` → `PathBuf`. Update construction sites.
- [ ] **AD-17:** `ChunkIdentity.origin: String` → `pub file: PathBuf`. Update `all_chunk_identities_filtered()` and remove `PathBuf::from()` wrappers in `diff.rs`.
- [ ] **AD-18:** `StaleFile.origin: String` → `pub file: PathBuf`. `StaleReport.missing: Vec<String>` → `Vec<PathBuf>`.

Commit: `fix(api): String→typed fields in OnboardEntry, TestSuggestion, ChunkIdentity, StaleFile (AD-13/15/17/18)`

---

### Task 2: API Design — Missing derives (AD-14, AD-19, AD-20, AD-21, AD-22)

**Files:** `src/diff.rs`, `src/suggest.rs`, `src/gather.rs`, `src/reference.rs`, plus ~13 files for Clone

- [ ] **AD-14:** Add `#[derive(Clone, serde::Serialize)]` to `DiffEntry` and `DiffResult` in `diff.rs`.
- [ ] **AD-19:** Add `#[derive(Clone, serde::Serialize)]` to `SuggestedNote` in `suggest.rs`.
- [ ] **AD-20:** Add `Clone` to: `ReviewResult`, `ReviewedFunction`, `RiskSummary`, `GateResult`, `CiReport`, `RelatedResult`, `GatherResult`, `PlanResult`, `StaleReport`, `NoteSearchResult`, `UnifiedResult`, `IndexStats`, `HealthReport`.
- [ ] **AD-21:** Add `serde::Serialize, PartialEq, Eq` to `GatherDirection` in `gather.rs`.
- [ ] **AD-22:** Add `Debug` to `ReferenceIndex` in `reference.rs` (manual impl — skip the `dyn VectorIndex` field).

Commit: `fix(api): add missing derives across result types (AD-14/19/20/21/22)`

---

### Task 3: Dead code removal (CQ-9, CQ-10, CQ-12, CQ-14)

**Files:** `src/store/chunks.rs`, `src/nl.rs`

- [ ] **CQ-9:** Delete `get_enrichment_hash` (zero callers).
- [ ] **CQ-10:** Delete 7 unused `NlTemplate` variants (keep `Compact` + `DocFirst` for test). Remove their match arms from `generate_nl_with_template`.
- [ ] **CQ-12:** Delete `get_by_content_hash`. Update 2 test call sites to use `get_embeddings_by_hashes(&[hash])`.
- [ ] **CQ-14:** Make `update_embeddings_batch` delegate to `update_embeddings_with_hashes_batch` with `None` hashes, or delete it and update the one test caller.

Commit: `fix(quality): remove dead code — get_enrichment_hash, NlTemplate variants, get_by_content_hash (CQ-9/10/12/14)`

---

### Task 4: Observability — tracing fixes (OB-9, OB-10, OB-11, OB-12, OB-13, OB-14)

**Files:** `src/embedder.rs`, `src/hnsw/search.rs`, `src/lib.rs`, `src/review.rs`, plus 10 files for OB-9, `src/cli/watch.rs`, `src/cli/commands/gc.rs`, `src/cli/commands/index.rs`

- [ ] **OB-9:** Convert 14 `tracing::warn!` positional format calls to structured fields. Sites: `project.rs:241,247`, `lib.rs:426`, `store/chunks.rs:632`, `parser/mod.rs:183,260,347,425`, `hnsw/persist.rs:70,517`, `hnsw/search.rs:68`, `cli/pipeline.rs:348`, `cli/commands/index.rs:261`, `cli/mod.rs:68`.
- [ ] **OB-10:** Add `info_span!("embed_documents", count = texts.len())` at top of `embed_documents`.
- [ ] **OB-11:** Add `debug_span!("hnsw_search", k, index_size = self.id_map.len())` at top of `HnswIndex::search`.
- [ ] **OB-12:** Replace remaining `set_hnsw_dirty().ok()` calls in gc.rs and index.rs with `if let Err(e)` + warn. (watch.rs already fixed in P1.)
- [ ] **OB-13:** Replace `tracing::info!` at `lib.rs:297` with `info_span!("index_notes", ...)`.
- [ ] **OB-14:** In `review.rs:136,149`, split `tracing::warn!("{}", msg)` into structured warn + separate warnings.push.

Commit: `fix(observability): structured tracing fields, missing spans, warn-on-error (OB-9/10/11/12/13/14)`

---

### Task 5: Documentation + Error handling + Platform (DOC-12, DOC-14, EH-11, EH-16, PB-10, PB-12)

**Files:** `SECURITY.md`, `README.md`, `src/cli/pipeline.rs`, `src/convert/mod.rs`, `src/embedder.rs`, `src/cli/commands/reference.rs`

- [ ] **DOC-12:** Add `.cqs.toml` and `projects.toml` to SECURITY.md Write Access table.
- [ ] **DOC-14:** Add `cqs index --llm-summaries` to README indexing examples.
- [ ] **EH-11:** Replace `get_summaries_by_hashes().unwrap_or_default()` in pipeline.rs:996 with match + warn.
- [ ] **EH-16:** Replace `if let Ok(entries) = std::fs::read_dir(dir)` in convert/mod.rs:333 with match + warn.
- [ ] **PB-10:** In embedder.rs symlink_providers, canonicalize paths before comparison, or verify src_dir is always absolute.
- [ ] **PB-12:** In reference.rs:183, replace `to_string_lossy()` with `crate::normalize_path()`.

Commit: `fix: docs, error handling, and platform fixes (DOC-12/14, EH-11/16, PB-10/12)`

---

### Task 6: Performance — LLM + search optimizations (PERF-11, PERF-13, PERF-15, PERF-16, PERF-17, PERF-18)

**Files:** `src/store/chunks.rs`, `src/llm.rs`, `src/search.rs`, `src/cli/pipeline.rs`

- [ ] **PERF-11:** Convert `upsert_summaries_batch` to multi-row INSERT via `QueryBuilder::push_values`.
- [ ] **PERF-13:** Truncate content before cloning in llm_summary_pass. Clone content_hash once.
- [ ] **PERF-15:** Change `apply_parent_boost` HashMap to use `&str` borrows instead of String clones.
- [ ] **PERF-16:** Allocate `MODEL.to_string()` once outside the loop, or use `&'static str`.
- [ ] **PERF-17:** Use `eq_ignore_ascii_case` instead of `.to_lowercase()` per candidate in search_by_candidate_ids.
- [ ] **PERF-18:** Pre-fetch all LLM summaries before the enrichment page loop instead of per-page queries.

Commit: `perf: batch INSERT, reduce allocations, pre-fetch summaries (PERF-11/13/15/16/17/18)`

---

### Task 7: Resource management (RM-11, RM-12, RM-14, RM-16, RM-17)

**Files:** `src/embedder.rs`, `src/cagra.rs`, `src/store/mod.rs`, `src/hnsw/persist.rs`, `src/cli/watch.rs`

- [ ] **RM-11:** In `embed_documents`, apply prefix per-batch instead of all upfront when len > MAX_BATCH.
- [ ] **RM-12:** In CAGRA search, reuse host arrays instead of double-allocating.
- [ ] **RM-14:** In `Store::open`, use `Builder::new_multi_thread().worker_threads(4)` instead of default all-cores.
- [ ] **RM-16:** In HNSW save, use `serde_json::to_writer(BufWriter)` instead of `to_string`. Compute checksum by reading file back.
- [ ] **RM-17:** Remove `files.len() == 1` condition from mtime pruning in watch.rs.

Commit: `fix(resources): reduce memory usage in embedding, HNSW save, Store open, watch (RM-11/12/14/16/17)`

---

### Task 8: Extensibility — Pattern macro + capture_name sync (EX-6, EX-7)

**Files:** `src/structural.rs`, `src/parser/types.rs`

- [ ] **EX-6:** Create `define_patterns!` macro for `Pattern` enum (generates enum, Display, FromStr, all_names). Matches the `define_chunk_types!` pattern.
- [ ] **EX-7:** Add `capture` field to `define_chunk_types!` entries and generate `capture_name_to_chunk_type()` from the macro. Variants where capture == display can omit the field.

Commit: `refactor: define_patterns! macro + capture_name generation from define_chunk_types! (EX-6/EX-7)`

---

### Task 9: Test fixtures (CQ-13)

**Files:** `src/lib.rs` (add `#[cfg(test)]` module), update callers in `src/search.rs`, `src/store/chunks.rs`, `src/store/calls.rs`, `src/store/types.rs`, `src/related.rs`

- [ ] **CQ-13:** Create `#[cfg(test)] pub mod test_fixtures` in lib.rs with shared `setup_store()`, `mock_embedding()`, `make_chunk()`. Update 4-6 test modules to use the shared versions.

Commit: `refactor(tests): extract shared test fixtures to lib::test_fixtures (CQ-13)`

---

### Task 10: Final verification + triage update

- [ ] Full build + test
- [ ] Update audit-triage.md P3 status
- [ ] Commit
