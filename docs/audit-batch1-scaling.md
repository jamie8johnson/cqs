## Scaling & Hardcoded Limits — v1.38.0+ post-PR-#1503

Prior SHL-V1.36-1..10 are all fixed (verified `store/mod.rs:734-745`,
`reference.rs:208`, `cli/commands/index/build.rs:1306`, `cagra.rs`,
`reranker.rs:89-112`, `cli/watch/reindex.rs:236`, `lib.rs:475` (32766
limit text), `cli/pipeline/types.rs:143-158`, `embedder/mod.rs:343`,
`cli/watch/socket.rs:51-61`).

The findings below are NEW since v1.36.2 — none of them duplicate the
SHL-V1.36 series.

#### SHL-V1.38-1: `MAX_PENDING_REBUILD_DELTA = 5_000` doesn't scale with embedding dim
- **Difficulty:** easy
- **Location:** `src/cli/watch/rebuild.rs:81-87`
- **Description:** Cap on per-rebuild HNSW delta entries. Comment explicitly says "5,000 × 1024 dim × 4 bytes ≈ 20 MB worst case" — same dim-blind anti-pattern as SHL-V1.36-3/4/5 (which were fixed). At 4096-dim (Qwen3-style) this becomes 80 MB held in memory until the next swap; at SPLADE-Code 1024-hidden / 2560-output it's larger still. No env override exists, so an operator on a wide-dim model can't shrink the cap. The comment's own arithmetic outdates the constant.
- **Suggested fix:** Pull the same `cqs::limits::dim_scaled_batch(5_000, dim, 500, 50_000)` helper used by `hnsw_batch_size` (`build.rs:1306-1311`), reading `dim` from the rebuild context's `store.dim()`. Add `CQS_PENDING_REBUILD_DELTA_MAX` env override matching the other `CQS_HNSW_*` knobs.

#### SHL-V1.38-2: `--require-fresh-secs` silently capped at hardcoded `600u64` literal
- **Difficulty:** easy
- **Location:** `src/cli/commands/eval/mod.rs:294-307`
- **Description:** SHL-V1.30-3 was fixed in #1235 by adding a warn — but the cap value itself is still a `600u64` literal hardcoded twice in the source (line 294 and 297, with the message "capped at 600 s (built-in eval ceiling)"). The `wait_for_fresh` defense-in-depth ceiling is `86_400 s`, so the eval-side ceiling has 144× headroom but won't budge. On a fresh checkout that triggers a full reindex of a 100K-chunk repo, embedder warmup + index build can exceed 10 min — the eval gate then fails after 600s of waiting even when the operator passed `--require-fresh-secs 1800`. The clamp is documented but not overridable.
- **Suggested fix:** Replace `600u64` with `crate::limits::parse_env_u64("CQS_EVAL_FRESH_BUDGET_CEILING", 600)`, mirroring the `CQS_*` env-knob pattern used everywhere else. Keep the warn (it's correct UX), but let an operator on a slow indexer push the ceiling up.

#### SHL-V1.38-3: `PIPELINE_FAN_OUT_LIMIT = 50` silently truncates batch pipelines, no env override
- **Difficulty:** easy
- **Location:** `src/cli/batch/pipeline.rs:10-12, 269-278, 340-347`
- **Description:** Pipeline command (`cqs callers foo | scout`) caps fan-out at 50 names per stage. Truncation is logged at `tracing::info`, but the limit itself is hardcoded. With Claude Code Tasks dispatching agents that build pipelines from `cqs callers <hub>` (>100 callers on hot functions like `Store::search_filtered` or `Embedder::embed_query`) the silent truncation drops half the call graph downstream. No `CQS_PIPELINE_FAN_OUT` knob; comment says "3-stage pipeline dispatches at most 1 + 50 + 50 = 101 calls" — but 101 is not the cost driver, the inner per-call latency (~50ms via daemon) is.
- **Suggested fix:** Add `CQS_PIPELINE_FAN_OUT` env knob with default 50, clamping `[10, 1000]`. Consider raising default to 100 — agents are the primary user and a 100-name fan-out is ~5s at daemon latency, not painful.

#### SHL-V1.38-4: Daemon socket request line capped at 1 MB while CLI accepts 50 MB
- **Difficulty:** medium
- **Location:** `src/cli/watch/socket.rs:113, 124` vs `src/cli/limits.rs:90`
- **Description:** `cqs review --stdin` and `cqs impact --diff` accept `MAX_DIFF_BYTES = 50 * 1024 * 1024` (env-overridable via `CQS_MAX_DIFF_BYTES`) on the CLI path. The same commands routed through the daemon hit a hardcoded 1 MB cap on the socket line (`take(1_048_577)` + post-hoc `n > 1_048_576`). Operators with a 5 MB squash-merge diff get `TooLarge` when the daemon is up, success when it's down — exactly the pre-CQ-V1.25-2 anti-pattern that drove the existence of `cli/limits.rs` in the first place. No env override on the daemon side either.
- **Suggested fix:** Replace the literal pair with `cli::limits::max_diff_bytes()` (the same resolver used by the CLI path) plus a small JSON-envelope overhead (~1 KB). The 1 MB ceiling came from "scout / status take a few KB"; review/impact are now first-class clients of the same socket and need the larger budget.

#### SHL-V1.38-5: `STREAM_BATCH_SIZE = 1024` in UMAP path is dim-blind with no env override
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/umap.rs:21, 86`
- **Description:** UMAP projection paginates `embedding_batches(1024)`. At 1024-dim (BGE-large) each batch is ~4 MB; at 4096-dim it's ~16 MB; at hypothetical 8192-dim it's 32 MB. `cqs index --umap` is opt-in (#1452 v22 schema), but on a wide-dim eval slot this drives heap higher than necessary. No comment explains why 1024 was picked, and the `payload` `Vec::with_capacity(12 + n_rows * (2 + id_max_len + dim * 4))` two lines down already shows dim-awareness — the inconsistency is the smell.
- **Suggested fix:** Make this `cqs::limits::dim_scaled_batch(1_024, dim, 64, 8_192)` and add a `CQS_UMAP_STREAM_BATCH` env knob. Cheap fix and matches the pattern established by SHL-V1.36-3/4/5.

#### SHL-V1.38-6: `parse_channel_depth() = 512` default ignores file size and chunk fan-out
- **Difficulty:** easy
- **Location:** `src/cli/pipeline/types.rs:115-130`
- **Description:** Parse stage channel buffers up to 512 `ParsedBatch` messages. Each batch holds a `Vec<Chunk>` for one file_batch slice (up to `file_batch_size() = 5_000` files), each chunk carries a `String content` (~1-10 KB typical). Worst-case buffered: 512 × 100 chunks/file × 5 KB = 256 MB. The 512 default has no scaling rationale — it's a guess from when `file_batch_size` was 1000. SHL-V1.36-8 fixed `embed_channel_depth` by deriving from a *byte budget*; the parse channel deserves the same treatment for the same reason.
- **Suggested fix:** Mirror `embed_channel_depth`: pin a byte budget (e.g., 32 MB), derive depth from `(file_batch_size() × estimated chunks/file × estimated bytes/chunk)`. `CQS_PARSE_CHANNEL_DEPTH` env override stays. Less aggressive: just halve to 256 (still env-overridable) — the queue rarely backs up since parsing is faster than embedding.

#### SHL-V1.38-7: LLM-pass `PAGE_SIZE = 500` literal duplicated in two files, no env override
- **Difficulty:** easy
- **Location:** `src/llm/mod.rs:94`, `src/llm/doc_comments.rs:164`
- **Description:** Two production LLM-pass paginators hard-code `PAGE_SIZE = 500` for `chunks_paged(cursor, PAGE_SIZE)`. SHL-V1.30-8 added `CQS_ENRICHMENT_PAGE_SIZE` for the parallel enrichment paginator (`src/cli/enrichment.rs`); these LLM-pass paginators were missed. On large repos (>100k chunks) the page count is `total / 500 = 200+ round-trips`, each fetching `ChunkSummary` (with content). With `--llm-summaries` running for hours, a smaller page (50-100) reduces peak heap; with a fast SSD a larger page reduces SQLite round-trip overhead. Operators can't tune either way.
- **Suggested fix:** Extract a single `crate::limits::llm_pass_page_size()` resolver reading `CQS_LLM_PASS_PAGE_SIZE` (default 500), used by both call sites. Unifies with `enrichment_page_size()` patterning.

#### SHL-V1.38-8: Reconcile streaming path `BATCH = 1000` files hardcoded
- **Difficulty:** easy
- **Location:** `src/cli/watch/reconcile.rs:342, 350-355`
- **Description:** `#1229 (RM-5)` streaming reconcile path buffers 1000 paths at a time. Comment claims "Peak heap is `O(BATCH)` — independent of tree size", but BATCH itself is hardcoded with no env. On a small repo (<5000 files) BATCH=1000 means ~5 reconcile steps, each issuing an N-row `IN (...)` SELECT against `chunks` — already pretty good. On a 200k-file monorepo it's 200 round-trips. SQLite handles `IN (?...)` with `max_rows_per_statement(N)` ceilings, so 1000 is fine for the SQL side, but operators on either extreme can't tune.
- **Suggested fix:** Add `CQS_RECONCILE_BATCH` env override (default 1000), clamping `[100, 32_000]` (latter aligns with the sql.rs SQLite ceiling).

#### SHL-V1.38-9: `summary_queue` thresholds (64/200ms/10_000) hardcoded with no env override
- **Difficulty:** medium
- **Location:** `src/store/summary_queue.rs:99, 103, 108`
- **Description:** `DEFAULT_FLUSH_THRESHOLD_ROWS = 64`, `DEFAULT_FLUSH_INTERVAL_MS = 200`, `HARD_CAP_ROWS = 10_000` are all fixed. Comment says "Starting guess: `N=64, T=200ms`. Run a benchmark on the local LLM path before committing to numbers" — i.e., explicitly punted on tuning. With Anthropic Batches finishing in ~5 min (no streaming) the queue stays below 64 rows trivially. With the local vLLM provider (`feedback_vllm_gemma.md`, ~50 chunks/sec sustained) the queue can hit 64 in <2 sec, triggering a flush every 2 sec. That's fine, but tunable would let an operator on a slow disk push to 256+/500ms.
- **Suggested fix:** Three env knobs: `CQS_SUMMARY_FLUSH_ROWS`, `CQS_SUMMARY_FLUSH_INTERVAL_MS`, `CQS_SUMMARY_HARD_CAP_ROWS`. Defaults unchanged. Wire through a single `summary_queue_config()` helper to keep all three reads in one place.

#### SHL-V1.38-10: `RETRY_BACKOFFS_MS` schedule is a hardcoded `&[u64]` slice
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:48-50`
- **Description:** Local LLM provider retry schedule is `&[500, 1000, 2000, 4000]` — 7.5s window total. Hardcoded, not even a const + env override. With local vLLM serving on a saturated GPU, transient 503 / connection-reset bursts can exceed 7.5s; the request fails after 4 tries instead of riding through the burst. Cousin sites (`embedder/mod.rs` ORT init backoff, `cli/watch/rebuild.rs::EmbedderBackoff` exponential to 5min) take very different shapes — the LLM path is the most fragile and got the most aggressive ceiling.
- **Suggested fix:** Add `CQS_LLM_RETRY_BACKOFFS_MS` parsing a comma-separated list (e.g. `"500,1000,2000,4000,8000"`); fall through to the current default. Optional: separate `CQS_LLM_RETRY_MAX_ATTEMPTS` knob so the slice length and `MAX_ATTEMPTS` (line 50) stay in sync without source edits.
