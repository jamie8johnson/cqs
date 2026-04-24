## Robustness

#### RB-1: `timeout_minutes * 60` unchecked multiplication on env-var input
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:343`, `src/cli/batch/mod.rs:378`
- **Description:** `let timeout = std::time::Duration::from_secs(timeout_minutes * 60);` — `timeout_minutes` is parsed directly from `CQS_BATCH_IDLE_MINUTES` / `CQS_BATCH_DATA_IDLE_MINUTES` via `.parse::<u64>().ok()`. A caller who sets `CQS_BATCH_IDLE_MINUTES=999999999999999999` lands in `u64` overflow (~307M year bound), silently wrapping to a small timeout value and evicting sessions on the very next tick — the opposite of what the user asked for. Debug builds panic, release silently wraps.
- **Suggested fix:** `Duration::from_secs(timeout_minutes.saturating_mul(60))` at both sites. Alternatively clamp `timeout_minutes` to a sane ceiling (e.g. `365 * 24 * 60`) in `idle_timeout_minutes()` / `data_cache_idle_timeout_minutes()`.

#### RB-2: `umap.rs` narrowing casts on row count / dim / id len without validation
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/umap.rs:104-106,116`
- **Description:** The UMAP wire-protocol writes `(n_rows as u32).to_le_bytes()`, `(dim as u32).to_le_bytes()`, `(id_max_len as u32).to_le_bytes()`, `(id_bytes.len() as u16).to_le_bytes()`. `id_bytes.len() > u16::MAX` is checked at line 109 but `n_rows > u32::MAX` and `id_max_len > u32::MAX` are not. `dim` is bounded by model (fine) but `n_rows` is `buffered.len()` where `buffered` grows one entry per chunk from `store.embedding_batches`. A corpus with >4B chunks (unrealistic) silently truncates the row count in the header, causing the Python UMAP script to read fewer bodies than exist, misalign indices, and return wrong coordinates — silent data-corruption path rather than an error. Same pattern for `id_max_len` (max single-chunk-id length; plausible only if caller constructs pathological ids, but still an unchecked narrowing).
- **Suggested fix:** After line 93 add `anyhow::ensure!(n_rows <= u32::MAX as usize, "UMAP input too many rows: {n_rows} > u32::MAX");` and a matching guard for `id_max_len` next to the `id_bytes.len()` check that already exists.

#### RB-3: `serve/data.rs` negative `line_start` from DB silently clamped to 0 then cast to `u32`
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:504`, and identical pattern in neighboring `Node`/`NodeRef` builders (`:787`, `:785-788`, etc.)
- **Description:** `line_start: r.get::<i64, _>("line_start").max(0) as u32,` — if `line_start` is somehow a negative i64 in the chunks table (corrupted index, migration bug, or a type-tree change), `max(0)` clamps to 0 and emits a "line 0" chunk in the served payload. If it's positive but > `u32::MAX`, the cast silently truncates. On the serve API this manifests as the frontend scrolling to the wrong line when the user clicks a node. No diagnostic.
- **Suggested fix:** Replace the shape with `let raw: i64 = r.get("line_start"); u32::try_from(raw).map_err(|_| ServeError::Internal(format!("chunk {id} has out-of-range line_start {raw}")))?`. Do the same at every call site (grep `line_start: r.get`). Alternatively emit a `tracing::warn!` with the chunk id and clamp, so stale data doesn't silently break the UI.

#### RB-4: `HierarchyDirection` query-string parsing panics on mixed-case / unicode input? — no: safely handled; skip
- **Skipped** (falsely suspected).

#### RB-5: `extract_l5k_regions` regex captures `.unwrap()` on group 0/1/2 — panic on concurrent regex corruption
- **Difficulty:** hard
- **Location:** `src/parser/l5x.rs:344-346,365`
- **Description:** `let routine_name = block.get(1).unwrap().as_str().to_string();` / `block.get(2).unwrap()` / `block.get(0).unwrap()`. The regex (`L5K_ROUTINE_BLOCK_RE` at line 323-325) has exactly two capture groups, so on a successful match groups 0, 1, and 2 must be present — this is safe against normal inputs. The only reachable panic is a `regex` crate bug where `captures_iter` yields a match with missing groups. No current evidence of such a bug, but the `.unwrap()` panic path is on user-content-derived input (L5X/L5K files in the indexing pipeline). If a corrupt file were to somehow produce a non-empty match-iterator whose capture layout is surprising, the whole indexer panics mid-walk. This is the only cluster of non-Mutex, non-fixed-size-try-into, non-regex-compile `.unwrap()`s in the production parser path.
- **Suggested fix:** Defensive — `let Some(routine_name) = block.get(1).map(|m| m.as_str().to_string()) else { tracing::warn!("L5K regex matched but group 1 missing — skipping"); continue; };`. Or accept the tiny risk and document it next to the regex (consistent with the `.expect("valid regex")` pattern used elsewhere). Low-impact, but a panic in the indexer aborts the whole `cqs index` / `cqs watch` pass.

#### RB-6: `chunk_count as usize` on u64→usize cast in CAGRA + CLI — silent truncation on 32-bit
- **Difficulty:** easy
- **Location:** `src/cagra.rs:606`, `src/cli/store.rs:344`, `src/cli/commands/index/build.rs:791,827`, `src/cli/commands/index/stats.rs:134,142-163`, `src/serve/data.rs` — widespread pattern
- **Description:** Many sites do `store.chunk_count()? as usize` where `chunk_count()` returns `u64`. On 32-bit targets this silently truncates at `usize::MAX == u32::MAX`, i.e. 4.3 billion chunks. cqs is 64-bit-only in practice (release targets are Linux x86_64, macOS ARM64, Windows x86_64), but there is no `#[cfg(target_pointer_width = "64")]` gate on the crate, and `cargo build --target i686-unknown-linux-gnu` is still mechanically buildable. Not a reachable panic on supported targets, but a silent wrap on pathological corpora in the (unsupported but buildable) 32-bit case.
- **Suggested fix:** Either gate the whole crate with `#[cfg_attr(not(target_pointer_width = "64"), compile_error!("cqs requires a 64-bit target"))]` in `src/lib.rs`, or replace the casts with `usize::try_from(chunk_count).map_err(StoreError::from)?`. Given the widespread pattern, the single-line crate-level gate is the cleaner fix.

#### RB-7: Channel `recv()` panics in indexing pipeline aren't routed to structured error
- **Difficulty:** medium (potentially no issue — verify)
- **Location:** survey didn't surface any `.recv().unwrap()` / `.send(...).unwrap()` in production — **likely no finding**, but the parallel-rayon pipeline (`src/cli/pipeline/parsing.rs`) uses `crossbeam_channel` with `?` propagation. No panic path confirmed. Skipping.
- **Suggested fix:** n/a

#### RB-8: `reranker.rs` batch_size × stride multiplication on inference output — no overflow guard
- **Difficulty:** easy
- **Location:** `src/reranker.rs:369,378`
- **Description:**
  ```rust
  let stride = if shape.len() == 2 { shape[1] as usize } else { 1 };
  // ...
  let expected_len = batch_size * stride;
  ```
  `shape[1]` is an `i64` from ORT. Cast to `usize` on a negative dimension wraps to a huge positive value (e.g. `-1 as usize = usize::MAX`). `batch_size * stride` then overflows and wraps. The subsequent `data.len() < expected_len` check still passes even on overflow (wrapped `expected_len` small), letting the function proceed with a broken stride. ORT shapes being negative is a spec-violating model but a malicious or corrupted `.onnx` file could have one.
- **Suggested fix:** After `shape[1] as usize`, add `if shape[1] < 0 { return Err(RerankerError::Inference(format!("negative output dim: {}", shape[1]))); }`. Then replace `batch_size * stride` with `.checked_mul(...)` and return `Inference` on None.

#### RB-9: `splade/mod.rs` `shape[N] as usize` on negative ORT dims — same pattern as RB-8
- **Difficulty:** easy
- **Location:** `src/splade/mod.rs:145,154,524,549,770-838` (multiple)
- **Description:** Six sites do `shape[N] as usize` on `i64` ORT shape values. Most are bounded by `shape.len() != 2/3` guards and subsequent `ArrayView2::from_shape` / `ArrayView3::from_shape` which would error on a misshape — but the immediate cast still wraps a negative dim silently into `usize::MAX`, then multiplies into `batch_size`/`seq_len`/`vocab` arithmetic *before* the `from_shape` check fires. A malicious SPLADE `.onnx` reports (batch=N, vocab=-1), vocab wraps to `usize::MAX`, `from_shape((batch, usize::MAX))` allocation attempt panics or OOMs the process (ndarray returns `ShapeError` rather than panicking in recent versions — safe — but the pattern is fragile).
- **Suggested fix:** Factor a helper `fn i64_dim_to_usize(d: i64, name: &str) -> Result<usize, SpladeError>` and use it at every `shape[N] as usize` site.

#### RB-10: `id_map.len() * dim * 4 * 2` in HNSW persist — unchecked mul on 64-bit (low risk on 32-bit)
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:647`
- **Description:** `let expected_max_data = id_map.len() * dim * std::mem::size_of::<f32>() * 2;` — bounded by `MAX_ID_MAP_ENTRIES = 10_000_000` and `dim` (nominal 1024). `10M × 1024 × 4 × 2 = 8.2 × 10^10` — fits `usize` easily on 64-bit. But the bound `dim` here comes from the loader's `dim` argument (caller-supplied from the Store's `model_info`). A pathological `model_info` with `dim = u32::MAX / 2` would overflow. On 64-bit targets this still fits (within usize), but on 32-bit it overflows silently and the subsequent `data_meta.len() as usize > expected_max_data` check would let a crafted file through.
- **Suggested fix:** `.checked_mul(dim)?.checked_mul(4)?.checked_mul(2)?` or `saturating_mul` with the same error path as the id_map guard above. Very small fix; defense-in-depth against future corpora with larger embedding dimensions.

Summary: 7 actionable findings (RB-1, RB-2, RB-3, RB-5, RB-6, RB-8, RB-9, RB-10 — 8 total; RB-4 and RB-7 skipped as false positives on deeper inspection). Most of the codebase has extensive saturating-arithmetic, `.ok_or_else` / `.get(i)?` patterns, and `.try_into()` with length guards already in place from prior audit rounds. The remaining issues are all narrow: (a) env-var multiplication overflows that nobody will hit in practice (RB-1); (b) unchecked u64→usize / i64→usize casts that are latent on 32-bit but not on supported 64-bit targets (RB-6, RB-10); (c) ORT shape[N]-as-usize casts that could wrap a negative dim into OOM/panic (RB-8, RB-9) — low probability but worth hardening for the security-critical ONNX surface.
