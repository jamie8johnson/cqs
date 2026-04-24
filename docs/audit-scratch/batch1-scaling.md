## Scaling & Hardcoded Limits

#### [SHL-V1.29-1]: `pad_2d_i64` hardcodes pad-token-id = 0 — breaks non-BERT tokenizers
- **Difficulty:** medium
- **Location:** `src/embedder/mod.rs:813-814`
- **Description:** Code: `let input_ids_arr = pad_2d_i64(&input_ids, max_len, 0);` and `let attention_mask_arr = pad_2d_i64(&attention_mask, max_len, 0);`. The pad token id is hardcoded to `0`. This is correct for BERT-family tokenizers (bert-base, bge-large, e5-base all use `[PAD] = 0`), but RoBERTa/XLM-R use `<pad> = 1`, and custom tokenizers can use any id. The `pad_2d_i64` call assumes `0` unconditionally — a user wiring in a custom RoBERTa-tokenized ONNX via `[embedding] model_path = ...` would silently get padding tokens that the model interprets as `<s>` (start-of-sequence) tokens. Attention mask uses 0 correctly (masked = 0 is universal), but `input_ids` padding is tokenizer-specific. No retrieval of `tokenizer.get_padding().pad_id()` anywhere in the embedder.
- **Suggested fix:** Add `pad_id: i64` to `ModelConfig` (default 0, override via model registry / config), or read `tokenizer.get_padding().map(|p| p.pad_id).unwrap_or(0)` at session-init and cache on `Embedder`. Thread into `pad_2d_i64` call.

#### [SHL-V1.29-2]: `MAX_BATCH_LINE_LEN = 1 MB` hardcoded — blocks large-diff review via batch/daemon
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:104`, used at `:1542`
- **Description:** `const MAX_BATCH_LINE_LEN: usize = 1_048_576;` rejects batch-mode lines above 1 MB with `"Line too long (max 1MB)"`. But the CLI path for `cqs review --stdin` / `cqs affected --stdin` accepts up to `MAX_DIFF_BYTES = 50 MB` (env `CQS_MAX_DIFF_BYTES`). So running the same workflow through the daemon (where the diff is quoted inline to a socket command) caps out 50× sooner than the direct CLI path. No env override. Error message doesn't mention how to bypass. With real-world PR diffs from monorepos easily reaching 5-10 MB, batch users silently hit this before the true limit.
- **Suggested fix:** Rename to `DEFAULT_MAX_BATCH_LINE_LEN`, add `batch_max_line_len()` reading `CQS_BATCH_MAX_LINE_LEN` (fallback 1 MB). Align the default with `MAX_DIFF_BYTES` or document why they differ. Error message should name the env var.

#### [SHL-V1.29-3]: `MAX_ID_MAP_SIZE = 100 MB` in `count_vectors` silently breaks large-corpus stats
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:732`
- **Description:** `const MAX_ID_MAP_SIZE: u64 = 100 * 1024 * 1024; // 100MB` in `count_vectors()`. If the id-map file exceeds 100 MB the function returns `None` silently (warns via `tracing::warn!` but `cqs stats` / health reports just see "unknown vector count"). With BGE-large id strings averaging ~50-60 bytes (e.g. `/long/path/to/file.rs:123:a1b2c3d4e5f6...`), 100 MB caps around ~1.7M chunks — well within the "we want to scale to 1M+" ambition. The hard-load path in `load()` has `MAX_ID_MAP_ENTRIES = 10_000_000` (10M entries) with a rationale; this stats-only path is tighter by an order of magnitude for no clear reason. No env override.
- **Suggested fix:** Either raise to match the hard-load cap (e.g. `10 * 1024 * 1024 * 1024` — 10 GB), or add `CQS_HNSW_ID_MAP_MAX_BYTES` env var, or compute from the load-path constant. At minimum, bump it; at best, rewrite `count_vectors()` to stream the JSON array count without holding the whole thing in memory.

#### [SHL-V1.29-4]: Onboard `MAX_CALLEE_FETCH = 30` / `MAX_CALLER_FETCH = 15` hardcoded, not env-configurable
- **Difficulty:** easy
- **Location:** `src/onboard.rs:30,33`
- **Description:** `const MAX_CALLEE_FETCH: usize = 30;` and `const MAX_CALLER_FETCH: usize = 15;`. These caps drop callee/caller content from the onboard reading list silently. For a codebase where a "central concept" (e.g. `parse_config`, `Store::query`) has 50+ callers, the user gets a truncated 15-caller list with no warning, no hint to raise the cap. No env override; no CLI flag; no knob at all. For small projects 15/30 is fine; for large projects / monorepos this is much too low. Contrast with `CALL_GRAPH_MAX_EDGES` which ships with `CQS_CALL_GRAPH_MAX_EDGES` env override.
- **Suggested fix:** Add `CQS_ONBOARD_MAX_CALLEES` / `CQS_ONBOARD_MAX_CALLERS` env vars, or better yet push them through `OnboardOptions` the way `ScoutOptions` threads `search_limit` / `search_threshold`.

#### [SHL-V1.29-5]: `task.rs` gather constants (`TASK_GATHER_DEPTH=2`, `TASK_GATHER_MAX_NODES=100`, `TASK_GATHER_LIMIT_MULTIPLIER=3`) hardcoded, no env
- **Difficulty:** easy
- **Location:** `src/task.rs:19-25`
- **Description:** All three `TASK_GATHER_*` constants are plain module-scope `const` with zero env/config/CLI plumbing. On a small project `max_nodes=100` is generous; on a 1M-chunk corpus the BFS gather phase truncates at 100 nodes and the task brief is tiny regardless of the user's `--limit`. Depth=2 / multiplier=3 are similarly fixed. No tracing warn on cap hit. The sibling `gather::GatherOptions` exposes these to callers, but `task()` ignores that and uses the hardcoded three.
- **Suggested fix:** Read `CQS_TASK_GATHER_DEPTH` / `CQS_TASK_GATHER_MAX_NODES` / `CQS_TASK_GATHER_LIMIT_MULTIPLIER` (with `OnceLock` caching like the other `CQS_*` helpers). Or plumb through `cmd_task` flags. Or accept a `TaskOptions` struct mirroring `GatherOptions`.

#### [SHL-V1.29-6]: `SCOUT_LIMIT_MAX = 50`, `SIMILAR_LIMIT_MAX = 100`, `RELATED_LIMIT_MAX = 50` hardcoded, no env override (unlike siblings in same file)
- **Difficulty:** easy
- **Location:** `src/cli/limits.rs:27,32,37`
- **Description:** Three `LIMIT_MAX` ceilings sit alongside `MAX_DIFF_BYTES`, `MAX_DISPLAY_FILE_SIZE`, `READ_MAX_FILE_SIZE`, `MAX_DAEMON_RESPONSE_BYTES` — every one of those has a resolver function (`max_diff_bytes()`, `max_display_file_size()`, etc.) reading its own `CQS_*` env var. The three `LIMIT_MAX` constants do not. They're re-exported via `src/cli/mod.rs:33` and consumed at 6 call sites (3 CLI, 3 batch). On a large corpus where an agent wants `cqs similar --limit 500` to see the full blast radius of a near-duplicate, it silently clamps to 100 with no warning and no way to override short of editing source. Inconsistent with the rest of the file.
- **Suggested fix:** Add `scout_limit_max()` / `similar_limit_max()` / `related_limit_max()` reading `CQS_SCOUT_LIMIT_MAX` / `CQS_SIMILAR_LIMIT_MAX` / `CQS_RELATED_LIMIT_MAX`. The `parse_env_usize` helper already exists in the same file.

#### [SHL-V1.29-7]: Health/suggest hotspot thresholds (`HOTSPOT_MIN_CALLERS=5`, `DEAD_CLUSTER_MIN_SIZE=5`, `HEALTH_HOTSPOT_COUNT=5`, `SUGGEST_HOTSPOT_POOL=20`) don't scale with corpus
- **Difficulty:** medium
- **Location:** `src/suggest.rs:14,18,21` and `src/health.rs:16`
- **Description:** On a 1M-chunk corpus, "5+ callers" is noise — every utility function hits that. The untested-hotspot detector surfaces hundreds-to-thousands of matches because the threshold doesn't scale. Similarly `HEALTH_HOTSPOT_COUNT=5` means `cqs health` always shows top-5 hotspots regardless of whether the corpus has 1k or 1M chunks. `SUGGEST_HOTSPOT_POOL=20` hard-limits pattern detection. None of these are env-configurable. The fix is either corpus-adaptive (thresholds scale with log2(chunk_count), mirroring the `cagra_itopk_max_default` pattern already in `src/cagra.rs:166`) or at minimum env-configurable.
- **Suggested fix:** Follow the `cagra_itopk_max_default` pattern — `HOTSPOT_MIN_CALLERS` = `(log2(n_chunks) * 0.6).clamp(5, 50)` or similar. At minimum, add `CQS_HOTSPOT_MIN_CALLERS`, `CQS_DEAD_CLUSTER_MIN_SIZE`, `CQS_HEALTH_HOTSPOT_COUNT` env vars.

#### [SHL-V1.29-8]: Risk-score thresholds (`RISK_THRESHOLD_HIGH=5.0`, `RISK_THRESHOLD_MEDIUM=2.0`) and blast_radius ranges (0..=2 / 3..=10) hardcoded, pub const but non-configurable
- **Difficulty:** medium
- **Location:** `src/impact/hints.rs:11,13` and `:148-152, 236-240`
- **Description:** Risk classification uses `score = caller_count * (1.0 - test_ratio)`; `>= 5` → High, `>= 2` → Medium. Blast-radius buckets `0..=2 → Low`, `3..=10 → Medium`, `>10 → High`. These were tuned for cqs-sized projects (~20k chunks). On a large monorepo where every module has 10-100 callers, the High/Medium/Low buckets collapse — almost everything is High. On a small script project, the buckets may never trigger beyond Low. No env override, no config section, no CLI flag. These values determine `cqs review` gate decisions (CI-blocking) so the wrong bucket silently changes the risk classification. The threshold is `pub const` (API-exposed) but nothing in the config schema scales it.
- **Suggested fix:** Add `[risk]` config section with `high_threshold` / `medium_threshold` / `blast_radius_low_max` / `blast_radius_high_min`, or env vars `CQS_RISK_HIGH` / `CQS_RISK_MEDIUM` / `CQS_BLAST_LOW_MAX` / `CQS_BLAST_HIGH_MIN`. Document the v1.29.0 defaults in `docs/notes.toml` so tuning is traceable.

#### [SHL-V1.29-9]: `DAEMON_PERIODIC_GC_INTERVAL_SECS=1800` and `DAEMON_PERIODIC_GC_IDLE_SECS=60` hardcoded, asymmetric with `DAEMON_PERIODIC_GC_CAP` which is env-overridable
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:887-888`
- **Description:** `DAEMON_PERIODIC_GC_CAP_DEFAULT` has a full env-resolver (`daemon_periodic_gc_cap()` reading `CQS_DAEMON_PERIODIC_GC_CAP`). But the two siblings — interval (30 min) and idle (60 s) — are hardcoded `const u64` with no resolver, no env, no config. A heavy-write environment (watch mode with continuous `cargo check`) might want a shorter interval so the daemon GC catches up; a laptop user on battery might want a longer one. Both knobs are already there in spirit but only one is plumbed. The CAP comment even says "Keeps each tick short" — so letting users tune the interval is the natural follow-through.
- **Suggested fix:** Mirror `daemon_periodic_gc_cap()` with `daemon_periodic_gc_interval_secs()` / `daemon_periodic_gc_idle_secs()` reading `CQS_DAEMON_PERIODIC_GC_INTERVAL_SECS` / `CQS_DAEMON_PERIODIC_GC_IDLE_SECS`.

#### [SHL-V1.29-10]: `convert/{html,mod}::MAX_FILE_SIZE = 100 MB` duplicated, hardcoded, no env override
- **Difficulty:** easy
- **Location:** `src/convert/html.rs:29` (`MAX_CONVERT_FILE_SIZE`) and `src/convert/mod.rs:363` (`MAX_FILE_SIZE` in `markdown_passthrough`)
- **Description:** Two separate `const X: u64 = 100 * 1024 * 1024;` declarations, same value, same semantic ("refuse to convert files above this size"). Previous audit P3 #106 extracted `DEFAULT_DOC_MAX_PAGES` and P3 #108 extracted `DEFAULT_DOC_MAX_WALK_DEPTH` to `src/limits.rs` with env overrides (`CQS_CONVERT_MAX_PAGES` / `CQS_CONVERT_MAX_WALK_DEPTH`). The per-file size cap got missed. A user converting a 150-MB HTML doc dump or a single large Markdown file silently fails with "exceeds 100 MB" — same class of silent-failure this file's top comment flags as the motivation for env-override plumbing. Also: `convert/webhelp.rs:117` has a separate `MAX_WEBHELP_BYTES = 50 MB` that's *not* the same constant but *is* similarly hardcoded.
- **Suggested fix:** Extract `DEFAULT_CONVERT_FILE_SIZE: u64 = 100 * 1024 * 1024` and `convert_file_size()` reading `CQS_CONVERT_MAX_FILE_SIZE` into `src/limits.rs` next to `doc_max_pages()` / `doc_max_walk_depth()`. Replace both hardcoded constants. Also thread `MAX_WEBHELP_BYTES` through the same helper or its own env var.
