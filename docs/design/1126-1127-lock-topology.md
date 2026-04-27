# Lock-Topology Design Brief — #1126 + #1127

Source: investigator agent for issues #1126 (`stream_summary_writer` bypasses `WRITE_LOCK`) and #1127 (Daemon serializes ALL queries through one `Mutex<BatchContext>`). Produced 2026-04-26 against post-#1142 main (`9be67800`).

**TL;DR.** Option A (RwLock<BatchContext>) is structurally blocked: `BatchContext` is `!Sync` because it uses `RefCell`/`Cell` for ~12 caches, and `RwLock<T>: Sync` requires `T: Send + Sync`. The minimum-correct, maximum-leverage fix is **Option C (recommended below)**: keep the outer `Mutex<BatchContext>` short, drain it to short critical sections that hand out `Arc`-cloned snapshots, and route `stream_summary_writer` through a new `Store::queue_summary_write()` API that flushes under `begin_write()`. **#1126 must land first** (it's a one-process correctness fix) so the new write path is the API surface #1127's eventual full topology change can lean on.

---

## 1. Current lock topology

| Lock | Site | Scope | Notes |
|---|---|---|---|
| `static WRITE_LOCK: Mutex<()>` | `src/store/mod.rs:53` | process-wide, all `Store<ReadWrite>` instances | Held across `pool.begin()` + tx commit (DS-5). Acquired only by `Store::begin_write()` at `src/store/mod.rs:1046-1060`. |
| `Arc<Mutex<BatchContext>>` | `src/cli/watch.rs:1895` | one Mutex per daemon thread; held entire `dispatch_tokens` call | `handle_socket_client` at `src/cli/watch.rs:188-465` takes `batch_ctx.lock()` at lines 386-390 inside `catch_unwind`, hands `&BatchContext` to `dispatch_tokens`. |
| `BatchContext` interior: `RefCell<Store<ReadOnly>>`, `RefCell<Option<...>>` × 8, `Cell<...>` × 3, `OnceLock<...>` × 3, `RefCell<lru::LruCache<...>>` | `src/cli/batch/mod.rs:227-307` | per-BatchContext, single-threaded by design (`!Sync`) | Comment at line 270: "Single-threaded by design — RefCell is correct, no Mutex needed". `check_index_staleness` does `store.borrow_mut()` (line 460); concurrent reads would panic. |
| `SqlitePool` (sqlx) | `src/store/mod.rs:891-912` | per-Store; `max_connections=4` (RW), `1` (RO); WAL + `busy_timeout=5s` | Pool itself is `Sync`; concurrent reads are fine. Writes serialize on SQLite's exclusive lock — **not** in-process; `WRITE_LOCK` does that. |
| `index.lock` flock | `src/cli/files.rs:185` (`acquire_index_lock`) + line 120 (`try_acquire_index_lock`) | cross-process | `cqs index` blocks; `cqs watch` non-blocking and skips reindex cycle if held. **NOT held by `cqs index --llm-summaries` *during* the LLM stream** — held only across the surrounding `cmd_index` body. |
| `LocalProvider on_item: Mutex<Option<OnItemCb>>` | `src/llm/local.rs:241` | per-provider | Workers acquire under each item; the callback inside is `stream_summary_writer`'s closure captured at `src/store/chunks/crud.rs:537`. |
| rayon implicit pool | `src/cli/batch/handlers/search.rs:296-298`, `src/gather.rs:14`, `src/cli/commands/search/query.rs`, `src/reference.rs`, `src/project.rs` | global | Daemon search/gather handlers spawn parallel work *while holding the `Mutex<BatchContext>`*. The rayon workers themselves don't take the daemon mutex, but they may take `WRITE_LOCK` if a handler calls a `&Store<ReadOnly>` method that internally writes — BatchContext's typestate forbids this (`!Sync` of write methods on `Store<ReadOnly>`), so the deadlock surface in #1127's body is theoretical, not actual, today. |

**Key point — `stream_summary_writer` bypass.** `src/store/chunks/crud.rs:528-565`: closure captures `Arc<SqlitePool>` + `Arc<Runtime>`, fires `INSERT OR IGNORE … .execute(&pool)` per item without `begin_write()`. Three callers: `llm_summary_pass`, `doc_comment_pass`, `hyde_query_pass` (`src/llm/{summary,doc_comments,hyde}.rs`), all and only invoked from `cmd_index` (`src/cli/commands/index/build.rs:23`). **None are reachable from the daemon dispatch path** — `cmd_index` is `BatchSupport::Cli` and `dispatch_gc` (the closest daemon-side cousin) explicitly bails: `src/cli/batch/handlers/misc.rs:467-471`.

**Cross-process implication for #1126.** The race is *not* `cqs watch` × `cqs index --llm-summaries` in the same process. It's:
- `cqs index --llm-summaries` (process A, holds `index.lock`) streams `INSERT OR IGNORE` per row.
- `cqs watch --serve` (process B, watch-loop reindex tries `try_acquire_index_lock`) — this **fails** while A holds the lock, so B skips its reindex. Lock-file mediation hides the race.
- The actual race window is: A's LLM workers fire `stream_summary_writer` *while A's main thread is also writing* (`upsert_chunks_calls_and_prune` etc.). Two writers in the same process, one taking `WRITE_LOCK`, one not. The non-`WRITE_LOCK` path can sneak between A's tx boundaries and trip SQLite busy_timeout (5s) or interleave fsyncs (perf regression).
- Multi-stream: per-row implicit transactions, no batching, 1 fsync per row per stream.

---

## 2. Concurrent scenarios

| Scenario (party A × party B) | Today | Target |
|---|---|---|
| `cqs index --llm-summaries` (main thread reindex) × LLM-worker `stream_summary_writer` (same process) | Race on SQLite exclusive lock; one side may SQLITE_BUSY after 5 s. Per-row fsync flood. | Both serialize through `WRITE_LOCK`. Stream batched (~100-200 ms) under one tx. |
| `cqs index` (process A, holds index.lock) × `cqs index --llm-summaries` (process B) | B blocks on `acquire_index_lock` (mandatory). | Unchanged. Cross-process serialization is by index.lock, not `WRITE_LOCK`. |
| `cqs index --llm-summaries` (process A) × `cqs watch --serve` watch-loop reindex (process B) | B's `try_acquire_index_lock` returns `None`, B skips this cycle. | Unchanged. |
| Two daemon reads (`cqs search` + `cqs callers`) | Serialize on `Mutex<BatchContext>`; second blocks until first's full handler returns. | Parallelize for genuine read paths. Mutator paths (`check_index_staleness` writes via `borrow_mut`, `invalidate`, `clear_caches`, `adopt_embedder`) still need exclusive. |
| Daemon read × outer-watch reindex (same process) | Daemon thread's `Store<ReadOnly>` SELECTs + watch thread's `Store<ReadWrite>` `begin_write` — sqlx pool's reader connection sees WAL frames from RW writer's concurrent commits. WAL allows readers and writer concurrently. Daemon's mutex is irrelevant here (different mutexes). | Unchanged. WAL handles this cleanly already. |
| Daemon read × in-process LLM stream | **Cannot happen** — LLM streams are CLI-only. The daemon's BatchContext owns a `Store<ReadOnly>`. | Same. |
| Multiple in-flight LLM streams firing INSERT-OR-IGNORE concurrently (same process) | Per-row implicit tx, sqlx pool serializes at the connection level (max_connections=4) but no application-level coalescing. Partial writes visible mid-batch. | All streams enqueue into one in-process MPSC; one writer thread drains under `begin_write()`. Atomic batch commit. |
| Daemon shutdown while LLM stream is mid-flight | LLM stream is in the **CLI process**, not the daemon. SIGTERM to daemon doesn't affect the LLM stream process. | Same. (Within `cqs index` process, ctrl-C signals are caught by `signal::setup_signal_handler`; pending queued summaries should drain or flush+abort cleanly.) |
| Migration (PR #1125 territory) × daemon | **Possibly bad**: per audit-findings.md:1330, `restore_from_backup` after a failed migration replaces `index.db` while the daemon's pool holds fds against the old inode. Daemon then serves phantom rows. **Note:** this was structurally fixed by #1125 / PR #1143; the lock-topology fix here must not regress that fix. | The refactor below adds no new long-lived fds and explicitly invalidates BatchContext when `index.db` identity changes (existing `check_index_staleness` already handles this). |
| Daemon mutex held + handler spawns rayon work that, in some hypothetical future, tries to take `WRITE_LOCK` | Theoretical deadlock per #1127 body. **Today: not reachable** — daemon BatchContext is `Store<ReadOnly>`, typestate forbids writes (#946). | Stays not reachable. The fix must not regress this invariant. |
| `cqs llm summary` Claude Batch poll blocks daemon | **#1127 body claims this — it doesn't happen.** The daemon has no Claude route (`dispatch_gc` bails; no LLM commands in `BatchCmd`). What does block: `gather`, `scout`, `task`, large `search` reranks. | Same set of slow paths but they no longer block other readers. |

---

## 3. Recommended fix — Option C: bounded daemon critical section + write-coalescing queue

Both Option A (RwLock outer) and Option B (per-resource locks inside) are wrong shapes for this codebase. Option C is the minimum surgery that fixes both bugs.

### Why not Option A
`RwLock<BatchContext>` requires `BatchContext: Send + Sync`. BatchContext is `!Sync` because:
- `RefCell<Store<ReadOnly>>` (mutated by `check_index_staleness` line 460)
- `RefCell<Option<CachedReload<Config>>>`, `RefCell<Option<CachedReload<AuditMode>>>` (TTL reload)
- `RefCell<Option<Arc<dyn VectorIndex>>>` × 2 (`hnsw`, `base_hnsw`) (lazy init)
- `RefCell<Option<Arc<CallGraph>>>`, `RefCell<Option<Arc<Vec<ChunkSummary>>>>`, `RefCell<Option<Arc<HashSet<PathBuf>>>>`, `RefCell<Option<Arc<Vec<Note>>>>` (data caches)
- `RefCell<lru::LruCache<...>>` (refs LRU)
- `RefCell<Option<SpladeIndex>>`
- `Cell<Option<DbFileIdentity>>`, `Cell<Option<Instant>>`, `Cell<Instant>`

To make Option A work you'd need to convert ~12 cell types to `Mutex`/`parking_lot::RwLock` with internal locking. That is a larger refactor than #1127 implies, and it deletes the comment block at `src/cli/batch/mod.rs:270` that explicitly invests in single-threaded interior. It's also the wrong shape: most of these are write-once (`OnceLock` style after first init) or write-rarely (TTL reload) — making them `RwLock` means every reader pays an atomic compare-exchange in the fast path.

### Why not Option B (verbatim)
"Push the mutex inside BatchContext to per-resource locks" suffers the same `!Sync` conversion. The end state is the same as Option A but with more knobs.

### Option C: keep the outer Mutex, shorten the hold

The daemon mutex's *only* job is to provide `Sync` over a `!Sync` context. Today it's held for all of `dispatch_tokens` — including the rayon-parallel BFS, sqlx queries, file reads. Reduce it to wrapping just the **operations that need exclusive interior** (cache borrows, store re-open). The handler then runs against `Arc`-cloned data outside the lock.

Concretely:

1. **Add `BatchContext::checkout_view()` — short, atomic snapshot.**
   Returns a plain struct of `Arc`s: `Arc<Store<ReadOnly>>`, `Arc<dyn VectorIndex>` (already cached as `Arc`), `Arc<CallGraph>`, `Arc<Vec<Note>>`, `Arc<HashSet<PathBuf>>`, `Arc<Vec<ChunkSummary>>`, `Config` (cheap to clone), `AuditMode` (cheap), `model_config` (cheap clone), and a `&'static OnceLock<Arc<Embedder>>` reference. All pulled inside one `Mutex<BatchContext>` lock; the lock is released as soon as the snapshot returns.
2. **Convert `store: RefCell<Store<ReadOnly>>` to `store: Mutex<Arc<Store<ReadOnly>>>`** so `checkout_view` clones the `Arc` instead of `borrow()`-ing. `check_index_staleness` swaps the Arc inside the Mutex. This keeps the typestate intact.
3. **Rewrite handlers to take a `BatchView` (owned Arcs), not `&BatchContext`.** Most handlers already use `ctx.store()`, `ctx.vector_index()`, `ctx.call_graph()` — these become methods on `BatchView` that just return clones from the stash.
4. **`handle_socket_client` becomes:** lock → `checkout_view` → unlock → run handler against the view → re-lock to bump counters / record telemetry. Lock hold drops from ~30 s worst case to microseconds.
5. **`stream_summary_writer` (#1126):** add a new `Store::queue_summary_write(custom_id, text, model, purpose)` that pushes into a per-`Store` `Arc<Mutex<Vec<PendingSummary>>>` + `Arc<AtomicI64>` last-flush instant. A flush thread (or end-of-batch hook) drains under `begin_write()` and commits one tx for ≤ N rows or every ≤ 200 ms, whichever comes first. Replace `stream_summary_writer`'s closure body to call `queue_summary_write` instead of executing directly.
6. **Final flush hook:** `cmd_index`'s LLM passes call `store.flush_pending_summaries()` (idempotent) before returning, and on signal handler. This guarantees no rows are lost on Ctrl-C between flushes, matching the existing `fetch_batch_results` re-persist contract documented at `src/store/chunks/crud.rs:524-527`.

### Why this is right
- **#1127 closes** because the daemon mutex hold drops from "entire dispatch" to "snapshot + counter bump". The RwLock vs Mutex distinction stops mattering at that hold length.
- **#1126 closes** because every llm_summaries write goes through `begin_write()` → `WRITE_LOCK`. The streaming path becomes a buffered queue + periodic flush, getting correctness AND the per-batch fsync amortization the audit asked for.
- **Theoretical deadlock surface in #1127's body** stops being theoretical-and-possibly-coming-tomorrow. Holding `Mutex<BatchContext>` only for snapshot pulls means no future handler can ever spawn rayon work that needs a shared lock back — the lock is already released.
- **No `!Sync` → `Sync` conversion required.** RefCell/Cell/OnceLock interior stays. The Mutex still provides Sync; we just hold it less.
- **Testable.** Each piece — checkout_view atomicity, write-queue ordering, queue drain on flush — has a clear unit-test seam.

### Tradeoffs
- **Slightly higher per-query overhead** for queries that re-poll a cache mid-flight (the snapshot is taken once; if the cache gets invalidated mid-flight by another connection's `Refresh`, the snapshot serves slightly stale data for the rest of that one query). Acceptable: the existing single-mutex design has the same property at coarser grain.
- **Two new `Arc` clones per query** (cheap; refcount bumps).
- **Write-queue adds a flush thread per Store<ReadWrite>** when LLM features are active. Cheap (idle most of the time) and gated by `#[cfg(feature = "llm-summaries")]`.

---

## 4. File-by-file change list (no code)

### #1126 (write-queue) — must do first

- **`src/store/chunks/crud.rs:497-565`** — replace `stream_summary_writer` body. The returned `OnItemCallback` closure must enqueue into a per-Store `PendingSummaryQueue` (new type on `Store<ReadWrite>`) instead of executing the INSERT. The closure still captures cheap clones (model, purpose). Errors: enqueue is infallible (in-memory).
- **`src/store/mod.rs`** (or new `src/store/summary_queue.rs`) — define `PendingSummaryQueue`: `Arc<Mutex<Vec<PendingSummary>>>` + last-flush `Cell` + `flush_threshold_rows: usize` + `flush_interval: Duration`. `Store<ReadWrite>` gets a field `summary_queue: Arc<PendingSummaryQueue>` initialized in `open` / `open_with_runtime`.
- **`src/store/chunks/crud.rs`** (alongside `stream_summary_writer`) — add `pub fn queue_summary_write(&self, custom_id, text, model, purpose)` which pushes into `summary_queue` and conditionally calls `flush_pending_summaries()` if either threshold (rows ≥ N OR elapsed ≥ flush_interval) is hit. Add `pub fn flush_pending_summaries(&self) -> Result<usize, StoreError>` that drains the queue under one `begin_write()` tx with a single multi-row `INSERT OR IGNORE` (or several batches of ≤ 52 rows like the chunk path does).
- **`src/llm/summary.rs:46`**, **`src/llm/doc_comments.rs:159`**, **`src/llm/hyde.rs:36`** — no signature change; the `set_on_item_complete(store.stream_summary_writer(...))` call still works. After each pass returns (success or failure), call `store.flush_pending_summaries()` to drain residue. Add the same call in the signal-interrupted exit path (`signal::check_interrupted` site in each).
- **`src/cli/commands/index/build.rs:23` (`cmd_index`)** — final `store.flush_pending_summaries()?` before lock drop, after all three LLM passes complete.
- **`src/store/chunks/crud.rs:526-527`** — update the doc comment to describe the new flow ("queue + drain under WRITE_LOCK"); remove the "scope=structural" caveat.
- **Tests:**
  - `tests/local_provider_integration.rs:161` (`item27_streaming_persist_writes_each_item`) — replace assertion that each row is visible immediately with: rows visible after a `flush_pending_summaries()` call, AND visible by the time `llm_summary_pass` returns.
  - `tests/local_provider_integration.rs:249` (`item29_concurrency_produces_equivalent_output`) — extend to assert no SQLITE_BUSY logs and no per-row fsync in trace counters (use the existing `tracing::info_span!("begin_write")` count).
  - New test in `src/store/chunks/crud.rs#tests` (or sibling) covering: queue → flush → all rows committed atomically; ctrl-C between batches loses no committed-pre-flush rows; concurrent reindex + flush serialize cleanly under `WRITE_LOCK`.

### #1127 (daemon snapshot) — second

- **`src/cli/batch/mod.rs:227`** — change `store: RefCell<Store<ReadOnly>>` to `store: Mutex<Arc<Store<ReadOnly>>>`. (Mutex, not RwLock — it's swapped, not concurrently read interior.) Update `check_index_staleness` (line 411-469) and `invalidate` (527+) to lock the Mutex and replace the Arc.
- **`src/cli/batch/mod.rs:710-713` (`fn store`)** — return `Arc<Store<ReadOnly>>` instead of `Ref<'_, Store<ReadOnly>>`. All call sites in `src/cli/batch/handlers/*.rs` already use the result as a value, not a borrow lifetime; the change is mechanical.
- **`src/cli/batch/mod.rs`** (new section) — define `BatchView` struct with the snapshot fields (Arc<Store>, Arc<dyn VectorIndex>, etc.) and `pub fn checkout_view(&self) -> BatchView`. Hold the BatchContext fields' interior locks only across the checkout, not for the dispatch.
- **`src/cli/watch.rs:188-465` (`handle_socket_client`)** — at line 386, replace `let ctx = batch_ctx.lock()...; ctx.dispatch_tokens(...)` with: lock → `checkout_view` → unlock → call new `dispatch_tokens_view(view, command, args, &mut output)` outside the lock. Re-lock only at end to bump `query_count` / `error_count` (or expose those as `AtomicU64` references in the view so no re-lock is needed; they're already `AtomicU64` per line 292/306).
- **`src/cli/batch/mod.rs:600-666`** — split `dispatch_parsed_tokens` into `(parse + idle-bookkeeping)` and `dispatch_via_view(view, cmd, out)`. Call sites:
  - Daemon: lock → checkout_view → unlock → dispatch_via_view.
  - Stdin batch (`cqs batch`): keep current shape, but call `dispatch_via_view(self.checkout_view(), ...)` so the same code path covers both. Hold the lock briefly; this is a single-threaded shell anyway.
- **`src/cli/batch/handlers/*.rs`** — rewrite handler signatures from `fn dispatch_xxx(ctx: &BatchContext, ...)` to `fn dispatch_xxx(view: &BatchView, ...)`. The methods on `BatchView` mirror the read-only subset of methods on `BatchContext` that handlers actually use. All handlers are read-only by design (audit confirmed in `BatchCmd` classification at `src/cli/batch/commands.rs:328-362`), so this is a `&BatchContext`→`&BatchView` rename with no method-body changes for ~95% of handlers.
- **`src/cli/watch.rs:1879-1905`** — comment at lines 1879-1894 (the P2.64 breadcrumb) gets removed/replaced with a comment explaining the new short-hold contract.
- **`src/cli/watch.rs:1928`** — `sweep_idle_sessions` `try_lock` path stays; it still operates on the BatchContext interior, which is fine.
- **Tests:**
  - New test in `src/cli/watch.rs#tests` (next to `spawn_handler` at line 4736): two slow handlers (use a fixture command that sleeps inside) issued concurrently must finish in `~max(t1, t2)`, not `t1 + t2`.
  - New test: `notes list` mid-`gather` returns promptly (specifically tests that #1127 is closed; pre-fix would block).
  - `handle_socket_client_round_trips_stats` (planned in `docs/audit-fix-prompts.md:5660`) — implement now as part of this PR.

### Shared

- **`docs/audit-findings.md:1333`, `docs/audit-findings.md:1376`** — mark resolved with PR refs.
- **`docs/notes.toml`** — add a positive-sentiment note about the new "snapshot pattern" so future agents find it.
- **`CHANGELOG.md`** — entry under unreleased: "Daemon no longer serializes queries through one mutex; LLM summary writes go through `WRITE_LOCK`."

---

## 5. Test plan (one test per closed scenario)

| Closed scenario | Test | Lives in |
|---|---|---|
| Same-process reindex × LLM stream collision | `summary_queue_serializes_with_reindex`: spawn `upsert_chunks_calls_and_prune` and `queue_summary_write`+flush concurrently; assert no SQLITE_BUSY and both transactions complete. | `src/store/chunks/crud.rs#tests` |
| Multi-stream concurrent INSERT-OR-IGNORE | `summary_queue_atomic_under_three_streams`: three threads pushing 100 rows each; final commit count == 300, all in one tx batch (assert via `tracing::info_span!("begin_write")` counter). | `src/store/chunks/crud.rs#tests` or `tests/local_provider_integration.rs` |
| Per-row fsync regression | `summary_queue_amortizes_fsync`: 200 enqueues should result in ≤ 5 commits (assert via begin_write span count). | `src/store/chunks/crud.rs#tests` |
| Stream killed mid-batch (data loss bound) | `summary_queue_loses_at_most_unflushed_window`: enqueue, kill before flush; assert that the next `cmd_index` re-runs successfully and persists the lost rows via `fetch_batch_results`. | `tests/local_provider_integration.rs` |
| Daemon read × daemon read concurrency | `daemon_two_slow_handlers_run_in_parallel`: two `gather` calls should overlap, total wall-clock ≈ max of individual times. | `src/cli/watch.rs#tests` |
| Daemon `notes list` mid-`gather` | `daemon_notes_list_unblocked_by_inflight_gather`: latency of `notes list` while `gather` is running ≤ 200 ms. | `src/cli/watch.rs#tests` |
| Daemon `check_index_staleness` correctness under concurrent reads | `daemon_concurrent_readers_see_consistent_store`: two readers issue queries while a third process replaces `index.db`; both readers either both see old or both see new (no torn snapshot). | `src/cli/watch.rs#tests` (or `tests/daemon_*` integration) |
| Migration × daemon (regression guard for #1125) | `daemon_invalidates_on_index_replacement`: rename-over `index.db`; next daemon query must re-open Store before serving. | `src/cli/batch/mod.rs#tests` (already partially covered by `check_index_staleness` tests; extend assertion). |
| Lock-hold duration regression | `daemon_mutex_hold_under_500us_p99`: instrument with a debug-only timing assertion that the daemon mutex is held for ≤ 500 µs per query. Catches accidental "hold across handler" regressions. | `src/cli/watch.rs#tests` |
| Theoretical deadlock surface stays unreachable | `daemon_handlers_cannot_take_write_lock`: typestate check — assert at compile time that `BatchContext::store()` returns `Arc<Store<ReadOnly>>`, not `Arc<Store<ReadWrite>>`. (Already true; pin it with a `static_assertions::assert_type_eq_all!` or similar.) | `src/cli/batch/mod.rs#tests` |

---

## 6. Fix order — #1126 first, then #1127, in two PRs

**#1126 first.** Reasons:

1. **#1126 is correctness; #1127 is throughput.** Correctness fixes ship first when they don't depend on the throughput change.
2. **#1127's refactor needs to know the write-API surface.** The `BatchView` design is for the **read** path. If we land #1127 first and then change what writes look like, we may discover the view needs to expose write hooks that we haven't designed yet. Better: lock the write API down in #1126 (the `Store::queue_summary_write` + `flush_pending_summaries` pair), then #1127 builds the read snapshot knowing the write surface is stable.
3. **#1126 is cleanly local.** It touches `src/store/chunks/crud.rs`, three LLM passes, one CLI handler, and adds new tests. No changes to `BatchContext` or `handle_socket_client`. Easier to review, easier to revert.
4. **#1127 is invasive.** Splitting `BatchContext` → `BatchView`, rewriting ~20 handlers, changing `dispatch_parsed_tokens`. A bigger PR. Building on top of a stable, in-tree #1126 is safer than landing both at once.
5. **The "deadlock surface" in #1127's body is theoretical today.** Daemon's `Store<ReadOnly>` typestate prevents writes; rayon work inside the daemon mutex doesn't take `WRITE_LOCK` (it can't — `WRITE_LOCK` is taken only by `Store<ReadWrite>::begin_write()`). So #1127 has no actual urgency from the deadlock angle — only from the "all-readers-block" UX angle. That UX issue is real but not corruption.

**Anti-arguments and rebuttals.**
- *"Land them together because they're conceptually one fix."* No. They're conceptually one design (lock-topology) but two distinct user-visible bugs. Splitting halves the review surface. Both PRs reference this brief.
- *"#1127 first because the daemon-mutex hold is what makes #1126 dangerous."* No — #1126's race is in `cqs index`, not in the daemon. The daemon path doesn't fire `stream_summary_writer`. Doing #1127 first leaves #1126 unfixed for an arbitrary window.

---

## 7. Open questions for the implementer

1. **Flush threshold.** What N (rows) and T (ms) make the right tradeoff? Starting guess: `N=64, T=200ms`. Run a benchmark on the local LLM path (`tests/local_provider_integration.rs::item27`) before committing to numbers.
2. **Flush timer mechanism.** Three options: (a) per-enqueue check (cheap, may add a few ms latency to last-row persist on idle workloads); (b) dedicated `std::thread` that sleeps T and flushes (one extra thread per `Store<ReadWrite>` with LLM features); (c) hook into the existing tokio runtime via `tokio::time::interval` (clean but adds a runtime dependency to the queue type). Recommend (a) plus a final flush in `cmd_index`. (b) is overkill for an interactive tool.
3. **Per-Store vs per-process queue.** The audit suggested `Mutex<Vec<(id, text)>>`. Design is per-`Store<ReadWrite>`; that means two `Store::open` calls in the same process get separate queues. Verify by grepping `Store::open` callers — today only `cmd_index` opens RW.
4. **Snapshot freshness (relevant to #1127, not #1126).** If a daemon query starts at t=0, gets a `BatchView`, and at t=15s the index is replaced, the in-flight query keeps using the old `Arc<Store>`. Acceptable: matches today's behavior. Pin with `daemon_inflight_query_uses_stable_snapshot`.
5. **`Refresh` handler semantics (relevant to #1127).** `dispatch_refresh` calls `ctx.invalidate()` which mutates BatchContext interior. After the refactor, takes the outer Mutex's lock, does the invalidation, drops. `Refresh` is rare and "stop the world" anyway.
6. **sqlx pool behavior under simultaneous read + write tx in the same process.** WAL + busy_timeout should handle this, but validate by stress test (1000 concurrent SELECTs while `begin_write` flushes a 64-row batch). If the read side hits SQLITE_BUSY it indicates a deeper sqlx config issue.
7. **`flush_pending_summaries` visibility.** `pub(crate)` is the right level — only the LLM passes and `cmd_index` should call it.
8. **`BatchView` lifetime (relevant to #1127).** Owned via Arcs; don't borrow from `BatchContext`. Simpler and matches the design.
9. **Notes write paths from the daemon.** `BatchCmd::Notes` is read-only. `cqs notes add` is CLI-only and uses its own flock on `notes.toml.lock`. Verified unaffected.
10. **Backpressure on the queue.** What if a runaway LLM job pushes 100k rows before flush runs? Add a hard cap (e.g. `Vec::with_capacity(10_000)` and synchronous flush at cap). Trivial to add and bounds worst-case memory.
11. **Existing notes/comments on RefCell discipline.** `src/cli/batch/mod.rs:270` says "Single-threaded by design — RefCell is correct, no Mutex needed." After the refactor, this is half-true (still single-threaded for everything except `store`). Update the comment.
