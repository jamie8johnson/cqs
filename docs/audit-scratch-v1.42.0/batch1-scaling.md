## Scaling & Hardcoded Limits

#### SHL-V1.36-1: `CQS_MAX_CONNECTIONS` default `unwrap_or(4)` ignores host parallelism
- **Difficulty:** easy
- **Location:** src/store/mod.rs:734-737
- **Description:** SQLite pool size defaults to 4 regardless of CPU count. On a 32-core host running `cqs serve`, every concurrent request contends for one of 4 connections — and `serve_blocking_permits()` (limits.rs:348) clamps spawn-blocking permits to this same `max_connections`, so the entire serve concurrency budget is fixed at 4 even though `available_parallelism()` reports 32. This is the same anti-pattern the v1.33 audit (SHL-V1.33-10) fixed in `project.rs::search_across_projects` and `reference.rs::load_references` (one of which still has the bug — see SHL-V1.36-2). The store has the most impact because it gates every other concurrency limit downstream of it.
- **Suggested fix:** Default to `available_parallelism().min(8)` (matching the v1.33 fix to `project.rs:260`). Power users on big hosts get linear scaling; small hosts stay capped. `CQS_MAX_CONNECTIONS` env override still wins verbatim.

#### SHL-V1.36-2: `reference.rs::load_references` still uses `unwrap_or(4)` after v1.33 fix to project.rs
- **Difficulty:** easy
- **Location:** src/reference.rs:208
- **Description:** v1.33 (SHL-V1.33-10) replaced the `unwrap_or(4)` rayon thread-count fallback in `project.rs:260` with `available_parallelism().get().min(8)`, citing 32-core hosts being under-utilized. The sibling site at `reference.rs:208` uses the same `CQS_RAYON_THREADS` env var with the same `unwrap_or(4)` pattern and was missed. Both sites read identical env vars and serve the same purpose (parallel store+HNSW load) — they should match. Comment at `project.rs:247` explicitly mentions matching `watch/runtime.rs:62-66`'s pattern; reference.rs got skipped.
- **Suggested fix:** Copy the v1.33 fallback closure verbatim from `project.rs:260-265` to `reference.rs:208`. Single-line edit.

#### SHL-V1.36-3: `BRUTE_FORCE_BATCH_SIZE = 5000` doesn't scale with model dim
- **Difficulty:** medium
- **Location:** src/search/query.rs:197
- **Description:** Cursor-based brute-force search loads 5000 chunks/iteration regardless of embedding dimension. At BGE-large (1024-dim, f32) that's ~20 MB per batch — fine. At Qwen3-4B (4096-dim) it's ~80 MB; at a hypothetical 8192-dim model it's ~160 MB held in `Vec<u8>` per row plus the bounded heap. The constant is private, not env-overridable, and the comment explicitly cites "bounds memory" as the rationale — but the bound now scales with model choice, not the constant. Same issue applies to the inline `5000`-row paginator in `EmbeddingBatchIterator` referenced in the comment.
- **Suggested fix:** Compute `batch_size = (TARGET_BYTES / (dim * 4)).clamp(500, 50_000)` with `TARGET_BYTES = 20 * 1024 * 1024` (~20 MB), or read `CQS_BRUTE_FORCE_BATCH_SIZE` env var. Pass the embedder's `dim` through `search_unified_with_index` callers (already in scope via `query.len()`).

#### SHL-V1.36-4: `HNSW_BATCH_SIZE = 10000` default doesn't scale with embedding dim
- **Difficulty:** easy
- **Location:** src/cli/commands/index/build.rs:1232-1238
- **Description:** HNSW insert batch defaults to 10000 vectors regardless of dim. At 1024-dim (BGE-large) that's 40 MB; at 4096-dim (custom Qwen3-style) it's 160 MB held in the per-batch `Vec`. `lib.rs:489` documents this as a fixed constant without flagging the dim coupling. `embed_batch_size_for(model)` (`src/embedder/models.rs:792`) already implements the right pattern (scale by `1024.0/dim`); HNSW build should mirror it.
- **Suggested fix:** Have `hnsw_batch_size()` accept the embedder's `dim` and scale: `(10_000 * 1024 / dim).clamp(500, 50_000)`. Env override unchanged.

#### SHL-V1.36-5: `cagra_stream_batch_size()` default 10000 doesn't scale with dim
- **Difficulty:** easy
- **Location:** src/cagra.rs:166 (function body)
- **Description:** Sister problem to SHL-V1.36-4. `cagra_stream_batch_size()` returns env or 10_000 verbatim — same dim-blind constant as the HNSW path. P3-15 in v1.33 triage (✅ #1363) addressed CAGRA `build_from_store` at the inner `BATCH_SIZE` level, but the `cagra_stream_batch_size()` resolver itself still ships a fixed default. The streaming path (cagra.rs:747 `Vec::with_capacity(chunk_count * dim)`) already shows dim-awareness in adjacent code — the inconsistency is the smell.
- **Suggested fix:** Same scaling treatment: `parse_env_usize_clamped("CQS_CAGRA_STREAM_BATCH_SIZE", scaled_default(dim), 500, 50_000)` where `scaled_default(dim) = (10_000 * 1024 / dim).max(500)`.

#### SHL-V1.36-6: `DEFAULT_RERANKER_BATCH = 32` and `splade_batch_size()` = 32 ignore model dim/seq
- **Difficulty:** medium
- **Location:** src/reranker.rs:41 ; src/cli/watch/reindex.rs:202
- **Description:** Two aux-model batch defaults are hardcoded to 32. Reranker comment explicitly says "32 is conservative because cross-encoder runs produce larger activations than plain encoder forward passes" — fine for ms-marco-MiniLM (384-hidden, 512-seq, ~100 MB activations at 32). But the Phase A model registry now supports custom rerankers and SPLADE variants; the default doesn't scale with hidden_size or max_seq_len. Same problem `embed_batch_size_for(model)` solves for the encoder. Result: small model → unused VRAM; large model → OOM at the documented default.
- **Suggested fix:** Mirror `ModelConfig::embed_batch_size`. Pull `hidden_size` and `max_seq_length` from the loaded reranker / SPLADE model and apply `32 * (384/hidden) * (512/seq)` rounded to a power-of-two, clamped `[1, 256]`. Env vars (`CQS_RERANKER_BATCH`, `CQS_SPLADE_BATCH`) still win.

#### SHL-V1.36-7: stale comment in `lib.rs` cites pre-3.32.0 SQLite 999 host-param limit
- **Difficulty:** easy
- **Location:** src/lib.rs:471-492
- **Description:** The "Batch Size Constants" doc-comment block in `lib.rs` says: "SQLite limit: max 999 bind parameters per statement. A query with N columns per row can batch `floor(999 / N)` rows." This is the legacy pre-2020 limit. Modern SQLite (3.32.0+, what `sqlx` ships) supports 32766, and `store/helpers/sql.rs:26` (`SQLITE_MAX_VARIABLES = 32766`) plus the `max_rows_per_statement(N)` helper already document the new ceiling. The lib.rs block lists the old "common sizes" (500/200/132/100) — readers using this as a guide for new code will pick numbers ~65× too small. This is the same documentation-vs-code drift that v1.33 found across P1-35..P1-39 (all fixed in #1324 by switching to `max_rows_per_statement`); the documentation block was missed.
- **Suggested fix:** Replace the block with a pointer to `store/helpers/sql.rs::max_rows_per_statement` and delete the stale 999/floor(999/N) explanation. New code should consult the helper, not the comment block.

#### SHL-V1.36-8: `EMBED_CHANNEL_DEPTH = 64` default ignores embedding dim
- **Difficulty:** easy
- **Location:** src/cli/pipeline/types.rs:115-129
- **Description:** Pipeline channel buffers up to 64 `EmbeddedBatch` messages. Each batch holds `embed_batch_size_for(model)` chunks × `dim` × 4 bytes of embeddings. At default BGE-large (batch=64, dim=1024) that's 64 × 64 × 1024 × 4 = 16 MB peak buffered. At Qwen3-style 4096-dim with batch=16 (post-scaling) it's 64 × 16 × 4096 × 4 = 16 MB — coincidentally similar. But the `64` default was picked when batch was a fixed 64; with `embed_batch_size_for` already dim-aware, the channel depth should be too: a model that scales batch *down* probably wants channel depth *up* (more parallelism since each msg is smaller), not unchanged.
- **Suggested fix:** Document the coupling at minimum (current doc says "smaller to bound memory" without mentioning dim). Better: scale depth inversely with batch — e.g. `64 * (default_batch / actual_batch)` clamped `[16, 256]`, or simply pin the *byte budget* (`32 MB / msg_bytes`) and derive depth.

#### SHL-V1.36-9: `DEFAULT_QUERY_CACHE_SIZE = 128` doesn't scale with available memory or dim
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:337
- **Description:** Query embedding LRU caches 128 entries regardless of dim. Comment says "~4 KB/entry at 1024-dim, ~3 KB/entry at 768-dim" — total ~512 KB. Fine for default models, but: (1) at 4096-dim it's ~16 KB/entry / 2 MB total — still fine but the rationale flips; (2) on a daemon serving an agent fleet the hit rate is highly dependent on size, and 128 entries is a coin toss for a 30-task batch loop hitting `cqs scout` for each task. Memory is cheap; cache misses are 50-200 ms per query. The default is too conservative for the daemon use case.
- **Suggested fix:** Bump default to 1024 (still ~4 MB at 1024-dim, trivial) or tier on `cqs serve`/`cqs watch --serve` vs CLI one-shot. Env var `CQS_QUERY_CACHE_SIZE` already exists as escape hatch.

#### SHL-V1.36-10: `MAX_CONCURRENT_DAEMON_CLIENTS = 16` is fixed regardless of host
- **Difficulty:** easy
- **Location:** src/cli/watch/socket.rs:40
- **Description:** Daemon caps concurrent clients at 16 with comment "matches typical agent fan-out... ~32 MB worst-case stack". The env override `CQS_MAX_DAEMON_CLIENTS` exists, but the *default* doesn't scale with cores or RAM. On a 64-core host running an agent swarm with 32+ parallel `cqs scout` calls, requests queue serially past 16. Stack memory isn't the binding constraint on modern 64-bit hosts (16 × 2 MB = 32 MB is rounding error); the cap was sized for "typical agent fan-out" of an unspecified era. With Claude Code Tasks-via-agents the typical fan-out is now 5-30.
- **Suggested fix:** Default `available_parallelism().get().clamp(16, 64)`. Keeps small hosts at 16, scales 32-core hosts to 32. Env override still wins.

DONE
