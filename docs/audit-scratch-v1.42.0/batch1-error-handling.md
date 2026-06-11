## Error Handling

#### EH-V1.36-1: `Embedder::warm` discards embed_query result via `let _ = ... ?;`
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:1113
- **Description:** `let _ = self.embed_query("warmup")?;` propagates the error (good) but the actual `Vec<f32>` result is silently dropped after running the full forward pass. The intent is "warm caches" — fine — but the `let _ =` here is functionally equivalent to `let _: Result<Vec<f32>, _> = self.embed_query("warmup");` if `?` were ever removed in a refactor. More importantly, the Ok-result is meaningful: a returned vector with all-zeros indicates a misconfigured ONNX session that the warm path would not catch. Today `warm()` says "warmed" even if the model produces nonsense.
- **Suggested fix:** Drop the `let _ =` (the function already returns `Result<(), _>`, so just call `self.embed_query("warmup")?;`) and assert the returned vector has length == declared `embedding_dim()` before logging "embedder warmed". Today a misconfigured model that returns shape `[1, 0]` would warm successfully and then fail at first user query.

#### EH-V1.36-2: `train_data` corpus parse path collapses `Err(_)` and panic into one silent skip
- **Difficulty:** easy
- **Location:** src/train_data/mod.rs:419
- **Description:** `Ok(Err(_)) | Err(_) => { corpus_parse_failures += 1; continue; }` lumps together (a) parser returning a real `ParserError` (e.g., grammar load failed, file too large) and (b) a panic caught via `catch_unwind`. Both increment the same counter with no `tracing::warn!` distinguishing which it was — operator can't tell whether the corpus has 5,000 panicking files (a real bug to file) or 5,000 files exceeding the size cap (expected). Compare with the per-commit branch at line 250 which already separates `Ok(Err(e))` (logs `Parse failed`) from `Err(_)` (logs `Parse panicked`).
- **Suggested fix:** Split into two arms identical to the commit-replay branch:
  ```rust
  Ok(Err(e)) => { tracing::debug!(path = %path.display(), error = %e, "Parse failed"); ... }
  Err(_)     => { tracing::warn!(path = %path.display(), "Parse panicked — skipping"); ... }
  ```

#### EH-V1.36-3: `doc_writer` cross-device fallback silently drops backup-restore I/O error
- **Difficulty:** easy
- **Location:** src/doc_writer/rewriter.rs:629
- **Description:** When `fs::write(path, data)` fails inside the cross-device fallback, the code attempts `let _ = std::fs::rename(&backup_path, path);` to restore the original. If that rename ALSO fails (e.g., `path` now half-written and locked, or backup got chmod'd), the user has lost the original file AND received only the original write error in the tracing warn. The backup_path file is left on disk for them to find, but they have no way to know that — the warn at line 631 only mentions `rename_error` and `write_error`, not "backup is at X, restore failed because Y, recover manually."
- **Suggested fix:** Capture the restore result and include backup_path + restore_err in the warn message: `let restore_err = std::fs::rename(&backup_path, path).err();` then in the warn add `restore_failed = restore_err.as_ref().map(|e| e.to_string()), backup_remaining_at = if restore_err.is_some() { Some(backup_path.display().to_string()) } else { None }`. Operator gets actionable recovery info.

#### EH-V1.36-4: `Embedder::pad_id` silently swallows tokenizer pad-id miss with model_config fallback
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:691-694
- **Description:** `tokenizer.get_padding().map(|p| p.pad_id as i64).unwrap_or(self.model_config.pad_id)`. If the tokenizer.json was loaded WITHOUT a `[padding]` section (which can happen with some HF exports), we silently use the model_config default. For BGE-large that's 0, but for any custom model with mismatched pad_id this produces wrong attention masks — embeddings that subtly drift versus the golden output. No warn, no metric.
- **Suggested fix:** Emit `tracing::warn!(model = %self.model_config.name, fallback_pad_id = self.model_config.pad_id, "tokenizer.json has no padding section — using model_config.pad_id")` exactly once via a OnceLock guard. Operators can correlate "embeddings look weird after model swap" with the warn.

#### EH-V1.36-5: `embedder/mod.rs` checksum-verified marker write swallows fs error
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:1528
- **Description:** `let _ = std::fs::write(&marker, &expected_marker);` after a successful `verify_checksum`. If the write fails (disk full, perms), every subsequent process re-runs the blake3 verify of the model.onnx file (~600 MB) on every cold start. Operator never sees why their daemon took 4s to come up — the warn at line 1106 just says "warmed" without distinguishing first-verify cost from post-marker cost.
- **Suggested fix:** `if let Err(e) = std::fs::write(&marker, &expected_marker) { tracing::warn!(path = %marker.display(), error = %e, "Failed to write checksum-verified marker — model will be re-verified next session"); }`

#### EH-V1.36-6: `Store::stored_model_name()` swallows query errors as `None`, masking schema corruption
- **Difficulty:** medium
- **Location:** src/store/metadata.rs:153-161
- **Description:** This function is `pub` and called from `cmd_doctor`, `slot promote`, and three other call sites that branch on "is this a fresh DB or a model-mismatched one." A query error (e.g., metadata table corrupted, sqlite I/O error) gets logged at warn but returns `None`, which every caller interprets as "fresh DB, no model recorded — treat as new". So a corrupted index is silently treated as a fresh one and a brand-new index gets initialized over it on the next `cqs index` call, *destroying the old data*.
- **Suggested fix:** Change return type to `Result<Option<String>, StoreError>` and let callers decide. The current behavior makes every caller default to the "destroy-and-recreate" path when the DB is unreadable — exact opposite of safe.

#### EH-V1.36-7: `slot/mod.rs:725` sentinel detail read silently empties on I/O error
- **Difficulty:** easy
- **Location:** src/slot/mod.rs:715-727
- **Description:** When a previous migration left a sentinel file, the code reports its contents in the error message via `.unwrap_or_default()`. If the sentinel exists but is unreadable (perms, locked by editor, etc.), the operator sees `"Sentinel contents:\n"` (empty) instead of an explanation that the file existed but couldn't be read. They're now stuck — they don't know if the migration fully or partially failed, and the recovery instruction `rm <path>` may delete useful diagnostic info.
- **Suggested fix:** Replace the `.unwrap_or_default()` chain with explicit Ok/Err handling: on Err, set `detail = format!("(could not read sentinel: {})", err)`. Operator now sees the I/O error and can `chmod`/recover before deleting.

#### EH-V1.36-8: `where_to_add::compute` silently skips files missing from batch-fetched chunks
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:215-217
- **Description:** `all_origins_chunks.remove(origin_key.as_ref()).unwrap_or_default()` — if the file appeared in `file_scores` (from search) but its chunks are missing from the batch fetch (race: file was deleted between search and chunk-fetch, or Store::get_chunks_by_origins_batch silently dropped a key), we synthesize a suggestion with empty `all_file_chunks`. The downstream `language` is `None` and `near_function` is "(top of file)". User sees an apparently-valid suggestion pointing at a file that no longer exists.
- **Suggested fix:** When `remove` returns `None`, log `tracing::debug!(file = %file.display(), "where_to_add: file in scores but no chunks fetched — skipping")` and `continue` rather than emit a malformed suggestion.

#### EH-V1.36-9: `cli/dispatch.rs:530` Err arm in router empty-suppression on per-cat alpha parse
- **Difficulty:** easy
- **Location:** src/search/router.rs:530
- **Description:** The match on `std::env::var(cat_key)` for per-category SPLADE alpha override has `Err(_) => {}` — meaning a missing env var is correctly silent, BUT a `VarError::NotUnicode` (env var contains invalid UTF-8) also falls into this arm and produces zero log output. An operator setting `CQS_SPLADE_ALPHA_NL=$'\xc3\x28'` (or pulling alpha from a Windows env with a stray BOM) gets no signal that their override was discarded.
- **Suggested fix:** `Err(std::env::VarError::NotPresent) => {}, Err(e) => tracing::warn!(var = %cat_key, error = %e, "Per-cat SPLADE alpha env var not unicode — ignored"),`. The triage notes EH-14 / dispatch.rs:208 already moved away from silent `.ok()`; this site never got the same treatment.

#### EH-V1.36-10: `train_data` skip-non-utf8 file uses bare `Err(_)` losing the actual decode error
- **Difficulty:** easy
- **Location:** src/train_data/git.rs:357
- **Description:** `Err(_) => { tracing::debug!(path, "Skipping non-UTF-8 file"); ... }` — the underlying `FromUtf8Error` carries the byte position of the first invalid sequence, which is useful for "is this a binary blob, a UTF-16 file, or a single curly-quote?" diagnostics. The bare `_` discards it. Files-skipped is a metric that can balloon to millions on a corpus mining run; debug log loses signal for "wait, I think these are actually UTF-16, why are we skipping them all?"
- **Suggested fix:** `Err(e) => { tracing::debug!(path, error = %e, valid_up_to = e.utf8_error().valid_up_to(), "Skipping non-UTF-8 file"); ... }`

DONE
