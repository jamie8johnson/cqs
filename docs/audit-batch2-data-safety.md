# Data Safety Audit (post-v1.38.0)

Eight findings. Three concern #1452 (`needs_embedding` flag) wiring gaps in
read paths that pull zero-vec sentinels into derived data structures
(CAGRA / UMAP / neighbor brute-force / base HNSW). Two concern slot-aware
path resolution drift in the batch daemon. The remainder cluster around
HNSW load-vs-save TOCTOU and a backup-prune disk-burn regression.

#### DS-V1.38-1: `embedding_batches` does NOT filter `needs_embedding=0` — CAGRA / UMAP / neighbors load zero-vec sentinels
- **Difficulty:** easy
- **Location:** `src/store/chunks/async_helpers.rs:549-624` (`EmbeddingBatchIterator::next`)
- **Description:** The sibling `EmbeddingHashBatchIterator` (line 660) was patched for #1452 to add `AND needs_embedding = 0`. The non-hash variant — exposed via `Store::embedding_batches()` and `Store::embedding_base_batches()` — was not. Production callers that consume zero-vec sentinels:
  - `src/cagra.rs:780` (CAGRA build) — pushes zero-vecs straight into the cuVS `flat_data` buffer; the resulting CAGRA index advertises a search neighborhood of "things near (0,0,...,0)" for every #1452 chunk. CAGRA is the default backend at chunk_count ≥ 5000 + GPU available.
  - `src/cli/commands/index/umap.rs:86` (UMAP projection) — projects zero-vecs alongside real ones; cluster view collapses #1452 chunks to a single (or pathological) point.
  - `src/cli/commands/search/neighbors.rs:108` (brute-force kNN) — `dot()` against zero-vec returns 0, contaminating the result set with low-relevance hits.

  `embedding_base_batches` is incidentally safe because the v18→v19 upsert path writes `embedding_base = NULL` for #1452 chunks (per the bind at async_helpers.rs:370-374) and the SELECT filters `embedding_base IS NOT NULL`. The plain `embedding` column gets the zero-vec sentinel by design (see batch_insert_chunks line 353), so any caller of `embedding_batches` is exposed.

  The foreground enriched HNSW build is safe because `build_hnsw_index_owned` uses `embedding_and_hash_batches` (the patched iterator). The CAGRA build is the same shape but uses the unpatched iterator.
- **Suggested fix:** In `EmbeddingBatchIterator::next` (line 565), append `AND needs_embedding = 0` to the SQL string. The base column path already filters `embedding_base IS NOT NULL`, so a single concatenation handles both column variants. Add a unit test that inserts a chunk via `upsert_chunks_unembedded_batch` and asserts `embedding_batches` does NOT yield it.

#### DS-V1.38-2: `enrichment_pass` clears `needs_embedding=0` but never repopulates `embedding_base` — base HNSW permanently misses #1452 chunks
- **Difficulty:** medium
- **Location:** `src/store/chunks/crud.rs:468-477` (`update_embeddings_with_hashes_batch`'s `UPDATE chunks SET ...`) and `src/store/chunks/async_helpers.rs:425` (ON CONFLICT clause)
- **Description:** Two compounding behaviors leave `embedding_base` permanently NULL for any chunk that ever passed through `--llm-summaries` first-pass-skip:
  1. `upsert_chunks_unembedded_batch` writes `embedding_base = NULL` on initial insert (correct per the comment at async_helpers.rs:363-369).
  2. `update_embeddings_with_hashes_batch` (the enrichment-pass embedding writer) updates `embedding`, `enrichment_hash`, and `needs_embedding=0` — but NOT `embedding_base`. So after enrichment, the chunk has a real `embedding`, `needs_embedding=0`, and a permanent `embedding_base = NULL`.
  3. The ON CONFLICT clause in `batch_insert_chunks` (line 425) overwrites `embedding_base = excluded.embedding_base` on EVERY conflict where `content_hash` or `parser_version` changed. So a content-changed re-upsert via `upsert_chunks_unembedded_batch` overwrites a previously-good `embedding_base` with NULL, and the same enrichment gap means it never recovers.

  Net effect: every `--llm-summaries` reindex permanently degrades the base HNSW (`build_hnsw_base_index` filters `WHERE embedding_base IS NOT NULL`), which is the routing target for DenseBase strategy (conceptual / behavioral / negation queries — exactly the queries where enriched embeddings hurt). The `--llm-summaries` performance win (#1452, ~halves GPU time) silently trades search quality on those query classes.
- **Suggested fix:** In `update_embeddings_with_hashes_batch`, when the row's prior state was `needs_embedding=1` AND `embedding_base IS NULL`, also write `embedding_base = t.embedding`. (Same source bytes as `embedding` because enrichment_pass first writes the raw NL embedding; the post-enrichment overwrite of `embedding` only happens later on the second pass.) Or — restructure the pipeline so the very first enrichment write fills `embedding_base` before the call-context enriched embedding lands in `embedding`. Add a regression test: insert via `upsert_chunks_unembedded_batch`, run enrichment, assert `base_embedding_count() == 1`.

#### DS-V1.38-3: `BatchContext::check_index_staleness` watches the LEGACY index path, not the active slot — daemon caches go stale silently
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:483, 607, 788, 2043, 2337` (every `cqs_dir.join(INDEX_DB_FILENAME)` site)
- **Description:** `BatchContext::cqs_dir` is set in `create_context_with_runtime` to `cqs::resolve_index_dir(&root)`, which returns the project `.cqs/` directory (line 2317). Then `check_index_staleness` (line 483) and `from_path` (line 2337) both watch `cqs_dir.join(INDEX_DB_FILENAME)` — i.e. `.cqs/index.db`. After PR #1105 (per-slot directories), the actual store opens at `.cqs/slots/<name>/index.db`. The staleness check now targets a path that doesn't exist on slot-migrated projects: `DbFileIdentity::from_path` returns `None`, the warn at line 491 fires once, and the daemon's mutable caches (HNSW, SPLADE, call_graph, file_set, notes, refs) NEVER invalidate when the operator runs `cqs index`. Operator workflow: edit code → `cqs index` → daemon keeps serving stale results.
  
  The five sites at lines 483, 607, 788, 2043, 2337 all share the same path-construction pattern. The Store opens correctly via `resolve_index_db` (line 2318) which DOES honor slot resolution; only the staleness-tracking `cqs_dir.join` sites drifted.
- **Suggested fix:** Resolve the active slot once at `BatchContext` construction and store the slot dir alongside (or in place of) `cqs_dir`. Replace each `cqs_dir.join(cqs::INDEX_DB_FILENAME)` with the slot-aware path. Add an integration test that creates `.cqs/slots/default/index.db`, opens a `BatchContext`, touches the slot DB, and asserts the next staleness check observes the change.

#### DS-V1.38-4: HNSW `load_with_dim` existence check happens BEFORE the shared lock — concurrent saver can rename files out from under reader
- **Difficulty:** medium
- **Location:** `src/hnsw/persist.rs:706-739` (existence check at 706, lock acquisition at 734-758)
- **Description:** The reader sequence is:
  1. Check `graph_path.exists() && data_path.exists() && id_map_path.exists()` (line 706). If any missing → return `NotFound`.
  2. Open + `try_lock_shared` on `<basename>.hnsw.lock` (line 734-752).
  3. Call `verify_hnsw_checksums` and proceed.

  Between steps 1 and 2, a concurrent `save()` can take the exclusive lock and rename graph/data/ids files into `.bak`. The reader then waits up to ~1s for the shared lock; if save finishes within that window, the reader proceeds with files that have already been replaced — `verify_hnsw_checksums` reads the NEW bytes vs an OLD checksum file (or vice versa, depending on rename order) and fails. The user sees a confusing checksum-mismatch error during a normal concurrent rebuild instead of either a clean retry or a clean read of the new index.

  The deeper hazard: the HNSW save renames extensions one at a time inside `rename_result` (lines 527-559). There is no atomic "all four files swap together" primitive on POSIX. A reader that grabs the shared lock between the `graph` rename and the `data` rename observes a graph from save N+1 paired with data from save N — a corrupt index that may pass checksum verification if the per-file checksum file was also half-renamed in the same window.
- **Suggested fix:** Move the file-existence check INSIDE the shared-lock critical section (re-check after `try_lock_shared` succeeds). For the half-renamed concern: the saver should write the NEW checksum file LAST so a reader observing a mismatched set fails the checksum check rather than loading corrupt data. Even better — bundle all four files into a single `<basename>.hnsw.bundle` and atomically replace the bundle, eliminating the half-state entirely. (Larger refactor; the existence-check-under-lock fix is the easy mitigation.)

#### DS-V1.38-5: Migration backup `prune_old_backups` swallows `read_dir` errors and emits `tracing::error!` only — no operator-actionable signal
- **Difficulty:** easy
- **Location:** `src/store/backup.rs:243-258` (post-DS-V1.36-10 partial fix)
- **Description:** DS-V1.36-10 was triaged as a P3 fix. The current implementation upgrades the failure log from `warn!` to `error!` with an `approx_dir_bytes` field, but still returns `Ok(())` to the caller. The migration's caller at `src/store/migrations.rs:122-126` already classifies prune failure as "non-fatal — the user's DB is at the correct version", which means a transient permission glitch that also happens during every subsequent migration produces:
  1. New backup written successfully (`copy_triplet`).
  2. `read_dir` for prune fails again → warn-then-Ok.
  3. Loop repeats indefinitely; `.bak-v*-v*-*.db` accumulates one new backup per migration with zero KEEP_BACKUPS enforcement.

  The `approx_dir_bytes` log addition helps post-mortem but doesn't solve the unbounded growth. On a heavily-migrating CI rotating through schema versions, this is hundreds of MB per run.
- **Suggested fix:** Track a per-process counter of consecutive prune failures; on the second failure, switch from "Ok with log" to surfacing the error so the migration caller can downgrade to a more visible warn (or emit a metric). Alternative: enforce KEEP_BACKUPS as a HARD CAP — if the prune step can't even read the dir, then before the NEXT migration, refuse to write a new backup unless the operator clears the dir manually. The "fail-loud-if-unbounded-growth" stance matches the spirit of CQS_MIGRATE_REQUIRE_BACKUP=1.

#### DS-V1.38-6: `WRITE_LOCK` is process-global, not per-Store — multi-store callers serialize all writes globally
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:54` (`static WRITE_LOCK: Mutex<()> = Mutex::new(());`)
- **Description:** The `static` Mutex serializes writes across every `Store` instance in the process. SQLite is one-writer-per-database, so within a single `Store` this is correct. But multiple `Store` instances against DIFFERENT databases (e.g. ReferenceIndex-cached refs in the daemon's `refs` LRU; eval scripts opening multiple slot DBs in parallel; future multi-tenant daemon serving slot A and slot B) all contend on the same global mutex. The daemon's reference indexes (`Arc<Mutex<lru::LruCache<String, Arc<ReferenceIndex>>>>` at batch/mod.rs:291) are read-only on the dispatch path so this isn't exploited today, but a future write-side feature (e.g. background ref reindex) would block unrelated slot-A writes on slot-B's pending writer.

  Additionally: `MutexGuard<'static, ()>` is held across `pool.begin().await` (line 1206-1207). If the await-suspends-the-task pattern lands in tokio (current sqlx behavior is non-suspend, but contract is async) this could deadlock the daemon. The DS-5 comment at line 1 acknowledges the cross-await hold.
- **Suggested fix:** Replace the `static` with a `Mutex<()>` field on `Store`. Each Store instance gets its own write lock, which is the correct granularity. Cross-process writer exclusion is already provided by SQLite's file locking. Run the existing concurrent-writer test suite to ensure the per-Store lock is sufficient (it should be — every existing test opens one Store).

#### DS-V1.38-7: `write_active_slot` uses `crate::temp_suffix()` for tmp-file naming but does NOT hold the `slots.lock` — read-modify-update of active_slot races with concurrent `slot promote`
- **Difficulty:** easy
- **Location:** `src/slot/mod.rs:647-694` (`write_active_slot`) — caller contract issue, not the function itself
- **Description:** `write_active_slot` validates the name and atomically replaces `.cqs/active_slot`. The DS-V1.33-2 fix added `temp_suffix()` so concurrent writers don't collide on a fixed `active_slot.tmp` name. But the function itself takes no lock — it relies on every CALLER holding `acquire_slots_lock` first. Production callers do (`slot_promote` at slot.rs:292, `slot_create` at slot.rs:239). However, three caller sites bypass the lock:
  - `migrate_legacy_index_to_default_slot` at slot/mod.rs:997 — runs INSIDE the slots lock (line 844), correct.
  - Test callers at slot/mod.rs:1248, 1296, 1308, 1320 — fine, single-threaded.
  - **Future risk**: any new caller that forgets the lock (e.g. a `cqs slot set-default` shortcut) silently loses updates. The function's doc comment doesn't state the lock requirement.
- **Suggested fix:** Either (a) acquire `slots.lock` inside `write_active_slot` itself (idempotent — slots.lock is reentrant via OS-level flock), making the function self-contained, or (b) add a `#[must_use]` / explicit `&FlockGuard` parameter that forces the caller to prove they hold the lock. Update the function doc to state the lock requirement either way.

#### DS-V1.38-8: v26→v27 migration adds `needs_embedding` column — but enrichment_pass on a freshly-migrated DB sees zero `needs_embedding=1` rows, so nothing forces a base-embedding repopulation
- **Difficulty:** medium
- **Location:** `src/store/migrations.rs:984-1010` (migrate_v26_to_v27) + interaction with `enrichment_pass`
- **Description:** The v26→v27 migration adds the column with `DEFAULT 0`. Pre-existing rows are stamped `needs_embedding=0`, treating them as already embedded. That's correct from a content-bytes standpoint — they DO have a real embedding. But the migration silently inherits one bug from the prior schema: any pre-v18 row that never went through Phase 5 has `embedding_base = NULL` (per migrate_v17_to_v18 at line 622-627). The v27 migration doesn't touch `embedding_base`, so post-migration these rows remain invisible to `build_hnsw_base_index`. The user sees no log indicating their base-HNSW coverage is partial.

  Compounding with DS-V1.38-2: a v27-migrated user who then runs `cqs index --llm-summaries` on changed content will write `embedding_base = NULL` for those changed chunks (per ON CONFLICT clause), and enrichment never repopulates. So the base index coverage erodes monotonically over time on `--llm-summaries` workflows.
- **Suggested fix:** Add an explicit operator-visible signal: after migration to v27, log the count of `embedding_base IS NULL AND needs_embedding = 0` rows at `info!` level. Once DS-V1.38-2 is fixed, queue these for re-embedding via the enrichment_pass `needs_embedding=1` mechanism — trigger the repopulation by a one-shot `UPDATE chunks SET needs_embedding=1 WHERE embedding_base IS NULL` in the v27 migration so the next index pass actually fills them. (This is the non-trivial fix; the log-only mitigation is the easy variant.)
