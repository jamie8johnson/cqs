## Error Handling

#### `cli/commands/io/brief.rs::build_brief_data` silently swallows 3 consecutive store errors, producing zeroed counts with no signal
- **Difficulty:** medium
- **Location:** `src/cli/commands/io/brief.rs:59-75`
- **Description:** Three back-to-back store queries each do `.unwrap_or_else(|e| { tracing::warn!(...); <empty> })`:
  ```rust
  let caller_counts = store.get_caller_counts_batch(&names).unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to fetch caller counts");
      HashMap::new()
  });
  let graph = store.get_call_graph().unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to load call graph for test counts");
      std::sync::Arc::new(cqs::store::CallGraph::from_string_maps(...))
  });
  let test_chunks = store.find_test_chunks().unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to find test chunks");
      std::sync::Arc::new(Vec::new())
  });
  ```
  If any store query fails, `cqs brief <path>` returns a `BriefData { caller_counts: 0, test_counts: 0 }` with no flag indicating partial data. An agent reading the JSON cannot distinguish "this function genuinely has no callers or tests" from "the call-graph query failed". `BriefData` has no `warnings`/`partial` field. This is the anti-pattern called out in MEMORY.md (`.unwrap_or_default()` swallowing store errors).
- **Suggested fix:** Either (a) propagate with `?` so the command errors out and the agent knows to retry, or (b) add a `warnings: Vec<String>` field to `BriefData` / `BriefOutput` that records which store queries degraded, and surface it in both the JSON envelope and the terminal output.

#### `ci::run_ci_analysis` silently downgrades dead-code-scan failure into "0 dead" with no flag
- **Difficulty:** easy
- **Location:** `src/ci.rs:100-128`
- **Description:** The dead-code scan is wrapped in a `match store.find_dead_code(true) { Ok(...) => ..., Err(e) => { tracing::warn!(error = %e, "Dead code detection failed — CI will report 0 dead code (not 'scan passed')"); Vec::new() }}`. `CiReport` has `dead_in_diff: Vec<DeadInDiff>` but no `dead_scan_ok: bool` / `warnings` field. A CI consumer reading the JSON output cannot distinguish "no dead code in diff" from "dead-code scan crashed". The gate evaluation in `evaluate_gate` only looks at `risk_summary`, so a dead-code query failure never fails the gate — meaning a CI run whose index is unreadable will green-light on the dead-code dimension even though no scan happened. Given the tracing::warn! message already admits the ambiguity ("not 'scan passed'"), the JSON should expose the same signal.
- **Suggested fix:** Add a `dead_scan_ok: bool` (or `warnings: Vec<String>`) field to `CiReport`; set it `false` on the Err arm. Optionally short-circuit `gate.passed = false` on `dead_scan_ok == false` when threshold != Off, so CI fails loud on the query error rather than fooling the reviewer.

#### `dispatch::try_daemon_query` silently falls back to CLI when daemon output re-serialization fails
- **Difficulty:** easy
- **Location:** `src/cli/dispatch.rs:785-790`
- **Description:**
  ```rust
  let output = resp.get("output")?;
  let text = match output {
      serde_json::Value::String(s) => s.clone(),
      other => serde_json::to_string(other).ok()?,
  };
  return Some(text);
  ```
  Two silent `?` exits: (1) `resp.get("output")?` returns None if the daemon sent `status: ok` but no `output` field, (2) `serde_json::to_string(other).ok()?` drops the serialization error entirely. Both short-circuit to `None`, which in turn causes silent fallback to CLI re-execution — exactly the "silently swallow daemon-reported problems" class of bug that EH-13 fixed for error statuses. The daemon path gives 3-19ms responses; the CLI re-runs pay the ~2s startup cost. Silent fallback masks daemon JSON bugs and denies operators latency signal.
- **Suggested fix:** Replace both with explicit `tracing::warn!(stage = "parse", "Daemon ok response missing/unserializable output — falling back to CLI")` before returning `None` (the pattern already used for bad status at line 766-772 and error responses at line 798-803). No need to change the fallback semantics, just the silence.

#### `cli/commands/io/blame.rs::build_blame_data` silently suppresses callers fetch failure
- **Difficulty:** easy
- **Location:** `src/cli/commands/io/blame.rs:52-59`
- **Description:**
  ```rust
  let callers = if show_callers {
      store.get_callers_full(&chunk.name).unwrap_or_else(|e| {
          tracing::warn!(error = %e, name = %chunk.name, "Failed to fetch callers");
          Vec::new()
      })
  } else { Vec::new() };
  ```
  `BlameData` then returns `callers: Vec::new()` to the caller. User ran `cqs blame foo --show-callers`; they get zero callers back with no indication the store query failed. `BlameData` has no partial-data flag. Same class as brief.rs above — the explicit tracing::warn! acknowledges that the user-facing signal is missing but doesn't fix it.
- **Suggested fix:** Either propagate via `?` so `cqs blame --show-callers` errors out cleanly, or add a `callers_query_failed: bool` / `warnings: Vec<String>` field on `BlameData` / the emitted JSON, and print a `Warning: callers query failed` line on the text path so agents and humans see the same signal the journal sees.

#### `suggest::generate_suggestions` silently skips dedup against existing notes on store error
- **Difficulty:** easy
- **Location:** `src/suggest.rs:62-65`
- **Description:**
  ```rust
  let existing = store.list_notes_summaries().unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to load existing notes for dedup");
      Vec::new()
  });
  ```
  If the notes query fails, `existing` is empty, and the dedup `.retain()` below does nothing. `cqs suggest` then returns suggestions that duplicate notes the user already has — and nothing in the response signals that dedup was skipped. Because `cqs suggest --apply` reindexes notes, this can silently generate duplicate note rows when the store glitches mid-call.
- **Suggested fix:** Propagate the error (the caller already returns `Result<Vec<SuggestedNote>, StoreError>`, so switching to `?` is a one-character change), or add a `dedup_skipped: bool` field to the output struct. Propagation is the low-friction fix since this function is not user-agent facing directly.

#### `where_to_add::suggest_placement_with_options_core` silently drops pattern data on batch fetch failure
- **Difficulty:** easy
- **Location:** `src/where_to_add.rs:208-214`
- **Description:**
  ```rust
  let mut all_origins_chunks = match store.get_chunks_by_origins_batch(&origin_refs) {
      Ok(m) => m,
      Err(e) => {
          tracing::warn!(error = %e, "Failed to batch-fetch file chunks for pattern extraction");
          HashMap::new()
      }
  };
  ```
  On failure, every file suggestion emerges with empty `LocalPatterns` (no imports, empty error-handling string, empty naming convention, `has_inline_tests: false`). An agent using `cqs where` for placement guidance would follow empty patterns into wrong conventions without realizing the pattern extraction failed. `PlacementResult` has no `warnings` field to signal partial data.
- **Suggested fix:** Propagate with `?` (the function already returns `Result<PlacementResult, AnalysisError>`). If propagation is undesirable because the file-scoring result is still useful on its own, add a `patterns_degraded: bool` field to `PlacementResult` and set each `LocalPatterns` to `None`/a sentinel so consumers can tell empty-because-no-data from empty-because-we-didn't-look.

#### `cache::EmbeddingCache::stats` aggregates 5 silent per-query failures into a single lying `CacheStats`
- **Difficulty:** easy
- **Location:** `src/cache.rs:408-461`
- **Description:** Five separate `unwrap_or_else(|e| { tracing::warn!(...); 0 /* or None */ })` calls inside `stats()`:
  ```rust
  let total_entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_cache").fetch_one(...).await.unwrap_or_else(|e| { ...; 0 });
  let total_size: i64 = sqlx::query_scalar("SELECT page_count * page_size ...").fetch_one(...).await.unwrap_or_else(|e| { ...; 0 });
  let unique_models: i64 = sqlx::query_scalar(...).fetch_one(...).await.unwrap_or_else(|e| { ...; 0 });
  let oldest: Option<i64> = sqlx::query_scalar(...).fetch_one(...).await.unwrap_or_else(|e| { ...; None });
  let newest: Option<i64> = sqlx::query_scalar(...).fetch_one(...).await.unwrap_or_else(|e| { ...; None });

  Ok(CacheStats { total_entries: total_entries as u64, total_size_bytes: total_size as u64, ... })
  ```
  The return type is `Result<CacheStats, CacheError>` — but it can never be `Err` despite the Ok path silently masking up to five independent query failures. `cqs cache stats --json` would report `total_entries: 0` on a corrupt/locked DB and look indistinguishable from an empty cache. An operator debugging cache issues via `cqs cache stats` sees a lie. No `warnings`/`degraded` field on `CacheStats`.
- **Suggested fix:** Change all five to `?` propagation so `stats()` returns `Err(CacheError)` on any query failure (the simplest fix — callers already handle Err). If per-field tolerance is desired, convert the five fields to `Option<u64>` with explicit None on error and surface a `warnings: Vec<String>` field.

#### Daemon startup/periodic GC silently treats poisoned `gitignore` RwLock as "no matcher" — re-indexes ignored files
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:1737, 1945`
- **Description:**
  ```rust
  // 1737 (startup GC) and 1945 (periodic GC)
  let matcher_guard = gitignore.read().ok();
  let matcher_ref = matcher_guard.as_ref().and_then(|g| g.as_ref());
  run_daemon_startup_gc(&store, &root, &parser, matcher_ref);
  ```
  `RwLock::read()` returns `LockResult`; `.ok()` only yields `None` when the lock is **poisoned** (panic occurred while holding write lock). Poison is a serious invariant violation — but here it's silently converted to `matcher_ref = None`, which makes `run_daemon_startup_gc` / `run_daemon_periodic_gc` run with no gitignore filtering. The GC pass will treat previously-ignored files as candidates for re-indexing, re-populating the index with paths the user explicitly excluded. No tracing::warn! on the poison path. The only signal is the sudden explosion of chunk counts.
- **Suggested fix:** Replace `.ok()` with:
  ```rust
  let matcher_guard = match gitignore.read() {
      Ok(g) => Some(g),
      Err(poisoned) => {
          tracing::error!("gitignore RwLock poisoned, recovering — indexing may include previously-ignored paths this tick");
          Some(poisoned.into_inner())
      }
  };
  ```
  Using `into_inner()` keeps the previously-written matcher visible rather than running GC with no filter at all, and the error log makes the daemon journal show the poison event.

#### `convert::pdf.rs::find_pdf_script` and `convert::chm.rs`-style zip-slip: `.ok()` hides permission-denied from walker
- **Difficulty:** medium
- **Location:** `src/cli/commands/io/read.rs:232-235` (related pattern), and generally anywhere `sqlx` results are `.unwrap_or_else(..., HashMap::new())`
- **Description:** This is a *pattern finding*, not one site. Several user-facing commands (`read --focus`, `brief`, `blame`, `suggest`, `where`) follow the idiom:
  ```rust
  let batch_results = store.some_batch_query(...)
      .unwrap_or_else(|e| { tracing::warn!(error = %e, "..."); HashMap::new() });
  ```
  On store query failure, every downstream operation silently produces empty results. The pattern is ergonomic and logs a warn, but the output schema has no way to signal partial data to the agent consumer. An agent running `cqs where "add caching"` and seeing empty LocalPatterns doesn't know whether to re-run, widen the search, or abandon the task. Past audits (EH-10, EH-11) fixed the same class for the ingest pipeline by re-buffering on failure; the user-facing commands should follow suit by at least adding a `warnings` / `partial: bool` field to each output struct.
- **Suggested fix:** Introduce a project-wide convention: every struct serialized to JSON gets an optional `#[serde(skip_serializing_if = "Vec::is_empty")] warnings: Vec<String>` field. When an `unwrap_or_else(..., empty)` fires with a `tracing::warn!`, also push a one-line human string onto `warnings`. This is a mechanical sweep across ~6 commands (brief, blame, suggest, where, scout, task) and mirrors the `ScoutResult::search_degraded` + `expansion_capped` pattern that already exists (used at `src/cli/commands/search/gather.rs:199-207`). Adopting that pattern consistently would close this entire class.

#### `cagra::CagraIndex::delete_persisted` discards both `remove_file` errors silently, no log
- **Difficulty:** easy
- **Location:** `src/cagra.rs:1097-1102`
- **Description:**
  ```rust
  pub fn delete_persisted(path: &Path) {
      let _span = tracing::debug_span!("cagra_delete_persisted", path = %path.display()).entered();
      let _ = std::fs::remove_file(path);
      let _ = std::fs::remove_file(meta_path_for(path));
  }
  ```
  Doc-comment says "Best-effort — missing files are treated as success". The problem is that *real* I/O errors (EACCES, EBUSY, disk full) are indistinguishable from `NotFound` here — both are silently discarded. The only caller (`src/cli/store.rs:365`) uses this to clean up after a load failure before rebuilding; if the remove fails with EACCES, the next daemon restart hits the same corrupt-sidecar path and logs the same warn every boot indefinitely with no operator signal that cleanup itself is failing. `let _ =` here gives up more than necessary — `ErrorKind::NotFound` is the only expected failure mode that should be suppressed.
- **Suggested fix:**
  ```rust
  pub fn delete_persisted(path: &Path) {
      let _span = tracing::debug_span!("cagra_delete_persisted", path = %path.display()).entered();
      for p in [path.to_path_buf(), meta_path_for(path)] {
          match std::fs::remove_file(&p) {
              Ok(()) => {}
              Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
              Err(e) => tracing::warn!(path = %p.display(), error = %e,
                  "CAGRA cleanup failed — next rebuild may re-hit the same corrupt blob"),
          }
      }
  }
  ```
