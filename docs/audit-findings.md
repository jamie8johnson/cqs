## Observability

#### OB-1: `stats()` silently swallows five SQLite query failures
- **Difficulty:** easy
- **Location:** src/cache.rs:296-324
- **Description:** `EmbeddingCache::stats()` runs five independent SQLite queries, each using `.unwrap_or(0)` or `.unwrap_or(None)`. When any query fails (e.g., corrupt page, locked DB), the error is discarded and zeros are returned. The function has an `info_span!` but no `tracing::warn!` on any failure path. The caller (`cache_stats` in cache_cmd.rs) therefore prints `0 entries, 0 MB` with no indication of a DB problem.
- **Suggested fix:** For each query, use `match ... { Ok(v) => v, Err(e) => { tracing::warn!(error = %e, "cache_stats query failed"); 0 } }` or propagate with `?` and remove the `unwrap_or`.

#### OB-2: `evict()` PRAGMA failure is silent, eviction silently skipped
- **Difficulty:** easy
- **Location:** src/cache.rs:262-270
- **Description:** The size query in `evict()` uses `.unwrap_or(0)`. If the PRAGMA fails, `size` is 0, the size check at line 269 passes (`0 <= max_size_bytes`), and the function returns `Ok(0)` — skipping eviction entirely with no log. A disk-full or WAL corruption condition that prevents the size query will silently bypass cache eviction indefinitely.
- **Suggested fix:** `let size = sqlx::query_scalar(...).fetch_one(&self.pool).await.map_err(|e| { tracing::warn!(error = %e, "cache_evict size query failed"); e })?;` — propagate or at minimum warn.

#### OB-3: `read_batch` blob-length mismatch silent drop (no debug log)
- **Difficulty:** easy
- **Location:** src/cache.rs:186-188
- **Description:** After decoding a blob to `Vec<f32>`, if `embedding.len() != expected_dim`, the entry is silently skipped (bare `continue`). The earlier dimension-column mismatch at line 170-178 logs a `tracing::debug!`, but this second guard — which catches blob truncation or structural corruption — is completely silent. This means a cache entry with a corrupted blob looks identical to a cache miss.
- **Suggested fix:** Add `tracing::debug!(hash = &hash[..8.min(hash.len())], actual = embedding.len(), expected_dim, "Cache blob length mismatch, skipping");` before the `continue`.

#### OB-4: `log_query` file-open failure is silent (no tracing)
- **Difficulty:** easy
- **Location:** src/cli/batch/commands.rs:315-321
- **Description:** When `log_query` cannot open the query log file (e.g., disk full, bad permissions, parent dir missing), it returns early with no trace output. The doc comment explicitly describes this as "silently ignored". During eval workflow development, silently missing log entries is a significant debugging gap — there is no way to distinguish "log write succeeded" from "log was skipped due to error".
- **Suggested fix:** Add `tracing::debug!(path = %log_path.display(), "Query log open failed, skipping");` at the early-return point. `debug!` is appropriate since this is truly best-effort.

#### OB-5: `dispatch()` in batch/commands.rs has no tracing span
- **Difficulty:** easy
- **Location:** src/cli/batch/commands.rs:340
- **Description:** `pub(crate) fn dispatch()` is the central routing function for all batch commands. It has no `tracing::info_span!` or `debug_span!`. Every handler dispatched from it (search, gather, scout, etc.) is therefore unrooted — the batch dispatch entry point is invisible in traces, making it impossible to correlate timing across a batch session.
- **Suggested fix:** Add `let _span = tracing::debug_span!("batch_dispatch", cmd = cmd.variant_name()).entered();` at the top of `dispatch()`, or at minimum an `info_span!`.

#### OB-6: `cache_stats`, `cache_clear`, `cache_prune` private helpers missing spans
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/cache_cmd.rs:49, 81, 99
- **Description:** The outer `cmd_cache` function at line 36 has an `info_span!("cmd_cache")`, but the three private helpers that do the actual work (`cache_stats`, `cache_clear`, `cache_prune`) have no child spans. This makes it impossible to distinguish which subcommand ran or how long each operation took when inspecting traces.
- **Suggested fix:** Add `let _span = tracing::info_span!("cache_stats").entered();`, `...("cache_clear", model = ?model).entered();`, and `...("cache_prune", days).entered();` at the top of each helper.

## Security

#### SEC-7: EmbeddingCache opens SQLite via unencoded path URL
- **Difficulty:** easy
- **Location:** src/cache.rs:74
- **Description:** `EmbeddingCache::open` constructs the SQLite connection URL with `format!("sqlite:{}?mode=rwc", path.display())`. If the home directory path contains special characters (`?`, `#`, spaces, or percent signs), the URL is malformed and sqlx may misparse the path component or the query-string options. `store/mod.rs:308-311` explicitly uses `SqliteConnectOptions::new().filename(path)` to avoid this exact problem and has an inline comment documenting it. The cache was written after that fix but did not follow the same pattern. This is a correctness issue that becomes a security issue if sqlx's URL parser falls back to an in-memory DB (silent data loss) or opens a different path than intended.
- **Suggested fix:** Replace `format!("sqlite:{}?mode=rwc", path.display())` + `pool.connect(&url)` with `SqliteConnectOptions::new().filename(path).create_if_missing(true).journal_mode(SqliteJournalMode::Wal)` and `pool_options.connect_with(opts)`, matching the store pattern.

#### SEC-8: EmbeddingCache DB and parent directory created world-readable
- **Difficulty:** easy
- **Location:** src/cache.rs:66, src/cache.rs:74-107
- **Description:** `EmbeddingCache::open` calls `std::fs::create_dir_all(parent)` without explicit permissions (umask-derived, typically 0o755 — world-readable and world-executable), then the SQLite DB is created with default file permissions (typically 0o644 — world-readable). The cache database stores content hashes for every indexed source file, allowing any local user to confirm which files are indexed. Every other user-specific file cqs creates is restricted: store DB (0o600, `store/mod.rs:355`), HNSW index (0o600, `hnsw/persist.rs:307`), notes (0o600, `note.rs:296`), config (0o600, `config.rs:455`), `.cqs` directory (0o700, `infra/init.rs:29`). The cache is the sole exception.
- **Suggested fix:** After `create_dir_all`, call `std::fs::set_permissions(parent, Permissions::from_mode(0o700))`. After the pool is created and the DB file exists, set the `.db`, `.db-wal`, and `.db-shm` files to 0o600, matching `store/mod.rs:355-364`.

#### SEC-9: query_log.jsonl created world-readable, no permission hardening
- **Difficulty:** easy
- **Location:** src/cli/batch/commands.rs:314-320
- **Description:** `log_query` creates `~/.cache/cqs/query_log.jsonl` via `OpenOptions::new().create(true).append(true)` with no explicit mode (umask-derived, typically 0o644). The log captures every search, gather, task, onboard, scout, and "where" query from batch mode with a Unix timestamp. On a shared system this exposes the full query history to other users. `src/cli/files.rs:68` already demonstrates the correct pattern: `.mode(0o600)` on the `OpenOptions`. The `~/.cache/cqs/` parent directory may also be 0o755 (see SEC-8, same parent dir).
- **Suggested fix:** Use `.mode(0o600)` on the `OpenOptions` chain (Unix-only; behind `#[cfg(unix)]` if portability matters). Also ensure the parent directory exists with 0o700 before opening, consistent with SEC-8.

#### SEC-10: `evict()` negative DB size casts to large u64, deletes entire cache
- **Difficulty:** easy
- **Location:** src/cache.rs:262-275
- **Description:** `evict()` fetches the DB file size via a PRAGMA query typed as `i64`, using `.unwrap_or(0)` on failure. If SQLite returns a negative value (possible on DB corruption, an overflowed page count product, or a driver bug), the cast `size as u64` on line 269 wraps to a very large positive number (e.g., `-1i64 as u64 == u64::MAX`). The guard `if (size as u64) <= self.max_size_bytes` then fails, and `excess = size as u64 - self.max_size_bytes` computes an astronomically large value. The resulting `entries_to_delete` would exceed the actual row count, causing the DELETE to silently destroy the entire cache. Because `evict()` is called unconditionally at the end of every index run (`pipeline/mod.rs:166`), a single corrupt PRAGMA response would wipe the cache on every subsequent index.
- **Suggested fix:** Add `if size <= 0 { return Ok(0); }` immediately after the PRAGMA fetch, before any cast. Alternatively, check `if size < 0 { tracing::warn!(size, "evict: negative DB size from PRAGMA, skipping"); return Ok(0); }` to distinguish the case from zero.

## Performance

#### PF-5: SPLADE encoding is single-threaded, one ONNX call per chunk — `encode_batch` exists but is never used
- **Difficulty:** medium
- **Location:** src/cli/commands/index/build.rs:393-407
- **Description:** The SPLADE encoding loop calls `encoder.encode(text)` once per chunk, sequentially. For a 13k-chunk codebase this is ~13k individual ONNX inference calls, each acquiring the session Mutex, tokenizing, building 1D tensors, running inference, and releasing the lock. `SpladeEncoder::encode_batch` exists (src/splade/mod.rs:184) but is never called from the index build path. The comment in `encode_batch` says "batching doesn't save much vs overhead of padding/unpadding" but no benchmark backs this up — true batched ONNX inference amortizes CUDA kernel launch overhead per sample.
- **Suggested fix:** Replace the per-chunk `encoder.encode(text)` loop with a call to `encoder.encode_batch(&text_refs)`. Collect all texts into a `Vec<&str>` first, then call `encode_batch` once. If ONNX padding overhead proves real at the batch level, implement sub-batching (e.g., 32 texts at a time) inside `encode_batch` rather than single-item calls.

#### PF-6: `write_batch` allocates a fresh `Vec<u8>` blob per embedding — reuse a scratch buffer
- **Difficulty:** easy
- **Location:** src/cache.rs:233
- **Description:** `write_batch` encodes each embedding as `embedding.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>()`. For a batch of 64 chunks at 1024 dimensions, this allocates 64 separate 4096-byte heap buffers, each freed after the `execute` call. A single scratch buffer that is `clear()`-d and reused across loop iterations would replace 64 allocations with 1.
- **Suggested fix:** Before the loop: `let mut blob = Vec::<u8>::with_capacity(dim * 4);`. Inside the loop: `blob.clear(); blob.extend(embedding.iter().flat_map(|f| f.to_le_bytes()));`. Pass `&blob` to `.bind()`.

#### PF-7: `read_batch` rebuilds the SQL placeholder string from scratch for every 100-entry sub-batch
- **Difficulty:** easy
- **Location:** src/cache.rs:147-154
- **Description:** For each chunk of 100 hashes, `read_batch` allocates a `Vec<String>` of placeholder strings (`"?2"`, `"?3"`, …`"?101"`), joins them, and concatenates into a new SQL string. A 13k-chunk index produces 130 such strings per read pass — 129 of which are identical (the full 100-entry batch). Each of the 100 placeholder strings is an individually heap-allocated `String`.
- **Suggested fix:** Pre-compute the full-batch (100-placeholder) SQL as a module-level `OnceLock<String>`. Use it for all full batches. Only the final partial batch needs a dynamically-sized string. This eliminates ~12,900 String allocations per read pass.

#### PF-8: `prepare_for_embedding` collects the hash list twice from the same slice
- **Difficulty:** easy
- **Location:** src/cli/pipeline/embedding.rs:43-71
- **Description:** `prepare_for_embedding` calls `windowed_chunks.iter().map(|c| c.content_hash.as_str()).collect::<Vec<&str>>()` at line 43-46 (for the global cache lookup) and again at line 68-71 (for the store lookup). Both produce an identical `Vec<&str>` from the same `windowed_chunks` slice. This is a redundant allocation and double iteration per pipeline batch.
- **Suggested fix:** Collect once: `let hashes: Vec<&str> = windowed_chunks.iter().map(|c| c.content_hash.as_str()).collect();` before the `if let Some(cache)` block and reuse `&hashes` for both lookups.

#### PF-9: GPU embed stage makes three separate passes over `texts` to compute max/avg/sum for one debug log
- **Difficulty:** easy
- **Location:** src/cli/pipeline/embedding.rs:238-250
- **Description:** Lines 238, 242, and 248 each invoke `.iter().map(|t| t.len())` on `prepared.texts`, producing three separate iterator chains to compute `max`, `sum/len`, and `sum`. All three values can be derived from a single fold. The chains execute unconditionally — tracing's lazy format-argument evaluation does not gate the `.iter()` chains themselves.
- **Suggested fix:** `let (max_len, total_chars) = prepared.texts.iter().fold((0, 0), |(mx, sm), t| (mx.max(t.len()), sm + t.len()));`. Derive `avg_len = total_chars / prepared.texts.len()`. Or gate the whole block with `if tracing::enabled!(tracing::Level::DEBUG)`.

#### PF-10: `as_slice().to_vec()` clones every newly-embedded vector just to write it to the global cache
- **Difficulty:** easy
- **Location:** src/cli/pipeline/embedding.rs:282, 410
- **Description:** After embedding, the cache write path does `emb.as_slice().to_vec()` to produce an owned `Vec<f32>` for each embedding before passing to `cache.write_batch(&[(String, Vec<f32>)])`. `Embedding::as_slice()` returns `&[f32]` — already a reference into the inner `Vec<f32>`. `write_batch` only reads the data to encode it as a blob; it never needs ownership of the float vector. This forces one full-dimension clone per newly-embedded chunk.
- **Suggested fix:** Change `write_batch` to accept `&[(&str, &[f32])]` or `&[(impl AsRef<str>, impl AsRef<[f32]>)]`. Callers pass `(chunk.content_hash.as_str(), emb.as_slice())` with no allocation. Inside `write_batch`, encode from the slice directly.

#### PF-11: `upsert_sparse_vectors` issues one DELETE per chunk — N separate SQL round trips inside the transaction
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:26-28
- **Description:** For each of N chunks in the batch, `upsert_sparse_vectors` executes a standalone `DELETE FROM sparse_vectors WHERE chunk_id = ?1`. With 13k chunks this is 13k individual SQL statements inside one transaction, each separately parsed and planned by SQLite. A bulk `DELETE FROM sparse_vectors WHERE chunk_id IN (...)` (chunked at 333 IDs to stay under SQLite's 999-variable limit) would accomplish the same result with ceil(N/333) ≈ 39 statements instead of 13k.
- **Suggested fix:** Collect all chunk IDs, split into batches of 333, issue one bulk DELETE per batch using `QueryBuilder`, then proceed with the existing batch INSERT logic. Transaction atomicity is preserved.

#### PF-12: `chunk_type_language_map` scans all 13k chunks on every filtered search — result not cached on Store
- **Difficulty:** medium
- **Location:** src/store/chunks/query.rs:441-460, called from src/search/query.rs:343, 490
- **Description:** Every SPLADE hybrid search and every HNSW filtered search calls `chunk_type_language_map()`, which does `SELECT id, chunk_type, language FROM chunks` across all chunks and builds a `HashMap<String, (ChunkType, Language)>` with ~13k String-keyed entries (~650KB). This map is identical for all searches within the same index state. In batch mode with 100 piped queries, the map is rebuilt 100 times, allocating and freeing 13k Strings on each call.
- **Suggested fix:** Cache the map in `Store` behind a `RwLock<Option<HashMap<String, (ChunkType, Language)>>>`. Invalidate on `upsert_chunks`, `prune_missing`, or any write that modifies chunk metadata. For a read-only search workload the map is computed once and reused.

#### PF-13: `SpladeIndex::search_with_filter` calls `id_map.get(chunk_idx)` in the innermost loop for every posting list hit
- **Difficulty:** easy
- **Location:** src/splade/index.rs:82-89
- **Description:** The score accumulation loop (iterating query terms × their posting lists) calls `self.id_map.get(chunk_idx)` to retrieve the chunk ID string for the filter predicate. For a query with 150 non-zero SPLADE terms and postings averaging 50 entries each, this is 7500 bounds-checked Vec accesses in the hot path. The `id_map` index is always valid (built from the same enumeration), so the bounds check is wasted overhead. The result-collection phase at lines 97-100 calls `self.id_map.get(idx)` again for each scored chunk.
- **Suggested fix:** Use `&self.id_map[chunk_idx]` (direct indexing — panics if out of range, but is always valid by construction) in the filter loop: `let chunk_id = &self.id_map[chunk_idx]; if filter(chunk_id) { *scores.entry(chunk_idx).or_insert(0.0) += ...; }`. In result collection, use `&self.id_map[idx]` directly. This eliminates the `Option` wrapping overhead on every hot-path access.

#### PF-14: `SpladeEncoder::encode` copies the full logits tensor (seq_len × vocab f32s) via `data.to_vec()`
- **Difficulty:** medium
- **Location:** src/splade/mod.rs:159
- **Description:** `try_extract_tensor::<f32>()` returns a `CowArray` that typically borrows ORT's output buffer zero-copy. Line 159 calls `data.to_vec()` unconditionally, allocating a new owned `Vec<f32>` and copying the entire logits tensor. For a 128-token sequence with 30522-vocab BERT, this is 128 × 30522 × 4 bytes ≈ 15.6MB allocated and freed per `encode` call. Across 13k chunks during a full index build this is ~200GB of allocator traffic purely for this copy. The copy is only needed because `Array2::from_shape_vec` requires owned data.
- **Suggested fix:** Instead of `Array2::from_shape_vec((seq_len, vocab), data.to_vec())`, use `ArrayView2::from_shape((seq_len, vocab), data.as_slice().expect("contiguous ORT output"))` to get a zero-copy view. Then call `.fold_axis(...)` on the view directly (ndarray supports fold on views). If ORT outputs may be non-contiguous, call `data.as_standard_layout()` (cheap if already standard, clones only if not) instead of `.to_vec()`.

## Error Handling

#### EH-13: SPLADE encode error silently dropped in batch search handler
- **Difficulty:** easy
- **Location:** src/cli/batch/handlers/search.rs:101
- **Description:** When SPLADE is enabled in batch mode, `enc.encode(&params.query).ok()` silently converts an encode error into `None`, causing the search to fall back to cosine-only with no log entry. The CLI command path (`src/cli/commands/search/query.rs:164-171`) handles the identically-shaped code with a `match` and `tracing::warn!`. A batch session where SPLADE encoding is broken (e.g., ORT session poisoned, tokenizer failure) produces no diagnostic output and returns silently degraded results.
- **Suggested fix:** Replace `.and_then(|enc| enc.encode(&params.query).ok())` with `.and_then(|enc| match enc.encode(&params.query) { Ok(sv) => Some(sv), Err(e) => { tracing::warn!(error = %e, "SPLADE query encoding failed in batch mode, falling back to cosine-only"); None } })`, matching the CLI path.

#### EH-14: `ensure_splade_index` silently ignores DB errors loading sparse vectors
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:266-271
- **Description:** `ensure_splade_index` uses `if let Ok(vectors) = self.store().load_all_sparse_vectors()` with no `else` branch. If the DB query fails (e.g., locked DB, table missing during a migration-straddled open), the index is left as `None` silently. The caller degrades to cosine-only with no log distinguishing "no sparse vectors exist" from "DB query failed". The CLI path (`src/cli/store.rs:162-176`) handles the same load with a `match` and explicit `tracing::warn!` on the error arm.
- **Suggested fix:** Change `if let Ok(vectors) = ...` to a `match`, adding `Err(e) => { tracing::warn!(error = %e, "Failed to load sparse vectors for SPLADE index, falling back to cosine-only"); }` on the error arm.

#### EH-15: `get_chunk_with_embedding` errors silently ignored in neighbor lookup
- **Difficulty:** easy
- **Location:** src/cli/commands/search/neighbors.rs:137
- **Description:** `build_chunk_map` iterates over chunk IDs and calls `store.get_chunk_with_embedding(id)` with `if let Ok(Some(...))`, silently discarding both `Err(_)` (DB failure) and `Ok(None)` (missing chunk). A downstream caller that expects all IDs to resolve gets an incomplete map with no indication of failure. A transient DB error or a chunk ID mismatch silently produces fewer neighbors than requested, with no log.
- **Suggested fix:** Add a warn on the Err arm: `match store.get_chunk_with_embedding(id) { Ok(Some((chunk, _))) => { map.insert(...); } Ok(None) => {} Err(e) => { tracing::warn!(id = %id, error = %e, "Failed to fetch chunk for neighbor display"); } }`

#### EH-16: `prune_orphan_sparse_vectors` defined but never called — orphaned rows accumulate
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:128
- **Description:** `Store::prune_orphan_sparse_vectors` is defined but has zero callers. During incremental updates (`cqs watch`, `--incremental`), chunks are deleted via `delete_chunks_by_origin` but their `sparse_vectors` rows are not pruned. Over time the `sparse_vectors` table accumulates orphaned rows for deleted chunks, increasing `load_all_sparse_vectors` query time and the SPLADE in-memory index size. (Full reindex is unaffected — the old DB is replaced atomically.)
- **Suggested fix:** Call `store.prune_orphan_sparse_vectors()` after `delete_chunks_by_origin` in the incremental update path, or at startup inside `ensure_splade_index` before building the index. Log the pruned count at `tracing::debug!` level.

## Robustness

#### RB-10: `SpladeEncoder::encode` panics on poisoned session Mutex
- **Difficulty:** easy
- **Location:** src/splade/mod.rs:134
- **Description:** `self.session.lock().unwrap()` is called in production code. If a previous call to `encode()` panicked while holding the lock (e.g., due to an ORT internal panic during `session.run()`), the Mutex becomes poisoned. Every subsequent call to `encode()` — including all index-build SPLADE passes and all hybrid search queries — will panic with "poisoned lock". The rest of the codebase uses `unwrap_or_else(|poisoned| poisoned.into_inner())` for this pattern (cagra.rs:138, 184, 191, 227, 241, 262, 276, 290).
- **Suggested fix:** `let mut session = self.session.lock().unwrap_or_else(|p| p.into_inner());` — consistent with the CAGRA index pattern. Add a `tracing::warn!` inside the `unwrap_or_else` closure if recovery feels risky.

#### RB-11: `format_timestamp` panics on negative `created_at` values in embedding cache DB
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/cache_cmd.rs:123
- **Description:** `format_timestamp(ts: i64)` computes `UNIX_EPOCH + Duration::from_secs(ts as u64)`. If `ts` is negative (DB corruption, external write, or a bug in the write path), casting to `u64` wraps to a value like `18446744073709551615`, and `UNIX_EPOCH + Duration::from_secs(very_large_value)` panics with "overflow when adding duration to instant" (verified). The `oldest_timestamp` and `newest_timestamp` fields come from SQL `MIN`/`MAX(created_at)` and are passed directly as `Option<i64>` without any range check. Reachable via `cqs cache stats`.
- **Suggested fix:** Guard at the top of `format_timestamp`: `if ts <= 0 { return "unknown".to_string(); }`. Alternatively use `UNIX_EPOCH.checked_add(Duration::from_secs(ts.max(0) as u64)).map(...)` to avoid the panic entirely.

#### RB-12: `SearchFilter::validate()` does not check `splade_alpha` — NaN and out-of-range values silently corrupt hybrid search scores
- **Difficulty:** easy
- **Location:** src/store/helpers/search_filter.rs:91-143, src/search/query.rs:412-435
- **Description:** `SearchFilter::validate()` validates `name_boost` (range check, NaN-safe) but has no corresponding check for `splade_alpha`. The batch search handler (`cli/batch/handlers/search.rs:86-96`) constructs a `SearchFilter` with `splade_alpha` from user input and never calls `validate()`. A NaN or out-of-range `splade_alpha` (e.g., from a batch JSON field or a misconfigured `ScoringConfig`) flows into the fusion formula `alpha * d + (1.0 - alpha) * s` at line 435, producing NaN scores for all results. NaN-scored results sort indeterminately (the `partial_cmp` fallback to `Ordering::Equal` makes the ordering non-reproducible), returning garbage results with no error.
- **Suggested fix:** Add to `validate()`: `if !(0.0..=1.0).contains(&self.splade_alpha) { return Err(format!("splade_alpha must be between 0.0 and 1.0, got {}", self.splade_alpha)); }`. Call `filter.validate()?` at the top of `dispatch_search` in batch/handlers/search.rs, matching the CLI path at cli/commands/search/query.rs:137.

#### RB-13: `SpladeEncoder::encode` does not cap input length — very long texts run full-sequence BERT inference
- **Difficulty:** easy
- **Location:** src/splade/mod.rs:99-181
- **Description:** `encode()` has an empty-text fast-path but no truncation. The HuggingFace `tokenizers` crate truncates tokenized output to the model's `max_length` by default only if truncation is explicitly enabled on the tokenizer. If the loaded tokenizer JSON has no truncation configured, a 20KB source file (common for large functions in `chunk_splade_texts()`) produces a full-length token sequence. For BERT this means up to 512 tokens × 30522 vocab f32s ≈ 62MB per inference — or, if the model has no position-embedding limit, an uncapped sequence. Index builds encoding large chunks would produce either OOM conditions or ORT errors (both are caught by the error-handling path, but silently degrade to missing SPLADE vectors for those chunks).
- **Suggested fix:** Before tokenizing, truncate `text` to a character budget: `let text = if text.len() > 4000 { &text[..text.char_indices().nth(4000).map(|(i,_)| i).unwrap_or(text.len())] } else { text };`. 4000 chars ≈ 1000 tokens for typical code, safely under BERT's 512-token position limit after WordPiece expansion. Log a `tracing::debug!` when truncation occurs.

#### RB-14: `token_id as u32` in `load_all_sparse_vectors` silently wraps negative DB values
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:85
- **Description:** `token_id: i64` is fetched from SQLite with `row.get("token_id")`, then cast to `u32` with `token_id as u32`. If the stored value is negative (which SQLite permits since the column has no CHECK constraint), the cast wraps: `-1i64 as u32 == 4294967295`. The resulting `SparseVector` contains a bogus token_id that will never match any real vocabulary entry, so those postings are harmlessly dead weight. However, the SPLADE index silently contains phantom entries, slightly inflating `unique_tokens()` counts and wasting memory. No error is logged.
- **Suggested fix:** Add a range check: `if token_id < 0 || token_id > u32::MAX as i64 { tracing::warn!(token_id, chunk_id, "Invalid token_id in sparse_vectors, skipping"); continue; }`. Also add a `CHECK (token_id >= 0)` constraint to the `sparse_vectors` table DDL in migrations.rs to prevent bad writes.

## Data Safety

#### DS-44: `write_batch` stores caller-supplied `dim` without validating against actual embedding length
- **Difficulty:** easy
- **Location:** src/cache.rs:227-247
- **Description:** `write_batch` accepts `dim: usize` and writes it directly to the `dim` column, but never checks `embedding.len() == dim`. If the caller passes mismatched values — possible through API misuse since `dim` is a separate parameter from the entries slice — the DB stores a wrong `dim`. On read, `dim as usize != expected_dim` at line 170 silently rejects the entry as a cache miss, leaving the corrupt row in the DB indefinitely. There is no write-time warning to surface the mismatch.
- **Suggested fix:** At the start of the write loop add: `if embedding.len() != dim { tracing::warn!(hash = &content_hash[..8.min(content_hash.len())], actual = embedding.len(), expected = dim, "Skipping cache write: embedding length does not match dim"); continue; }`.

#### DS-45: `INSERT OR IGNORE` silently retains stale cache entry when `model_fingerprint()` falls back to repo name
- **Difficulty:** medium
- **Location:** src/cache.rs:235-246, src/embedder/mod.rs:352-364
- **Description:** `model_fingerprint()` falls back to `self.model_config.repo.clone()` (e.g., `"BAAI/bge-large-en-v1.5"`) when it cannot read the ONNX file (lines 352-363). Two invocations with different model weights but the same repo string collide on `(content_hash, model_fingerprint)`. `INSERT OR IGNORE` silently discards the newer embedding, and the older (potentially wrong) embedding is served for all subsequent searches until `cqs cache clear` is run manually. This fallback is reachable when the model file is temporarily unavailable during download, which coincides with the first index run when the cache is cold.
- **Suggested fix:** At the fallback site in `model_fingerprint()`, emit a `tracing::warn!` and return a unique sentinel (e.g., append a random nonce) so that cache writes under an unreliable fingerprint create new entries rather than colliding. Alternatively, skip cache writes entirely when fingerprint computation failed, by checking `fingerprint.starts_with(REPO_PREFIX_SENTINEL)` in `write_batch`.

#### DS-46: Negative `dim` in embedding cache DB wraps to large `usize`, silently masking corruption
- **Difficulty:** easy
- **Location:** src/cache.rs:170
- **Description:** The `dim` column is SQLite `INTEGER` (i64). A negative value — possible via direct DB modification or a future migration bug — causes `dim as usize` to wrap to `usize::MAX - |dim| + 1` on 64-bit platforms. The comparison `dim as usize != expected_dim` evaluates to `true`, so the row is silently treated as a cache miss. The corruption is invisible: `stats()` reports it as a valid entry, `read_batch` skips it, and no log records the problem.
- **Suggested fix:** `if dim < 0 || dim as usize != expected_dim { tracing::debug!(hash = &hash[..8.min(hash.len())], cached_dim = dim, expected_dim, "Cache dim mismatch or invalid, skipping"); continue; }`.

#### DS-47: `EmbeddingCache` pool missing `busy_timeout` — SQLITE_BUSY is not retried
- **Difficulty:** easy
- **Location:** src/cache.rs:76-107
- **Description:** The main `Store` pool sets `.busy_timeout(Duration::from_secs(5))` and `synchronous(Normal)` (`store/mod.rs:314-320`). The embedding cache pool sets neither. With `max_connections(2)` in WAL mode, a concurrent `cqs cache stats` and an `evict()` call from the index pipeline contend for the write lock. Without a busy timeout, the reader receives `SQLITE_BUSY` immediately rather than retrying for 5 seconds. The best-effort fallback swallows the error, so the user sees `0 entries, 0 MB` (OB-1) with no indication that the DB was simply locked.
- **Suggested fix:** Add `.busy_timeout(std::time::Duration::from_secs(5))` and `.synchronous(sqlx::sqlite::SqliteSynchronous::Normal)` to the `SqlitePoolOptions` chain at lines 76-79, matching the main Store.

#### DS-48: `VerifyReport` is a dead exported type — no `verify()` method exists
- **Difficulty:** easy
- **Location:** src/cache.rs:34-41
- **Description:** `pub struct VerifyReport { pub sampled, matched, mismatched, missing }` is exported from `cqs::cache` with `pub` visibility. No method on `EmbeddingCache` returns a `VerifyReport`, and no code anywhere constructs one (zero callers in the codebase). Its presence implies a cache integrity verification feature that does not exist, misleading callers who look at the public API.
- **Suggested fix:** Remove `VerifyReport` until a `verify()` implementation is added. If integrity checking is planned, add a stub `fn verify(&self, embedder: &Embedder, sample_size: usize) -> Result<VerifyReport, CacheError>` to anchor the type.

#### DS-49: `evict()` measures physical DB pages including free-page list, not logical data size
- **Difficulty:** medium
- **Location:** src/cache.rs:261-288
- **Description:** `SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()` returns total allocated file size including SQLite's free-page list. After heavy `prune_older_than` usage, free pages accumulate while logical data shrinks. `evict()` sees an inflated size and deletes entries unnecessarily (over-eviction), degrading cache hit rate. The `entries_to_delete` estimate (`excess / 4200`) also hard-codes the 1024-dim entry size (4096 bytes of floats + ~100-byte overhead), causing 25% over-eviction for 768-dim caches and 75% under-eviction for 384-dim caches. Combined with SEC-10 (negative size wraps), this logic has multiple independent failure modes.
- **Suggested fix:** Measure logical data: `SELECT SUM(LENGTH(embedding)) + COUNT(*) * 200 FROM embedding_cache`. Alternatively subtract `freelist_count * page_size` from the PRAGMA total. Derive the per-entry size estimate from `dim` rather than hard-coding 4200.

#### DS-50: Concurrent `block_on` from GPU and CPU threads serializes on shared `current_thread` runtime
- **Difficulty:** medium
- **Location:** src/cache.rs:69-72, src/cli/pipeline/mod.rs:116-130
- **Description:** `EmbeddingCache` wraps a `tokio::runtime::Builder::new_current_thread()` runtime shared via `Arc` between the GPU embed thread and CPU embed thread (pipeline/mod.rs:116-130). Both threads call `cache.read_batch()` and `cache.write_batch()`, which invoke `self.rt.block_on()`. The `current_thread` scheduler serializes concurrent `block_on` callers via a take-core spinlock: the second caller spins until the first releases the scheduler core (tokio `scheduler/current_thread/mod.rs:195-222`). During large index runs where both GPU and CPU are actively embedding, this creates unexpected cross-thread serialization — a GPU cache write blocks all CPU cache reads and vice versa. The stall is proportional to batch size and SQLite write latency.
- **Suggested fix:** Replace `new_current_thread()` with `new_multi_thread().worker_threads(1)`. The single worker thread preserves sequential task execution within the runtime while allowing concurrent `block_on` calls from multiple OS threads without spin-waiting.

## Code Quality

#### CQ-1: `VerifyReport` is a dead public type — no methods, no callers
- **Difficulty:** easy
- **Location:** src/cache.rs:35
- **Description:** `VerifyReport` struct (fields `sampled`, `matched`, `mismatched`, `missing`) is declared `pub` but has no associated methods and zero callers anywhere in the codebase. No `verify` function exists in `cache.rs`. It was likely scaffolded for a cache verification feature that was never implemented.
- **Suggested fix:** Delete `VerifyReport`, or implement the verification function it was intended to support.

#### CQ-2: `SearchFilter::new()` is dead and self-deprecating
- **Difficulty:** easy
- **Location:** src/store/helpers/search_filter.rs:78
- **Description:** `SearchFilter::new()` is an `#[inline]` pub method that calls `Self::default()`. Its own doc comment says "Equivalent to `SearchFilter::default()`. Prefer `Default::default()` or struct literal syntax." It has zero callers. The method adds public API surface that actively discourages use of itself.
- **Suggested fix:** Delete the method. `SearchFilter` already implements `Default`; any callers can use `SearchFilter::default()` directly.

#### CQ-3: `test_eviction` duplicates 30+ lines of `EmbeddingCache::open` internals
- **Difficulty:** easy
- **Location:** src/cache.rs:529-575
- **Description:** `test_eviction` manually replicates the full schema setup from `EmbeddingCache::open` (tokio runtime creation, URL construction, pool options, WAL pragma, `CREATE TABLE`, `CREATE INDEX`) just to construct an `EmbeddingCache` with `max_size_bytes: 1`. The env var `CQS_CACHE_MAX_SIZE` is already read at `open` time (line 109-112), so the test could set that var and call `EmbeddingCache::open`. When `open` changes, the test schema diverges silently.
- **Suggested fix:** Set `CQS_CACHE_MAX_SIZE=1` before calling `EmbeddingCache::open(&path)`, then restore the env var afterward. Guard with a test mutex if needed.

#### CQ-4: `cache_path_display()` recomputes `default_path()` — ignores the already-computed local variable
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/cache_cmd.rs:117
- **Description:** `cmd_cache` computes the cache path into `cache_path` at line 38. `cache_stats` (the only caller) invokes `cache_path_display()` at line 63, which calls `EmbeddingCache::default_path()` a second time. `cache_path` is not passed to `cache_stats`. This allocates a duplicate `PathBuf` for a value already in scope.
- **Suggested fix:** Pass `&cache_path` to `cache_stats` and remove the `cache_path_display()` helper entirely.

#### CQ-5: Batch `search` command missing `--include-type` / `--exclude-type` added in PR #838
- **Difficulty:** medium
- **Location:** src/cli/batch/commands.rs:32, src/cli/batch/handlers/search.rs:9
- **Description:** PR #838 added `--include-type` and `--exclude-type` to CLI search (`src/cli/definitions.rs:163,167`) and to `SearchFilter` (`exclude_types` field). The batch `Search` command in `BatchCmd` and `SearchParams` has neither field. Agents using batch mode cannot filter by chunk type, while CLI mode can. The batch handler hardcodes `chunk_types: Some(ChunkType::code_types())` with no override, making it impossible to search for tests, endpoints, variables, or stored procs by type from batch sessions.
- **Suggested fix:** Add `include_type: Option<Vec<String>>` and `exclude_type: Option<Vec<String>>` to `SearchParams` and `BatchCmd::Search`. Parse them the same way `cmd_query` does (src/cli/commands/search/query.rs:92-121).

#### CQ-6: Double rerank guard in `dispatch_search` — inner `len() > 1` check is unreachable
- **Difficulty:** easy
- **Location:** src/cli/batch/handlers/search.rs:144, 151
- **Description:** The outer `if params.rerank && results.len() > 1` at line 144 enters only when there are 2+ results. The `into_iter().map()` that produces `code_results` is infallible and preserves count. The inner `if code_results.len() > 1` at line 151 is therefore always true within this branch — the reranker call is always made. The redundant guard obscures the actual control flow.
- **Suggested fix:** Remove the inner `if code_results.len() > 1` wrapping and call `reranker.rerank(...)` unconditionally within the outer `if` block.

#### CQ-7: `resolve_parent_context` dedup check uses child ID instead of parent ID — never fires
- **Difficulty:** medium
- **Location:** src/cli/commands/search/query.rs:692
- **Description:** The comment "Skip if already resolved (multiple children share same parent)" describes avoiding repeated source file reads. But the check is `parents.contains_key(&sr.chunk.id)` (child ID as key), not the parent ID. Since each `sr.chunk.id` is unique per result, the check is always false and the `continue` is unreachable. When multiple search results share a parent, the same source file is read from disk and the same content is allocated once per child result.
- **Suggested fix:** Track resolved parent IDs: `let mut resolved_parent_ids: HashSet<&str> = HashSet::new();`. Check `resolved_parent_ids.contains(parent_id.as_str())` and insert after resolution. The `parents` output map (keyed by child ID) stays correct for consumers.

#### CQ-8: `search_hybrid` uses bare `.unwrap()` on `Option` whose `None` case already returned
- **Difficulty:** easy
- **Location:** src/search/query.rs:328
- **Description:** After `if !filter.enable_splade || splade.is_none() { return ...; }` at line 324, line 328 calls `splade.unwrap()` to destructure the `Option`. The unwrap is logically safe but is still a bare `unwrap()` in production code with no context message. Clippy's `clippy::unwrap_used` may flag it.
- **Suggested fix:** Replace with `let (splade_index, sparse_query) = splade.expect("splade is Some: None case returned at line 324");` or restructure as `if let Some((splade_index, sparse_query)) = splade { ... }`.

## Test Coverage

#### TC-17: `HnswIndex::search_filtered` has zero unit tests
- **Difficulty:** easy
- **Location:** src/hnsw/search.rs:32-45
- **Description:** `search_filtered` (added in PR #826 for traversal-time --chunk-type and --lang filtering) has no unit tests. Coverage exists only through ignored integration tests in `tests/pipeline_eval.rs`. The function builds an id_filter closure mapping DataId (usize) to chunk_id string to predicate result. None of the following are tested: a filter that rejects all candidates (should return empty), a filter passing fewer than k candidates, a filter for a chunk ID not in the index, or the non-finite score guard at `search.rs:98-104`. The `src/hnsw/build.rs` test module has 10+ tests for `search` but none for `search_filtered`.
- **Suggested fix:** Add unit tests in `src/hnsw/build.rs`: build a small `HnswIndex` with distinct chunk IDs, call `search_filtered` with a predicate that accepts only half the IDs, assert that no rejected IDs appear in results. Add a test where the filter rejects everything and asserts `results.is_empty()`.

#### TC-18: `find_rank` in eval_harness matches by name only — file disambiguation untested
- **Difficulty:** easy
- **Location:** tests/eval_harness.rs:87-116
- **Description:** `find_rank` identifies the ground-truth chunk by matching `r.chunk.name == primary_answer.name`. The `GroundTruth` struct has a `file` field intended for disambiguation but it is never consulted. Two functions with the same name in different files (e.g., `parse` in `parser/rust.rs` and `parser/python.rs`) produce an inflated R@1: finding either counts as a hit. There are no unit tests for `find_rank` at all — it is exercised only by the ignored `test_eval_matrix`. The `acceptable_answers` branch also ignores file, compounding the ambiguity.
- **Suggested fix:** Add unit tests with mock `SearchResult` vectors including same-name/different-file scenarios. Fix the function to also match on file path when `primary_answer.file` is non-empty.

#### TC-19: `VerifyReport` struct defined in cache.rs with no `verify()` method — untestable dead code
- **Difficulty:** easy
- **Location:** src/cache.rs:33-40
- **Description:** `pub struct VerifyReport { sampled, matched, mismatched, missing }` is declared at module level but there is no `verify()` or `verify_cache()` method on `EmbeddingCache` that produces one. No other file references `VerifyReport`. The struct is publicly exported as part of the cache API but represents entirely unimplemented functionality. It cannot be obtained through any API and therefore can never be tested.
- **Suggested fix:** Either implement `EmbeddingCache::verify(sample_size: usize, model_fp: &str, re_embed: impl Fn(&str) -> Option<Vec<f32>>) -> Result<VerifyReport, CacheError>` with tests, or remove the struct. If deferred, replace with a `// TODO: implement verify()` comment rather than a public exported type.

#### TC-20: `read_batch` SQLite chunking path (>100 hashes) never exercised by tests
- **Difficulty:** easy
- **Location:** src/cache.rs:147-196
- **Description:** `read_batch` splits queries into groups of 100 to stay under SQLite's variable limit. The test `test_batch_write` writes 100 entries and reads all 100 back — this exercises the inner loop exactly once with a full 100-item batch, never triggering multi-batch iteration. A read of 101 hashes runs the SQL query twice and merges two HashMaps; this path is untested. A partial final batch (e.g., 250 hashes: two batches of 100 + one of 50) is also untested.
- **Suggested fix:** Add `test_read_batch_crosses_100_boundary`: write 250 entries with distinct hashes, read all 250 back in a single `read_batch` call, assert `result.len() == 250` and spot-check values. Add `test_read_batch_exactly_101` as a boundary test.

#### TC-21: Cache never tested with NaN or Infinity embeddings
- **Difficulty:** easy
- **Location:** src/cache.rs:201-255
- **Description:** `write_batch` encodes embeddings as raw `f32` little-endian bytes with no guard against NaN or Inf values. These are stored verbatim and round-trip back as NaN/Inf when read by `read_batch`. Any downstream consumer (cosine similarity, dot product, HNSW scoring) receiving a NaN embedding silently produces NaN scores, corrupting search ranking. All existing tests use only well-formed embeddings from `make_embedding(dim, seed)`. No test writes a NaN-containing embedding and checks whether the cache accepts, rejects, or flags it.
- **Suggested fix:** Add `test_nan_embedding_write_read`: create an embedding with `f32::NAN`, call `write_batch`, then `read_batch`. Assert either the entry is absent or all returned floats are finite. Add a guard in `write_batch`: `if embedding.iter().any(|f| !f.is_finite()) { tracing::warn!(...); continue; }`.

#### TC-22: `log_query` has zero tests despite being the sole eval-capture mechanism
- **Difficulty:** easy
- **Location:** src/cli/batch/commands.rs:309-333
- **Description:** `log_query` appends JSONL to `~/.cache/cqs/query_log.jsonl` and has no unit tests. Three silent-failure branches are never exercised: no home directory, file open failure, and `writeln` failure. The format string constructs JSON with raw string interpolation: `"cmd":\"{}\"` where `command` is inserted unescaped — if any future caller passes a string containing `"` or `\`, the output is invalid JSON. No test verifies the JSONL is parseable. No test verifies that calling `dispatch` with a search or gather command actually appends to the log.
- **Suggested fix:** Expose `log_query` as `pub(crate)` or accept a path parameter. Add: (1) a test that calls it with a tempfile path, reads back, and parses each line as `serde_json::Value`; (2) a test with Unicode and `"` in the query to catch JSON injection; (3) a test with an unwritable path to confirm no panic.

#### TC-23: Chunk type filter tests omit all new chunk types (Test, Endpoint, Service, StoredProc, Variable)
- **Difficulty:** easy
- **Location:** src/search/scoring/filter.rs:350-360
- **Description:** `test_chunk_type_filter_set_membership` verifies only `Function` and `Method` are recognized in filter sets. The five types added since v1.13 — `Test`, `Endpoint`, `Service`, `StoredProc`, and `Variable` — are absent. Since `--chunk-type test` and `--chunk-type endpoint` are user-facing CLI flags (PR #826), there should be at least one round-trip test confirming each type's string form parses back via `capture_name_to_chunk_type` and routes correctly through the filter predicate in `filter.rs:94`. Note: `Variable` uses `capture = "var"` not `"variable"`, making a round-trip test especially important.
- **Suggested fix:** Extend `test_chunk_type_filter_set_membership` to include all five new types. Add `test_new_chunk_type_capture_names` that calls `capture_name_to_chunk_type` with `"test"`, `"endpoint"`, `"service"`, `"storedproc"`, and `"var"` and asserts the correct variant is returned.

#### TC-24: `prune_older_than(0)` and `prune_older_than(u32::MAX)` edge cases never tested
- **Difficulty:** easy
- **Location:** src/cache.rs:359-378
- **Description:** `prune_older_than(0)` computes `cutoff = now - 0 = now` and issues `DELETE WHERE created_at < now`. Entries inserted in the same second survive due to the strict `<` operator — a boundary that would silently break if changed to `<=`. `prune_older_than(u32::MAX)` computes `u32::MAX as i64 * 86400 = 370_287_945_062_400`, which does not overflow `i64::MAX`, but produces a cutoff far in the future that deletes all rows — opposite of the expected no-op behavior for an absurdly large value. Neither edge case is tested.
- **Suggested fix:** Add `test_prune_zero_days`: write entries, call `prune_older_than(0)`, assert count unchanged. Add `test_prune_large_days` with a large value: verify no panic. Document whether `days = 0` means "keep nothing older than now" (current behavior) or "keep everything" (possibly expected).

#### TC-25: Eval harness helper functions have no unit tests
- **Difficulty:** easy
- **Location:** tests/eval_harness.rs:221-413
- **Description:** `aggregate`, `aggregate_by_category`, `generate_report`, and `save_results_jsonl` are exercised only by the ignored `test_eval_matrix`. `aggregate_by_category` with an empty result set or single category is untested. `generate_report` produces a Markdown report with a paired-bootstrap section that only appears for 2+ configs — neither the structure nor the condition is tested. `save_results_jsonl` calls `File::create(...).expect(...)` unconditionally and would panic on an unwritable path, but even the happy path has no test.
- **Suggested fix:** Add unit tests using small synthetic `EvalQueryResult` vectors: `test_aggregate_empty` (all metrics are 0.0), `test_aggregate_by_category_two_categories` (each gets its own row), `test_generate_report_single_config` (contains config ID and query count), `test_save_results_jsonl_roundtrip` (write to tempfile, read back, parse as JSON).

#### TC-26: `write_batch` duplicate content_hash behavior untested and undocumented
- **Difficulty:** easy
- **Location:** src/cache.rs:235-249
- **Description:** `write_batch` uses `INSERT OR IGNORE`. If the same `content_hash` appears twice in a single `entries` vec (possible when upstream deduplication is incomplete), the second entry is silently dropped and `written` reports 1 instead of 2. No test exercises this. The first-wins semantics are undocumented: if two embeddings differ for the same hash (e.g., two model runs racing), the caller cannot know which is persisted. `prepare_for_embedding` in `pipeline/embedding.rs` does not deduplicate by content_hash before calling `write_batch`.
- **Suggested fix:** Add `test_write_batch_duplicate_hashes`: write two entries with the same hash but different embedding values in one call, assert `written == 1`, then `read_batch` and verify only the first embedding is returned. Document the first-wins semantics in the `write_batch` doc comment.
