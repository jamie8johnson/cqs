# Audit Findings — v1.9.0+

Audit started 2026-03-29. All 14 categories, 3 batches.

## API Design

#### AD-44: `Cli.model` help text says "e5-base (default)" — default is now bge-large
- **Difficulty:** easy
- **Location:** src/cli/definitions.rs:177
- **Description:** The `--model` argument help text reads `"Embedding model: e5-base (default), bge-large, or custom"` but the default model has been bge-large since v1.9.0. This will confuse any user reading `cqs --help`.
- **Suggested fix:** Change to `"Embedding model: bge-large (default), e5-base, or custom"`.

#### AD-45: `EmbeddingConfig` serde default comment says "e5-base" — default is bge-large
- **Difficulty:** easy
- **Location:** src/embedder/models.rs:191
- **Description:** The doc comment on `EmbeddingConfig.model` says `/// Model name or preset (default: "e5-base")`. The actual serde default (`default_model_name()`) correctly returns `"bge-large"` via `ModelConfig::default_model().name`, but the human-readable comment is stale. The comment and the code already disagree.
- **Suggested fix:** Change doc comment to `(default: "bge-large")`.

#### AD-46: `store::MODEL_NAME` / `EXPECTED_DIMENSIONS` doc comments say "E5-base-v2"
- **Difficulty:** easy
- **Location:** src/store/mod.rs:95-105
- **Description:** Two public constants `MODEL_NAME` and `EXPECTED_DIMENSIONS` have doc comments saying "compile-time default for E5-base-v2". The values are derived from `DEFAULT_MODEL_REPO` and `EMBEDDING_DIM` which now point to BGE-large (1024-dim). The values are correct but the human-readable docs are stale.
- **Suggested fix:** Change both comments to say "BGE-large-en-v1.5" instead of "E5-base-v2".

#### AD-47: `EMBEDDING_DIM` doc comment says "Default embedding dimension for E5-base-v2 (768)"
- **Difficulty:** easy
- **Location:** src/lib.rs:214-217
- **Description:** The doc comment on `pub const EMBEDDING_DIM` reads "Default embedding dimension for E5-base-v2 (768)." The actual value is 1024 (from `embedder::DEFAULT_DIM`). The comment has three errors: wrong model name, wrong number, and wrong description.
- **Suggested fix:** Change to "Default embedding dimension for the configured model (BGE-large: 1024)."

#### AD-48: Three layers of default model name indirection
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:27, src/store/mod.rs:99, src/embedder/models.rs:31
- **Description:** The "default model name" is defined in three places that all ultimately derive from the same source but via different paths: (1) `embedder::DEFAULT_MODEL_REPO` = `"BAAI/bge-large-en-v1.5"` (canonical), (2) `store::helpers::DEFAULT_MODEL_NAME` = `crate::embedder::DEFAULT_MODEL_REPO`, (3) `store::MODEL_NAME` = `crate::embedder::DEFAULT_MODEL_REPO`. Both store aliases exist for "callers outside the store" but serve identical purposes. Previously flagged as AD-41 in v1.7.0 when there were three *independent* definitions — now they're at least derived, but the aliasing is still confusing.
- **Suggested fix:** Remove `store::MODEL_NAME`. Any external caller (e.g., `doctor.rs`) can use `cqs::embedder::DEFAULT_MODEL_REPO` directly. Keep `helpers::DEFAULT_MODEL_NAME` as `pub(crate)` since it's used internally by `check_model_version`.

#### AD-49: `--json` vs `--format` inconsistency across commands
- **Difficulty:** medium
- **Location:** src/cli/definitions.rs (throughout)
- **Description:** Commands use three different output format patterns: (1) `--json` only (most commands: callers, callees, blame, explain, similar, etc.), (2) `--format text|json|mermaid` + `--json` shorthand with `conflicts_with` (impact, trace), (3) `--format text|json` + `--json` shorthand with custom parser rejecting mermaid (review, ci). The `--json` shorthand on format-aware commands requires `conflicts_with = "format"` declarations and merging logic in dispatch (`let format = if json { ... } else { format }`). This is boilerplate on every format-aware command and a source of inconsistency. Adding a new command requires remembering which pattern to use.
- **Suggested fix:** Consider a shared `OutputArgs` struct (like `GatherArgs`/`ImpactArgs`) that encapsulates the format + json shorthand pattern, reducing per-command boilerplate. Alternatively, standardize on `--format` everywhere with `json` as default if `--json` is passed, removing the `conflicts_with` dance.

#### AD-50: `VectorIndex` trait missing `dim()` method
- **Difficulty:** easy
- **Location:** src/index.rs:21-42
- **Description:** The `VectorIndex` trait has `search()`, `len()`, `is_empty()`, and `name()` but no `dim()`. Both concrete implementations (`HnswIndex`, `CagraIndex`) have a `dim` field. Callers that accept `dyn VectorIndex` can't ask for the dimension without downcasting. Currently not a problem because callers always have access to the concrete type, but it violates the abstraction — the trait describes the index's capability but omits a fundamental property.
- **Suggested fix:** Add `fn dim(&self) -> usize;` to `VectorIndex`. Trivial one-line impl in both HnswIndex and CagraIndex.

#### AD-51: `Embedder::new` vs `Embedder::new_cpu` differ only in GPU flag — not composable
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:245,253
- **Description:** `new()` and `new_cpu()` are near-identical constructors that differ only in provider selection strategy (GPU-capable vs. CPU-only). This was noted as CQ-28 in v1.7.0 but remains. The pattern doesn't compose — if a third variant were needed (e.g., specific GPU device), it would require a third constructor.
- **Suggested fix:** Single `new()` constructor with a `force_cpu: bool` parameter, or an `EmbedderOptions` builder struct. Low priority since only two variants exist.

#### AD-52: `ModelInfo` lives in `store::helpers` but is a general-purpose type
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:714
- **Description:** `ModelInfo` describes an embedding model (name, dimensions, version) and is used by `Store::init`, `Embedder`, and CLI commands. It's defined in `store::helpers` alongside `SearchFilter`, `ChunkSummary`, and other store-specific types. Its natural home would be `embedder::models` alongside `ModelConfig`, since it represents model metadata rather than store internals. `ModelConfig` describes the model configuration (repo, paths, prefixes), `ModelInfo` describes the indexed model state (name, dim, version) — they're two sides of the same coin.
- **Suggested fix:** Move to `embedder::models` or `embedder::mod.rs`. The store can re-export it.

#### AD-53: `ModelInfo.dimensions` is `u32` but `ModelConfig.dim` and `Store.dim` are `usize`
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:716 vs src/embedder/models.rs:20 vs src/store/mod.rs:209
- **Description:** Embedding dimension is represented as `u32` in `ModelInfo` but `usize` in `ModelConfig` and `Store`. This forces `as u32` / `as usize` casts at every boundary (e.g., `ModelInfo::new(name, dim as u32)`, `store.dim as u32`). The `u32` choice was for SQLite storage (metadata table), but `usize` is the natural Rust type for array sizes.
- **Suggested fix:** Change `ModelInfo.dimensions` to `usize`. SQLite binding can convert at the serialization boundary. This eliminates scattered `as u32` casts throughout the codebase.

#### AD-54: `Embedding::new` accepts any dimension silently — `try_new` is the validated path but rarely used
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:100
- **Description:** `Embedding::new(data)` is infallible and accepts any `Vec<f32>` — zero-length, NaN-filled, wrong dimension. `try_new()` validates non-empty and finite. In production, the Embedder produces valid embeddings so `new()` is safe. But test code constructs `Embedding::new(vec![0.0; dim])` freely, including zero vectors that cause NaN cosine distances. The dual-API creates ambiguity about which constructor callers should use.
- **Suggested fix:** Keep both, but add a doc comment to `new()` explicitly stating "For embedder output only; test code should prefer `try_new()` or acknowledge zero-vector risks."

## Documentation

*Note: AD-44 through AD-47 in the API Design section above also cover stale E5-base-v2 doc comments in cli/definitions.rs, embedder/models.rs, store/mod.rs, and lib.rs. Not duplicated here.*

#### DOC-38: README "Embedding Model" section still describes E5-base-v2 as the default
- **Difficulty:** easy
- **Location:** README.md:56, README.md:576
- **Description:** Two remaining README locations (beyond those covered by AD-44–47) describe E5-base-v2 as the default. Line 56: `cqs ships with E5-base-v2 (768-dim) as the default.` Line 576: `Configurable embedding model (E5-base-v2 default, BGE-large preset, or custom ONNX)`. The retrieval quality table at line 606 already correctly labels BGE-large as "cqs default", but these introductory statements contradict it.
- **Suggested fix:** Line 56 → `cqs ships with BGE-large-en-v1.5 (1024-dim) as the default. E5-base-v2 is available as a lighter preset via \`CQS_EMBEDDING_MODEL=e5-base\`.` Line 576 → `(BGE-large default, E5-base-v2 preset, or custom ONNX)`.

#### DOC-39: src/embedder/mod.rs has three stale "E5-base-v2" doc comments
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:55, src/embedder/mod.rs:155, src/embedder/mod.rs:196
- **Description:**
  - Line 55 (`Embedding` doc): `Dimension depends on the configured model (e.g., 768 for E5-base-v2).` — should cite BGE-large/1024 as the primary example.
  - Line 155 (`Embedding::len` doc): `Returns 768 for cqs embeddings (E5-base-v2).` — factually wrong for the default model (1024).
  - Line 196 (`Embedder` struct doc): `Text embedding generator using a configurable model (default: E5-base-v2)` — wrong since v1.9.0.
- **Suggested fix:** Line 55 → `(e.g., 1024 for BGE-large-en-v1.5)`. Line 155 → `Returns the embedding dimension of the loaded model (e.g. 1024 for BGE-large, 768 for E5-base-v2).` Line 196 → `(default: BGE-large-en-v1.5)`.

#### DOC-40: src/store/helpers.rs ModelInfo doc comments reference "E5-base-v2, 768-dim"
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:712, src/store/helpers.rs:774
- **Description:**
  - Line 712 (ModelInfo struct doc): `` `ModelInfo::default()` for tests only (E5-base-v2, 768-dim). `` — the `Default` impl (line 742) correctly says "BGE-large with `EMBEDDING_DIM` (1024)" but the struct-level doc is stale.
  - Line 774 (ModelInfo.name field doc): `/// Embedding model used (e.g., "intfloat/e5-base-v2")` — the example should reference the current default model.
- **Suggested fix:** Line 712 → `(BGE-large-en-v1.5, 1024-dim)`. Line 774 → `(e.g., "BAAI/bge-large-en-v1.5")`.

#### DOC-41: src/test_helpers.rs mock_embedding and scoring/candidate.rs test_embedding say "768-dim"
- **Difficulty:** easy
- **Location:** src/test_helpers.rs:17, src/search/scoring/candidate.rs:507
- **Description:** Both functions use `crate::EMBEDDING_DIM` internally (now 1024) but have doc comments saying "768-dim".
- **Suggested fix:** Replace "768-dim" with "`EMBEDDING_DIM`-dim" in both comments.

#### DOC-42: SECURITY.md index storage hardcodes "768-dim vectors" and wrong default model
- **Difficulty:** easy
- **Location:** SECURITY.md:40–41, SECURITY.md:172
- **Description:**
  - Lines 40–41 (Network Requests): `Default: huggingface.co/intfloat/e5-base-v2 (~438MB)` and `Preset: bge-large (BAAI/bge-large-en-v1.5)`. Since v1.9.0 BGE-large is the default (~1.3GB) and E5-base is the preset (~438MB). The labels are swapped and the download size is wrong.
  - Line 172 (Index Storage): `Contains: code chunks, embeddings (768-dim vectors), file metadata` — dimension is model-dependent, 1024 for the current default.
- **Suggested fix:** Swap Default/Preset labels in the model download table, update download size to ~1.3GB for BGE-large. Line 172 → `embeddings (dimension depends on model — 1024 for default BGE-large)`.

#### DOC-43: src/nl/mod.rs has stale "E5-base-v2 handles ~512 tokens" comment
- **Difficulty:** easy
- **Location:** src/nl/mod.rs:189
- **Description:** `// E5-base-v2 handles ~512 tokens (~2000 chars).` This is in `generate_nl_with_template` and attributes the token limit to E5-base-v2 specifically, implying it's the operative model. Both E5-base-v2 and BGE-large have a 512-token limit, but the comment incorrectly anchors the reasoning to a non-default model.
- **Suggested fix:** `// Embedding model max sequence length is 512 tokens (~2000 chars). Budget:` — removes model-specific naming.

#### DOC-44: src/store/migrations.rs v14→v15 user-visible log message mentions "768-dim embeddings"
- **Difficulty:** easy
- **Location:** src/store/migrations.rs:188–189
- **Description:** The log message for v14→v15 migration (which ran for users upgrading old databases) says `"Run 'cqs index --force' to rebuild with 768-dim embeddings."` If a user with an extremely old database hits this migration path today, they'd be told to rebuild with 768-dim when the current default is 1024-dim.
- **Suggested fix:** Change to `"Run 'cqs index --force' to rebuild embeddings with the current model."` The function doc comment (line 174) is historical context and can stay.

#### DOC-45: CONTRIBUTING.md Architecture Overview missing `cqs-verify` skill
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:232–244
- **Description:** The `.claude/skills/` listing in CONTRIBUTING.md shows 14 skills but omits `cqs-verify/`, which exists on disk and is referenced in CLAUDE.md as the mandatory first step on every session start. The skill verifies all command categories and catches regressions.
- **Suggested fix:** Add `cqs-verify/   - Verify all command categories (run on session start and after compaction)` to the skills listing.

#### DOC-46: CQS_ONNX_DIR env var not documented in README or SECURITY.md
- **Difficulty:** easy
- **Location:** README.md (Embedding Model section), SECURITY.md (Filesystem Access section)
- **Description:** `CQS_ONNX_DIR` was added in v1.9.0 and is implemented in `src/embedder/mod.rs:692`. It allows bypassing HuggingFace download by pointing at a local ONNX directory. It is mentioned in CHANGELOG.md:18 but absent from:
  1. README.md — the Embedding Model section documents `CQS_EMBEDDING_MODEL` and custom ONNX via `export-model` but not `CQS_ONNX_DIR`.
  2. SECURITY.md — the Filesystem Access read-access table lists model-related paths but not this env var override.
- **Suggested fix:** Add to README Embedding Model section: `export CQS_ONNX_DIR=/path/to/model-dir  # skip HF download, load model.onnx + tokenizer.json from local dir`. Add to SECURITY.md read-access table: `$CQS_ONNX_DIR/ | Local ONNX model directory override | When CQS_ONNX_DIR is set`.

#### DOC-47: README config example comment says `defaults to e5-base`
- **Difficulty:** easy
- **Location:** README.md:152
- **Description:** In the configuration example `.cqs.toml` block, the comment reads `# Embedding model (optional — defaults to e5-base)`. Should say `bge-large`.
- **Suggested fix:** `# Embedding model (optional — defaults to bge-large)`

## Observability

#### OB-28: `detect_provider` and `create_session` still missing tracing spans (OB-23 carryover)
- **Difficulty:** easy
- **Location:** src/embedder/provider.rs:219, src/embedder/provider.rs:247
- **Description:** `detect_provider()` does GPU availability checks (CUDA → TensorRT → CPU fallback) and logs the selected provider via `tracing::info!`, but has no `tracing::info_span!` entry. `create_session()` (pub(crate)) logs "Creating ONNX session" via `tracing::info!` at entry but also has no span. Both functions are called from `select_provider()` / `Embedder::new()` on the hot startup path. Without spans, these don't appear in flame graphs or distributed traces when debugging slow startup or GPU detection failures. This was triaged as OB-23 in v1.7.0 and remains unfixed.
- **Suggested fix:** Add `let _span = tracing::info_span!("detect_provider").entered();` to `detect_provider` and `let _span = tracing::info_span!("create_session", provider = ?provider).entered();` to `create_session`.

#### OB-29: `parse_unified_diff` has no tracing span
- **Difficulty:** easy
- **Location:** src/diff_parse.rs:34
- **Description:** `parse_unified_diff` is the entry point for all diff-based impact analysis (`cqs impact-diff`, `cqs ci`, `cqs review`). It does file-boundary splitting and regex hunk extraction. It has no `tracing::info_span!` entry and no `tracing::warn!` for malformed input (e.g., missing `+++ b/` headers — the function silently skips those hunks). Without a span, there's no timing data when impact-diff is slow on large diffs, and no visibility into parse failures.
- **Suggested fix:** Add `let _span = tracing::debug_span!("parse_unified_diff", input_len = input.len()).entered();` at entry. Consider adding `tracing::debug!` when hunks are skipped due to missing file header (currently silent).

#### OB-30: `find_changed_functions` has no tracing span
- **Difficulty:** easy
- **Location:** src/train_data/diff.rs:128
- **Description:** `find_changed_functions` matches diff hunks against function spans (hunk overlap detection + deduplication of nested spans). It is called by the training data generator inside `generate_training_data` per-commit, potentially thousands of times. It has zero tracing instrumentation — no span, no debug log. Absent from profiles when training data generation is slow.
- **Suggested fix:** Add `let _span = tracing::debug_span!("find_changed_functions", hunks = hunks.len(), functions = functions.len()).entered();` at entry.

#### OB-31: `load_audit_state` and `save_audit_state` have no tracing spans
- **Difficulty:** easy
- **Location:** src/audit.rs:70, src/audit.rs:105
- **Description:** Both public functions do filesystem I/O (read/write `audit-mode.json`). `load_audit_state` has `tracing::debug!` for parse failures but no entry span. `save_audit_state` has zero tracing. Neither shows up in profiles or distributed traces. When audit mode silently expires or fails to save, there's no trace of which code path handled it.
- **Suggested fix:** Add `let _span = tracing::debug_span!("load_audit_state").entered();` and `let _span = tracing::debug_span!("save_audit_state").entered();`.

#### OB-32: `update_embeddings_batch` silent on zero-row updates
- **Difficulty:** easy
- **Location:** src/store/chunks/crud.rs:84
- **Description:** `update_embeddings_batch` is a thin wrapper that delegates to `update_embeddings_with_hashes_batch`. The inner function logs `tracing::debug!(chunk_id = %id, "Enrichment update found no row")` per-chunk, but only when `rows_affected == 0`. However, there is no aggregate `tracing::info!` or `tracing::warn!` when the entire batch updates zero rows (i.e., all IDs are stale/missing). A silent no-op batch is a quality issue in the enrichment pass that's invisible without the `DEBUG` log level enabled.
- **Suggested fix:** In `update_embeddings_with_hashes_batch`, after the transaction commits: if `updated == 0 && !updates.is_empty()`, emit `tracing::warn!(count = updates.len(), "update_embeddings_batch: all chunk IDs missing, zero rows updated");`.

## Error Handling

#### EH-40: `resume()` calls `get_all_content_hashes()` twice — error state lost, second call is TOCTOU
- **Difficulty:** easy
- **Location:** src/llm/batch.rs:559–568
- **Description:** `resume()` calls `get_all_content_hashes()` on line 559, mapping `Err(e)` to an empty `HashSet` (with a warn log). Then on line 568 it calls `get_all_content_hashes()` a *second time* in the condition `valid_hashes.is_empty() && store.get_all_content_hashes().is_err()` to distinguish "DB failure" from "genuinely empty DB". This is incorrect: (1) the first call's error is already discarded, so the second call is a new independent query that may succeed or fail differently (TOCTOU); (2) this always makes two DB round-trips instead of one when the DB is healthy. The intended semantics (skip storage on error, store everything on empty DB) require preserving the first call's `Result`, not re-querying.
- **Suggested fix:** Match on a single `Result<HashSet>` instead of mapping to empty on the first call:
  ```rust
  let hash_result = store.get_all_content_hashes()
      .map(|v| v.into_iter().collect::<HashSet<_>>());
  let (valid_results, stale_count) = match hash_result {
      Err(e) => { tracing::error!(...); return Ok(results); }
      Ok(hashes) if hashes.is_empty() => (results, 0usize),
      Ok(hashes) => { /* filter */ }
  };
  ```

#### EH-41: `notes_need_reindex` error silently swallowed with no log in `index_notes_from_file`
- **Difficulty:** easy
- **Location:** src/cli/commands/index.rs:368–372
- **Description:** `store.notes_need_reindex(&notes_path).unwrap_or(Some(0))` maps a DB error to `Some(0)` (treated as "reindex needed") with no warning log. A DB failure at this point is unexpected and indicates store health problems. Silently treating the error as "needs reindex" masks the root cause — the reindex will then proceed against a potentially unhealthy store and may produce corrupt results.
- **Suggested fix:**
  ```rust
  let needs_reindex = force || match store.notes_need_reindex(&notes_path) {
      Ok(result) => result.is_some(),
      Err(e) => {
          tracing::warn!(error = %e, "notes_need_reindex failed, assuming reindex needed");
          true
      }
  };
  ```

#### EH-42: `chunk_count()` DB error silently swallowed in `build_vector_index_with_config`
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:109
- **Description:** `store.chunk_count().unwrap_or(0)` swallows a DB error with no log. If the store fails, `chunk_count` returns 0, which is `< CAGRA_THRESHOLD` (5000), so code falls through to HNSW building. While the fallback is safe, a DB failure here is a signal of a deeper store health problem, and the operator has no indication that the chunk count check failed.
- **Suggested fix:**
  ```rust
  let chunk_count = match store.chunk_count() {
      Ok(n) => n,
      Err(e) => {
          tracing::warn!(error = %e, "chunk_count failed, falling back to HNSW");
          0
      }
  };
  ```

#### EH-43: `submit_fresh` swallows `set_pending` failure — batch ID lost on crash
- **Difficulty:** medium
- **Location:** src/llm/batch.rs:640–642
- **Description:** After `submit()` succeeds and returns a batch ID, `set_pending(store, Some(&id))` is called but its error is only warned. If the store write fails, the batch is submitted to the API but its ID is never persisted. The next run has no record of the in-flight batch and submits a fresh one — doubling API costs and losing results from the first batch. The batch ID is the only handle to retrieve results; losing it is unrecoverable without manual API inspection. This was flagged as EH-35 in v1.7.0 P2 and remains unfixed.
- **Suggested fix:** Propagate the `set_pending` error. Returning `Err` here causes the caller to surface the failure; the user can then manually check the API for the in-flight batch ID (visible in the warn log line before the error). At a minimum, log the batch ID at `error` level before warning about the write failure, so the ID is visible in logs even if it can't be persisted.

#### EH-44: `response.text()` errors silently replaced with empty string in API error paths
- **Difficulty:** easy
- **Location:** src/llm/batch.rs:65, :139, :166, :221
- **Description:** Four error-handling branches use `response.text().unwrap_or_default()` to extract the HTTP error body. If `response.text()` fails (encoding error or dropped connection), the body is silently replaced with an empty string, producing useless error messages like `"Batch submission failed: HTTP 500: "`. This makes it harder to diagnose API errors, especially for non-UTF8 bodies or partial responses.
- **Suggested fix:** Log the decode failure before falling back:
  ```rust
  let body = match response.text() {
      Ok(t) => t,
      Err(e) => {
          tracing::warn!(error = %e, "Failed to decode HTTP error response body");
          String::new()
      }
  };
  ```

## Code Quality

#### CQ-34: `process_file_changes` has 12 parameters — needs a context struct
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:397
- **Description:** `process_file_changes` takes 12 positional arguments: `root`, `cqs_dir`, `store`, `parser`, `embedder`, `embedder_backoff`, `pending_files`, `last_indexed_mtime`, `hnsw_index`, `incremental_count`, `quiet`, `model_config`. This is the worst case of parameter sprawl in the codebase — `clippy::too_many_arguments` is suppressed. The caller (`cmd_watch` main loop at line 271) passes all state bags individually. The closely related `collect_events` at line 335 also has 9 parameters.
- **Suggested fix:** Extract a `WatchState` struct holding the mutable state (`pending_files`, `last_indexed_mtime`, `hnsw_index`, `incremental_count`, `embedder_backoff`). Pass immutable context (`root`, `cqs_dir`, `store`, `parser`, `model_config`, `quiet`) as a `WatchConfig` or keep as separate args since they're few. Reduces `process_file_changes` to ~5 args. Same treatment for `collect_events`.

#### CQ-35: `check_model_version` and `check_model_version_with` are dead code suppressed by `#[allow(dead_code)]`
- **Difficulty:** easy
- **Location:** src/store/metadata.rs:93, src/store/metadata.rs:102
- **Description:** Both functions have `#[allow(dead_code)]` and are only called from test code. The comment at `store/mod.rs:447` says "Model mismatch is checked at index time via check_model_version_with()" but no production code calls either function. The `#[allow(dead_code)]` suppresses the compiler's ability to flag this if the situation changes.
- **Suggested fix:** Either: (1) remove both functions since model validation is no longer needed at open time (configurable models v1.7.0), or (2) if they should be called at index time, wire them in and remove the `#[allow(dead_code)]`. If removing: move the test assertions to validate via the underlying SQL query instead.

#### CQ-36: `doc_comment_pass` duplicates the chunk scanning pattern from `collect_eligible_chunks`
- **Difficulty:** medium
- **Location:** src/llm/doc_comments.rs:170-200, src/llm/mod.rs:55-124
- **Description:** `doc_comment_pass` reimplements the cursor-based chunk scanning loop (`chunks_paged` -> filter by `is_callable` + `window_idx` -> collect) instead of reusing `collect_eligible_chunks`. The difference is minor: doc_comments adds `is_test_chunk` and `is_source_file` filters, and has an `improve_all` mode. This is the CQ-23 pattern from v1.5.0 ("LLM chunk scanning loop duplicated 3-4 places") -- it's now consolidated to 2 places, but the remaining duplication persists.
- **Suggested fix:** Add optional predicate parameters to `collect_eligible_chunks` (e.g., `extra_filter: Option<&dyn Fn(&ChunkSummary) -> bool>`) so `doc_comment_pass` can pass its `is_test_chunk` + `is_source_file` checks. Alternatively, extract a `ChunkScanner` iterator that yields filtered chunks.

#### CQ-37: `nl/mod.rs` still 1056 lines after split — core NL generation logic could move to submodule
- **Difficulty:** medium
- **Location:** src/nl/mod.rs
- **Description:** The nl.rs split (CQ-33 from v1.7.0) extracted `fts.rs` (294 lines), `fields.rs` (539 lines), and `markdown.rs` (203 lines), but `mod.rs` remains 1056 lines. It contains the core NL description generation functions (`generate_nl_description`, `generate_nl_with_call_context`, `generate_nl_with_call_context_and_summary`, `generate_nl_with_template`) plus `CONTEXT_KEYWORDS` and `should_skip_line`. The generation functions form a cohesive group that could be a `generation.rs` submodule.
- **Suggested fix:** Move the `generate_nl_*` family and their helpers (`CONTEXT_KEYWORDS`, `should_skip_line`, `NlTemplate`, `CallContext`) to `nl/generation.rs`. This would drop `mod.rs` to ~200-300 lines of re-exports and leave each submodule under 600 lines.

#### CQ-38: `parser/markdown.rs` is 2030 lines — largest source file in the project
- **Difficulty:** hard
- **Location:** src/parser/markdown.rs
- **Description:** `parser/markdown.rs` is the largest source file at 2030 lines, containing heading extraction, chunk assembly, frontmatter handling, list item extraction, code block processing, and 3 functions suppressing `clippy::too_many_arguments`. It handles both pure Markdown and documentation-specific patterns (JSDoc, `@param`, etc.). The file predates the nl/ split and hasn't been touched by any modularization effort.
- **Suggested fix:** Split into `parser/markdown/mod.rs` (public API + chunk assembly), `parser/markdown/headings.rs` (heading extraction + hierarchy), `parser/markdown/frontmatter.rs` (TOML/YAML frontmatter), `parser/markdown/code_blocks.rs` (fenced code block extraction). This is a large refactor but the file has natural seams.

#### CQ-39: Nine `clippy::too_many_arguments` suppressions across the codebase
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:334,396; src/cli/pipeline.rs:273; src/parser/markdown.rs:23,731,834; src/cli/commands/gather.rs:10; src/cli/batch/handlers/misc.rs:30; src/cli/commands/query.rs:133; src/scout.rs:174
- **Description:** Nine functions suppress `clippy::too_many_arguments`. The worst offenders: `process_file_changes` (12 params), `collect_events` (9 params), `parser_stage` (8 params). Most of these could be addressed with context structs that group related parameters. For the pipeline stages specifically, the `Arc<AtomicUsize>` counters could be a single `PipelineCounters` struct.
- **Suggested fix:** Prioritize `process_file_changes` (CQ-34 above) and `parser_stage` (group `parser`, `store`, `parsed_count`, `parse_errors` into a `ParseContext`). The markdown functions are internal to an already-complex parser and lower priority.

## Algorithm Correctness

#### AC-25: BFS node cap allows unbounded overshoot within a single expansion step
- **Difficulty:** easy
- **Location:** src/impact/bfs.rs:31-45 (reverse_bfs), :78-106 (reverse_bfs_multi), :147-175 (reverse_bfs_multi_attributed), :238-253 (test_reachability)
- **Description:** All four BFS functions check `ancestors.len() >= DEFAULT_BFS_MAX_NODES` at the *top* of the outer loop, but the inner loop (lines 40-45 in `reverse_bfs`) iterates over all callers of the current node and inserts them without any cap check. If a hub function has 5000 callers and the ancestors map is at 9999 (just below the 10000 cap), all 5000 callers are added, reaching ~15000 nodes before the next outer-loop iteration checks the cap. The cap was added in v1.9.0 (RT-RES-1) specifically to prevent unbounded expansion, but the implementation allows a single expansion step to exceed it by an arbitrary amount. In production call graphs, hub functions (like `Store::new`, `tracing::info!`) can have hundreds to thousands of callers. The overshoot is bounded by the max fan-out of a single node, which in practice is ~500-2000 for hub functions in a medium codebase. Not catastrophic, but defeats the cap's purpose of bounding memory.
- **Suggested fix:** Add `if ancestors.len() >= DEFAULT_BFS_MAX_NODES { break; }` inside the inner `for caller in callers` loop (after the insert), or refactor to check before each insert. Same fix needed in all four BFS functions. This would make the cap exact rather than approximate.

#### AC-26: `test_reachability` equivalence class ignores test-node identity in BFS
- **Difficulty:** medium
- **Location:** src/impact/bfs.rs:192-261
- **Description:** The equivalence class optimization groups tests by their first-hop callee set (BTreeSet of direct callees), then BFS-es once per unique class. However, two tests with the same direct callees may themselves appear as callees in different parts of the graph. The BFS starts from `callee_set` at depth 1, never inserting the test node itself. This means the function correctly excludes the test node from counts (documented behavior), but also means test functions that are themselves reachable via the call graph (e.g., a test helper that other tests call) are not counted when they should be. Specifically: if `test_a` calls `[X]` and `test_b` calls `[X]`, they share an equivalence class. If `X` calls `test_a` (a test calling another test), the BFS from `{X}` would reach `test_a` and count it — which is correct. However, `test_b` is never seeded into the BFS and never reachable, so `test_b` is never counted as reachable from anything. This is the documented behavior (test nodes excluded at depth 0), but it means `test_reachability` cannot detect test-to-test call chains where the caller test is in a different equivalence class. The practical impact is low because test-to-test calls through production code are rare.
- **Suggested fix:** No code change needed — document the limitation more explicitly. The current comment says "the test node itself is excluded from counts" but doesn't explain why (to avoid self-counting). Add: "Tests that appear as callees in the graph ARE counted when reached via BFS from other classes' callee sets — this is intentional. Tests within the same equivalence class cannot reach each other since neither is seeded."

#### AC-27: `waterfall_pack` surplus calculation can double-count unused budget
- **Difficulty:** medium
- **Location:** src/cli/commands/task.rs:134-148
- **Description:** The waterfall budget surplus flows from each section to the next. However, the surplus formula adds `section_budget.saturating_sub(section_used)` to the *next section's base allocation* before capping at `remaining`. The issue is subtle: when `index_pack` uses the first-item-guarantee (keeping one item even if it exceeds the section budget), `scout_used` can exceed `scout_budget`. Lines 130-132 correctly charge this overshoot to `remaining`. But lines 135-136 compute the code section's surplus as `scout_budget.saturating_sub(scout_used)` which is 0 when `scout_used > scout_budget` — so the surplus is lost (correct). However, consider a scenario where scout uses exactly its budget: `remaining` is reduced by `scout_used`, and code_budget is `base + 0` capped at `remaining`. Since `remaining` already accounts for the scout usage, this is correct. The actual risk is at the impact section (line 146-148): `code_budget.saturating_sub(code_used)` adds surplus, but `code_budget` itself might have included surplus from scout. If code underspent its inflated budget, the impact section gets more than intended. This cascading surplus was flagged as AC-16 in v1.5.0 and the `.min(remaining)` cap was added to prevent total overshoot, which does work. But the total can still exceed the original `budget` by the amount of a single first-item-guarantee overshoot (from `index_pack` keeping one item even if it exceeds the section budget). The `.min(remaining)` cap prevents *cascading* overshoot but not the initial one.
- **Suggested fix:** This is a known limitation documented in the comment at line 130. The first-item guarantee is deliberate (returning zero items for a section is worse than a small overshoot). No code change needed unless the total overshoot becomes a problem. Consider adding a final `total_used = total_used.min(budget + max_single_item_tokens)` bound for predictability.

#### AC-28: `full_cosine_similarity` accumulates in f32 — precision loss for 1024-dim vectors
- **Difficulty:** easy
- **Location:** src/math.rs:49-56
- **Description:** `full_cosine_similarity` accumulates dot product, norm_a, and norm_b as `f32` sums over 1024 elements. For 1024-dim vectors with typical embedding values (~0.01-0.03 per component), the accumulated sum exceeds f32's ~7 significant digits of precision. With 1024 additions, the last few elements contribute less than the rounding error of the accumulated sum. By contrast, the `cosine_similarity` function uses `simsimd::SpatialSimilarity::dot` which uses SIMD accumulation (typically f64 or compensated summation internally), and its f64 fallback path explicitly accumulates in f64. This inconsistency means `full_cosine_similarity` is less accurate than `cosine_similarity` for the same inputs. The practical impact is small (cross-store comparison is rare and approximate anyway), but the fix is trivial.
- **Suggested fix:** Change accumulators to `f64`: `let mut dot = 0.0f64; let mut norm_a = 0.0f64; ...` and cast `*x as f64` in the loop. Return `(dot / denom) as f32`. Same pattern as `cosine_similarity`'s fallback path.

#### AC-29: `BoundedScoreHeap::new(0)` accepts capacity 0 — push never inserts
- **Difficulty:** easy
- **Location:** src/search/scoring/candidate.rs:160
- **Description:** `BoundedScoreHeap::new(0)` creates a heap with `capacity: 0`. The `push` method checks `self.heap.len() < self.capacity` (line 175), which is `0 < 0` = false, so it falls through to the peek-compare path. `heap.peek()` returns `None` on an empty heap, so the `if let Some(...)` doesn't match, and the item is silently dropped. Every push is silently discarded. This is not currently reachable from production code because `search_filtered` passes `semantic_limit = limit * 3` where `limit` comes from CLI (minimum 1), and `BoundedScoreHeap::new(semantic_limit)` always gets at least 3. But the type's API contract is broken — a capacity-0 heap silently discards all input rather than erroring or maintaining at least 1 element.
- **Suggested fix:** Add `debug_assert!(capacity > 0, "BoundedScoreHeap capacity must be > 0")` in `new()`. Alternatively, change `capacity: 0` to `capacity: 1` to match `index_pack`'s first-item guarantee behavior.

#### AC-30: `token_pack` / `index_pack` first-item guarantee inconsistency
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:141-143 (token_pack), :189 (index_pack)
- **Description:** Both functions implement a "first-item guarantee" — the highest-scored item is always included even if it exceeds the budget. But the implementation differs subtly. `token_pack` (line 141): `if used + tokens > budget && kept_any { break; }` — breaks only if we've already kept something. Then (line 144): `if !kept_any && tokens > budget { tracing::debug!(...) }` — logs when first item exceeds budget. This means: if the first item exceeds budget, it's kept; if the second item also exceeds, it's *not* kept (because `kept_any` is true). `index_pack` (line 189): `if used + cost > budget && !kept.is_empty() { break; }` — identical logic. Both are correct and consistent. However, neither function handles `budget == 0` as a special case in `token_pack`. With `budget == 0`, the first item always exceeds budget, gets kept anyway, `used` becomes positive, and the function returns `(vec![first_item], first_item_cost)`. This is the intended behavior (always return at least one result). But `index_pack` has a short-circuit: `if budget == 0 { return (Vec::new(), 0) }` — it returns ZERO items for budget 0. These differ: `token_pack` returns 1 item for budget 0, `index_pack` returns 0 items.
- **Suggested fix:** Decide on one behavior. If "always return at least one" is desired, remove the `budget == 0` short-circuit from `index_pack`. If "budget 0 means nothing," add `if budget == 0 { return (Vec::new(), 0) }` to `token_pack`. The waterfall code uses `index_pack` and the query code uses `token_pack`, so the inconsistency doesn't cause bugs today, but it's a latent mismatch.

#### AC-31: `reverse_bfs` does not handle `max_depth == 0` — returns target only (correct but undocumented)
- **Difficulty:** easy
- **Location:** src/impact/bfs.rs:17-50
- **Description:** When `max_depth == 0`, the target is inserted at depth 0, then the first iteration checks `d >= max_depth` (0 >= 0 = true) and continues. All subsequent queue entries (if any) are also at depth 0 and get skipped. The function returns only `{target: 0}`. This is arguably correct (zero depth means "no traversal"), but the doc comment doesn't mention this edge case. The multi-source variants behave identically. Callers pass `max_depth` from CLI `--depth` flags (minimum 1 in clap), so this is currently unreachable from production. But `test_reachability` receives `max_depth` from `impact_analysis()` which defaults to 5 — never 0. All paths are safe, but the API allows 0 without documenting the behavior.
- **Suggested fix:** Add to doc comment: "When `max_depth` is 0, returns only the target(s) at depth 0 with no traversal." No code change needed.

## Extensibility

#### EX-35: `extract_method_name_from_line` hardcodes visibility modifiers — not in `LanguageDef`
- **Difficulty:** medium
- **Location:** src/nl/fields.rs:187–199
- **Description:** `extract_method_name_from_line` strips a fixed list of 13 modifiers (`pub(crate)`, `pub(super)`, `pub`, `private`, `protected`, `public`, `internal`, `override`, `virtual`, `abstract`, `static`, `async`, `final`) before matching method-declaration keywords. This modifier list is hardcoded — it covers Rust, Java/C#, Python, JavaScript, Kotlin. When a new language is added (e.g., Dart with `external`, Nim with `exported`, VB.NET with `Overridable`), the author must remember to also update this hardcoded list, which is completely separate from the language module file. `LanguageDef` already has `skip_line_prefixes` for per-language skip patterns and `field_style.strip_prefixes` for field prefix stripping — there's a clear precedent for moving per-language data into the definition. The fallback match arm for the generic language case also hardcodes function keywords (`fn`, `def`, `func`, `fun`, `sub`, `proc`, `method`), which diverges from the data-driven approach used by `FieldStyle`.
- **Suggested fix:** Add a `method_modifiers: &'static str` field to `LanguageDef` (space-separated, same pattern as `FieldStyle::strip_prefixes`). Replace the 13 chained `trim_start_matches` calls with a loop over the language's modifiers. Universal modifiers (applicable across all languages) can be a module-level constant and applied first, or folded into each language's definition.

#### EX-36: `doc_format` is a stringly-typed tag — unknown values silently fall back to default
- **Difficulty:** easy
- **Location:** src/doc_writer/formats.rs:51–127, src/language/mod.rs:328
- **Description:** `LanguageDef.doc_format` is `&'static str` used as a lookup key in `doc_format_from_tag()`. The valid tags (`"triple_slash"`, `"python_docstring"`, etc.) are documented only in the `LanguageDef.doc_format` field comment. If a language module author misspells a tag (e.g., `"triple-slash"` or `"tripleslash"`), `doc_format_from_tag` silently falls back to the `//` default — no compile-time error, no runtime warning. There are currently 10 valid tags plus `"default"`. Adding a new doc format requires editing both `formats.rs` (new match arm) and every language module that should use it — there is no compile-time enforcement that a referenced tag exists. The pattern diverges from `FieldStyle`, `SignatureStyle`, and `ChunkType` which are proper enums with compile-time safety.
- **Suggested fix:** Convert `LanguageDef.doc_format` from `&'static str` to a `DocFormat` struct (or a new `DocFormatTag` enum). Each language module would construct `DocFormat { prefix: ..., line_prefix: ..., suffix: ..., position: ... }` directly, eliminating `doc_format_from_tag()` entirely. This is consistent with how `FieldStyle` is defined. The `DocFormat` struct already exists — the language definition just needs to embed it directly instead of using an indirection through a string tag.

#### EX-37: Model download size in `cmd_init` is a hardcoded heuristic, not on `ModelConfig`
- **Difficulty:** easy
- **Location:** src/cli/commands/init.rs:45–49
- **Description:** `cmd_init` prints "Downloading model ({size})..." using the heuristic `if cli.model_config().dim >= 1024 { "~1.3GB" } else { "~547MB" }`. This couples download size knowledge to the dimension value and hardcodes two strings that are not on `ModelConfig`. Adding a third preset (e.g., a 1024-dim smaller model, or a 768-dim larger model) would produce the wrong size estimate. When a new preset is added to `ModelConfig`, the developer must also remember to update this unrelated heuristic in `init.rs`.
- **Suggested fix:** Add `pub download_size_hint: &'static str` to `ModelConfig`. Set it in `e5_base()` → `"~438MB"`, in `bge_large()` → `"~1.3GB"`, and in `ModelConfig::resolve()` for custom models → `"unknown size"`. `cmd_init` then uses `cli.model_config().download_size_hint` directly.

#### EX-38: `CAGRA_THRESHOLD` (5000) is not configurable — users can't tune CAGRA vs HNSW cutover
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:108
- **Description:** `build_vector_index_with_config` hardcodes `const CAGRA_THRESHOLD: u64 = 5000` as a local constant. This determines when CAGRA (GPU, ~1s rebuild cost) is preferred over HNSW. The threshold is correct for the benchmark environment (RTX 4000 8GB), but users with different GPU hardware may want to tune it: a weak GPU may not make CAGRA worthwhile until 20K+ vectors; a strong GPU may benefit at 1K+ vectors. There is no env var override and no config file option. The `build_vector_index_with_config` signature already accepts `ef_search: Option<usize>` as a per-call override, showing the pattern is established — `CAGRA_THRESHOLD` is missing from the same configurability surface.
- **Suggested fix:** Read from `CQS_CAGRA_THRESHOLD` env var (with `u64` parse and fallback to 5000), or expose as a field in `cqs::config::Config` (e.g., `cagra_threshold: Option<u64>`). No schema migration needed — it's a search-time parameter, not stored.

#### EX-39: `HYDE_MAX_TOKENS` (150) not configurable via `CQS_LLM_MAX_TOKENS` — separate constant users can't override
- **Difficulty:** easy
- **Location:** src/llm/mod.rs:160, src/llm/hyde.rs:80
- **Description:** `HYDE_MAX_TOKENS = 150` is a separate constant from `MAX_TOKENS = 100` (summary max tokens). `LlmConfig::resolve()` accepts `CQS_LLM_MAX_TOKENS` and config `llm_max_tokens` to override `MAX_TOKENS`, but `HYDE_MAX_TOKENS` bypasses `LlmConfig` entirely — it's imported directly from `mod.rs` in `hyde.rs` and passed as `max_tokens: HYDE_MAX_TOKENS` to `submit_hyde_batch`. Users who need longer HyDE predictions (more queries, longer descriptions) cannot override this. The pattern set by `CQS_LLM_MAX_TOKENS` for summaries should extend to HyDE.
- **Suggested fix:** Add `hyde_max_tokens` to `LlmConfig` (resolved from `CQS_LLM_HYDE_MAX_TOKENS` env var > config `llm_hyde_max_tokens` > default 150). Thread `LlmConfig` through to `hyde_query_pass`. Alternatively, a single `CQS_LLM_MAX_TOKENS` could override both if that's the simpler UX.

## Test Coverage

#### TC-41: `watch.rs` has zero unit tests (789 lines)
- **Difficulty:** hard
- **Location:** src/cli/watch.rs
- **Description:** The entire watch module (789 lines, 7 functions) has zero `#[cfg(test)]` tests. Functions include `EmbedderBackoff` (backoff logic), `collect_events` (event filtering/dedup), `process_file_changes` (the core reindex loop), `process_note_changes`, `reindex_files`, and `reindex_notes`. The `EmbedderBackoff` struct is particularly testable — it's pure logic with no I/O dependencies, yet has zero tests. `collect_events` has complex filtering logic (extension check, mtime dedup, cqs_dir skip, notes detection, `MAX_PENDING_FILES` cap) that is all untested at the unit level. The only coverage is indirect via integration tests that call `cmd_watch`, which can't exercise edge cases like backoff timing, the pending files cap, or mtime dedup races.
- **Suggested fix:** Extract `EmbedderBackoff` tests (trivial: `new()`, `record_failure()` timing, `reset()`, saturation at 300s). Extract `collect_events` into a testable form — it takes concrete types that can be constructed in tests. Add unit tests for: (1) backoff exponential growth and cap, (2) cqs_dir path filtering, (3) `MAX_PENDING_FILES` overflow behavior, (4) extension filtering, (5) mtime dedup.

#### TC-42: `delete_phantom_chunks` has zero direct tests
- **Difficulty:** easy
- **Location:** src/store/chunks/crud.rs:452-499
- **Description:** `delete_phantom_chunks` is a non-trivial SQL function that deletes chunks whose origin matches a file but whose ID is not in a live set. It handles three cases: (1) normal phantom deletion, (2) empty `live_ids` delegates to `delete_by_origin`, (3) FTS cleanup in the same transaction. It has zero direct unit tests — the only coverage is indirect via `reindex_files` in watch.rs integration tests (depth 2+). Edge cases untested: empty `live_ids` delegation, large `live_ids` list (approaching SQLite parameter limits), file with no existing chunks, mixed phantom/live chunks.
- **Suggested fix:** Add tests in `crud.rs::tests`: (1) insert 3 chunks for file, call with 2 live IDs, verify 1 deleted; (2) call with empty `live_ids`, verify all deleted via `delete_by_origin`; (3) call for nonexistent file, verify 0 deleted; (4) call where all IDs are live, verify 0 deleted.

#### TC-43: `ModelConfig::resolve` custom model path traversal rejection untested (SEC-20)
- **Difficulty:** easy
- **Location:** src/embedder/models.rs:136-151
- **Description:** `ModelConfig::resolve` has explicit path traversal protection for custom model `onnx_path` and `tokenizer_path` — it rejects paths containing `..` or absolute paths. This security-relevant validation has zero tests. An attacker-controlled config file with `onnx_path = "../../etc/passwd"` should be rejected, and this behavior should have regression tests.
- **Suggested fix:** Add tests: (1) `onnx_path` with `..` falls back to default, (2) `tokenizer_path` with absolute path falls back to default, (3) normal relative paths accepted. These are easy to write — construct an `EmbeddingConfig` with traversal paths and verify `resolve` returns the default model.

#### TC-44: `SearchFilter::validate` has zero direct tests
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:655-680
- **Description:** `SearchFilter::validate()` checks 3 constraints: (1) `name_boost` in [0.0, 1.0] with NaN safety, (2) `query_text` required when `name_boost > 0` or `enable_rrf`, (3) `path_pattern` length and control character validation. None of these branches have direct tests. The NaN-safe range check `!(0.0..=1.0).contains(&self.name_boost)` correctly rejects NaN (since NaN is not contained in any range), but this behavior is only implicitly relied upon. Indirect coverage exists via higher-level search tests, but these don't exercise the validation boundary conditions (NaN name_boost, empty query_text with rrf enabled, 501-char path pattern, control characters in pattern).
- **Suggested fix:** Add a `test_search_filter_validate` test group: (1) valid filter passes, (2) NaN name_boost rejected, (3) name_boost > 1.0 rejected, (4) name_boost < 0.0 rejected, (5) empty query_text with enable_rrf rejected, (6) path_pattern over 500 chars rejected, (7) control characters in path_pattern rejected.

#### TC-45: `ensure_model` / `CQS_ONNX_DIR` path resolution has zero tests
- **Difficulty:** medium
- **Location:** src/embedder/mod.rs:688-708
- **Description:** The `ensure_model` function has a `CQS_ONNX_DIR` code path that: (1) checks for model files at `dir/config.onnx_path` + `dir/config.tokenizer_path`, (2) falls back to flat layout at `dir/model.onnx` + `dir/tokenizer.json`, (3) warns and falls through to HF download if neither exists. None of these branches are tested. The function is called lazily on first embed, so integration tests exercise the HF download path but never the `CQS_ONNX_DIR` override. This is the primary mechanism for air-gapped/offline deployments.
- **Suggested fix:** Add unit tests using `tempdir`: (1) set `CQS_ONNX_DIR` to a temp dir with both files at config paths, verify returned paths; (2) flat layout test with `model.onnx` + `tokenizer.json`; (3) empty dir falls through (returns Err or triggers HF). Requires env mutex like models.rs tests.

#### TC-46: `batch.rs` error path coverage still minimal (TC-27/TC-32 carryover)
- **Difficulty:** medium
- **Location:** src/llm/batch.rs
- **Description:** `batch.rs` (820 lines) has 4 tests, all for happy-path or validation. Error paths untested: (1) `submit_fresh` when provider returns HTTP error — no test verifies the error propagation or the 4 `response.text().unwrap_or_default()` fallbacks (EH-44); (2) `resume()` when `get_all_content_hashes()` fails — the double-call TOCTOU (EH-40) is untested; (3) `submit_or_resume` when `set_pending` fails after successful submit (EH-43) — no test verifies the batch ID is still logged. The `MockBatchProvider` exists but is only used for the happy path. TC-27 (v1.5.0) and TC-32 (v1.7.0) both flagged this gap. Mock infrastructure is now in place but error scenarios weren't added.
- **Suggested fix:** Extend `MockBatchProvider` with configurable failure modes (e.g., `fail_on_submit: bool`, `fail_on_results: bool`). Add tests: (1) submit fails -> error propagated; (2) results fetch fails -> error propagated; (3) mock `set_pending` failure after successful submit -> verify batch ID in error message.

#### TC-47: `HNSW build_batched_with_dim(dim=0)` behavior untested (TC-40 carryover)
- **Difficulty:** easy
- **Location:** src/hnsw/mod.rs
- **Description:** `build_batched_with_dim` with `dim=0` is untested. With dim=0, the HNSW index would attempt to build with zero-dimensional vectors. The behavior is undefined — hnsw_rs may panic, return an error, or silently create a degenerate index. This was flagged as TC-40 in v1.7.0 and remains unfixed. While `ModelConfig::resolve` now rejects `dim=0` at the config layer (since v1.9.0), the HNSW API itself is still unguarded.
- **Suggested fix:** Add a test that calls `build_batched_with_dim` with `dim=0` and empty data, and verify the behavior (either graceful error or empty index). If it panics, add a guard in the function. This is defense-in-depth — config validation shouldn't be the only protection.

#### TC-48: `clamp_config_f32` NaN passthrough documented but downstream impact untested
- **Difficulty:** easy
- **Location:** src/config.rs:130-141
- **Description:** TC-36 (v1.7.0) documented that NaN passes through `clamp_config_f32` unchanged because `NaN < min` and `NaN > max` both return false. The test `tc36_nan_threshold_passes_clamp_unchanged` documents this behavior but doesn't test the downstream impact. A NaN `threshold` that survives validation will cause all similarity comparisons against it to return false (i.e., no results pass the threshold filter). No test verifies that a NaN threshold produces the expected search behavior (likely zero results). The `SearchFilter::validate` NaN check for `name_boost` is correct, but `threshold` has no similar guard.
- **Suggested fix:** Either: (1) add NaN check to `clamp_config_f32` (`if value.is_nan() { *value = min; }`) with a test, or (2) add a test that constructs a `SearchFilter` with NaN threshold and verifies it produces zero results (documenting the behavior). Option 1 is safer.

#### TC-49: `validate_finite_f32` CLI helper has zero direct tests
- **Difficulty:** easy
- **Location:** src/cli/definitions.rs:71-77
- **Description:** `validate_finite_f32` is used as a clap value parser for CLI float arguments (`--threshold`, `--name-boost`). It rejects NaN and infinity at the CLI boundary. Despite being a security-relevant validation function (prevents NaN injection from CLI), it has zero direct tests. Its behavior is only tested indirectly through integration tests that pass valid values.
- **Suggested fix:** Add unit tests in `definitions.rs` or a nearby test module: (1) finite value passes, (2) NaN rejected, (3) positive infinity rejected, (4) negative infinity rejected, (5) zero passes, (6) negative value passes.

## Robustness

#### RB-29: `generate_nl_description` can produce empty string — zero-vector embedding silently stored
- **Difficulty:** easy
- **Location:** src/nl/mod.rs:181, src/embedder/mod.rs:675-678
- **Description:** `generate_nl_description` always calls `parts.push(name_words)` (line 259) where `name_words = tokenize_identifier(&chunk.name).join(" ")`. If `chunk.name` is empty (possible for parser edge cases — anonymous functions, lambda captures, unnamed declarations), `name_words` is `""`. With no file context, no doc, and no signature, `parts.join(". ")` returns `""`. This empty string reaches `embed_documents` → `embed_batch`, which calls `tokenizer.encode_batch` on it. The tokenizer produces an all-zero attention mask for an empty string, causing the mean-pooling to fall into the `count == 0.0` branch (line 675-678): `vec![0.0f32; embedding_dim]`. This zero-vector is then stored via `Embedding::new(normalize_l2(...))`. `normalize_l2` of a zero vector returns a zero vector (division by zero avoided by `if norm > 0.0` guard). The zero embedding produces undefined cosine similarity — `cosine_similarity` returns `Some(0.0)` for zero-norm vectors (AC-23 pattern), meaning these chunks appear with similarity 0 and pollute search results. The root cause is that `embed_documents` has no guard for empty strings (unlike `embed_query` which rejects them at line 447).
- **Suggested fix:** Two-layer fix: (1) In `generate_nl_description`, fall back to `chunk.file` basename if `parts.is_empty()` after all template logic; (2) In `embed_documents` or `Embedder::embed_batch`, filter out empty texts with a `tracing::warn!`, returning a zero-vector only with an explicit warning so the caller knows. This mirrors the existing `embed_query` guard.

#### RB-30: `checkpoint_sha.as_ref().unwrap()` — non-obvious safety invariant in hot path
- **Difficulty:** easy
- **Location:** src/train_data/mod.rs:165
- **Description:** `checkpoint_sha.as_ref().unwrap()` is called inside `if !past_checkpoint { ... }`. The safety argument is: `past_checkpoint = checkpoint_sha.is_none()` (line 160), so `!past_checkpoint` implies `checkpoint_sha.is_some()`. This is correct but non-obvious — the invariant spans 5 lines and is not documented. A future refactor that changes the initialization logic (e.g., early-exit logic, additional `past_checkpoint = true` paths) could silently introduce a panic. The pattern is a readability/maintenance hazard in a loop that processes potentially thousands of commits.
- **Suggested fix:** Replace with `if let Some(ref sha) = checkpoint_sha` to make the Option handling explicit and eliminate the unwrap:
  ```rust
  if let Some(ref sha) = checkpoint_sha {
      if !past_checkpoint {
          if &commit.sha == sha { past_checkpoint = true; }
          stats.commits_skipped += 1;
          continue;
      }
  }
  ```

#### RB-31: `Language::grammar()` panics — callers in `Parser::new()` path have no fallback
- **Difficulty:** medium
- **Location:** src/language/mod.rs:858-863, src/parser/mod.rs:114, src/parser/calls.rs:37, src/parser/injection.rs:282
- **Description:** `Language::grammar()` panics with `"{} has no tree-sitter grammar — use custom parser"` when called on a grammar-less language (currently only Markdown). Nine call sites in `parser/mod.rs`, `parser/calls.rs`, and `parser/injection.rs` call `language.grammar()` inside `get_or_try_init` closures. The `Parser::new()` constructor at `mod.rs:78-84` also panics with a different message if a registered language has no `Language` enum variant. Both panics are design-time bugs (wrong language passed to grammar-requiring code) but would crash the indexing pipeline in production if a new grammar-less language is added without updating all call sites. The safe alternative `try_grammar()` exists at line 868 but is not used in production paths.
- **Suggested fix:** `get_query`/`get_call_query`/`get_type_query` should call `language.try_grammar()` and return `Err(ParserError::QueryCompileFailed(...))` if `None` instead of propagating the panic from `grammar()`. The `Parser::new()` constructor's `unwrap_or_else(|_| panic!(...))` (mod.rs:79-84) should return `Result<Self, ParserError>` and propagate the parse error.

#### RB-32: `CagraIndex::search` silently returns empty results on index mutex poison
- **Difficulty:** easy
- **Location:** src/cagra.rs:184-187, src/cagra.rs:217-223, src/cagra.rs:231-235
- **Description:** `CagraIndex::search` uses `unwrap_or_else(|poisoned| { ...; poisoned.into_inner() })` to recover from poisoned mutexes on both `resources` and `index` locks. Recovery is correct for transient panics, but there are three error paths (search params creation failure, query shape error, search execution failure) where the code manually restores the index and returns `Vec::new()`. These silent empty-result returns are indistinguishable to the caller from a legitimate "no results" outcome. Combined with the poisoned-lock recovery, a GPU panic will silently degrade search quality across all future queries without any indicator that something went wrong (the `tracing::error!` logs exist but will not surface in the search results or CLI output).
- **Suggested fix:** The `VectorIndex::search` trait returns `Vec<IndexResult>` (not `Result`), so surfacing errors directly is not possible without a trait change. At minimum, ensure a `tracing::error!` fires in each silent-empty path (some already exist — verify completeness). Consider returning an error via a thread-local or adding `fn search_result(&self, ...) -> Result<Vec<IndexResult>, IndexError>` to the trait.

#### RB-33: `doc_writer/rewriter.rs` — parallel `rewrite_file` calls could race on same file
- **Difficulty:** medium
- **Location:** src/doc_writer/rewriter.rs:1, src/llm/doc_comments.rs:170
- **Description:** `rewrite_file` reads the file, applies edits, and writes atomically via `tempfile::NamedTempFile` → `persist`. If two concurrent `--improve-docs` passes are run on the same project (e.g., two `cqs` processes), or if `cqs watch` triggers a reindex while `--improve-all` is in progress, both processes could read the same version of the file, independently produce edits, and the second `persist()` would silently overwrite the first's changes. The atomic write prevents partial writes but not concurrent overwrites. There is no file lock around the read→edit→write cycle.
- **Suggested fix:** Wrap the read→edit→write cycle in a file-level advisory lock (e.g., `fs4` or `std::fs::File` locking since Rust 1.89 MSRV). Alternatively, document that `--improve-docs` is not safe for concurrent invocation and add a guard in the CLI that checks for a running `cqs` process before starting.

#### RB-34: `hnsw/build.rs` `build_with_dim` — `chunks_exact` with `dim=0` silently produces wrong output
- **Difficulty:** easy
- **Location:** src/hnsw/build.rs:77
- **Description:** `data.chunks_exact(dim)` at line 77 with `dim=0` would panic with "chunk size must be non-zero" in Rust's slice API. However, `build_with_dim` allows `dim=0` to reach this line: `prepare_index_data` returns `Err` for zero embeddings but not for `dim=0` (it validates embedding dimensions match `expected_dim`, and if all embeddings are 0-dimensional, they'd all "match" 0). This means `build_with_dim(vec![(id, Embedding::new(vec![]))], 0)` reaches `data.chunks_exact(0)` and panics. `build_batched_with_dim` has the same gap (TC-47 from v1.7.0). Neither function validates `dim > 0` at entry.
- **Suggested fix:** Add `if dim == 0 { return Err(HnswError::Build("dim must be non-zero".into())); }` at the top of both `build_with_dim` and `build_batched_with_dim`. `prepare_index_data` could also check this, but defense-in-depth at the public API is cleaner.

## Platform Behavior

#### PB-32: `CQS_ONNX_DIR` path not canonicalized with `dunce` — UNC prefix on Windows produces wrong paths
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:692-707
- **Description:** `ensure_model` reads `CQS_ONNX_DIR` and constructs paths via `PathBuf::from(dir).join(...)` without calling `dunce::canonicalize`. On Windows (or WSL with a Windows-native path), the env var may contain a UNC path (`\\?\C:\models\bge`) which `Path::join` passes through verbatim. The resulting path is passed to ORT's session loader, which may reject `\\?\`-prefixed paths on some ORT versions. Additionally, if the user sets a relative path in `CQS_ONNX_DIR`, it is resolved against the process CWD with no warning if the resolved path doesn't exist. All other path entry points (`find_project_root`, `enumerate_files`, `cmd_read`) call `dunce::canonicalize` at intake; `ensure_model` is the only exception.
- **Suggested fix:** After `let dir = PathBuf::from(dir);`, add `let dir = dunce::canonicalize(&dir).unwrap_or(dir);`. Strips `\\?\` prefix on Windows, resolves relative paths, and makes `ensure_model` consistent with the rest of the codebase.

#### PB-33: `export_model` passes output path to Python via `display()` — inconsistent with codebase path-to-string conventions
- **Difficulty:** easy
- **Location:** src/cli/commands/export_model.rs:70
- **Description:** The output path is passed to the Python subprocess as `&output.display().to_string()`. While `dunce::canonicalize` at line 31 already strips `\\?\` UNC prefixes, `Path::display()` produces an OS-native string (`\`-separated on Windows, `/`-separated elsewhere). The rest of the codebase uses `path.to_string_lossy()` when converting paths to strings for subprocess arguments (e.g., `src/cli/commands/blame.rs:100`). The inconsistency is low-risk since the canonicalized path does not start with `\\?\` and Python handles Windows backslash paths. But it's a pattern violation: `display()` is for human-readable output (logging, UI), not for constructing subprocess arguments.
- **Suggested fix:** Change line 70 from `&output.display().to_string()` to `output.to_string_lossy().as_ref()`. Consistent with how blame.rs and other commands pass path arguments to subprocesses.

#### PB-34: `prune_missing` macOS case-fold uses `to_lowercase()` — incorrect for non-ASCII filenames (PB-24 partial fix)
- **Difficulty:** medium
- **Location:** src/store/chunks/staleness.rs:49-55, :134-139
- **Description:** The PB-24 fix (v1.5.0) for macOS case-insensitive APFS normalizes paths via Rust's `str::to_lowercase()`. This handles ASCII correctly but diverges from APFS case folding for non-ASCII characters. APFS uses a Unicode NFD + locale-independent case fold (per HFS+ Extended rules), while `str::to_lowercase()` applies locale-aware Unicode case folding. The divergence matters for Turkish `I`/`ı`, German `ß`/`ss`, and other non-ASCII uppercase/lowercase pairs that APFS and Unicode handle differently. In practice, source file names are overwhelmingly ASCII, so this rarely triggers. However, the fix is incomplete as documented: repositories with non-ASCII filenames on macOS can still produce false "missing" classifications after a case-only rename.
- **Suggested fix:** For correctness, use the `unicase` or `caseless` crate's Unicode case-fold comparison instead of `to_lowercase()`. As a minimum, add a comment at lines 51 and 137 noting: `// Note: str::to_lowercase() diverges from APFS case folding for non-ASCII chars; this is correct for ASCII-only filenames.`

#### PB-35: `onnx_path` field stores a forward-slash HF path but is used directly in `PathBuf::join` — contract not documented
- **Difficulty:** easy
- **Location:** src/embedder/models.rs:16, :52, :66
- **Description:** `ModelConfig.onnx_path` and `tokenizer_path` are declared as `String` and contain HuggingFace repository-relative paths (`"onnx/model.onnx"`, `"tokenizer.json"`). The field is used in two different contexts: (1) `hf_hub::repo.get(&config.onnx_path)` — expects a forward-slash HF API path; (2) `dir.join(&config.onnx_path)` in `ensure_model` — `PathBuf::join` accepts forward slashes on all platforms. Currently correct, but the dual-use is undocumented. A custom model set via config could supply a Windows-style path (`onnx\model.onnx`), which would be accepted by `PathBuf::join` on Windows but rejected by the HF Hub API. The validation at `models.rs:145` (`("onnx_path", &onnx_path)`) checks for `..` traversal but not for backslashes or absolute Windows paths.
- **Suggested fix:** In `ModelConfig::resolve`, after the path traversal check, add: `if onnx_path.contains('\\') { tracing::warn!("onnx_path contains backslash — use forward slashes for cross-platform compatibility"); }`. Alternatively, normalize backslashes in the field at resolve time. Add a doc comment to the `onnx_path` field: `/// HF-style relative path (forward-slash, no leading slash). Used with both hf_hub API and PathBuf::join.`

## Security

#### SEC-25: `CQS_ONNX_DIR` path not canonicalized or validated — symlink following outside intended directory
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:692-706
- **Description:** When `CQS_ONNX_DIR` is set, the code does `PathBuf::from(dir)` then `dir.join(&config.onnx_path)` without canonicalizing or validating the result. While `config.onnx_path` is validated against `..` traversal in `ModelConfig::resolve` (SEC-20 fix at models.rs:148), the `CQS_ONNX_DIR` value itself is unchecked. A malicious or misconfigured env var pointing to a symlink directory could cause cqs to load an ONNX model from an unexpected location. Given the trust model (local user is trusted, env vars are user-controlled), this is low severity — the user can already run arbitrary code. But it's inconsistent with the `dunce::canonicalize` pattern used in `export_model` (PB-30) and `cqs read`. The `onnx_path` SEC-20 validation protects against `..` but not symlinks in the base directory.
- **Suggested fix:** Add `let dir = dunce::canonicalize(&dir).unwrap_or(dir);` after line 693 to resolve symlinks and normalize the path. Then validate final `model_path` and `tokenizer_path` are inside `dir` (the same `canonical.starts_with` pattern used by `cqs read`).

#### SEC-26: `LlmConfig` logs `api_base` URL at info level — proxy URLs may contain embedded credentials
- **Difficulty:** easy
- **Location:** src/llm/summary.rs:29, src/llm/mod.rs:194
- **Description:** Already reported as SEC-24 in v1.7.0 triage. Verified it is still present: `LlmConfig::resolve` logs `api_base` at debug level (mod.rs:194), and `submit_batch` logs it at info level (summary.rs:29). If a user configures an API proxy with embedded auth (e.g., `https://user:pass@proxy.internal/v1`), credentials appear in logs. The HTTPS scheme warning at mod.rs:196-200 also logs the full URL at warn level. Low severity given local-only tool, but credentials in logs are a well-known anti-pattern.
- **Suggested fix:** Redact the `api_base` before logging: strip the `userinfo@` component from the URL, or just log the scheme + host without the full URL. For the warn-level HTTPS check, log just the scheme, not the full URL.

#### SEC-27: `train_data::git_show` path parameter accepts colons — `sha:path` spec injection
- **Difficulty:** easy
- **Location:** src/train_data/git.rs:129-150
- **Description:** `git_show` validates that `sha` and `path` don't start with `-` or contain `\0`, then constructs `format!("{}:{}", sha, path)`. However, `path` is not checked for embedded colons. A path like `HEAD:../../etc/passwd` would be parsed by git as `sha="<real_sha>"` colon `path="HEAD:../../etc/passwd"`, which git would reject. But a path containing a colon like `file:extra` would create spec `<sha>:file:extra` which git interprets as `<sha>:file:extra` (git only splits on the first colon, so this actually works correctly). The real concern is different: `git_show` is called from `train_data` where the path comes from `git diff-tree` output — a trusted source. But `blame.rs:84` has an explicit colon check for the same pattern. The inconsistency is the finding: `train_data/git.rs` should apply the same validation as `blame.rs`.
- **Suggested fix:** Add `if path.contains(':') { return Err(...) }` after the existing null byte check at line 138, consistent with `blame.rs:84`.

#### SEC-28: `EmbeddingConfig` custom `repo` field not validated — HuggingFace API call with user-supplied repo ID
- **Difficulty:** medium
- **Location:** src/embedder/models.rs:156, src/embedder/mod.rs:713
- **Description:** When a custom model is configured via `[embedding]` in `cqs.toml`, the `repo` field is passed directly to `hf_hub::Api::model(config.repo.clone())` at mod.rs:713. Unlike `export_model` which validates repo format (SEC-18: rejects `"`, `\n`, `\`, requires `/`), the `ensure_model` path does no repo validation. A config file with `repo = "../../../../etc/passwd"` or `repo = "evil\"\n[malicious]"` would be passed directly to the HuggingFace Hub API. The HF Hub API itself validates the repo ID format (org/model), so this is defense-in-depth rather than an exploitable vulnerability. But the inconsistency with `export_model`'s validation is a gap.
- **Suggested fix:** Extract the SEC-18 repo validation from `export_model.rs:34` into a shared helper (e.g., `fn validate_hf_repo_id(repo: &str) -> Result<()>`) and call it in `ModelConfig::resolve` at line 156 before constructing the custom `ModelConfig`.

#### SEC-29: `webhelp_to_markdown` walks directories without path containment check
- **Difficulty:** easy
- **Location:** src/convert/webhelp.rs:68-86
- **Description:** `webhelp_to_markdown` uses `walkdir::WalkDir::new(&content_dir)` to find HTML files, and `filter_entry(|e| !e.path_is_symlink())` to skip symlinks. However, unlike `chm_to_markdown` (which canonicalizes the temp dir and verifies all entries are inside it), the webhelp converter does no containment check — a crafted content directory with a symlink to an external directory could be traversed if the symlink points to a directory (not a file). The `filter_entry(!is_symlink)` check on the WalkDir skips symlinked entries from the iterator, but `is_webhelp_dir` at line 21 only rejects the top-level dir if it's a symlink, not the `content/` subdirectory itself. If `content/` is a symlink to another directory, `webhelp_to_markdown` would walk that external directory.
- **Suggested fix:** Add a symlink check on `content_dir` itself before walking: `if content_dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) { bail!("content/ is a symlink"); }`. Or better, canonicalize `content_dir` and verify all walked entries are inside it, matching the CHM converter's pattern.

#### SEC-30: Prior findings still open — status verification
- **Difficulty:** easy
- **Location:** (multiple)
- **Description:** Verified status of prior security findings from v1.5.0 and v1.7.0 triages:
  - **SEC-14** (git SHA injection in `train_data/git.rs`): FIXED — `git_diff_tree` and `git_show` now validate `sha` doesn't start with `-` or contain `\0` (lines 92, 132).
  - **SEC-15** (http:// API base): FIXED — `LlmConfig::resolve` now warns at mod.rs:196 when `api_base` doesn't start with `https://`. Warning only, not blocked.
  - **SEC-17** (git_show path injection): FIXED — path validated at line 138.
  - **SEC-18** (export_model TOML injection): FIXED — repo validated at export_model.rs:34.
  - **SEC-19** (model.toml permissions): FIXED — 0o600 on Unix at export_model.rs:111.
  - **SEC-20** (custom model path traversal): FIXED — `..` and absolute path check at models.rs:148.
  - **SEC-23** (run_git_diff null byte check): FIXED — null byte check at commands/mod.rs:220.
  - **SEC-24** (api_base logged): STILL OPEN — see SEC-26 above.
- **Suggested fix:** N/A — tracking entry.

## Data Safety

#### DS-33: `delete_phantom_chunks` does not batch `live_ids` — exceeds SQLite 999-parameter limit on large files
- **Difficulty:** medium
- **Location:** src/store/chunks/crud.rs:464-499
- **Description:** `delete_phantom_chunks` builds a single SQL `NOT IN (...)` clause from all `live_ids` without batching. Each live ID is a bound parameter (`?2, ?3, ...`), and the origin takes `?1`, giving `1 + live_ids.len()` parameters per query. SQLite's default `SQLITE_MAX_VARIABLE_NUMBER` is 999. A file with 999+ chunks (large generated files, proto outputs, big modules) will exceed this limit and produce a SQLite error. The comment at line 467 says "Batch at 500 to stay well under SQLite limits" but this is aspirational — no batching is implemented. Compare with `get_enrichment_hashes_batch` (crud.rs:166) and `get_embeddings_by_hashes` (embeddings.rs:76) which both batch at 500. In watch mode, a single large file change triggers this function, and the error propagates as a warning (watch.rs:738) while leaving phantom chunks undeleted — silently degrading search quality over time.
- **Suggested fix:** Batch the `live_ids` parameter list the same way `prune_missing` batches origins (staleness.rs:76). For each batch of 498 IDs (998 params + 1 for origin = 999): build the `NOT IN` clause, execute delete for that batch. Alternatively, use a temp table: insert live IDs into a temp table, then `DELETE FROM chunks WHERE origin = ?1 AND id NOT IN (SELECT id FROM temp_live_ids)`.

#### DS-34: Watch mode `reindex_notes` reads notes.toml without holding notes lock — TOCTOU vs concurrent `cqs notes add`
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:765-789
- **Description:** Watch mode's `reindex_notes` calls `parse_notes(&notes_path)` which acquires a shared lock on `notes.toml.lock`, reads the file, then releases the lock. It then calls `cqs::index_notes(&notes, &notes_path, store)` which writes to SQLite. Between the shared lock release and the SQLite write, a concurrent `cqs notes add` can: (1) acquire exclusive lock, (2) rewrite notes.toml with the new note, (3) release lock. The watch mode's `index_notes` then stores the *old* notes (pre-add) into SQLite, overwriting the note that was just added. The next notes change will fix it (notes are fully replaced on each reindex), but there's a window where a `cqs notes add` appears to succeed (file is written) but the index doesn't reflect it. The `cqs notes add` command itself calls `reindex_notes_cli()` (notes.rs:105) which does the same parse+index cycle, creating a second race window. The race requires two cqs processes to run `rewrite_notes_file` and `index_notes` in overlapping windows — unlikely but possible with `cqs watch` + manual `cqs notes add`.
- **Suggested fix:** Hold the notes lock for the entire parse+index cycle in `reindex_notes`. Change `reindex_notes` to: (1) acquire shared lock on `notes.toml.lock`, (2) `parse_notes_str` on the content (not `parse_notes` which re-acquires), (3) call `index_notes`, (4) release lock. Alternatively, make `index_notes` idempotent by comparing file mtime before writing — but `notes_need_reindex` already does this at the caller level in `index.rs:370`, so the watch path just needs to hold the lock longer.

#### DS-35: HNSW incremental inserts accumulate unbounded orphan vectors across watch restarts
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:500-503, :37-38, :228-229
- **Description:** Watch mode uses incremental HNSW insertion (line 512) which appends new vectors for modified chunks without removing old vectors for the same chunks. Old vectors become orphans — they exist in the HNSW graph but their chunk IDs no longer exist in SQLite. Orphans are cleaned on full rebuild every `HNSW_REBUILD_THRESHOLD` (100) incremental inserts. However, if the watch process exits (Ctrl-C, crash, system restart) before reaching the threshold, orphaned vectors persist on disk. The next `cqs watch` starts fresh (`incremental_count = 0`, line 229) and will accumulate another 100 inserts before rebuilding. Over many restarts, HNSW can accumulate unbounded orphans: each restart resets the counter but the persisted index keeps growing. The index is saved to disk after each incremental insert (line 516), so orphans survive restarts. In extreme cases (many restarts with small change counts), the HNSW index could be mostly orphans, wasting memory and slowing search (more ANN candidates to post-filter). A `cqs gc` or `cqs index --force` cleans up, but watch mode itself never self-heals across restarts.
- **Suggested fix:** On watch startup, check HNSW vector count vs SQLite chunk count. If HNSW has more than 2x the chunks in SQLite, trigger a full rebuild before entering the watch loop. This is a one-line check: `if hnsw.len() > 2 * store.chunk_count()? { rebuild; }`. Alternatively, persist `incremental_count` to metadata so it survives restarts.

#### DS-36: HNSW save backup silently ignores rename failure — `.bak` files may not be created
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:277-283
- **Description:** The HNSW save creates `.bak` backups of existing files before overwriting (line 280-282): `let _ = std::fs::rename(&final_path, &bak_path);`. The `let _` discards rename errors. If the backup rename fails (permissions, disk full, concurrent lock), the save proceeds without backups. If the subsequent temp-to-final rename then fails mid-way (e.g., after moving graph but before data), the rollback (line 322-332) tries to restore from `.bak` files that don't exist. The rollback's `if bak_path.exists()` guard prevents errors, but the result is data loss: the old index files are gone (renamed to final by partial temp moves) and the `.bak` restoration silently does nothing. The user is left with a corrupted HNSW index and no backup. This is the DS-24 pattern from v1.5.0 — the backup step was added to fix it, but the backup itself can fail silently.
- **Suggested fix:** Check the backup rename result. If any backup fails, abort the save before overwriting originals:
  ```rust
  for ext in &all_exts {
      let final_path = dir.join(format!("{}.{}", basename, ext));
      let bak_path = dir.join(format!("{}.{}.bak", basename, ext));
      if final_path.exists() {
          if let Err(e) = std::fs::rename(&final_path, &bak_path) {
              tracing::warn!(error = %e, ext, "Failed to backup HNSW file, aborting save");
              // Restore any backups already made, clean temp dir
              let _ = std::fs::remove_dir_all(&temp_dir);
              return Err(HnswError::Internal(format!("Cannot backup {}: {}", ext, e)));
          }
      }
  }
  ```

#### DS-37: Watch mode phantom chunk cleanup is outside the upsert transaction — stale chunks visible to concurrent searches
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:732-740
- **Description:** `reindex_files` upserts chunks+calls atomically via `upsert_chunks_and_calls` (line 732), then separately calls `delete_phantom_chunks` (line 738). These are two separate transactions. If the process crashes between the upsert commit and the phantom delete, phantom chunks survive in the index. On the next watch cycle, the same file will be re-parsed, re-upserted (idempotent), and phantom deletion will be reattempted — so the state self-heals. However, the window between commit and cleanup exposes phantom chunks to concurrent search queries. The phantom chunks have valid embeddings from a prior version of the function and appear in search results with potentially outdated content. The `upsert_chunks_and_calls` function (crud.rs:390) already runs in a transaction — phantom deletion could be part of the same transaction.
- **Suggested fix:** Extend `upsert_chunks_and_calls` to accept an optional `origin: &Path` parameter. When provided, after upserting chunks, delete phantoms in the same transaction: `DELETE FROM chunks WHERE origin = ?1 AND id NOT IN (...)` using the IDs from the upserted batch. This makes chunk-update-and-cleanup atomic. Alternatively, accept the current behavior as a documented trade-off — phantoms are transient and self-heal, and the window is milliseconds.

## Resource Management

#### RM-35: `clear_session` does not flush the LRU query cache — idle embedder retains cached embeddings
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:515-519
- **Description:** `clear_session` drops the ONNX session (releasing GPU VRAM and ~500MB model weights), but leaves the `query_cache: Mutex<LruCache<String, Embedding>>` untouched. After a 5-minute idle in watch mode, the ONNX session is freed (`emb.clear_session()`), but the LRU cache still holds up to 32 embedding vectors (~4KB each at 1024-dim BGE-large: `32 × 1024 × 4 bytes = 128KB`). This is minor in absolute terms but inconsistent: the intent of `clear_session` is "release memory during idle periods" (per the doc comment at line 510), yet cached embeddings are silently retained. If `DEFAULT_QUERY_CACHE_SIZE` is ever increased, this gap becomes more significant.
- **Suggested fix:** In `clear_session`, after dropping the session, also clear the cache: `let mut cache = self.query_cache.lock().unwrap_or_else(...); cache.clear();`

#### RM-36: `DEFAULT_QUERY_CACHE_SIZE` comment says "~3KB (768 floats + key)" — wrong for BGE-large (1024 floats)
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:233-234
- **Description:** The comment `/// Default query cache size (entries). Each entry is ~3KB (768 floats + key).` was written for E5-base-v2 (768-dim). With the BGE-large default (1024-dim), each entry is `1024 × 4 = 4096 bytes ≈ 4KB` plus the key string. The comment understates actual cache memory footprint by ~33%. Not a runtime bug, but misleading for capacity planning and audits.
- **Suggested fix:** Change comment to `/// Each entry is ~4KB (1024 floats × 4 bytes + key string) for BGE-large default; ~3KB for E5-base-v2.`

#### RM-37: RM-32 `content_length` check only applies when server provides `Content-Length` — chunked transfer encoding bypasses it
- **Difficulty:** medium
- **Location:** src/llm/batch.rs:229-240
- **Description:** The RM-32 fix checks `if let Some(len) = response.content_length()` before buffering the batch results. But `content_length()` returns `None` when the server uses chunked transfer encoding (`Transfer-Encoding: chunked`), which the Anthropic Batch API uses for large responses. When `content_length()` returns `None`, the check is silently skipped and `response.text()` proceeds to buffer the entire body — potentially hundreds of MB. The 100MB cap is correct when `Content-Length` is present, but provides no protection for chunked responses, which is the common case for large batch results.
- **Suggested fix:** Replace `response.text()` with streaming line-by-line parsing using `response.bytes_stream()` or limit the body at the reqwest level with a custom reader that counts bytes and errors at 100MB. Alternatively, buffer into a `String` while counting bytes: use `response.chunk()` in a loop, accumulating into a `String` with a running byte count and early return on overflow.

#### RM-38: `git_log` with `max_commits=0` loads the entire git history into memory before iteration
- **Difficulty:** easy
- **Location:** src/train_data/git.rs:31-79
- **Description:** When `max_commits=0` (unlimited), `git_log` collects the full git log output into a single `Vec<CommitInfo>`. The caller then iterates the `Vec` for diff generation. For repositories with large histories (50k+ commits), this allocates a `Vec` of potentially 50k+ `CommitInfo` structs (each ~100 bytes → ~5MB for 50k commits) before processing starts. While test defaults all set `max_commits=0` without calling this with large repos, the function lacks a warning for unbounded loads. The interactive CLI at `cli/definitions.rs:635` says `/// Maximum commits to process per repo (0 = unlimited)` with no documented risk.
- **Suggested fix:** Add a warning when `max_commits=0` and the result exceeds a threshold (e.g., 100k): `if max_commits == 0 && commits.len() > 100_000 { tracing::warn!(...) }`. Document in the CLI help that `0` can be slow on large repos. A proper fix would stream via `git log --format=...` with line-by-line processing, but that requires the function's interface to change.

#### RM-39: `find_contrastive_neighbors` allocates both N×N similarity matrix and result `HashMap` simultaneously — peak is ~2× the matrix
- **Difficulty:** medium
- **Location:** src/llm/summary.rs:247-282
- **Description:** After `drop(valid); drop(embeddings);` (RM-33 fix at line 243), the code computes `let sims = matrix.dot(&matrix.t())` (line 247 — N×N × 4 bytes), then immediately begins populating `result: HashMap<String, Vec<String>>` (line 251). Both `sims` (N×N matrix) and `result` are live simultaneously during the neighbor extraction loop (lines 252-281). At 15k chunks (the DS-21 cap), `sims` = 15k × 15k × 4 bytes = ~900MB, while `result` grows to ~15k entries with neighbor Vec<String> per entry. Peak RSS at the cap is ~900MB + ~10MB for `result` ≈ 910MB. The comment at line 140 says "~550MB at 12k callable chunks" — the actual cap is 15k, and peak is higher than documented. Additionally, `matrix` itself (15k × 1024 × 4 = ~61MB) is still live until `sims` is computed, so true peak is `matrix` + `sims` = ~61MB + 900MB = ~961MB.
- **Suggested fix:** Drop `matrix` explicitly before the neighbor extraction loop: `drop(matrix);` between lines 247 and 251. This eliminates the ~61MB matrix overlap and reduces peak to ~900MB + result. Document the actual peak at 15k cap in the comment. For a more significant reduction, compute neighbors row-by-row without materializing the full N×N matrix (dot product one row at a time), reducing peak to O(N) instead of O(N²).

#### RM-40: `HNSW` index is fully loaded into RAM on every command — no mmap or lazy load
- **Difficulty:** medium
- **Location:** src/hnsw/persist.rs:492-524, src/cli/mod.rs:138-142
- **Description:** Every cqs command that searches (query, gather, callers, etc.) calls `HnswIndex::load_with_dim` which reads the `.hnsw.graph` and `.hnsw.data` files entirely into RAM via `hnsw_rs`'s `HnswIo::load_hnsw`. For a 100k-chunk codebase at 1024-dim, the HNSW data file is roughly `100k × 1024 × 4 bytes × ~2 (graph overhead) ≈ 800MB`. This means every single CLI invocation (even `cqs "simple query"`) requires ~800MB RSS for the HNSW load, plus ~60MB embedder session, plus SQLite pools. There is no persistent daemon or mmap — each command does a full cold load. The 500MB file-size cap at line 420 bounds the `.graph` and `.data` files separately but the combined in-memory representation can be larger. `watch` mode mitigates this by keeping the HNSW in memory across cycles, but per-command invocations do not benefit.
- **Suggested fix:** This is a known architectural limitation with no quick fix — hnsw_rs does not support mmap. Document the memory model clearly in README (currently there is no per-command RAM footprint guidance). For medium term: consider switching to usearch or another HNSW library with mmap support. Short term: expose a `--no-index` flag that forces brute-force search (bypassing HNSW load) for memory-constrained environments.

## Performance

#### PERF-40: `update_embeddings_with_hashes_batch` issues N individual UPDATE statements in one transaction
- **Difficulty:** medium
- **Location:** src/store/chunks/crud.rs:121-148
- **Description:** Despite its name, `update_embeddings_with_hashes_batch` issues one `UPDATE` statement per item inside a single transaction. For the default enrichment batch of 64 items (`ENRICH_EMBED_BATCH`), this is 64 round-trips within the async executor per flush. At 10k enriched chunks / 64-item batches = ~156 flush calls × 64 statements = ~10,000 prepared statement executions total per enrichment pass. The function has two separate SQL templates depending on whether `hash` is `Some` or `None`, which prevents unifying into a single bulk path. SQLite WAL mode amortizes the commit cost across the transaction — but per-statement prepare/bind/execute cycles are not. A CASE-expression UPDATE or INSERT into a staging table would reduce this to a constant number of SQL round-trips per batch.
- **Suggested fix:** For the `Some(hash)` path (the hot enrichment path): use a single UPDATE with a CASE expression — `UPDATE chunks SET embedding = CASE id WHEN ?1 THEN ?2 WHEN ?3 THEN ?4 ... END, enrichment_hash = CASE id ... WHERE id IN (...)`. This collapses N statements into 1. Alternatively, insert into a temporary table and `UPDATE chunks FROM tmp`. The `None` path has fewer callers and can stay per-row.

#### PERF-41: `get_enrichment_hashes_batch` builds manual positional placeholders instead of using `make_placeholders`
- **Difficulty:** easy
- **Location:** src/store/chunks/crud.rs:167-172
- **Description:** `get_enrichment_hashes_batch` manually builds `?1, ?2, ...` via `batch.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect::<Vec<_>>().join(",")`. Two functions lower in the same file, `get_summaries_by_hashes` (line 230) uses the shared `crate::store::helpers::make_placeholders(batch.len())` helper for identical work. `make_placeholders` has caching behaviour for common sizes; the manual construction allocates one `String` per slot every call. This inconsistency is a v1.7.0 triage item (PERF-32: "`get_summaries_by_hashes` manual placeholders bypasses cached `make_placeholders`") — PERF-32 was fixed in `get_summaries_by_hashes` but `get_enrichment_hashes_batch` was not updated when it was added later.
- **Suggested fix:** Replace lines 167-172 with `let placeholders = crate::store::helpers::make_placeholders(batch.len());`. One-line fix, no behaviour change.

#### PERF-42: `delete_phantom_chunks` comment says "Batch at 500" but never batches — misleading comment, fragile for large files
- **Difficulty:** easy
- **Location:** src/store/chunks/crud.rs:467-490
- **Description:** The comment at line 467 reads "Build IN-list for live IDs. Batch at 500 to stay well under SQLite limits." The code does not batch: the full `live_ids` slice is bound in a single `NOT IN (...)` clause. This was separately reported as DS-33 (Data Safety) which identified the >999-parameter crash risk on very large files. The Performance angle: even for moderately large files (200-400 chunks), both the FTS delete and the chunks delete each bind all live IDs, doubling the binding work. The `NOT IN (...)` plan on SQLite for large lists (>100 elements) degrades to O(N×M) rather than a hash-join. For watch-mode re-indexing on hot files (edited frequently), this runs on every modified file.
- **Suggested fix:** Remove the misleading comment (no batching is implemented). The DS-33 fix (batching or temp table) would address both the crash risk and the performance issue simultaneously.

#### PERF-43: `find_contrastive_neighbors` heap extraction is O(N²) in Rust after the BLAS matmul — inner loop dominates at 10k chunks
- **Difficulty:** medium
- **Location:** src/llm/summary.rs:252-282
- **Description:** After computing the N×N similarity matrix via `matrix.dot(&matrix.t())` (line 247, executed in BLAS), top-K extraction iterates every row with a Rust inner loop over all N columns (lines 258-271). This is `O(N²)` heap push/pop operations in interpreted Rust. At 10k chunks (current: 10,473 vectors), that is 10^8 BinaryHeap operations. The code comment claims "~1.3s for 10k chunks" — this measures the BLAS matmul plus the heap loop combined. With BGE-large (1024-dim), the matmul is `10k × 10k × 1024` FMAs in BLAS (~fast); the heap loop is `10k × 10k` Rust BinaryHeap calls (~slow). The RM-33 fix (drop embeddings HashMap before matmul, lines 243-244) is already applied. The remaining bottleneck is the per-row top-K extraction.
- **Suggested fix:** Replace the per-row heap with a partial sort: `let mut row_vec: Vec<(f32, usize)> = (0..n).filter(|&j| j != i).map(|j| (sims[[i, j]], j)).collect(); row_vec.select_nth_unstable_by(limit-1, |a, b| b.0.total_cmp(&a.0));` — O(N) per row via `select_nth_unstable_by` instead of O(N × log K). Total: O(N²) instead of O(N² × log K). Alternatively, use FAISS flat index or the existing HNSW for ANN neighbor lookup (~O(N × log N) total).

#### PERF-44: Notes loaded and `NoteBoostIndex` rebuilt twice per index-guided search
- **Difficulty:** easy
- **Location:** src/search/query.rs:69-97 and src/search/query.rs:337-361
- **Description:** `search_filtered` (lines 69-97) and `search_by_candidate_ids` (lines 337-361) each independently call `self.cached_notes_summaries()` and construct `NoteBoostIndex::new(&notes)`. When `search_filtered_with_index` dispatches to `search_by_candidate_ids` (line 305), neither is passed through — both are reconstructed. On a warm cache (typical) this adds one extra `Vec<NoteSummary>` deep-clone (PERF-46 below) plus one `NoteBoostIndex::new()` call (HashMap construction over all notes) per search. At 114 notes it is cheap — but the pattern duplicates both code and allocation on every search, and grows with note count.
- **Suggested fix:** Move notes loading and `NoteBoostIndex` construction into `search_filtered_with_index`. Pass the pre-built `NoteBoostIndex` to `search_by_candidate_ids` as a parameter (or via an internal `_with_notes` variant). The public API is unchanged.

#### PERF-45: `EMBED_BATCH_SIZE: 32` was halved without diagnosing root cause — may leave GPU throughput unused
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:34-35
- **Description:** The comment reads `// Embedding batch size (backed off from 64 — crashed at 2%)`. The batch was halved reactively after a crash during indexing. The enrichment pass uses `ENRICH_EMBED_BATCH = 64` (enrichment.rs:75) with the same embedder without incident — suggesting the crash was specific to the parallel pipeline (GPU + CPU threads competing for memory) rather than batch size per se. At batch=32 with BGE-large (1024-dim), the GPU processes 32 sequences at max 512 tokens each. If typical sequences are 100-200 tokens, GPU memory pressure is well below the 8GB RTX 4000 limit. Permanently halving batch size for all inputs because of rare outliers (very long functions or GPU contention) leaves throughput on the table.
- **Suggested fix:** Diagnose the root cause: add `tracing::debug!(batch_size, max_token_len, "embed_batch start")` to `embed_batch` to log per-call max token lengths. If the crash was from a single 512-token sequence, reduce batch size only for token-heavy batches via dynamic sizing. If from GPU memory contention between threads, fix the concurrency model (e.g., one shared embedder mutex). Target the fix rather than permanently halving throughput.

#### PERF-46: `cached_notes_summaries` deep-clones all notes entries on every search call (warm path)
- **Difficulty:** easy
- **Location:** src/store/metadata.rs:301-321
- **Description:** On a cache hit, `cached_notes_summaries` returns `ns.clone()` — a deep clone of the full `Vec<NoteSummary>`. Each `NoteSummary` has at least two heap-allocated `String` fields (text, mentions list). At 114 notes (from `cqs health: note_count: 114`), every search call allocates and deep-copies 114+ entries. The code comment says "The clone cost is negligible — notes are typically <100 entries" — the current count already exceeds this threshold, and notes accumulate over time. PERF-44 above shows this clone happens twice per index-guided search (both `search_filtered` and `search_by_candidate_ids` call it independently).
- **Suggested fix:** Change the cache to `Option<Arc<Vec<NoteSummary>>>`. `cached_notes_summaries` returns `Arc::clone` (atomic pointer increment) instead of a deep clone. `NoteBoostIndex::new` accepts `&[NoteSummary]` and can borrow from the `Arc` deref. The return type becomes `Arc<Vec<NoteSummary>>` — minimal API change and eliminates all allocations on warm cache hits.
