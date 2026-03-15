# Audit Findings — v1.0.7

## API Design

#### AD-13: `generate_nl_with_call_context` takes 5 positional parameters — `max_callers`/`max_callees` should live in `CallContext`
- **Difficulty:** easy
- **Location:** src/nl.rs:274-279, src/cli/pipeline.rs:999-1005
- **Description:** `generate_nl_with_call_context(chunk, ctx, callee_doc_freq, max_callers, max_callees)` has 5 positional parameters. The last two are configuration that controls how `ctx` is applied, but they are passed separately from it. The call site uses magic literals `5, 5` with inline comments to identify them (pipeline.rs:1003-1004). A new caller has to discover the intended defaults by reading the existing call site. `CallContext` is a public struct exported in `lib.rs:120`. Folding `max_callers` and `max_callees` into `CallContext` (with `Default` yielding 5) would make the API self-documenting and reduce the function arity. The IDF threshold (0.10, hardcoded at nl.rs:307) is a third implicit parameter that could join the same struct for completeness.
- **Suggested fix:** Extend `CallContext` with the limits and reduce function arity:
  ```rust
  pub struct CallContext {
      pub callers: Vec<String>,
      pub callees: Vec<String>,
      pub max_callers: usize,    // default: 5
      pub max_callees: usize,    // default: 5
      pub idf_threshold: f32,    // default: 0.10 (optional addition)
  }
  // Signature becomes:
  pub fn generate_nl_with_call_context(chunk: &Chunk, ctx: &CallContext, callee_doc_freq: &HashMap<String, f32>) -> String
  ```

#### AD-14: `callee_document_frequencies` return type forces callers to perform the frequency normalization themselves
- **Difficulty:** easy
- **Location:** src/store/calls.rs:1242, src/cli/pipeline.rs:926-929
- **Description:** `callee_document_frequencies()` returns `Vec<(String, usize)>` (raw caller counts), but its sole caller immediately converts the result into `HashMap<String, f32>` (fractional frequencies, 0.0–1.0) by dividing each count by `total_chunks`. The function name uses the term "document frequencies", which in information retrieval means a ratio, but it returns raw counts. Callers must know to perform the division and must independently obtain `total_chunks` to do so. This splits a single conceptual operation across two call sites and introduces a second failure mode (wrong divisor). Additionally, the `usize` return requires a cast to `f32` in the caller, which is a silent precision loss for large codebases.
- **Suggested fix:** Option A (naming): rename to `callee_caller_counts()` to match what the SQL actually returns (`COUNT(DISTINCT caller_name)` per callee). Option B (API): take `total_chunks: usize` as a parameter and return `HashMap<String, f32>` directly, keeping the normalization co-located with the function that knows the semantics.

#### AD-15: `update_embeddings_batch` parameter tuple `(String, Embedding)` — `String` role undocumented
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:79-82
- **Description:** `update_embeddings_batch(updates: &[(String, Embedding)])` takes a slice of tuples where `String` is a chunk ID. This is not stated in the function signature or the doc comment (there is no doc comment). Other store batch methods either use domain types (`&[(Chunk, Embedding)]` in `upsert_chunks_batch`) or document their tuple element roles. A reader encountering this signature cannot tell whether `String` is an ID, a name, a hash, or an origin path without tracing the call site. Prior audit AD-1 addressed the same class of issue for `file` fields (String → PathBuf). The fix here is lighter weight: a doc comment is sufficient; a newtype is optional.
- **Suggested fix:** Add a doc comment:
  ```rust
  /// Update embeddings for existing chunks without changing their content.
  ///
  /// `updates` is a slice of `(chunk_id, embedding)` pairs. Chunk IDs not found
  /// in the store are silently skipped (rows_affected == 0).
  pub fn update_embeddings_batch(&self, updates: &[(String, Embedding)]) -> Result<usize, StoreError>
  ```

#### AD-16: `chunks_paged` cursor-return convention is ambiguous and inconsistent with `embedding_batches`
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:1085-1116
- **Description:** `chunks_paged(after_rowid, limit)` returns `(Vec<ChunkSummary>, i64)` where the `i64` is the max rowid seen in the page, for use as the next `after_rowid`. Two issues:
  1. When the returned vec is empty, the cursor is unchanged (equals `after_rowid`). The doc mentions "iteration is complete" but not what value the cursor holds, and a caller that passes the returned cursor unconditionally will loop forever.
  2. `embedding_batches()` (same file, line 1312) solves identical pagination via an `Iterator` that handles the cursor internally — callers never see a raw rowid. Having both APIs for the same operation is inconsistent. The enrichment pass in `pipeline.rs` uses `chunks_paged` with manual cursor management; `embedding_batches` exists as the better pattern.
- **Suggested fix:** Document the empty-page cursor value: "When the returned vec is empty, the returned cursor equals `after_rowid`; do not pass it to a subsequent call." Consider adding `chunks_batches(batch_size) -> impl Iterator<Item=Result<Vec<ChunkSummary>>>` analogous to `embedding_batches`, and routing `enrichment_pass` through it.

#### AD-17: `enrichment_pass` `quiet: bool` parameter — output-control concern leaks into internal pipeline function
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:911
- **Description:** `enrichment_pass(store, embedder, quiet: bool)` has no doc comment and takes a `quiet` flag that controls progress-bar display and `eprintln!` output. `run_index_pipeline` (same file), which owns the first indexing pass, does not have a `quiet` flag — it emits output unconditionally via `eprintln!`. The asymmetry means these two closely related pipeline functions have inconsistent output contracts. `quiet` is a CLI concern (the user's `--quiet` flag from `index.rs:143`) flowing into an internal `pub(crate)` function, coupling the function to CLI semantics. It also lacks a doc comment explaining any of its parameters or return value.
- **Suggested fix:** At minimum, add a doc comment:
  ```rust
  /// Re-embed all chunks with call graph context (caller/callee names).
  ///
  /// Returns the number of chunks re-embedded. Pass `quiet = true` to suppress
  /// progress bar and eprintln output (e.g., in `--quiet` CLI mode).
  ```
  Longer-term: unify output policy with `run_index_pipeline` (both suppress or both accept a verbosity level).

## Observability

#### OB-8: `enrichment_pass()` missing skip metrics and timing
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:911-1029
- **Description:** `enrichment_pass()` has a basic span but logs only final enriched_count. Missing:
  1. Skip count: how many chunks had no callers/callees (line 985-986 silently skips)
  2. Batch flush count: how many batches were flushed
  3. Callee frequency count: total unique callees suppressed (could be valuable for tuning)
  4. Timing context: how long the pass took (compare to pipeline main pass)

  For comparison, `run_index_pipeline()` (line 889-897) logs detailed structured metrics: `total_embedded`, `total_cached`, `gpu_failures`, `parse_errors`, `total_type_edges`, `total_calls`. The enrichment pass should follow the same pattern.
- **Suggested fix:** Add structured logging fields to the final `tracing::info!` call:
  ```rust
  tracing::info!(
      enriched_count,
      skipped_leaf_nodes = total_chunks - enriched_count,  // approximation at least
      total_callee_freq_entries = callee_doc_freq.len(),
      utility_callees_filtered = callees_suppressed_count,
      "Enrichment pass complete"
  );
  ```
  Also track `skipped_count` explicitly during the loop to get exact count.

#### OB-9: `update_embeddings_batch()` uses per-row UPDATE instead of batch UPDATE
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:1050, src/store/chunks.rs:79-105
- **Description:** `update_embeddings_batch()` executes one `UPDATE` per embedding (line 95-100), unlike the pattern used by `upsert_chunks_batch()` which batches INSERT with `QueryBuilder`. For 64-item batches, this means 64 individual SQLite transactions within a single outer transaction, reducing observability and likely hurting latency.

  The missing span context makes it hard to see: (a) how long embedding serialization takes, (b) how long the SQL executions take, (c) whether batching would help. The span captures `count` but no per-operation timing.
- **Suggested fix:** Either:
  1. Implement batch UPDATE with `QueryBuilder::push_values()` (prefer, matches `upsert_chunks_batch` pattern)
  2. Add child spans around the loop to surface per-op cost:
     ```rust
     let _span = tracing::info_span!("embedding_serialization").entered();
     let embedding_bytes = ...; // serialize
     drop(_span);

     let _span = tracing::info_span!("embedding_updates", count = updates.len()).entered();
     for (...) { ... }  // individual updates
     ```

#### OB-10: `chunks_paged()` has debug span but no result metrics
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:1085-1114
- **Description:** `chunks_paged()` (debug-level span, line 1090) logs the inputs (`after_rowid`, `limit`) but not the output: how many chunks were returned. When called in a loop (enrichment_pass line 967), debug logs don't show progress. Compare to `upsert_chunks_batch` which logs `count` (input), and callers know how many were written. For pagination debugging, the actual chunk count retrieved is essential.
- **Suggested fix:** Add `fetched` field to the span's structured data:
  ```rust
  let chunks: Vec<ChunkSummary> = ...;
  let fetched = chunks.len();
  tracing::debug!(fetched, "Paged {} chunks", fetched);
  ```
  Or use `Span::record()` if entering the span before the query is needed.

#### OB-11: `flush_enrichment_batch()` missing tracing span
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:1032-1054
- **Description:** `flush_enrichment_batch()` has no tracing span, called from `enrichment_pass()` on each flush (line 1011, 1018). The function does three observable operations: (1) serialize NL texts, (2) embed documents, (3) update store. Without a span, it's invisible to structured logging. Callers only see the parent span from `enrichment_pass()`.
- **Suggested fix:** Add tracing span with key metrics:
  ```rust
  fn flush_enrichment_batch(...) -> Result<usize> {
      let _span = tracing::debug_span!("flush_enrichment_batch", batch_size = batch.len()).entered();
      let texts: Vec<&str> = ...;
      let embeddings = embedder.embed_documents(&texts)...?;
      // span auto-records elapsed via tracing infrastructure
      ...
  }
  ```

#### OB-12: `callee_document_frequencies()` returns usize but no span context on result
- **Difficulty:** easy
- **Location:** src/store/calls.rs:1242-1261
- **Description:** `callee_document_frequencies()` has a debug span but doesn't log the result: how many unique callees were found. For enrichment_pass (line 923), knowing "found 127 unique callees with frequency data" is useful for debugging IDF filtering logic. The span captures no output size.
- **Suggested fix:** Record the result count in the span:
  ```rust
  let result: Vec<_> = ...;
  let count = result.len();
  tracing::debug!(count, "Computed callee frequencies");
  Ok(result)
  ```

## Error Handling

#### EH-8: `flush_enrichment_batch` leaves progress bar dangling on error
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:1011, 1018 / src/cli/pipeline.rs:950-961
- **Description:** `enrichment_pass` creates a `ProgressBar` at line 950, but when `flush_enrichment_batch` returns an error the `?` propagates immediately, bypassing `progress.finish_and_clear()` at line 1021. The progress bar is left in a spinning/incomplete state in the terminal. The caller in `index.rs` catches the error (line 149) and prints a warning, but the terminal is already corrupted with the dangling bar.
- **Suggested fix:** Use a guard or call `progress.abandon()` before returning, or restructure with a closure/defer pattern:
  ```rust
  let result = (|| -> Result<usize> {
      // loop body
  })();
  progress.finish_and_clear();
  result
  ```

#### EH-9: `flush_enrichment_batch` silent truncation on embedding count mismatch
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:1043-1047
- **Description:** `batch.drain(..).zip(embeddings)` stops at the shorter iterator. If `embed_documents` returns fewer embeddings than texts (edge case in internal batching or a partial GPU failure), items are silently dropped from the batch — they are consumed by `drain` but never written to the store. The returned count will be understated, and there is no warning. The same pattern does not exist in `upsert_chunks_batch` which asserts lengths match.
- **Suggested fix:** Assert or validate lengths before zipping:
  ```rust
  anyhow::ensure!(
      embeddings.len() == texts.len(),
      "Embedding count mismatch: expected {}, got {}",
      texts.len(), embeddings.len()
  );
  ```

#### EH-10: `update_embeddings_batch` silently no-ops for unknown chunk IDs
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:95-100
- **Description:** The `UPDATE chunks SET embedding = ?1 WHERE id = ?2` query executes without checking `rows_affected()`. If a chunk ID no longer exists (e.g., pruned between enrichment fetch and write, or a race with `delete_by_origin`), the UPDATE silently updates 0 rows. `enriched_count` in `enrichment_pass` is incremented for these phantom writes (it counts items in `updates`, not actual DB rows changed). No warning is emitted. The mismatch between claimed and actual enrichment count could mask stale data issues.
- **Suggested fix:** Check rows_affected and emit a debug/warn if any ID was not found:
  ```rust
  let result = sqlx::query("UPDATE chunks SET embedding = ?1 WHERE id = ?2")
      .bind(&embedding_bytes[i])
      .bind(id)
      .execute(&mut *tx)
      .await?;
  if result.rows_affected() == 0 {
      tracing::debug!(chunk_id = %id, "Enrichment update found no row (pruned race?)");
  }
  ```

#### EH-11: `flush_enrichment_batch` drains batch before store write — items lost on store error
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:1043-1051
- **Description:** `batch.drain(..)` is called to build `updates` (line 1044), emptying the batch before `store.update_embeddings_batch(...)` is called (line 1050). If the store write fails, the drained items are gone from `batch` — the caller cannot retry them. The `?` at line 1051 propagates the error, so in practice the enrichment pass aborts entirely, but the drain-before-write ordering means retry logic (if added later) would lose items. This is a latent correctness issue. By contrast, the drain should happen after a successful store write, or the IDs should be preserved for re-queue.
- **Suggested fix:** Drain after success, or hold `updates` and only clear batch on success:
  ```rust
  let texts: Vec<&str> = batch.iter().map(|(_, nl)| nl.as_str()).collect();
  let embeddings = embedder.embed_documents(&texts).context("...")?;
  let updates: Vec<(String, Embedding)> = batch.iter()
      .zip(embeddings)
      .map(|((id, _), emb)| (id.clone(), emb.with_sentiment(0.0)))
      .collect();
  store.update_embeddings_batch(&updates).context("...")?;
  batch.clear(); // only clear after successful write
  Ok(updates.len())
  ```

#### EH-12: `ProgressStyle::template().unwrap()` in production path
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:956-957
- **Description:** `ProgressStyle::default_bar().template("...").unwrap()` is called in the non-quiet code path of `enrichment_pass`. The template string is a compile-time literal so this is infallible in practice, but it is an `unwrap()` outside of tests in production code, violating the project convention (`No unwrap() except in tests`). A malformed template would panic at runtime.
- **Suggested fix:** Replace with `expect("valid progress template")` to make the intent clear, or use `unwrap_or_else` with a fallback style. The same pattern appears in `run_index_pipeline` — check and fix consistently.

## Documentation

#### DOC-1: Language files list in CONTRIBUTING.md missing vue.rs and aspx.rs
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:124
- **Description:** The Architecture Overview lists 49 language module files (rust.rs through markdown.rs), but cqs actually supports 51 languages. Vue (.vue files) and ASP.NET Web Forms (.aspx/.ascx/.asmx/.master files) were added in v0.28.0 (Vue) and v1.0.5 (ASPX) but the list is stale. The enum in `src/language/mod.rs` and the language count claims in README.md and lib.rs are all correct, only the CONTRIBUTING.md file list is incomplete.
- **Suggested fix:** Append `vue.rs, aspx.rs` after `vbnet.rs,` on line 124 to show all 51 language modules.

#### DOC-2: Function doc comment for IDF filtering threshold lacks explicit value
- **Difficulty:** easy
- **Location:** src/nl.rs:265-330 (CallContext struct + generate_nl_with_call_context function)
- **Description:** New function `generate_nl_with_call_context()` added in v1.0.7 (#590) has solid doc comments explaining the call-graph enrichment pass. However, the IDF filtering threshold is mentioned only inline as ">10% of chunks" in the code comment (line 288), but the rustdoc comment on the function doesn't state the exact threshold value (0.10 as f32). This makes the threshold non-discoverable via `rustdoc` without reading source code.
- **Suggested fix:** Add threshold value to rustdoc comment: `/// Callees appearing in >10% of chunks are filtered as utilities (IDF threshold: 0.10).` This would make the threshold discoverable via `rustdoc` without reading source.

#### DOC-3: README.md schema version context unclear for upgrading users
- **Difficulty:** easy
- **Location:** README.md:35
- **Description:** Line 35 states "(current schema: v12)" which is correct, but the upgrade instruction `cqs index --force` doesn't mention the v11→v12 migration is automatic. For users upgrading from v1.0.4 or earlier, the v11→v12 migration happens automatically during first index open. The comment is accurate but could be clearer about when rebuilds are required vs. automatic migrations.
- **Suggested fix:** Minor improvement — change to "(current schema: v12, auto-migrates from v11)" to clarify the migration is automatic. Or add a note: "Schema changes in v1.0.5+ auto-migrate existing indexes."

#### DOC-4: src/store/chunks.rs missing documented update pattern for new functions
- **Difficulty:** easy
- **Location:** src/store/chunks.rs header comments (lines 1-5)
- **Description:** Two new public functions added in v1.0.7 (#590): `update_embeddings_batch()` (line 79) and `chunks_paged()` (line 1085) are well-documented individually. However, the module's doc comment doesn't mention these new capabilities. Line 1 says "//! Chunk CRUD operations" but `update_embeddings_batch()` is not exactly CRUD — it's embedding-only update without content change. And `chunks_paged()` is cursor-based iteration. These are documented in the function comments but not in the module overview.
- **Suggested fix:** Update module-level doc comment to include: `//! - Embedding-only updates (for enrichment passes)` and `//! - Cursor-based pagination (for streaming operations)`. This helps future readers understand the module scope.

## Code Quality

#### CQ-7: `replace_file_chunks` is dead code — zero callers in production
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:164-265
- **Description:** `replace_file_chunks` (100 lines) has no callers in production code. The entire call graph: the function is called only from three unit tests in the same file (lines 1624, 1668, 1693). The pipeline uses `upsert_chunks_and_calls` instead, and the watch mode no longer calls this function. The function also duplicates ~65 lines of INSERT logic from the `batch_insert_chunks` private helper (itself used by `upsert_chunks_batch` and `upsert_chunks_and_calls`). Keeping dead code inflates maintenance surface: the INSERT SQL at line 203 must stay in sync with `batch_insert_chunks` at line 1356 (19 columns, same order), and the FTS batch logic at line 234 duplicates `upsert_fts_conditional`. The v1.0.5 audit fixed PERF-3 ("upsert_chunks_and_calls duplicates ~120 lines of chunk-upsert logic"), but `replace_file_chunks` was not addressed.
- **Suggested fix:** Remove `replace_file_chunks`. Its test cases should be ported to test `upsert_chunks_and_calls` (the live path), or kept as integration tests exercising the actual pipeline. If the delete-and-replace semantic is needed in future, re-implement it by calling `delete_by_origin` followed by `upsert_chunks_batch`, or add it to `upsert_chunks_and_calls` with a `replace: bool` flag.

#### CQ-8: `extract_file_context` doc comment is a merged accident — two functions' docs fused into one
- **Difficulty:** easy
- **Location:** src/nl.rs:618-625
- **Description:** The doc comment on `extract_file_context` at line 625 begins with the first two lines of what should be `truncate_doc`'s doc comment ("Truncate a doc comment to its first sentence (or 150 chars, whichever comes first). Keeps the most informative part of the doc within the embedding token budget.") followed by the correct doc for `extract_file_context` ("Extract concise module context from a file path..."). The `truncate_doc` function at line 674 has no doc comment at all — its doc was accidentally attached to the wrong function. This likely occurred when the two functions were defined adjacent and the doc comment blocks were merged during an edit.
- **Suggested fix:** Split the doc comments: give `truncate_doc` its own doc ("Truncate a doc comment to its first sentence (or 150 chars, whichever comes first). Keeps the most informative part of the doc within the embedding token budget.") and give `extract_file_context` only its correct doc ("Extract concise module context from a file path...").

#### CQ-9: `NlTemplate::Standard` doc comment is stale — claims "Current production template" but `Compact` is used
- **Difficulty:** easy
- **Location:** src/nl.rs:239-240
- **Description:** The `Standard` variant's doc comment reads "Current production template: doc + 'A {type} named {name}' + params + returns". However, `generate_nl_description` (the production entry point) uses `NlTemplate::Compact`, not `Standard`. `Standard` is only exercised in the evaluation harness (`tests/model_eval.rs:511`). The stale comment creates false confidence: a reader could conclude `Standard` is the active format and write tests validating "A function named ..." structure, not knowing it's a retired experiment.
- **Suggested fix:** Change the doc to "Baseline evaluation template (inactive): doc + 'A {type} named {name}' + params + returns. Use `Compact` for production. Kept for A/B testing via `model_eval`."

#### CQ-10: `callee_document_frequencies` IDF metric is misnamed — counts distinct callers, not document occurrences
- **Difficulty:** easy
- **Location:** src/store/calls.rs:1242-1261, src/cli/pipeline.rs:920-929
- **Description:** `callee_document_frequencies()` is named using information-retrieval terminology ("document frequency"), which conventionally means "the fraction of documents (here: chunks) in which a term (here: callee name) appears." The SQL implementation uses `COUNT(DISTINCT caller_name)` — counting how many unique *callers* reference each callee, not how many chunks contain it. The comment in `enrichment_pass` (line 922) reinforces the mislabeling: "A callee appearing in >10% of chunks is a utility". The denominator `total_chunks` compounds the confusion: dividing distinct-caller-count by total-chunks gives a ratio that has no standard IR interpretation. A callee called by 1 000 distinct callers in a 10 000-chunk index yields 0.10 and hits the threshold — but if those callers are spread across 9 000 unique chunks, the actual document frequency is 0.90. The metric may still work empirically (high-caller-count callees are utilities), but the semantic drift between the name, the comment, and the implementation creates a maintenance hazard: a future developer may "fix" the denominator to `DISTINCT_CALLERS` or change the SQL to `COUNT(DISTINCT chunk_id)` expecting equivalent behavior.
- **Suggested fix:** Option A: Rename to `callee_caller_counts()` and update `enrichment_pass` comments to say "A callee called by >10% of unique callers is suppressed as a utility." Option B: Fix the SQL to use `COUNT(DISTINCT c.id)` with a JOIN to the chunks table, making it a true document frequency. Then the metric and name align.

#### CQ-11: `batch_count_query` injects column names via `format!` — internal but creates fragile SQL generation pattern
- **Difficulty:** easy
- **Location:** src/store/calls.rs:1087-1115
- **Description:** `batch_count_query(filter_column, group_column, count_expr, names)` constructs SQL by formatting column names and expressions directly into the query string (line 1100). The function is private (`async fn`) and all three callers (`get_caller_counts_batch`, `get_callee_counts_batch`) pass hardcoded string literals — so there is no current injection risk. However, the API signature accepts arbitrary `&str` for column names and SQL expressions, so a future caller could pass user-influenced strings. The pattern is also inconsistent with the rest of `store/`: all other dynamic SQL uses only `make_placeholders` for values (which are bound via sqlx parameters). Column-name parameterization is a qualitatively different operation with no parameterized equivalent in SQLite, and mixing it silently with sqlx's bound-parameter pattern makes auditing harder.
- **Suggested fix:** Add a comment documenting that `filter_column`, `group_column`, and `count_expr` must be compile-time constants and must not accept user input. Or use an enum to restrict the valid column names:
  ```rust
  enum CountColumn { CalleeName, CallerName }
  async fn batch_count_query(&self, filter: CountColumn, names: &[&str]) -> Result<...>
  ```

#### CQ-12: `generate_nl_with_call_context` has zero test coverage — new core function for SQ-4
- **Difficulty:** easy
- **Location:** src/nl.rs:274-322
- **Description:** `generate_nl_with_call_context` was added in v1.0.7 (#590) as the key function for the SQ-4 call-graph enriched embeddings feature. It is public API (exported from `lib.rs:119`), called in production via `enrichment_pass`, and governs the embedding quality for all chunks with callers or callees. Despite being central to a major feature, it has zero unit tests in `nl.rs`. The test module (line 914) covers `tokenize_identifier`, `normalize_for_fts`, `extract_params_nl`, `extract_return_nl`, `generate_nl_description`, `parse_jsdoc_tags`, and `strip_markdown_noise` — but not `generate_nl_with_call_context` or its IDF-filtering behavior.
- **Suggested fix:** Add unit tests covering:
  1. Caller names appended correctly: `ctx.callers = ["foo", "bar"]` → NL includes "Called by: foo, bar"
  2. Callee IDF filtering: a callee with `freq >= 0.10` is excluded; one with `freq < 0.10` is included
  3. `max_callers`/`max_callees` truncation: more callers than limit → only first N appear
  4. Empty caller/callee → returns base NL unchanged
  5. Both empty → `extras.is_empty()` branch returns without trailing ". "

## Test Coverage

#### TC-B1: `update_embeddings_batch` and `chunks_paged` have zero unit tests
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:79-111 (`update_embeddings_batch`), src/store/chunks.rs:983-1014 (`chunks_paged`)
- **Description:** Both functions were added in v1.0.7 as core infrastructure for the SQ-4 enrichment pass. `update_embeddings_batch` is responsible for writing all enriched embeddings to the store; `chunks_paged` is the sole pagination mechanism for iterating chunks during enrichment. Neither has a single unit test in `store/chunks.rs`. The test module (line 1460+) covers `upsert_chunks_batch`, `embedding_batches`, `all_chunk_identities_filtered`, `get_chunks_by_origin`, and stale-file detection — but not the two new functions. Key behaviors that should be tested:
  - `update_embeddings_batch`: writes embedding bytes correctly, skips unknown IDs without error, returns correct count of actually-updated rows, empty input returns 0.
  - `chunks_paged`: returns chunks after cursor, advances cursor correctly, terminates when empty, handles `after_rowid = 0` (first page), handles store with exactly one chunk.
- **Suggested fix:** Add tests for both. Minimal test for `update_embeddings_batch`: insert a chunk, call `update_embeddings_batch` with a new embedding, re-fetch and assert the embedding changed. Minimal test for `chunks_paged`: insert 3 chunks, call with `after_rowid=0, limit=2`, assert 2 returned and cursor advances; call again with the returned cursor, assert the remaining 1 is returned.

#### TC-B2: `callee_caller_counts` has zero unit tests
- **Difficulty:** easy
- **Location:** src/store/calls.rs:1242-1261
- **Description:** `callee_caller_counts()` is the source of truth for the IDF filter in `enrichment_pass`. It returns `(callee_name, distinct_caller_count)` pairs used to compute the frequency ratio that decides whether a callee appears in the enriched NL. Despite being the critical input to the IDF suppression logic, the function has no unit test. The calls.rs test module covers `get_caller_counts_batch`, `get_callee_counts_batch`, `find_shared_callers/callees`, dead code detection, and confidence scoring — but not `callee_caller_counts`. Without a test, a SQL change (e.g., removing `DISTINCT`) would go undetected until enrichment quality degraded.
- **Suggested fix:** Add tests using the existing `seed_call_graph` fixture. In the seeded graph (A→B, A→C, B→C, D→B), `callee_caller_counts` should return: `func_b → 2` (called by func_a and func_d), `func_c → 2` (called by func_a and func_b). Test the empty store case (returns empty vec) and verify the DISTINCT behavior by having the same caller call the same callee twice (should still count as 1).

#### TC-B3: `enrichment_pass` has no integration test — the SQ-4 feature path is untested end-to-end
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:911-1036, tests/ (no matching test file)
- **Description:** The SQ-4 enrichment pass is exercised only through the CLI (`cqs index`), which is covered by smoke tests. No integration test calls `enrichment_pass` directly with a seeded store and verifies that chunk embeddings are actually updated. The function is 125 lines with multiple failure points (stats, callee counts, identity load, callers/callees batch fetch, pagination loop, flush). No test covers: (a) a chunk with callers gets its embedding updated; (b) a leaf node (no callers, no callees) is correctly skipped; (c) the IDF filter correctly suppresses high-frequency callees in the NL; (d) the function returns 0 on an empty store. The existing `tests/store_calls_test.rs` tests the call graph store functions but not the enrichment pipeline.
- **Suggested fix:** Add a test in `tests/` (or in a new `enrichment_test.rs`) that: creates a Store, inserts chunks with embeddings, seeds a call graph, calls `enrichment_pass` with a mock/CPU embedder, and asserts that the embedding for a chunk with callers has changed (not equal to original). Use `#[ignore]` if the embedder requires a downloaded model, or use a mock that returns deterministic vectors.

## Robustness

#### RB-B1: Name-based callers/callees lookup merges call context for same-named functions across files
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:940-948, src/store/calls.rs:625-667
- **Description:** `enrichment_pass` looks up callers and callees by chunk name (`ci.name`), not by chunk ID. `get_callers_full_batch` groups by `callee_name` in `function_calls` — so all callers of *any* function named `parse` in the index are merged into one entry. When the loop later does `callers_map.get(&cs.name)` (line 980), a chunk named `parse` in `src/config.rs` gets the same callers list as a chunk named `parse` in `src/network.rs`, even though they are different functions. For common method names (`new`, `build`, `parse`, `from`, `into`), this produces spurious enrichment NL like "Called by: deserialize_config, connect_to_db, ..." for functions that are actually independent. This degrades embedding quality for functions with common names, which are often the most performance-critical ones (constructors, builders, converters).
- **Suggested fix:** Two options. Option A (lightweight): post-filter the callers list by file — when processing `cs` with `cs.file = "src/config.rs"`, keep only CallerInfo entries where `caller.file == cs.file` (same-file callers are unambiguous). Option B (correct): look up callers/callees by chunk ID, not name. The `function_calls` table uses `caller_name`/`callee_name` (names, not IDs), so this would require a schema change to store chunk IDs. Document the current limitation in a code comment so future maintainers understand the approximation.

#### RB-B2: `enrichment_pass` loads the entire call graph into memory before processing any chunks
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:936-948
- **Description:** The enrichment pass makes three full-scan memory allocations before paging through chunks: `all_chunk_identities()` (all chunk names, IDs, and metadata), `get_callers_full_batch(&all_names)` (all caller lists for every name), and `get_callees_full_batch(&all_names)` (all callee lists for every name). For a large codebase (50K chunks, 200K call edges), this could hold ~200MB in memory simultaneously: all identities (~10MB), the callers HashMap (~50-100MB for Vec<CallerInfo> with file paths), and the callees HashMap (~50-100MB). The `chunks_paged` loop then iterates through the same data in pages — the page-based approach doesn't reduce peak memory because the full call graph is already loaded. The pagination only affects the `content`-heavy `ChunkSummary` structs.
- **Suggested fix:** The current design is reasonable for codebases up to ~20K chunks. For larger codebases, the fix is to process chunks in pages and fetch callers/callees per page instead of loading everything up front. This requires one `get_callers_full_batch` call per page (500 chunks × N pages) instead of one global call. Add a comment documenting the memory model: "Loads full call graph into memory. For indexes with >50K chunks, consider switching to per-page caller/callee lookup."

## Algorithm Correctness

#### AC-B1: IDF filter threshold comment says `>10%` but code uses `>=10%` — off-by-one at boundary
- **Difficulty:** easy
- **Location:** src/nl.rs:299, 307
- **Description:** Line 299 comment: "A callee appearing in >10% of chunks is likely a utility." Line 307 predicate: `freq >= 0.10` (greater-than-or-equal). The two disagree at the exact boundary: a callee appearing in exactly 10% of chunks is suppressed by the code but would be kept by the comment's description. Additionally, the unit test (line 1471) uses `0.15` for the filtered case and `0.05`/`0.02` for the kept cases — it never tests the exact boundary value `0.10`, so neither the `>` nor `>=` interpretation is verified. For small codebases (10 chunks), a callee called by exactly 1 function hits the threshold (`1/10 = 0.10`) and is suppressed as a "utility", even though 1-caller callees are typically domain-specific and valuable to include.
- **Suggested fix:** Pick one interpretation and make code and comment agree. `>=` (current code) is more conservative — suppresses more callees. If that's intended, update the comment to "appearing in >=10%" or ">= 10% of chunks". Add a boundary test: insert a callee with exactly `freq = 0.10` and assert it is filtered.

#### AC-B2: `page_size` is a `let` local rather than a named constant — inconsistent with `ENRICH_EMBED_BATCH`
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:934
- **Description:** The enrichment pass uses two magic numbers that control its memory/throughput tradeoff. `ENRICH_EMBED_BATCH = 64` is declared as a named `const` at line 964. `page_size = 500` is declared as a plain `let` binding at line 934 — unnamed, untyped, and undocumented. Both serve the same extensibility role (tunable batch sizes), but only one is consistently named. The `page_size` value also has no comment explaining why 500 was chosen (e.g., "500 * ChunkSummary ~= Xmb per page" or "balances SQLite round-trips with memory"). A reader has no basis for choosing a different value.
- **Suggested fix:** Promote to a named constant adjacent to `ENRICH_EMBED_BATCH`:
  ```rust
  /// Chunks fetched per page during enrichment iteration.
  /// Balances SQLite round-trips vs. memory per batch.
  const ENRICH_PAGE_SIZE: usize = 500;
  const ENRICH_EMBED_BATCH: usize = 64;
  ```

## Extensibility

#### EX-B1: Four enrichment-pass tuning values are hardcoded with no config path
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:934, 964, 1005-1006; src/nl.rs:307
- **Description:** The enrichment pass behavior is controlled by four magic values with no configuration surface:
  1. `page_size = 500` — chunks per page during iteration (pipeline.rs:934)
  2. `ENRICH_EMBED_BATCH = 64` — embeddings per flush (pipeline.rs:964)
  3. `5, 5` — max_callers and max_callees passed to `generate_nl_with_call_context` (pipeline.rs:1005-1006)
  4. `0.10` — IDF suppression threshold (nl.rs:307)

  These values govern embedding quality vs. token budget tradeoff. Users with large codebases may want more callers/callees in the NL; users with small codebases may want a lower IDF threshold to avoid over-suppression. There is no way to tune these without recompiling. By contrast, many other algorithmic parameters in cqs are already in `Config` (e.g., `ef_search`, `batch_size`, `max_depth`, `note_only`). The four values above follow no such pattern.
- **Suggested fix:** Add an `[enrichment]` section to `Config` (or extend the existing `[index]` section):
  ```toml
  [enrichment]
  max_callers = 5
  max_callees = 5
  idf_threshold = 0.10
  embed_batch_size = 64
  ```
  Pass through `enrichment_pass` and `generate_nl_with_call_context`. This does not require all four to be configurable immediately — even exposing `max_callers`/`max_callees` would address the most user-visible tradeoff.

## Platform Behavior

No new platform-behavior findings specific to the SQ-4 code. The enrichment pass operates only on chunk IDs (strings) and embeddings (bytes); paths are already normalized to forward slashes before storage. `ChunkRow::from_row` and the `callers_map`/`callees_map` use no OS-specific APIs. The existing platform findings from prior batches (PB-1 through PB-7) are unrelated to the new code.

---

## Batch 3: Security

No injection, path traversal, secrets, or access-control findings in the new SQ-4 code. All new SQL statements in `callee_document_frequencies` and `chunks_paged` use fully static query strings with sqlx bound parameters for values — no user input reaches them. `extract_file_context` operates on paths already stored in the database (never on raw user input) and performs only string slicing — no filesystem interaction. The enrichment pass's `embed_batch` and `flush_enrichment_batch` process only chunk IDs (UUIDs from the store) and NL strings generated internally; no user-controlled input enters those paths.

## Batch 3: Data Safety

#### DS-B1: `update_embeddings_batch` transaction does not protect against partial write on connection failure mid-loop
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:97-113
- **Description:** `update_embeddings_batch` opens a single SQLite transaction and runs one `UPDATE` per embedding inside it. If the connection is lost or the process is killed between `tx.begin()` and `tx.commit()`, SQLite rolls back the entire transaction — correct. However, if `execute(&mut *tx).await?` returns an `Err` for a specific row (e.g., SQLITE_BUSY or a constraint error), the function propagates the error via `?` without explicitly calling `tx.rollback()`. `sqlx` will rollback the transaction when `tx` is dropped, so data integrity is preserved. The issue is operational: the `updated` counter at the point of failure understates progress — the caller in `flush_enrichment_batch` gets an error and aborts the entire enrichment pass. Chunks processed before the failure in this batch had their embedding updates rolled back (good), but chunks processed in *previous* successfully-committed batches are permanently enriched while remaining chunks are not. On re-run, the enrichment pass starts from scratch (cursor = 0), re-processing already-enriched chunks unnecessarily.
- **Suggested fix:** Document the non-idempotent re-run behavior in `enrichment_pass`: "If interrupted, previously enriched chunks will be re-enriched on re-run." Optionally, add a `source_mtime`-style column or a boolean flag `enriched: bool` to `chunks` so the pass can skip already-enriched chunks on retry. This avoids re-embedding thousands of chunks after an interrupted run.

#### DS-B2: `chunks_paged` cursor can skip rows if `rowid` gaps exist after compaction
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:987-1018
- **Description:** `chunks_paged` iterates by SQLite `rowid` using `WHERE rowid > ?1`. SQLite `rowid` values are not dense — rows deleted by `prune_missing` or `delete_by_origin` leave permanent gaps. This is not a bug in normal operation (the cursor still advances past gaps correctly), but the comment "Loads chunks without loading everything into memory" implies the pagination reduces per-page data. For a codebase where many chunks were pruned, `ENRICHMENT_PAGE_SIZE = 500` may return far fewer than 500 chunks per page (e.g., 50 chunks for a rowid range covering 500 rowid slots with 450 gaps). The enrichment pass does not detect this case — it just processes fewer chunks per iteration without adjusting. This is a functional non-issue but creates misleading performance expectations: a codebase with heavy pruning history may take many more page iterations than expected.
- **Suggested fix:** Document the gap behavior in `chunks_paged`: "Page size is an upper bound; returned count may be lower after deletions leave rowid gaps. The cursor always advances past processed rows." No code change needed — the behavior is correct.

#### DS-B3: `name_file_count` HashMap key uses cloned String when identities are already live
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:942-944
- **Description:** The enrichment pass builds `name_file_count: HashMap<String, usize>` by cloning `ci.name` for each entry (line 944). `identities` is still alive (used on line 946 for `all_names`), so the name strings already exist in memory. `name_file_count` holds a second copy of each unique name as an owned key. For a 50K-chunk codebase with 20K unique names averaging 15 chars, this is ~300KB of avoidable duplication. More importantly, `name_file_count` is queried at line 1000 via `name_file_count.get(&cs.name)`, using `cs.name` (a `String` from `ChunkSummary`), which is correct. The fix is cosmetic — the allocation is small — but it illustrates an unnecessary clone in a hot loop.
- **Suggested fix:** Use `entry` with a borrowed key by building the map from identities as `&str` slices (possible if `HashMap<&str, usize>` lifetime is tied to `identities`), or simply accept the ~300KB overhead and add a comment. Not worth a refactor.

## Batch 3: Performance

#### PERF-B1: `chunks_paged` fetches full chunk content (including `content`, `doc`, `signature`) for all chunks, including those skipped as leaf nodes or ambiguous names
- **Difficulty:** medium
- **Location:** src/store/chunks.rs:994-997, src/cli/pipeline.rs:992-1001
- **Description:** `chunks_paged` selects all columns including `content`, `doc`, and `signature` — the three largest per-chunk fields. For a typical Rust function, `content` alone can be 500–2000 bytes. In the enrichment pass, chunks are fetched in pages of 500. But the majority of chunks are skipped before content is used:
  1. Leaf nodes (no callers, no callees) are skipped at line 992-994 — these are typically 40–70% of all chunks.
  2. Ambiguous names are skipped at line 1000-1001 (functions named `new`, `parse`, `build`, etc.).
  Only the remaining ~20–40% of chunks actually need full content for `generate_nl_with_call_context` (which calls `generate_nl_description`, which reads `content`, `doc`, `signature`). The other 60–80% load all that data from SQLite and then discard it immediately.
  For 50K chunks at 1KB average content: 500 chunks × 1KB = 500KB per page loaded; 300–400KB immediately discarded.
- **Suggested fix:** Add a lightweight page variant that fetches only `(rowid, id, name, chunk_type, language, parent_id)` for the filtering phase, then fetch full content only for chunks that pass both the leaf-node and ambiguity filters via `fetch_chunks_by_ids_async`. This reduces I/O proportionally to the skip rate. The enrichment pass would then be a two-step: filter page → fetch survivors → generate NL → embed.

#### PERF-B2: `callee_caller_counts` full-table scan runs once per `cqs index` on every re-index, including incremental runs
- **Difficulty:** easy
- **Location:** src/store/calls.rs:1242-1261, src/cli/commands/index.rs:136
- **Description:** `enrichment_pass` is called unconditionally whenever `stats.total_calls > 0` (index.rs:136). For incremental re-indexes (where only 1–5 files changed), the enrichment pass still re-embeds all chunks with callers/callees, even if none of their callers or callees changed. The `callee_caller_counts` query scans the entire `function_calls` table, and `get_callers_full_batch` and `get_callees_full_batch` scan all edges. For a 200K-call codebase, this means three full table scans plus re-embedding potentially thousands of chunks on every file save (if triggered by `cqs watch`, which does not currently call `enrichment_pass` at all — but `cqs index` on a changed file does).
  Currently watch mode does not call `enrichment_pass`, so the cost is only paid on explicit `cqs index` runs. But if watch mode is enhanced to call it in future, this becomes a 200K-row scan on every file change.
- **Suggested fix:** Track which chunks were enriched and skip them on re-index if neither their callers nor callees changed. A lightweight approach: store the enrichment timestamp in the `chunks` table (`enriched_at`), and only re-enrich chunks where `function_calls.updated_at > chunks.enriched_at`. This is a non-trivial schema change. Short-term: add a `--skip-enrichment` flag to `cqs index` for fast incremental runs, letting users opt out of the enrichment pass when they know only non-call-graph files changed.

#### PERF-B3: `generate_nl_description` called twice per enriched chunk — once in the pipeline, once in the enrichment pass
- **Difficulty:** easy
- **Location:** src/nl.rs:281 (inside `generate_nl_with_call_context`), src/cli/pipeline.rs:187
- **Description:** Every chunk with callers/callees has its NL description generated twice: once during the main pipeline pass (pipeline.rs:187, via `generate_nl_description`) when its embedding is first computed, and once during the enrichment pass (nl.rs:281, called by `generate_nl_with_call_context`) when the enriched embedding replaces it. The enriched pass rebuilds the full base NL — including tokenization, doc-comment parsing, field extraction, and file-context extraction — before appending the caller/callee context. This is CPU-bound work duplicated for every enriched chunk. For 10K enriched chunks at ~5μs per `generate_nl_description` call: ~50ms of wasted work (minor at current scale). The concern is architectural: the enrichment pass doesn't have access to the pre-computed base NL from the pipeline pass (it's not stored in the DB), forcing the recomputation.
- **Suggested fix:** Store the base NL text in a `nl_text` column in `chunks` during the pipeline pass, and read it back during enrichment (appending context without full recomputation). This also enables caching embeddings by NL hash. Alternatively, document that double NL generation is intentional and accepted.

## Batch 3: Resource Management

#### RM-B1: `enrichment_pass` loads three large data structures before processing any chunks — peak memory is 3× higher than necessary for large codebases
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:939-954
- **Description:** Before entering the pagination loop, `enrichment_pass` loads:
  1. `identities`: all `ChunkIdentity` structs from the DB — for 50K chunks, ~5–8MB (id: ~36B UUID, origin: ~60B path, name: ~15B, plus metadata = ~130B per entry × 50K = ~6.5MB).
  2. `callers_map: HashMap<String, Vec<CallerInfo>>`: for 200K call edges, each `CallerInfo` has a `PathBuf` (~60B) + `String` name (~15B) + `u32`. Total ~75B × 200K + HashMap overhead ≈ **~25–40MB**.
  3. `callees_map: HashMap<String, Vec<(String, u32)>>`: similar, ~75B × 200K ≈ **~20–30MB**.
  4. `name_file_count: HashMap<String, usize>`: ~1–2MB of cloned name strings.
  All four live simultaneously until `identities` goes out of scope after line 946 (it doesn't — it lives until the end of `enrichment_pass` because `all_names` borrows from it). Total peak: ~60–80MB for a 50K-chunk/200K-edge codebase.
  The `chunks_paged` loop then adds 500 × `ChunkSummary` (~500KB with content) per page, but this is small compared to the pre-loaded maps.
  For codebases with 200K+ chunks (large monorepos), this could exceed 300–500MB before processing begins. The previously documented finding RB-B2 noted this and suggested per-page lookup; this finding quantifies the memory cost more precisely.
- **Suggested fix:** The `identities` vec is only used to build `name_file_count` and `all_names`. After line 946, only `name_file_count`, `callers_map`, `callees_map` are needed. `identities` cannot be dropped early because `all_names` borrows from it. Fix: collect `all_names` as owned `Vec<String>` rather than `Vec<&str>` — then `identities` can be dropped after line 946, freeing ~6.5MB before the maps are built. Document the remaining memory model in a comment above line 939: "Peak memory: callers_map + callees_map ≈ 50–80MB for 200K edges. For larger codebases, switch to per-page lookup."

#### RM-B2: Enrichment pass creates a fresh `Embedder` instance after the pipeline's embedder has been dropped — doubles model-load time
- **Difficulty:** easy
- **Location:** src/cli/commands/index.rs:142
- **Description:** `cmd_index` creates an `Embedder` for the enrichment pass (line 142) after `run_index_pipeline` has returned and its GPU embedder thread has exited (dropping its `Embedder`). Each `Embedder::new()` call performs lazy ONNX session initialization on first use: `~500ms` init time + ~500MB GPU/CPU memory load. So `cqs index` on a codebase with a non-empty call graph loads the ONNX model **twice** per invocation — once for the pipeline, once for the enrichment pass. On GPU, that's ~1s of pure model init overhead; on CPU, potentially longer.
  The enrichment pass embedder is scoped to `index.rs:142-155` and drops at line 156. The pipeline's embedder drops when its thread is joined at line 866-873 of pipeline.rs. They do not coexist in memory simultaneously (sequential execution), so this is not a double-memory issue. It is a double-init-time issue.
- **Suggested fix:** Pass the pipeline's `Embedder` out of `run_index_pipeline` and into `enrichment_pass`, avoiding the second initialization. Or, refactor `cmd_index` to create one `Embedder` before the pipeline and pass it through both phases. This saves ~500ms per `cqs index` invocation when the call graph is non-empty.

## Red Team — RT-FS: Filesystem Boundary Violations

#### RT-FS-1: `read_context_lines` reads files from index DB paths without boundary validation
- **Severity:** low
- **Location:** `src/cli/display.rs:116-121, 143-148, 316-321, 344-349`
- **Attack vector:** Corrupt or tamper with `.cqs/index.db` to insert a chunk with `file = "../../etc/shadow"`. Then run `cqs search <query> --context 1` where the search returns the poisoned chunk.
- **PoC:** `display_unified_results()` calls `root.join(&r.chunk.file)` at line 116 and passes the result to `read_context_lines()` which does `std::fs::read_to_string(file)` at line 35 with no canonicalize+starts_with check. The `chunk.file` value comes directly from the SQLite DB (`chunks` table, `origin` column). If the DB contains `../../etc/shadow`, the join resolves to an out-of-project path and the file is read and its context lines are printed to stdout.
- **Impact:** File contents from outside the project root are leaked to stdout via context display. Limited to files readable by the current user. Requires DB tampering (the DB is documented as trusted-user, but the stated protection is "Commands cannot read files outside project root" per SECURITY.md line 20).
- **Mitigating factors:** (1) The DB is stored inside `.cqs/` which is user-writable by design — this is a self-attack scenario. (2) The context lines only show a few lines around the chunk's line range, not the whole file. (3) The attacker must already have write access to `.cqs/index.db`. (4) Normal indexing stores only project-relative paths (pipeline.rs line 320).
- **Suggested mitigation:** Add canonicalize+starts_with validation in `read_context_lines()` or at the call sites in `display.rs`, consistent with the protection in `validate_and_read_file()`.

#### RT-FS-2: `resolve_parent_context` reads files from index DB paths without boundary validation
- **Severity:** low
- **Location:** `src/cli/commands/query.rs:652-653`
- **Attack vector:** Same DB tampering as RT-FS-1. Insert a chunk with a traversal path and a `parent_id` that is NOT in the DB (forcing the fallback to file read). Run `cqs search <query> --expand`.
- **PoC:** `resolve_parent_context()` at line 652 does `root.join(&sr.chunk.file)` and passes it to `std::fs::read_to_string()` at line 653. This code path is the fallback when a parent chunk ID is not found in the DB (the "windowed chunk" case). No boundary check is performed. The read content is inserted into `ParentContext` and displayed to the user.
- **Impact:** Same as RT-FS-1 — out-of-project file content leaked to stdout via parent context display.
- **Mitigating factors:** Same as RT-FS-1, plus: this path is only reached for windowed chunks whose parent is missing from the DB, which is an unusual state.
- **Suggested mitigation:** Add canonicalize+starts_with check before the `read_to_string` call at line 652.

#### RT-FS-3: Verified protections — no findings

The following attack surfaces were tested and found to be properly protected:

1. **`cqs read` path traversal (validate_and_read_file):** `dunce::canonicalize()` + `starts_with()` at `src/cli/commands/read.rs:37-43`. Both CLI and batch handlers call through this shared function. Confirmed solid.

2. **`cqs read --focus` path:** Reads `chunk.content` from the DB directly (read.rs line 197), never re-reads the file from disk based on `chunk.file`. No FS boundary issue.

3. **Batch `read` handler:** Delegates to `validate_and_read_file` at `src/cli/batch/handlers.rs:1091`. Same protection as CLI.

4. **`cqs context` command:** Reads only from the index DB (chunk summaries, caller/callee data). Does NOT read source files from disk based on `chunk.file`. No FS boundary issue.

5. **Reference name path traversal (`cqs ref add`):** `validate_ref_name()` at `src/reference.rs:214-231` rejects `/`, `\`, `..`, `.`, null bytes, and leading dots. Called before `ref_path()` constructs any filesystem path. Confirmed solid.

6. **`cqs ref remove` directory deletion:** Uses `canonicalize+starts_with` at `src/cli/commands/reference.rs:240-244` before `remove_dir_all`. Does NOT call `validate_ref_name` but does not need to — the canonicalize check is sufficient. Confirmed solid.

7. **CHM zip-slip containment:** `dunce::canonicalize()` + `starts_with()` on all extracted paths at `src/convert/chm.rs:58-91`. Symlinks skipped. Confirmed solid.

8. **`cqs convert --output`:** Output directory is intentionally unsandboxed (documented in SECURITY.md line 93: "The output path is not sandboxed beyond normal filesystem permissions"). User explicitly provides the output path. Source-overwrite guard exists at `src/convert/mod.rs:251-265`. Not a finding — by design.

9. **Reference symlink rejection:** `load_references()` at `src/reference.rs:47-60` rejects reference paths that are symlinks before opening the DB. Prevents symlink-based redirection of reference index loading.

10. **`cqs blame` file argument injection:** `run_git_log_line_range()` at `src/cli/commands/blame.rs:79-85` rejects file paths starting with `-` and containing `:`. Prevents git argument injection via `chunk.file`.

## Red Team — RT-INJ: Input Injection & Command Injection

#### RT-INJ-1: `CQS_PDF_SCRIPT` env var allows arbitrary Python script execution without path validation
- **Severity:** medium
- **Location:** `src/convert/pdf.rs:57-71`
- **Attack vector:** Set `CQS_PDF_SCRIPT=/tmp/evil.py` via a `.envrc`, `.env`, or shell profile in a cloned repository. When any user runs `cqs convert` on a PDF in that project, the attacker's script runs under `python3`.
- **PoC:** `find_pdf_script()` at line 58 reads `std::env::var("CQS_PDF_SCRIPT")`. If the file exists (line 67), it is returned immediately. The only guard is a non-`.py` extension warning (line 61-65), which is a log message, not a rejection. The path is then passed to `Command::new(&python).arg("--").arg(&script).arg(path)` at line 21-23. While `--` prevents argument injection into the Python interpreter, the script itself runs with full user privileges. A project-local `.envrc` (auto-loaded by direnv) or `.env` (loaded by many tools) can set this variable, causing arbitrary code execution when the user runs `cqs convert` on any PDF. The threat model says "external docs (PDF/CHM) are semi-trusted" but this vector bypasses the document entirely -- the code runs even if the PDF is benign.
- **Impact:** Arbitrary code execution under the user's identity. The script receives the PDF path as argv[1] and can exfiltrate data, modify files, or establish persistence.
- **Suggested mitigation:** (1) Reject non-`.py` extensions (hard error, not warning). (2) Require the script to be inside the project root or a well-known system path. (3) Validate the script path is not a symlink. (4) Document the security implications of `CQS_PDF_SCRIPT` in SECURITY.md.

#### RT-INJ-2: Batch `read_line` allocates unboundedly before the 1MB limit check
- **Severity:** low
- **Location:** `src/cli/batch/mod.rs:392-418`
- **Attack vector:** Pipe a single line of 2GB+ without any newline character into `cqs batch` via stdin.
- **PoC:** `reader.read_line(&mut line)` at line 398 reads until `\n` or EOF. If stdin provides 2GB of data with no newline, `read_line` allocates the entire buffer before returning. The 1MB check at line 410 (`if line.len() > MAX_BATCH_LINE_LEN`) happens AFTER the allocation. The comment at line 392-394 acknowledges this: "read_line allocates incrementally (8KB chunks) but we enforce MAX_BATCH_LINE_LEN before processing." However, the stated protection #5 in the threat model is "1MB batch line limit" -- this implies the allocation is bounded, when it is not. The `line.clear()` at line 397 retains the capacity (String reuse), so subsequent lines don't re-allocate, but the peak allocation is unbounded. This bypasses the stated "1MB batch line limit" protection for a single malicious line.
- **Impact:** OOM kill of the `cqs batch` process. No data corruption. Requires the attacker to control stdin, which is typically the calling AI agent or a pipe.
- **Mitigating factors:** (1) The code acknowledges this in SEC-1 comments. (2) The process is killed by OOM killer, not exploited further. (3) String reuse prevents repeated allocations.
- **Suggested mitigation:** Use `BufReader::read_until` with a custom implementation that stops reading after 1MB and discards the remainder of the line. Or use `take(MAX_BATCH_LINE_LEN + 1)` to wrap the reader before `read_line`.

#### RT-INJ-3: Verified protections -- no findings

The following injection surfaces were tested and found to be properly protected:

1. **FTS5 injection via `normalize_for_fts()`:** `src/nl.rs:123-170` strips all non-alphanumeric/underscore characters. Only lowercased identifier tokens joined by spaces pass through. FTS5 operators (`OR`, `AND`, `NOT`, `NEAR`), quotes, parentheses, `*`, `+`, `-`, `^`, `:` are all eliminated. Confirmed solid.

2. **FTS5 injection via `sanitize_fts_query()`:** `src/store/mod.rs:148-166` provides defense-in-depth. Independently strips `"`, `*`, `(`, `)`, `+`, `-`, `^`, `:` from characters and filters `OR`, `AND`, `NOT`, `NEAR` as whole words. The double-pass pattern (`normalize_for_fts` then `sanitize_fts_query`) means either layer alone prevents injection. Confirmed solid.

3. **FTS5 injection via `search_by_name` format! construction:** `src/store/mod.rs:643-650` uses `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)` which looks dangerous, but `sanitize_fts_query` at line 633 independently strips double quotes, AND there is a runtime `contains('"')` guard at line 647-649 that returns empty results if a double quote somehow survives. The `debug_assert!` at line 643-645 catches this in development. Confirmed solid (triple-layered protection).

4. **FTS5 injection via `search_by_names_batch`:** `src/store/chunks.rs:910-928` uses the same `sanitize_fts_query` + `contains('"')` guard + `debug_assert` pattern as `search_by_name`. Same triple protection. Confirmed solid.

5. **TOML injection via notes text:** `src/note.rs:240` uses `toml::to_string_pretty(&file)` which serializes through serde. The `toml` crate v1.x properly escapes all TOML metacharacters in string values: newlines become `\n`, backslashes become `\\`, double quotes become `\"`. A note containing `"""`, `[[note]]`, or `\n[note]` in its text will be properly escaped by the serializer. Round-trip test: `NoteFile` derives both `Deserialize` and `Serialize` (line 56-57), and the fuzz tests at line 524-557 cover arbitrary input without panics. Confirmed solid.

6. **SQL injection via parameterized queries:** All SQL in `src/store/`, `src/search.rs` uses `sqlx::query().bind()` parameterization. The only `format!` SQL constructions create placeholder strings (`?1`, `?2`) or static column/table names -- never user data. Confirmed solid.

7. **Batch pipeline boundary bypass:** `src/cli/batch/pipeline.rs:135-148` splits on standalone `|` token. Since `shell_words::split` handles quoting, a `|` inside quotes becomes part of a token and is NOT treated as a pipe separator. Attempting `search "foo | bar"` correctly treats `foo | bar` as a single quoted argument, not a pipeline. Confirmed solid.

8. **`shell_words::split` with unbalanced quotes:** `src/cli/batch/mod.rs:433` catches `shell_words::split` parse errors (returned as `Err` for unclosed quotes) and reports them as JSON errors. Null bytes in input survive `shell_words::split` but are passed to clap, which rejects them as invalid arguments. Confirmed solid.

9. **`--ref` name validation on search path:** `src/cli/commands/query.rs:109` passes the `--ref` name to `find_reference()` at `src/cli/commands/resolve.rs:26-38`, which matches against names loaded from `.cqs.toml` config. No `validate_ref_name` call is needed because: (a) the name is matched by string equality against config entries, not used to construct filesystem paths directly, and (b) `load_references` at `src/reference.rs:42-62` rejects symlink paths and constructs DB paths from config, not from the `--ref` CLI value. Confirmed solid.

10. **`--path` glob ReDoS:** `src/store/helpers.rs:580-612` validates path patterns: max 500 chars, max 10 brace nesting depth, control character rejection. The `globset` crate compiles globs to DFA (not regex), so catastrophic backtracking is not possible. `src/search.rs:324-331` compiles globs once per search, not per chunk. Confirmed solid.

## Red Team — RT-RES: Adversarial Robustness

#### RT-RES-1: Chat mode has no input length limit — OOM via large paste
- **Severity:** low
- **Location:** `src/cli/chat.rs:123-141`
- **Attack vector:** Paste an extremely long line (>1GB) into the `cqs chat` REPL.
- **PoC:** `editor.readline("cqs> ")` at line 123 uses rustyline's internal line buffer. Unlike batch mode, there is no `MAX_BATCH_LINE_LEN` check anywhere in the chat path. The line goes directly to `shell_words::split()` at line 141, then to `BatchInput::try_parse_from()` at line 160 and command dispatch. A sufficiently long line could cause OOM during tokenization or during embedding if it reaches the embedder. Batch mode has the 1MB `MAX_BATCH_LINE_LEN` guard (batch/mod.rs:410), but chat mode skips this entirely.
- **Impact:** OOM crash of the `cqs chat` process. Lower severity because chat mode is interactive (human-driven), making adversarial >1GB pastes unlikely in practice. However, programmatic use of chat mode (piping into it) would be vulnerable.
- **Suggested mitigation:** Add a line length check after `editor.readline()` returns, matching batch mode's 1MB limit. Reject with an error message before passing to `shell_words::split`.

#### RT-RES-2: `node_letter` in trace.rs uses truncating `as u8` cast
- **Severity:** low
- **Location:** `src/cli/commands/trace.rs:182-186`
- **Attack vector:** `cqs trace A Z --max-depth 50` where the BFS path visits >256 nodes (requires a deep call graph).
- **PoC:** `node_letter(i)` at line 183 does `((b'A' + i as u8) as char).to_string()` for `i < 26`. For `i >= 26`, line 185 does `((b'A' + (i % 26) as u8) as char)` which is safe since `i % 26` is 0-25. However, the `i < 26` branch casts `i` directly to `u8` -- this is also safe since the branch guard ensures `i < 26`. The real issue is that `i` could exceed 255 in the `i >= 26` branch if `i / 26` exceeds 255, but `i / 26` is used as a Display integer, not cast to u8. **Currently safe** but fragile. The equivalent function in `src/impact/format.rs:200-209` uses a robust spreadsheet-style implementation that handles arbitrary indices without any `as u8` casts.
- **Impact:** No crash or incorrect behavior with current `max_depth` limit of 50. Would produce cosmetically wrong Mermaid node IDs if path length exceeded 256+26 = 282 nodes (unreachable with current limits).
- **Suggested mitigation:** Replace trace.rs `node_letter` with the robust `impact/format.rs` implementation or extract a shared helper.

#### RT-RES-3: Pipeline fan-out is correctly bounded — verified safe
- **Severity:** informational
- **Location:** `src/cli/batch/pipeline.rs:12, 232-240, 293-305`
- **Attack vector:** `search "common_pattern" | callers | callers | callers` in batch mode.
- **PoC:** `PIPELINE_FAN_OUT_LIMIT` (line 12) is 50 per stage. Stage 0 runs 1 command. Each subsequent stage extracts up to 50 names from the previous result and dispatches 50 commands. The intermediate merge at lines 293-305 also enforces the 50-name cap via `if merged_names.len() >= PIPELINE_FAN_OUT_LIMIT { break 'merge; }`. Total dispatches for N stages: 1 + 50*(N-1). For a 4-stage pipeline, that's 151 dispatches — linear, not exponential. The deduplication via `HashSet` at line 295 further reduces fan-out in practice. **Verified protection — no vulnerability.**
- **Impact:** None.
- **Suggested mitigation:** None needed.

#### RT-RES-4: BFS gather depth clamped and node-capped — verified safe
- **Severity:** informational
- **Location:** `src/cli/batch/handlers.rs:370-374`, `src/gather.rs:23, 200`
- **Attack vector:** `gather "query" --expand 999999` in batch mode.
- **PoC:** The batch gather handler at line 372 clamps: `expand_depth: expand.clamp(0, 5)`. The task module uses `TASK_GATHER_DEPTH = 2` and `TASK_GATHER_MAX_NODES = 100` (task.rs:19-22). Even without the depth clamp, `bfs_expand()` in gather.rs enforces `opts.max_expanded_nodes` (default 200) at lines 200-201 and 209-210, breaking out of the BFS loop once the node count is reached. **Two independent caps — depth AND node count — prevent unbounded expansion.**
- **Impact:** None.
- **Suggested mitigation:** None needed.

#### RT-RES-5: Graph cycle handling in all BFS traversals — verified safe
- **Severity:** informational
- **Location:** `src/gather.rs:189`, `src/impact/bfs.rs:16`, `src/cli/commands/trace.rs:203`, `src/cli/batch/handlers.rs:494`
- **Attack vector:** Index a codebase with mutual recursion: `fn a() { b(); }` and `fn b() { a(); }`.
- **PoC:** Every BFS implementation in the codebase uses a visited set that prevents re-processing. Specifically: (1) `bfs_expand()` uses `visited: HashSet<String>` (gather.rs:189) and checks `!visited.contains(&neighbor)` at line 213, (2) `reverse_bfs()` uses `ancestors: HashMap<String, usize>` (bfs.rs:16) and checks `!ancestors.contains_key(caller)` at line 27, (3) `bfs_shortest_path()` uses `visited: HashMap<String, String>` (trace.rs:203) and checks `!visited.contains_key(callee)` at line 229, (4) batch test-map handler uses `ancestors: HashMap` (handlers.rs:494) and checks `!ancestors.contains_key(caller)` at line 505. **All four BFS implementations are cycle-safe.** Additionally, `reverse_bfs_multi` has stale-entry protection (bfs.rs:63) that prevents incorrect depth propagation.
- **Impact:** None. Verified that mutual recursion cannot cause infinite traversal.
- **Suggested mitigation:** None needed.

#### RT-RES-6: `--tokens` accepts `usize::MAX` without upper bound — benign
- **Severity:** informational
- **Location:** `src/cli/mod.rs:154-161`
- **Attack vector:** `cqs --tokens 18446744073709551615 "query"`
- **PoC:** `parse_nonzero_usize()` at line 155 only rejects 0. A value like `usize::MAX` is accepted. The token budgeting code uses this as a greedy knapsack ceiling — with `usize::MAX`, everything is packed, equivalent to no budget. No OOM because the budget only limits already-loaded results (it doesn't allocate memory proportional to the budget). This is functionally equivalent to omitting `--tokens`.
- **Impact:** None. No crash, no OOM.
- **Suggested mitigation:** Optional: add a reasonable upper bound (e.g., 10,000,000) to catch typos, but this is cosmetic.

#### RT-RES-7: HNSW corrupted file handling — checksums + size limits verified
- **Severity:** informational
- **Location:** `src/hnsw/persist.rs:54-104, 347-391`
- **Attack vector:** Replace `.cqs/index.hnsw.graph` with a 2GB file of random bytes.
- **PoC:** `HnswIndex::load()` enforces three defenses: (1) `verify_hnsw_checksums()` at line 345 checks blake3 hashes before loading, rejecting corrupted files; (2) file size limits (graph <=500MB at line 367, data <=1GB at line 368, ID map <=500MB at line 348) prevent OOM from oversized files; (3) ID map count validation at lines 419-427 verifies the deserialized vector count matches the HNSW graph. A tampered file will fail checksum or size validation before reaching the bincode deserializer. The only attack surface for bincode deserialization is if an attacker modifies both files AND matching checksums — documented in persist.rs:47-51 as outside the threat model. **Verified safe.**
- **Impact:** None. Corrupted files produce clean error messages.
- **Suggested mitigation:** None needed.

#### RT-RES-8: `Embedder::split_into_windows` overlap validation prevents exponential window count
- **Severity:** informational
- **Location:** `src/embedder.rs:339-357`
- **Attack vector:** Call `split_into_windows(text, 100, 99)` — overlap nearly equal to max_tokens.
- **PoC:** Validation at line 352 rejects `overlap >= max_tokens / 2`. With `max_tokens=100`, any `overlap >= 50` fails. This ensures `step = max_tokens - overlap > max_tokens/2`, guaranteeing linear window count: `O(n / step)` where `step > max_tokens/2`. The `max_tokens == 0` case returns empty vec at line 346-347. **Verified safe.**
- **Impact:** None.
- **Suggested mitigation:** None needed.

#### RT-RES-9: Watch mode pending file set bounded — verified safe
- **Severity:** informational
- **Location:** `src/cli/watch.rs:41, 141, 289`
- **Attack vector:** Rapidly create >10,000 files while `cqs watch` runs.
- **PoC:** `MAX_PENDING_FILES = 10_000` (line 41). `collect_events()` at line 289 checks `pending_files.len() < MAX_PENDING_FILES` before insertion. The `last_indexed_mtime` HashMap is bounded by comment ("pruned when >10k entries"). Files exceeding the pending limit are silently dropped and picked up on the next cycle. **Verified safe against event storm attacks.**
- **Impact:** None.
- **Suggested mitigation:** None needed.

#### RT-RES-10: Batch `OnceLock` caches are fixed-size snapshots — no unbounded growth
- **Severity:** informational
- **Location:** `src/cli/batch/mod.rs:74-96`
- **Attack vector:** Run thousands of commands in a single `cqs batch` session.
- **PoC:** `BatchContext` caches (call_graph, test_chunks, file_set, notes_cache, audit_state, config) are initialized once via `OnceLock` and never grow thereafter. They are snapshots of the index state at session start. ONNX sessions (embedder, reranker) are cleared after 5 minutes idle (lines 99-124). The only potentially growing cache is `refs: RefCell<HashMap<String, ReferenceIndex>>`, which grows by one entry per distinct `--ref` value used. In practice, projects have 0-3 references. **Memory is bounded by codebase size, not command count.**
- **Impact:** None.
- **Suggested mitigation:** None needed.

#### RT-RES-11: Empty query string handled cleanly — no panic
- **Severity:** informational
- **Location:** `src/embedder.rs:420-424`, `src/store/mod.rs:633-636`
- **Attack vector:** `cqs "" --json` or batch `search ""`
- **PoC:** `embed_query()` trims input and returns `Err(EmbedderError::EmptyQuery)` for empty strings. `sanitize_fts_query()` returns empty string for empty/whitespace input, and `search_by_name()` returns `Ok(vec![])` for empty normalized queries. Both paths produce clean error messages. **Verified safe.**
- **Impact:** None.
- **Suggested mitigation:** None needed.

#### RT-RES-12: `validate_finite_f32` coverage — clap rejects NaN/Infinity strings
- **Severity:** informational
- **Location:** `src/cli/mod.rs:163-170`, CLI threshold parsing
- **Attack vector:** `cqs --threshold NaN "query"` or `cqs --threshold inf "query"`
- **PoC:** `validate_finite_f32()` is called in batch handlers (handlers.rs:305, 1193-1194) and command modules (similar.rs:44, drift.rs:19-20), but NOT in the main CLI search dispatch path. However, clap's `f32` value parser rejects `NaN`, `nan`, `inf`, `Infinity`, `-inf` etc. as invalid float strings — they never reach `validate_finite_f32`. A NaN could theoretically reach the threshold via the batch handler if a custom parser accepted it, but the batch handler at handlers.rs:305 calls `validate_finite_f32(threshold, "threshold")?` before use. **All paths verified safe.**
- **Impact:** None.
- **Suggested mitigation:** Add `validate_finite_f32` to the CLI search dispatch for defense-in-depth, though it is not strictly needed.

#### RT-RES-13: `store/helpers.rs::build_placeholders` — `as u8` casts verified safe
- **Severity:** informational
- **Location:** `src/store/helpers.rs:748-756`
- **Attack vector:** Call `search_by_names_batch` with >255 names.
- **PoC:** For `i < 10`, `i as u8` ranges 1-9 (safe). For `10 <= i < 100`, `i / 10` ranges 1-9 and `i % 10` ranges 0-9, both safe as u8. For `i >= 100`, the code falls through to `write!(s, "{}", i)` at line 755 which handles arbitrary values correctly. **Verified safe.**
- **Impact:** None.
- **Suggested mitigation:** None needed.

## Red Team — RT-DATA: Silent Data Corruption

#### RT-DATA-1: `build_batched` HNSW ID / id_map desync when zero-vector embeddings are skipped
- **Severity:** high
- **Location:** `src/hnsw/build.rs:148-167`
- **Attack vector:** A batch containing one or more zero-vector embeddings (norm_sq == 0.0) triggers the `continue` at line 162, skipping the id_map push but NOT adjusting the `base_idx + i` value used as the HNSW internal ID for subsequent items in the same batch. This causes a mismatch between the HNSW graph's internal IDs and the id_map positions.
- **PoC:** Consider a batch of 3 embeddings: `[("a", valid), ("b", zero_vec), ("c", valid)]`. Before this batch, `id_map.len() == N`, so `base_idx = N`.
  - `i=0`: "a" is valid. `id_map.push("a")` -> id_map[N] = "a". HNSW gets ID `N+0 = N`. Correct.
  - `i=1`: "b" is zero-vector. `continue`. id_map not pushed.
  - `i=2`: "c" is valid. `id_map.push("c")` -> id_map[N+1] = "c". HNSW gets ID `N+2`. But id_map[N+2] does not exist yet.
  When search returns HNSW ID `N+2`, `search()` at search.rs:50 checks `idx < self.id_map.len()`. If the id_map grew to N+3 from a later batch, it returns the WRONG chunk ID (whatever is at position N+2). If not, it logs a warning and drops the result silently.
  The `save()` validation at persist.rs:128-135 compares `hnsw.get_nb_point()` to `id_map.len()`. With zero-vector skips, HNSW has fewer points than the highest assigned ID implies, and the IDs don't form a contiguous sequence matching id_map indices. This is the production code path (`build_batched` is used by `cmd_index` unconditionally per build.rs:39-41).
- **Impact:** When triggered: search returns wrong chunk IDs silently, or drops valid results. The bug is dormant when no zero-vector embeddings exist (common case), but any embedding failure producing a zero vector activates it.
- **Suggested mitigation:** Use a separate counter for inserted items:
  ```rust
  let mut inserted = 0usize;
  for (i, (chunk_id, embedding)) in batch.iter().enumerate() {
      if norm_sq == 0.0 { continue; }
      id_map.push(chunk_id.clone());
      data_for_insert.push((embedding.as_vec(), base_idx + inserted));
      inserted += 1;
  }
  ```

#### RT-DATA-2: Enrichment pass has no idempotency guard -- interrupted re-run produces inconsistent embeddings
- **Severity:** medium
- **Location:** `src/cli/pipeline.rs:911-1049`, `src/store/chunks.rs:83-107`
- **Attack vector:** The enrichment pass iterates all chunks with callers/callees, generates enriched NL descriptions with call context, re-embeds them, and calls `update_embeddings_batch` which does `UPDATE chunks SET embedding = ?1 WHERE id = ?2`. There is no flag marking a chunk as "enriched" vs "base-embedded". If the process is interrupted mid-enrichment, some chunks have enriched embeddings and others have base embeddings, with no way to distinguish them.
- **PoC:** 1) Run `cqs index` on a codebase with 1000 chunks and a non-empty call graph. 2) Kill the process when enrichment shows ~50% progress. 3) The index has ~500 enriched and ~500 base-only chunks. Enriched chunks incorporate caller/callee names in their embedding text, giving them different similarity profiles. 4) A search query matching a non-enriched chunk may rank it lower than an enriched chunk with weaker semantic match. Re-running `cqs index` produces different enrichment when the call graph changes, so the same chunk gets different embeddings across runs.
- **Impact:** Non-reproducible search results. Mixed enrichment state after interruption causes inconsistent ranking. The lack of an enrichment marker makes partial enrichment undetectable.
- **Suggested mitigation:** Store an `enrichment_hash` column alongside the embedding, or add `is_enriched` boolean. Skip chunks whose hash hasn't changed on re-enrichment.

#### RT-DATA-3: Watch mode HNSW orphan accumulation degrades search quality silently
- **Severity:** medium
- **Location:** `src/cli/watch.rs:394-442`
- **Attack vector:** Watch mode inserts new vectors into in-memory HNSW for modified files. Old vectors for replaced chunks remain in the graph (hnsw_rs has no deletion API, acknowledged at watch.rs:396-399). Orphan vectors consume HNSW exploration budget. `search_filtered_with_index` retrieves `limit * 5` candidates; with a high orphan ratio, many point to stale chunk IDs absent from SQLite.
- **PoC:** 1) Start `cqs watch`. 2) Modify the same file 50 times. 3) With 10 chunks/file, after 50 edits the HNSW has 500 orphan vectors plus 10 live ones. 4) `search_by_candidate_ids` queries SQLite for candidates; orphans return nothing, shrinking the effective pool below `limit`. 5) Full rebuild only at `HNSW_REBUILD_THRESHOLD = 100`.
- **Impact:** Gradually degrading search recall during long watch sessions. No error or warning reported. Self-heals after 100 incremental inserts.
- **Suggested mitigation:** Track orphan ratio and trigger early rebuild when it exceeds ~20%. Or reduce `HNSW_REBUILD_THRESHOLD` to 20-30.

#### RT-DATA-4: Notes file lock on FD does not survive atomic rename -- concurrent writers can lose data
- **Severity:** low
- **Location:** `src/note.rs:190-289`
- **Attack vector:** `rewrite_notes_file` acquires an exclusive lock on the notes.toml file descriptor (line 200), reads from it (line 226), writes a temp file, then renames the temp over the original (line 263). On Unix, `rename()` replaces the directory entry to point at a new inode. The exclusive lock is held on the OLD inode's FD. After rename, a concurrent writer that opened the file before the rename holds an FD to the old (now unlinked) inode and reads stale content.
- **PoC:** Writer1 opens notes.toml (inode A), locks, reads. Writer2 opens notes.toml (inode A), blocks on lock. Writer1 renames temp over notes.toml (now inode B), drops lock. Writer2 acquires lock on inode A (unlinked), reads stale content from inode A, applies mutation to stale data, renames -- overwriting inode B. Writer1's changes silently lost.
- **Impact:** Lost note writes under concurrent `cqs notes add` from two processes. Very unlikely in practice.
- **Suggested mitigation:** Use a separate lock file (`notes.toml.lock`) that survives the data file rename.

#### RT-DATA-5: Batch/chat `OnceLock` caches become silently stale when index changes externally
- **Severity:** medium
- **Location:** `src/cli/batch/mod.rs:74-96`
- **Attack vector:** `BatchContext` caches call_graph, test_chunks, notes, file_set, and config in `OnceLock` fields that are never invalidated (lines 82-90, documented at lines 60-73). During a `cqs chat` session, running `cqs index` in another terminal updates SQLite but the chat session's caches remain frozen.
- **PoC:** 1) `cqs chat` in terminal 1. 2) `callers parse_file` returns [A, B]. 3) In terminal 2, add caller C, run `cqs index`. 4) In terminal 1, `callers parse_file` still returns [A, B]. No error, no staleness warning.
- **Impact:** Stale call graph, test mapping, and dead code results in long-running sessions. Documentation acknowledges this (lines 60-73) but users won't see source comments.
- **Suggested mitigation:** Compare SQLite's `updated_at` metadata timestamp against session start value on each command. Emit warning if changed.

#### RT-DATA-6: HNSW save is not atomic with SQLite commit -- crash window causes desync
- **Severity:** medium
- **Location:** `src/cli/watch.rs:410-414`, `src/cli/commands/gc.rs:64-87`
- **Attack vector:** Watch mode commits chunks to SQLite (watch.rs:616) then separately saves HNSW (watch.rs:412). GC prunes SQLite (gc.rs:44-46) then rebuilds HNSW (gc.rs:84). A crash between SQLite commit and HNSW save leaves them inconsistent.
- **PoC:** 1) `cqs watch` running. 2) Modify a file. Watch mode upserts new chunks to SQLite. 3) SIGKILL during `index.save()` at watch.rs:412. 4) New process loads stale HNSW. HNSW has vectors for old chunk IDs absent from SQLite. Search retrieves stale candidates, reducing effective result set.
- **Impact:** After crash: HNSW/SQLite desync causes degraded search results until next full rebuild. No automatic detection.
- **Suggested mitigation:** Store an "HNSW generation" counter in SQLite metadata, incremented on each HNSW save. On load, compare against SQLite value. Mismatch triggers automatic rebuild.

#### RT-DATA-7: Verified protections -- no findings

The following data integrity mechanisms were tested and confirmed working:

1. **PRAGMA quick_check on DB open:** Both `Store::open` (mod.rs:291) and `Store::open_readonly` (mod.rs:369) run `PRAGMA quick_check`. Catches B-tree corruption.

2. **Parameterized SQL:** All queries use `sqlx::query().bind()`. No string interpolation in SQL.

3. **`validate_finite_f32()`:** Used in CLI drift, similar, and batch handlers to reject NaN/Infinity parameters.

4. **Embedding dimension validation:** `embedding_to_bytes()` (helpers.rs:778) validates on write. `embedding_slice()` (helpers.rs:793) validates on read. `check_model_version()` (mod.rs:501) validates model name and dimension on store open.

5. **HNSW checksum verification:** blake3 checksums on save (persist.rs:196-230), verified on load (persist.rs:54-104). Missing checksum file is a hard error.

6. **HNSW id_map / vector count validation:** `save()` (persist.rs:128-135) and `load()` (persist.rs:420-427) verify `hnsw.get_nb_point() == id_map.len()`.

7. **Non-finite HNSW score filtering:** `search()` (search.rs:55-62) checks `score.is_finite()`. `cosine_similarity()` (math.rs:25-28) returns `None` for non-finite. Both sort paths use `total_cmp`.

8. **Notes file locking:** `parse_notes()` acquires shared lock. `rewrite_notes_file()` acquires exclusive lock and reads from locked FD. Correct for single-process use.

9. **Migration atomicity:** `migrate()` (migrations.rs:43-56) wraps all steps in a single transaction with rollback on failure.

10. **RRF sort stability:** `rrf_fuse()` and `search_filtered()` both use `total_cmp` for sorting, handling NaN correctly.
