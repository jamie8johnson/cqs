# Resource Management Audit (post-v1.38.0)

Scope: idle daemon cost, cache TTLs, ONNX session lifecycle, subprocess buffer caps, unbounded buffer growth, cross-slot embedding cache cap enforcement.

Prior context: triage `docs/audit-triage.md` (v1.36.2). RM-V1.36-1/2/4 closed in #1456; #1471 dropped daemon idle CPU to ~0; #1502 trimmed HNSW id_map RSS via `Box<str>`.

## Findings

#### RM-V1.38-1: EmbeddingCache eviction never fires on a query-only daemon
- **Difficulty:** easy
- **Location:** `src/cli/watch/mod.rs:1390-1400` (gated inside the reindex branch); `src/cli/batch/mod.rs:867-873` (one-shot `warm()` evict only)
- **Description:** `evict_embeddings_cache_with_runtime` is wired in three places: `cqs index` pipeline tail, `BatchContext::warm()` once at daemon startup, and the watch reindex branch (1 hr throttle). A daemon serving only queries — no file events for hours/days — does not enter the reindex branch and so never trims `embeddings_cache.db` after the boot-time call. `CQS_CACHE_MAX_SIZE` (default 10 GB) and `CQS_QUERY_CACHE_MAX_SIZE` (100 MB) become advisory until the next file-change burst. Worst case: a small project queried heavily (eval runs, multi-agent batch) without code edits accumulates blob writes via the search path's caching of new embeddings without ever hitting the cap-enforcement path.
- **Suggested fix:** add an idle-tick gated cache evict alongside `sweep_idle_sessions` in `daemon.rs:130` (the existing 60-s minute tick) — open the cache, call `evict()`, drop, on a 1-hr cadence regardless of file-event activity. Mirror the existing throttle counter from `mod.rs:1213` (`last_cache_evict`) but located in the daemon-thread loop.

#### RM-V1.38-2: LocalProvider stash cap is per-batch-count, not per-byte
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:73, 427-449`
- **Description:** `MAX_STASH_BATCHES = 128` evicts old batches when the count exceeds 128, but a single batch holds `HashMap<custom_id, response_text>`. With LLM summary passes producing 5–10 KB responses for tens of thousands of items per batch, one un-fetched batch can be 100s of MB; 128 of them is multi-GB. The eviction key is `keys().min()` (lex-smallest UUID) — random eviction order means a slow consumer can't predict survival of its own batch.
- **Suggested fix:** add a byte budget alongside the count cap (e.g. `MAX_STASH_BYTES = 256 * 1024 * 1024`). Track running total during insert, evict by oldest-insert (use `IndexMap` for FIFO order, not lex UUID) until under both budgets. Lex-min eviction is documented as "evictee is unfetched — that's a leak signal" but the order is still arbitrary; FIFO is no harder.

#### RM-V1.38-3: PLACEHOLDER_CACHE backing Vec is permanently sized to 32,467 OnceLocks
- **Difficulty:** medium
- **Location:** `src/store/helpers/sql.rs:76-84`
- **Description:** `PLACEHOLDER_CACHE: Vec<OnceLock<String>>` is allocated at length 32,467 (≈ 520 KB metadata) on first use and lives for process lifetime. Each `OnceLock<String>` populated by a query stays populated forever — there is no eviction. A daemon that hits varied batch sizes (e.g., 47, 234, 1,000, 8,116) over its lifetime accumulates one full placeholder string per distinct n; the largest single string is ~190 KB at n = 32,466 (`?32466` × 32466). Common batch sizes total ~1–2 MB; a daemon that touches every distinct n on a busy reindex burst tops ~3 GB worst case. The doc-comment dismisses memory cost as "microseconds to allocate" but only counts the metadata, not populated cells.
- **Suggested fix:** populate strings only for n the daemon actually uses (already true — lazy), but cap the Vec length at a smaller bound (e.g. `MAX_CACHED_PLACEHOLDER_N = 4096`, covers all observed prod usage) and fall back to `build_placeholders` for larger n. Past 4 K, the per-call build cost is microseconds and the saved memory is real.

#### RM-V1.38-4: UMAP subprocess `wait_with_output` buffers full stdout/stderr
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/umap.rs:171-180`
- **Description:** `child.wait_with_output()` buffers the entire stdout (which carries `n_rows` × ~64-byte coordinate lines, ~64 MB at 1 M chunks) and unbounded stderr in RAM before yielding `output.stdout`. A python script wedged in a tight error-print loop — or a UMAP run on a multi-million-chunk corpus — will OOM the indexer process. Sibling fix RM-V1.36-2 hardened `pdf_to_markdown`; this site shipped after that PR landed.
- **Suggested fix:** stream stdout line-by-line via `BufReader::lines().take(MAX_LINES)` per the v1.36.2 RM-V1.36-7 pattern (with per-line cap). Cap stderr capture at 64 KB (truncate-with-marker) — operators only need the tail for diagnostics.

#### RM-V1.38-5: `export_model.rs` ONNX export uses unbounded `Command::output()`
- **Difficulty:** easy
- **Location:** `src/cli/commands/train/export_model.rs:60-62, 74-86`
- **Description:** Two `Command::output()` calls (deps probe + actual `optimum.exporters.onnx` invocation) buffer subprocess stdout/stderr unbounded. The second call invokes Python optimum, which on a large model export prints multi-MB progress logs; on a wedged HuggingFace download it can hang for hours while RAM grows. Same pattern flagged in RM-V1.36-5 (chm/convert/train_data) but `export_model` was missed in the sweep.
- **Suggested fix:** spawn + `take(MAX_BYTES)` on both stdout and stderr handles with `wait_timeout` (e.g. 30 min for the export, 30 s for the deps probe). Apply the same `RM-V1.36-5` fix shape.

#### RM-V1.38-6: BatchContext SPLADE encoder slot held forever even after data-cache eviction
- **Difficulty:** medium
- **Location:** `src/cli/batch/mod.rs:294 (field), 403-456 (sweep_idle_sessions)`
- **Description:** `sweep_idle_sessions` clears the SPLADE *session* (line 427-433) after `CQS_BATCH_IDLE_MINUTES`, and the data caches (HNSW, splade_index, call_graph, test_chunks, file_set, notes_cache) after `CQS_BATCH_DATA_IDLE_MINUTES`. But the `splade_encoder: Arc<OnceLock<Option<SpladeEncoder>>>` itself is never reset to `None`; only its inner ONNX session is freed. The `SpladeEncoder` struct still holds the tokenizer (~10 MB BPE), the decoded vocab map, and pinned model paths. On a long-idle daemon this is small but real, and the asymmetry with the embedder/reranker (which share the same `clear_session` contract) is surprising. More importantly: a slot/model swap that changes the SPLADE config can't take effect because `OnceLock` has no `take()` mid-flight.
- **Suggested fix:** wrap the slot in `Mutex<Option<...>>` (mirroring `splade_index`) and have the data-cache eviction branch null it out. Already paired with the data caches (28 mins of idle = no SPLADE work coming) so dropping the encoder costs nothing on the steady-state path.

#### RM-V1.38-7: `last_indexed_mtime` prune tied to size threshold, not age — long-idle daemon never trims
- **Difficulty:** easy
- **Location:** `src/cli/watch/gc.rs:60-70`; `src/cli/watch/events.rs:258`
- **Description:** `prune_last_indexed_mtime` early-returns when `map.len() <= 5_000` (default). A daemon that has indexed 4,999 files and then sits idle for weeks holds those 4,999 entries forever — `process_file_changes` is the only caller and only fires on file events. On a small project this is fine; on a slowly-growing project that crosses the threshold during a burst then quiesces, the prune fires once and the trigger never re-arms. The age cutoff (`LAST_INDEXED_PRUNE_AGE_SECS`) only matters when the size gate opens.
- **Suggested fix:** add a periodic age-based prune call from the watch loop's idle branch (already runs `cycles_since_clear` book-keeping at `mod.rs:1413`), gated to once per hour. Reuses the same cutoff logic; makes the trim bounds time-shaped rather than peak-shaped.

#### RM-V1.38-8: SOCKET-thread tokio runtime worker count not adaptive — fixed at startup
- **Difficulty:** medium
- **Location:** `src/cli/watch/runtime.rs:56-77`
- **Description:** `build_shared_runtime` resolves `worker_threads = min(num_cpus, 4)` at process start and never adjusts. On idle, all 4 workers stay parked but pin their stack (default 2 MB each = 8 MB). On a 16-core workstation under heavy load, `CQS_DAEMON_WORKER_THREADS` is the only escape — but no per-load adjustment. Tokio doesn't shrink multi-thread runtimes; this is a tokio-design constraint. Worth noting because the doc says "shrink workers on idle" is an audit cut to make.
- **Suggested fix:** keep multi_thread for hot-path concurrency; document explicitly in `runtime.rs` that workers are static for the daemon's life and any operator wanting different sizing must restart with `CQS_DAEMON_WORKER_THREADS`. Optionally: set `thread_stack_size(512 * 1024)` to bound the parked-worker RSS contribution to 2 MB instead of 8 MB.

#### RM-V1.38-9: Notes cache (`Arc<Vec<Note>>`) has no size accounting; eviction tied only to mtime/idle
- **Difficulty:** medium
- **Location:** `src/cli/batch/mod.rs:282 (notes_cache field)`; `src/store/metadata.rs:338-361 (cached_notes_summaries)`
- **Description:** The daemon's `notes_cache` field caches the full `Vec<Note>` for the project. Notes are typically small (KB), but a project running heavy `cqs notes add` ingestion (e.g. importing a large external observation set) can grow this past 10s of MB. There is no max-size cap; eviction depends on the data-cache idle timer (default 30 min) or an index-mtime change. Symmetric for `Store::notes_summaries_cache` (RwLock-backed inside Store) — both grow with note count without an upper bound.
- **Suggested fix:** add a soft cap on `notes_cache` (e.g. 5,000 notes or 32 MB serialized), beyond which the cache returns from the DB on each call instead of caching. For the typical project (200–500 notes) the cap never trips; for runaway note-churn cases the daemon stops accumulating.

#### RM-V1.38-10: `LocalProvider` `pool_max_idle_per_host = concurrency` survives forever via `OnceLock` reuse
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:157-162`
- **Description:** The `reqwest::Client` is built with `pool_max_idle_per_host(concurrency)` and `pool_idle_timeout(30s)` — sound. But the `LocalProvider` itself is constructed once per CLI invocation; in the daemon path, it's held inside the `LlmClient` for the daemon's lifetime. The 30-s idle pool prune happens, but each client thread that hits `submit_via_chat_completions` over time accumulates *crossbeam_channel* sender/receiver pairs (line 221). They're cleaned via `std::thread::scope`'s join, so per-batch — but if `submit` is interleaved across many small batches against many distinct local LLM hosts (e.g. multi-server eval comparing vLLM endpoints), each new endpoint string spawns a fresh in-memory pool with no cross-batch reuse. Minor — flagged for the audit cut about "connection lifecycle."
- **Suggested fix:** confirm only one `LocalProvider` exists per `LlmClient` per host (looks correct from current callers but no unit test). Add a `Drop` impl that explicitly clears `self.stash` and logs un-drained batches at warn — an operator hitting the `MAX_STASH_BATCHES` warning has no easy way to spot the leak without it.

## Summary

Ten findings, mostly easy. The dominant theme: **TTL/eviction logic is well-built but tied to file events** rather than wall-clock time. A daemon that serves queries 24/7 without code changes drifts past every cap (`embeddings_cache.db`, `query_cache.db`, `last_indexed_mtime`, SPLADE encoder slot, notes_cache) — the eviction infrastructure exists but the periodic-tick wiring is incomplete on the read-only path. RM-V1.38-1 is the highest impact (cache eviction never fires), then RM-V1.38-4/-5 (subprocess output buffering, sibling regressions to RM-V1.36-2/-5). RM-V1.38-3 (PLACEHOLDER_CACHE 32k slots) is medium-impact but documented as "by design" — the cap is the right shape, not the OnceLock-per-n approach. RM-V1.38-2 closes a per-batch-bytes gap in the prior "128 batch count cap" fix. The remaining items (RM-V1.38-6/-7/-9/-10) are smaller asymmetries worth tightening but not single-handedly load-bearing.

The pattern to extract for the v1.38.x triage: **every cap that depends on a file-event tick needs a parallel idle-tick path** — the daemon's poll-driven loop (#1471) made the file-event-tick assumption stale.
