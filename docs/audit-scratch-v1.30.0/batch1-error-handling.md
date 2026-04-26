## Error Handling

Findings target code paths added in v1.29.x → v1.30.0 (cache+slots #1105, local-LLM provider #1101, serve auth #1118, command registry #1114, non-blocking HNSW #1113, embedder Phase A #1120). Items already triaged in `docs/audit-triage-v1.30.0.md` (EH-V1.29-1..10) are explicitly skipped.

#### `LocalProvider::submit_via_chat_completions` silently loses every batch result on Mutex poisoning
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:155, 196, 271-279, 305, 545`
- **Description:** `LocalProvider` (PR #1101) drives the local LLM batch via per-thread workers that share a `Mutex<HashMap<String, String>> results` accumulator. Every single store-side and finalization step uses `.lock().unwrap()` or `if let Ok(mut g) = lock.lock()` and silently drops the poison case. Specifically:
  - Line 196: `if let Ok(mut map) = results_ref.lock() { map.insert(...) }` — if a *prior* worker's panic poisoned the lock, every subsequent successful item is silently dropped (`Ok(Ok(Some(text)))` branch becomes a no-op).
  - Line 271-272, 278-279: `let ok = *succeeded.lock().unwrap();` etc. — first poisoned lock crashes the batch.
  - **Worst case at line 305**: `let results_map = results.into_inner().unwrap_or_default();` — `Mutex::into_inner` returns `LockResult`. On poison, `unwrap_or_default()` quietly substitutes an **empty `HashMap`**, then writes that into the stash. `submit_via_chat_completions` then logs `"local batch complete"` with the real (truthful) succeeded/failed counts and returns the batch_id. The user later calls `fetch_batch_results(batch_id)` and gets `{}` — *all results lost* — with no error, no warning, and no signal that the count `succeeded` ≠ map size. Combined with the `Ok("ended")` from `check_batch_status`, the doc/HyDE pipelines treat this as a successful empty batch (every chunk's "summary failed silently") and persist nothing.
- **Suggested fix:** (1) Replace `Mutex::into_inner().unwrap_or_default()` at line 305 with `match results.into_inner() { Ok(m) => m, Err(p) => { tracing::error!(succeeded = ok, "results mutex poisoned during batch — recovering inner state"); p.into_inner() } }` so the partially-populated map is preserved. (2) Add a post-finalize invariant check: `if results_map.len() != ok { return Err(LlmError::Internal(format!("batch accounting drift: ok={ok} map_len={}", results_map.len()))); }` so accounting drift surfaces instead of silently shipping a short stash. (3) Audit-mode rule: every `*foo.lock().unwrap()` in worker hot paths gets either `expect("…")` with a meaningful message or explicit `into_inner()` recovery — `unwrap()` was forbidden in this codebase per CLAUDE.md.

#### `dispatch::try_daemon_query` warns user about daemon protocol error then silently re-runs the same command in CLI
- **Difficulty:** easy
- **Location:** `src/cli/dispatch.rs:445-462, 199-204`
- **Description:** When the daemon responds with a non-`ok` status (EH-13's "protocol error" branch), the function logs a warning, prints `cqs: daemon error: <msg>` and a hint to set `CQS_NO_DAEMON=1` — then returns `None`. The caller at `:200` interprets `None` uniformly as "fall back to CLI", so the CLI re-executes the *same* command moments later. The user sees the daemon error *and* the CLI's output for what should have been a single query. Two failure modes follow: (a) a daemon-only feature (e.g. a slot the daemon owns but the CLI re-resolves to a different active slot) returns a different result than the daemon would have, and the user has no way to tell which run their pipeline consumed; (b) write-side commands that the daemon refused for safety reasons get re-run from the CLI which may not have the same gating. The comment at line 459 — `"Still return None so we fall through to CLI path, but the user has been told why — no silent fallback"` — explicitly contradicts the EH-13 fix premise quoted at line 445-449 ("Falling back to CLI now would mask daemon bugs").
- **Suggested fix:** Either (a) propagate the daemon error: change `try_daemon_query` to return `Result<Option<String>, anyhow::Error>` and bubble protocol errors up so dispatch exits non-zero with the daemon's message — matching the comment's intent; or (b) keep the fallthrough but wire a sticky `daemon_error_seen: bool` so the CLI path's output is prefixed with `WARN: daemon errored ("…"); falling back — results may differ` rather than silently producing a "second" result the user never asked for.

#### `LocalProvider::fetch_batch_results` returns empty map on missing batch_id with no error — masks producer/consumer mismatch
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:542-547`
- **Description:**
  ```rust
  fn fetch_batch_results(&self, batch_id: &str) -> Result<HashMap<String, String>, LlmError> {
      let mut stash = self.stash.lock().unwrap();
      Ok(stash.remove(batch_id).unwrap_or_default())
  }
  ```
  The doc-comment says "returning empty if the id was already fetched or never existed". This collapses three distinct conditions — *id never existed*, *id was double-fetched*, *id existed but the worker pool dropped its results* (see prior finding) — into the same `Ok(HashMap::new())` reply. The summary/doc-gen loops at `src/llm/summary.rs` and `src/llm/doc_comments.rs` then commit zero rows for the affected batch and move on, with the only signal being a divergence between the `succeeded` count logged at submission and the row count actually persisted. There is no `tracing::warn!` on the missing-id path.
- **Suggested fix:** Replace with explicit error variants: `match stash.remove(batch_id) { Some(m) => Ok(m), None => Err(LlmError::Internal(format!("local batch_id {batch_id} not found in stash — already fetched or submission silently lost results"))) }`. Add a `LlmError::BatchNotFound(String)` variant so callers can distinguish "double fetch" from a real provider error and either retry or surface the drift to the operator.

#### Embedder model fingerprint silently falls back to `repo:timestamp` — invalidates entire embedding cache on hash failure
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:435-466`
- **Description:** When `model_fingerprint()` (the cache-key seed for the project-scoped embeddings cache, PR #1105) cannot stream-hash the ONNX file, three nested error arms fall back to `format!("{}:{}", self.model_config.repo, ts)`. `ts` is `SystemTime::now()` — **a different value every run**. The `EmbeddingCache` keys entries by `(content_hash, model_id)` where `model_id` is built from this fingerprint. Result: every time fingerprint hashing fails (transient I/O hiccup, AV scanner holding the file open on Windows, EBUSY), every chunk re-embeds against a brand-new model_id and the cache delivers 0% hit rate without surfacing a single signal to `cqs cache stats` or `cqs index`. Operators see "5 GB cache, growing every reindex" and the only diagnostic is the warn line buried in the journal. This also bloats the cache forever — old `repo:ts1` entries remain, never revisited, until `cqs cache prune --model 'repo:ts1'` (which the operator wouldn't know to run).
- **Suggested fix:** Promote the hash failure to a hard error: change the fingerprint type to `Result<String, EmbedderError>` and propagate via `?`. The cost (no cache hit on a single bad reindex) is dramatically lower than the silent-storm cost. If a fallback is desired for legacy compatibility, hash a stable proxy (file path + file size + mtime — all syscalls that don't read content) instead of `now()`, so retries within the same `mtime` window reuse the same cache. Either way, surface a `cqs doctor` warning when the stored model_id has the `repo:<ts>` shape, since that's a signal of broken cache.

#### Pattern: `serde_json::to_value(...).unwrap_or_else(|_| json!({}))` in impact/format.rs and 4 sibling sites silently turns serialization bugs into empty output
- **Difficulty:** easy
- **Location:** `src/impact/format.rs:11-16, 101-106`; `src/cli/commands/io/context.rs:94-97, 320-323, 498-501`; `src/cli/commands/io/blame.rs:240-243`
- **Description:** Six call sites use the pattern:
  ```rust
  serde_json::to_value(&output).unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to serialize ...");
      serde_json::json!({})
  })
  ```
  The output value is the *entire* result of the command (impact graph, context bundle, blame data). On any `Serialize` impl bug — typically only triggered after a refactor adds a non-serializable field — the user receives `{}` as the JSON envelope payload. An agent piping `cqs impact foo --json | jq '.callers'` gets `null` and treats it as "no callers" rather than "serialization broke and you need to file a bug." Combined with `dispatch_diff` / `dispatch_impact` paths, an entire batch handler can return `{}` for every input and the agent never sees a real error. `serde_json::to_value` only fails on `Serialize` impl bugs (and a few overflow cases) — these are programmer errors, not runtime conditions, and silencing them violates the "no silent failure" principle codified in EH-V1.29-9.
- **Suggested fix:** Switch all six to `serde_json::to_value(&output)?` and propagate. The function signatures (`impact_to_json`, `build_*_output`) currently return `Value`; bumping them to `Result<Value, serde_json::Error>` is a single-character touch at each callsite — they all already terminate in `crate::cli::json_envelope::emit_json(&obj)?` which already handles `Result`. Programmer errors should fail loud, not produce `{}` and a journal warn the operator never reads.

#### `cache_cmd::cache_stats` silently treats `QueryCache::open` failure as "0 bytes" — operator can't tell empty cache from broken cache
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/cache_cmd.rs:120-139`
- **Description:** New cache subcommand (PR #1105) reports the QueryCache file's size alongside the embedding cache. The lookup is wrapped in a `match QueryCache::open(...) { Ok(qc) => qc.size_bytes()..., Err(e) => { tracing::warn!(...); 0 }}`. When the QueryCache is locked by another process, has bad permissions, or hit a schema migration error, the JSON envelope reports `query_cache_size_bytes: 0` exactly as if the file was empty, and `cqs cache stats --json` consumers (the docs explicitly call this out as a P3 #124 fix) can't tell the difference. Same anti-pattern called out in EH-V1.29-7 (`EmbeddingCache::stats`) but on the new sibling code.
- **Suggested fix:** Add a `query_cache_status: "ok" | "missing" | "error: <message>"` field to the JSON envelope and print a one-line `Query cache: <error>` next to the size on the text path. Don't reuse the success-shaped numeric field for failure.

#### `slot_remove` masks `list_slots` failure as "the only slot remaining" — bails with confusing message instead of the real I/O error
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/slot.rs:303-313`
- **Description:**
  ```rust
  let active = read_active_slot(...).unwrap_or_else(|| DEFAULT_SLOT.to_string());
  let mut all = list_slots(project_cqs_dir).unwrap_or_default();    // <-- swallows StoreError
  all.retain(|n| n != name);
  if name == active {
      if all.is_empty() {
          anyhow::bail!("Refusing to remove the only remaining slot '{}'. Create another slot first.", name);
      }
      ...
  }
  ```
  If `list_slots` fails (slots/ readdir fails, permission denied), `all` is `vec![]`, then `retain` does nothing, then the active-slot branch fires with "Refusing to remove the only remaining slot" — even though ten slots may exist on disk. Operator sees a contradiction with `cqs slot list` output and has no way to debug. Same pattern at `src/cli/commands/infra/slot.rs:273, 304, 313` and `src/cli/commands/infra/doctor.rs:923`.
- **Suggested fix:** Propagate with `?` (the function already returns `Result<()>` via anyhow): `let mut all = list_slots(project_cqs_dir).context("Failed to list slots while validating remove")?;`. Same change for the three other sites — none of them are on a "best-effort" path where empty-on-error is semantically correct.

#### `build_token_pack` swallows `get_caller_counts_batch` failure with no warnings field
- **Difficulty:** easy
- **Location:** `src/cli/commands/io/context.rs:438-441`
- **Description:**
  ```rust
  let caller_counts = store.get_caller_counts_batch(&names).unwrap_or_else(|e| {
      tracing::warn!(error = %e, "Failed to fetch caller counts for token packing");
      HashMap::new()
  });
  let (included, used) = pack_by_relevance(chunks, &caller_counts, budget, &embedder);
  ```
  `pack_by_relevance` ranks chunks by caller count to pack the highest-value ones first into the token budget. With an empty `caller_counts` map, every chunk ties at "0 callers" and packing degrades to whatever stable-sort fallback kicks in — typically file-order. The agent receives a token-packed context that *looks* correct (right token count, right shape) but selected the *wrong* chunks. Sibling functions in the same file (`build_full_data`) carry a `warnings` field; this one is missed in the EH-V1.29-9 sweep. There is no signal — JSON or text — that ranking degraded.
- **Suggested fix:** Either propagate (`build_token_pack` already returns `Result`), or thread a `warnings: &mut Vec<String>` parameter through, push `"caller_counts query failed: {e}; token-pack ranking degraded to file-order"`, and have the caller include it in the typed output struct. The propagation path is one keystroke (`?`) and is the right call here — packing without ranking signal is worse than failing the command.

#### `read --focus` silently empties `type_chunks` on store batch failure — focused read returns chunk with no type definitions
- **Difficulty:** easy
- **Location:** `src/cli/commands/io/read.rs:230-235`
- **Description:** Same `unwrap_or_else(|e| { tracing::warn!(...); HashMap::new() })` pattern as the EH-V1.29-9 family but in `read --focus`. Output struct `FocusedReadOutput` does not currently carry a `warnings` field. An agent calling `cqs read --focus my_fn --json` to gather `my_fn` plus its type dependencies receives the function body alone with no signal that the type lookup failed — the agent then makes downstream decisions based on "no relevant type defs exist" when the truth was a transient store error.
- **Suggested fix:** Mirror the `SummaryOutput::warnings` pattern from `cli/commands/io/context.rs:464`. Add `#[serde(skip_serializing_if = "Vec::is_empty")] warnings: Vec<String>` to the focused-read output and push `"search_by_names_batch failed: {e}; type definitions omitted"` on the warn arm. This finding parallels the existing pattern call-out (EH-9 in the prior batch1 file) but specifically for the `read --focus` command which was missed in the v1.29.x sweep.

#### `serve::data::build_chunk_detail` collapses NULL `signature` and `content` columns to empty string — UI can't tell "missing chunk body" from "empty function"
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:488-492`
- **Description:**
  ```rust
  let signature: String = row.get::<Option<String>, _>("signature").unwrap_or_default();
  let doc: Option<String> = row.get("doc");
  let content: String = row.get::<Option<String>, _>("content").unwrap_or_default();
  ```
  `doc` is correctly typed as `Option<String>` so the UI sees `null` vs `""`. `signature` and `content` get the same nullable column treatment in SQL but are coerced to `""` here. The UI's chunk-detail sidebar then shows a chunk with **no signature line and no body** — visually identical to an empty function. The `#[derive(Serialize)] ChunkDetail { signature: String, content: String }` typing makes this fix mechanical: change to `Option<String>` and handle `None` in the JS so the user can see "<missing — DB column NULL>" rather than a blank pane that looks like correct rendering of an empty struct. Real-world cause: a partial write during indexing (SIGKILL between INSERT phases) where the chunk row exists but content didn't make it.
- **Suggested fix:** Change both fields on `ChunkDetail` to `Option<String>`, drop the `unwrap_or_default()`. Update `serve/assets/views/chunk-detail.js` to render `null` as a `<missing>` placeholder rather than the empty string. NULL in SQL is a real signal — flattening it loses the diagnostic.
