# Audit Batch 1 — Test Coverage (Adversarial)

Audit cut against v1.38.0 + post-v1.38 work (HEAD `fe7126c4`). Prior triage in
`docs/audit-triage.md` (v1.36.2). Skipped: TC P1-21/22/23, P2-4/5/6, P3 TC
Adversarial 5-pack, P4-1, SEC-V1.36-6 query-token-leak (PR #1456),
SPLADE-α bounds (PR #1479-era).

The PRs the prompt cited (#1505 "[index.policy] parser", #1507 "project search
filter", #1511) don't exist in git history. #1505 is actually `cqs index
--model parity + drift detection`; #1506 is `cqs ref reindex` flag parity. No
[index.policy] parser shipped. The findings below cover the real recent code.

---

#### TC-ADV-V1.38-1: `validate_repo_id` `..` rejection has no regression test
- **Difficulty:** easy
- **Location:** `src/cli/commands/train/export_model.rs:33` (rejection); `src/cli/commands/train/export_model.rs:227-263` (test mod)
- **Description:** SEC-V1.36-7 added an explicit `if repo.contains("..")` rejection at line 33 — the comment notes `org/../../etc/secrets` would otherwise let `optimum` walk above CWD. Tests cover TOML injection, leading dash, missing slash, but **no test exercises the `..` path-traversal case**. A future refactor that simplifies the validator (e.g. relies on the char-set whitelist alone) silently regresses the security fix because `.` and `/` are individually allowed.
- **Suggested fix:** add `validate_repo_id_rejects_parent_directory_refs` asserting `validate_repo_id("org/../../etc/passwd").is_err()`, `validate_repo_id("..").is_err()`, `validate_repo_id("foo/..").is_err()`, `validate_repo_id("a..b/c").is_err()` (substring match, even mid-segment).

#### TC-ADV-V1.38-2: `NoteBoostIndex::boost` / `OwnedNoteBoostIndex::boost` NaN/+Inf defenses untested
- **Difficulty:** easy
- **Location:** `src/search/scoring/note_boost.rs:147-153` and `src/search/scoring/note_boost.rs:247-253`
- **Description:** Both `boost()` methods comment-cite `test_upsert_notes_infinity_sentiment_roundtrips` (in `store/notes.rs:497`) but no test calls `NoteBoostIndex::new(&[note_with_inf_sentiment]).boost(...)` to assert the result is `1.0` (or any finite value). P1-36 was about the BoundedScoreHeap drop; the *consumer-side* clamp at the boost call has zero direct coverage. A future inlining or refactor of the clamp can silently revert P1-36 and tests will pass. Production `OwnedNoteBoostIndex` (`Arc<>` cache form) is the actual hot path and has even less coverage than the borrowed variant.
- **Suggested fix:** add `boost_clamps_inf_sentiment_to_finite_multiplier` and `boost_treats_nan_sentiment_as_zero` for both `NoteBoostIndex` and `OwnedNoteBoostIndex` — assert returned multiplier is finite and within `[1.0 - factor, 1.0 + factor]` when sentiment is `f32::INFINITY`, `f32::NEG_INFINITY`, `f32::NAN`.

#### TC-ADV-V1.38-3: `upsert_sparse_vectors` write-path NaN/Inf untested
- **Difficulty:** easy
- **Location:** `src/store/sparse.rs:138-303`
- **Description:** P1-21/31/37 fixed the *read* path (`token_dump_paged` / `load_all_sparse_vectors` filter non-finite). The *write* path at line 218/231 binds `weight: f32` directly via `push_bind(w)` with no `is_finite` check. SQLite REAL accepts `+Inf`/`-Inf`/`NaN` (NaN gets coerced to NULL on the bind, which then violates `NOT NULL`, but Inf goes through). A buggy SPLADE encoder that produces `Inf` would persist to disk and only be filtered by the loader — meaning HNSW rebuild logs warnings forever and the row never disappears. No test confirms that `upsert_sparse_vectors([(c, [(0, f32::INFINITY)])])` either errors or scrubs the value at write time.
- **Suggested fix:** add `test_upsert_sparse_vectors_inf_weight_behavior` and `test_upsert_sparse_vectors_nan_weight_behavior` — assert which contract is in force (either `Err(StoreError::Runtime)` or scrubbed to a valid float). Pin behaviour so a future write-side scrubber is a deliberate change.

#### TC-ADV-V1.38-4: `lookup_main_cqs_dir` "stray .cqs FILE" path lacks test
- **Difficulty:** easy
- **Location:** `src/worktree.rs:135` (P1-43 fix); `src/worktree.rs:333-410` (test mod)
- **Description:** P1-43 changed `own_cqs.exists()` → `own_cqs.is_dir()` so a stray *file* named `.cqs` (mistaken `touch .cqs`, packaged tarball with the wrong entry) doesn't get treated as an index dir. None of the four worktree tests covers this — `lookup_prefers_own_cqs_dir` only creates `.cqs/` as a *directory*. A regression to `.exists()` passes all current tests but reintroduces the "is not a directory" downstream confusion.
- **Suggested fix:** add `lookup_ignores_stray_cqs_file_falls_through` — `std::fs::write(dir.path().join(".cqs"), b"")`, then assert `lookup_main_cqs_dir(dir.path())` returns `MainIndexLookup::NotWorktree` (not `OwnIndex`).

#### TC-ADV-V1.38-5: `write_active_slot` concurrent-writer race fix has no regression test
- **Difficulty:** medium
- **Location:** `src/slot/mod.rs:647-694` (DS-V1.33-2 fix at lines 664-669)
- **Description:** DS-V1.33-2 added `crate::temp_suffix()` so two concurrent `write_active_slot` callers (legacy migration + `cqs slot promote`) each stage to their own temp file before `atomic_replace`. Before the fix, both raced on `active_slot.tmp` and one would clobber the other's temp before the rename. The slot test mod has zero `thread::spawn` calls — DS-V1.33-2 is implicitly tested by the round-trip test, which can't observe the race. A revert that removes the suffix passes all tests.
- **Suggested fix:** add `write_active_slot_two_concurrent_writers_dont_corrupt` — spawn 8 threads × 50 writes each with `slot_a` and `slot_b` interleaved, `join` all, then assert the final file content is *exactly one of* `"slot_a"` or `"slot_b"` (not partial / empty / mixed bytes). This catches both the temp-collision regression and an atomic-replace contract violation.

#### TC-ADV-V1.38-6: `check_index_model_drift` case-insensitive / whitespace edges untested
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/build.rs:1501-1524` (PR #1505)
- **Description:** Drift detection uses byte-exact `==` for `stored_name == requested_name || stored_name == requested_repo`. No test for: (a) `--model BGE-Large` vs stored `bge-large` — would erroneously bail, sending the operator into a `--force` rebuild they didn't actually need; (b) trailing newline / whitespace in stored value (could happen if a future migration writes `format!("{}\n", name)`); (c) empty `requested_name` / `requested_repo` (passes through with `Some(stored) != ""`); (d) stored with embedded NUL byte. Operator-facing footgun: silent over-trigger of drift could destroy a 30-min HNSW build for a cosmetic name diff.
- **Suggested fix:** add `check_index_model_drift_is_case_sensitive_pin` (current contract is case-sensitive — pin it so a future case-fold is deliberate); `check_index_model_drift_rejects_whitespace_drift` (stored = `"bge-large\n"` vs requested = `"bge-large"` — pin behaviour); `check_index_model_drift_empty_requested_bails`.

#### TC-ADV-V1.38-7: `embed_query` oversized-input truncation path untested
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:965-981` (RT-RES-5)
- **Description:** RT-RES-5 truncates queries longer than `max_query_bytes()` at a UTF-8 char boundary. No test exercises: (a) `text.len() > max_query_bytes` — truncate path never runs in tests; (b) multi-byte boundary handling — a query of `max_query_bytes - 1` bytes followed by a 4-byte emoji that crosses the cap. The `is_char_boundary` walk has no asymptotic bound (`while ... end > 0`) but in practice converges in ≤4 iterations — still a behaviour worth pinning. A future regression that uses byte-slicing without the boundary check would panic on UTF-8 violation in production.
- **Suggested fix:** add `test_embed_query_truncates_oversized_input_at_char_boundary` — set `CQS_MAX_QUERY_BYTES=100`, then `embed_query("a".repeat(99) + "🦀")` (4-byte emoji at boundary 99); assert it returns `Ok(_)` and doesn't panic. Add `test_embed_query_truncate_fits_under_cap` asserting the byte length actually used is `<= cap`.

#### TC-ADV-V1.38-8: `score_candidate` `threshold = NaN` / `±Inf` behaviour untested
- **Difficulty:** easy
- **Location:** `src/search/scoring/candidate.rs:371` (`if score >= ctx.threshold`)
- **Description:** `threshold: f32` flows from `search_filtered(.., threshold)` straight into `score >= ctx.threshold`. With `threshold = f32::NAN`, IEEE-754 says `score >= NaN` is always false → silent empty result set. With `threshold = f32::NEG_INFINITY`, all candidates pass; with `+Inf`, none pass. None of these are tested — and `search_filtered` doesn't validate or clamp threshold at the public boundary either. A caller that computes threshold via `(score / count) as f32` where `count = 0` produces NaN and the daemon returns `[]` with no error.
- **Suggested fix:** add `test_search_filtered_nan_threshold_returns_empty_not_error` (current contract — pin so future `is_finite` rejection is a deliberate change), `test_search_filtered_neg_inf_threshold_returns_all`, `test_search_filtered_inf_threshold_returns_empty`. Even better: add validation at `search_filtered` entry that bails on non-finite threshold; tests then assert `Err`.

#### TC-ADV-V1.38-9: `load_synonym_overlay` / `load_classifier_vocab_overlay` `MAX_BYTES` cap untested
- **Difficulty:** easy
- **Location:** `src/search/synonyms.rs:120,135` and `src/search/router.rs:586,601`
- **Description:** Both overlays cap at 4 KiB via `take(MAX_BYTES).read_to_string(...)`. If a 100 MiB hostile config landed on disk, the cap saves the indexer from OOM. No test writes a > 4 KiB file to verify the cap actually kicks in — current "malformed TOML" tests use a few-byte payload. A future refactor that drops the `.take(MAX_BYTES)` (e.g. switching to `fs::read_to_string`) silently reintroduces the unbounded read.
- **Suggested fix:** for each overlay add `loader_truncates_at_max_bytes` — write a 5 KiB file with a valid `[synonyms]` / `[classifier]` table at the start, then ~4 KiB of `# comment` padding, then a *malformed* TOML marker after byte 4096. Assert the loader returns the table from the first 4 KiB without falling into the malformed-TOML branch (or assert the alternative — that truncation mid-table produces empty due to malformed parse). Either way, pin which contract is in force.

#### TC-ADV-V1.38-10: `embedding_to_bytes` write-path doesn't validate finiteness
- **Difficulty:** easy
- **Location:** `src/store/helpers/embeddings.rs:15-27`
- **Description:** The write path checks dim but not finiteness — `bytemuck::cast_slice` happily serializes NaN/Inf bytes. `Embedding::try_new` would have caught these, but `Embedding::new` (the unchecked constructor) is used at 19 sites in `src/` (see `cqs callers Embedding::new`). Loading-side test `test_embedding_slice_passes_nan_bytes_through` deliberately pins the read-side passthrough; the write-side equivalent ("can NaN actually reach disk?") has no test. If any of those 19 unchecked constructors gets a NaN value (from a buggy reranker, drift normalization, etc.), the bytes land in SQLite undetected and corrupt every cosine score that touches the chunk.
- **Suggested fix:** add `test_embedding_to_bytes_nan_input_behavior` and `test_embedding_to_bytes_inf_input_behavior` — call `embedding_to_bytes(&Embedding::new(vec![f32::NAN; DIM]), DIM)`; assert which contract holds (current: `Ok` with NaN bytes; better: `Err(StoreError::Runtime)`). Pin behaviour, then file a follow-up to flip to the rejecting form mirroring `Embedding::try_new`.

---

## Summary

10 findings. All easy except TC-ADV-V1.38-5 (medium, requires multi-thread harness).

Themes:
- **Comment-cited fixes without regression tests** (TC-ADV-V1.38-1 SEC-V1.36-7 `..`,
  TC-ADV-V1.38-2 P1-36 boost clamp, TC-ADV-V1.38-4 P1-43 `is_dir`,
  TC-ADV-V1.38-5 DS-V1.33-2 race) — 4 of 10. The fix shipped, the test
  didn't. A revert-style regression passes all current tests.
- **NaN/Inf passthrough cluster sequel** (TC-ADV-V1.38-2, -3, -8, -10) — v1.36.2
  closed many of these on read paths; write paths and consumer-side clamps
  remain undertested. `embedding_to_bytes` is the highest-impact: a NaN that
  reaches SQLite poisons every search touching the chunk and only surfaces
  via cosine-NaN drops downstream.
- **Recent-PR adversarial gaps** (TC-ADV-V1.38-6 #1505 drift, TC-ADV-V1.38-9
  PR #1482/#1483 overlay caps) — happy-path tests exist; edge cases don't.

Prioritization: TC-ADV-V1.38-1 first (security fix without test), then
TC-ADV-V1.38-10 (write-side data-corruption surface), then -2/-3/-5
(claimed-fixed contracts).
