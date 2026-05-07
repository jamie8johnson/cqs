## Error Handling

Audit pass against the post-v1.38.0 main branch (4a31285e). EH-V1.36-* items
from PR #1456 are excluded; the destructive `cmd_model_swap` migration to
`try_stored_model_name` (PR #1504) is excluded. Findings below cover
remaining call sites that still use the lossy variant in destructive /
data-correctness paths, plus a handful of new silent-error sites in
recently-touched modules.

#### EH-V1.38-1: `watch/mod.rs:1037` resolves embedding model via lossy `stored_model_name()` — silent metadata-read failure → wrong dim → corrupted incremental reindex
- **Difficulty:** medium
- **Location:** `src/cli/watch/mod.rs:1037`
- **Description:** `let stored_model_for_watch = store.stored_model_name();` is
  the watch-loop's index-aware model resolver — its result feeds
  `ModelConfig::resolve_for_query` so the daemon embeds new chunks with the
  same model that built the index. Per the EH-V1.36-6 finding the unfixed
  `stored_model_name()` returns `None` on **any** SQL error (corrupt
  metadata table, sqlite I/O error, schema mismatch). When that happens the
  resolver silently falls through to CLI flag → env → config → default —
  exactly the corrupting-incremental-reindex footgun the fix in PR #1504
  was meant to close. The comment block at lines 1032-1036 explicitly
  states the consequence ("would embed new chunks with a different dim
  than the index, corrupting incremental reindex") yet the call still
  uses the swallowing variant.
- **Suggested fix:** `let stored_model_for_watch = match store.try_stored_model_name() { Ok(s) => s, Err(e) => { tracing::error!(error = %e, "watch: failed to read stored_model_name; refusing to start to avoid mixed-dim writes"); return Err(e.into()); } };` Watch is a long-running daemon — bail rather than silently degrade.

#### EH-V1.38-2: `watch/rebuild.rs:187` same pattern in `resolve_index_aware_model_for_watch`
- **Difficulty:** medium
- **Location:** `src/cli/watch/rebuild.rs:187`
- **Description:** The companion helper used during daemon-thread bring-up
  (`daemon_model_config`) also calls `s.stored_model_name()` (lossy) inside
  an `Ok(s) =>` arm. The `Err(e)` arm (open-readonly failed) does warn,
  but the inner call returns silently on metadata read failure. Same
  consequence as EH-V1.38-1: dim drift in the daemon's reindex path
  without observability.
- **Suggested fix:** Replace the inner `s.stored_model_name()` with
  `s.try_stored_model_name()` and surface the error via `tracing::warn!`
  with `path = %index_path.display()` before falling back to None.

#### EH-V1.38-3: `model.rs:240` `cmd_model_show` reports "<unrecorded>" for both fresh-DB and corrupt-DB cases
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/model.rs:239-241`
- **Description:** `let model = store.stored_model_name().unwrap_or_else(|| "<unrecorded>".to_string());` — `cqs model show` is the operator's first-line diagnostic when troubleshooting model-mismatch errors. If the metadata read fails (corrupt DB, schema skew), the user sees "<unrecorded>" identical to a fresh DB and concludes "I haven't indexed yet" — when actually the DB is broken and the next `cqs index` call without `--force` will hit the EH-V1.38-1 path. The strict variant exists; show is the diagnostic surface that most needs it.
- **Suggested fix:** Branch on the `Result`: emit `<unrecorded>` only on `Ok(None)`; on `Err(e)` print a distinct `<read-error: {e}>` and add a warn line so the user can `cqs doctor` from there.

#### EH-V1.38-4: `slot.rs:206` slot listing silently empties model column on metadata read failure
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/slot.rs:206`
- **Description:** `cqs slot list` displays `model` per slot. When the
  metadata read fails (e.g., a slot's index is mid-rebuild and metadata
  is locked), the column shows empty — same display as a fresh slot.
  Operators auditing which slots use which model can't tell broken from
  fresh. Note the surrounding code at lines 210-218 already has a warn
  ladder for the **store open** failure case; the metadata read inside
  `Ok(store) =>` is the only branch missing observability.
- **Suggested fix:** Replace `let model = store.stored_model_name();` with
  `let model = match store.try_stored_model_name() { Ok(m) => m, Err(e) => { tracing::warn!(slot = name, error = %e, "Failed to read model_name from slot metadata"); None } };`

#### EH-V1.38-5: `cli/dispatch.rs:139-142` slot SPLADE α resolver still uses `.ok()` despite EH-V1.30.1-3 fixing the same pattern 30 lines above
- **Difficulty:** easy
- **Location:** `src/cli/dispatch.rs:139-142`
- **Description:** Lines 107-119 of the same function fixed an identical
  silent-suppression bug (EH-V1.30.1-3 cited in the comment) by
  converting `resolve_slot_name(...).ok()` into a `match` with
  `tracing::warn!`. Lines 139-142 then immediately re-introduce the same
  `.ok()` pattern for the SPLADE α resolution: `cqs::slot::resolve_slot_name(...).ok().map(...).unwrap_or_default()`. If the user passes `--slot foo` and resolution fails (typo, missing slot file), they silently get default-slot α overrides — the only signal being *different search results from what they asked for*. The fix is the exact one already applied above.
- **Suggested fix:** Replicate the match-with-warn pattern from lines
  107-119; on Err, emit a warn citing `slot = ?cli.slot` and fall through
  to the empty alpha table.

#### EH-V1.38-6: `cli/commands/infra/hook.rs:393, 536` conflate `NotFound` with permission-denied / oversize errors
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/hook.rs:393` and `src/cli/commands/infra/hook.rs:536`
- **Description:** `match read_hook_capped(&path) { Err(_) => report.not_present.push(...), ... }` lumps three distinct conditions: (a) hook actually missing (expected, fine), (b) hook present but permission-denied or other I/O error, (c) hook present but exceeds the 1 MiB cap (already logged as warn inside `read_hook_capped` but then gets silently downgraded to "not present" in the report). On `cqs hook status`, the operator sees "missing" and runs `cqs hook install`, which then unconditionally writes to the path — silently overwriting whatever oversize/perm-locked file is there. Compare with the inline `warn` already in `read_hook_capped` for the cap case — that warn fires but the caller's `Err(_)` arm reports "missing" anyway, contradicting the warn.
- **Suggested fix:** Match on `e.kind()`: `ErrorKind::NotFound => report.not_present.push(...)`; everything else into a new `report.unreadable: Vec<(String, String)>` (or push to a third bucket) and surface in the output. At minimum, `tracing::warn!(hook, error = %e, "Hook present but unreadable; treating as not-present")` so the operator sees the discrepancy.

#### EH-V1.38-7: `embedder/mod.rs:1325-1334` triple-cascade tensor extract silently swallows f32 and f16 extract errors
- **Difficulty:** medium
- **Location:** `src/embedder/mod.rs:1325-1334`
- **Description:** The dtype probing chain `try_extract_tensor::<f32>() → ::<f16>() → ::<bf16>()` uses bare `Err(_) =>` for the first two fallbacks. ORT's extract errors include both the expected "wrong dtype" (innocuous, the next branch handles it) AND real failures like "session output index out of range" or "tensor backing memory invalid". When a non-dtype error fires for f32, we silently try f16, get a different error there, silently try bf16, and finally surface only the bf16 error via `ort_err`. The operator sees a confusing "bf16 extract failed" message while the actual problem was the f32 extract — wrong root cause in the log. Hot path; this is the cqs query-time inference loop.
- **Suggested fix:** Distinguish "wrong dtype" from "real error" via the `OrtError` variant. Or simpler: probe the dtype once via `output.dtype()` and dispatch directly — no cascade needed. Each branch knows up front which extract to call.

#### EH-V1.38-8: `parser/{calls,injection,aspx}` log tree-sitter `LanguageError` via `error = ?e` (Debug) instead of `error = %e` (Display)
- **Difficulty:** easy
- **Location:** `src/parser/calls.rs:53`, `src/parser/injection.rs:278`, `src/parser/aspx.rs:236`
- **Description:** Three sites log a tree-sitter `LanguageError` (returned by `parser.set_language`) using `error = ?e`. The neighboring code in the same files (e.g. `aspx.rs:241`, `injection.rs:287`) uses `%e` for the next call's `IncludedRangesError` — inconsistent within the file. tree-sitter errors implement `Display`; the `?e` form expands to multi-line Debug output (`LanguageError { ... }`) instead of a one-line summary. Audit-finding type EH-V1.36 already established the project's preference for `%`; these three sites missed the bus.
- **Suggested fix:** Replace `error = ?e` with `error = %e` at all three sites. Same change in `cagra.rs:268` if `CagraError` impls Display (it does, via `thiserror`).

#### EH-V1.38-9: `cli/json_envelope.rs:371` and `cli/batch/mod.rs:2235` discard original `to_string_pretty` / `to_writer` error in the sanitize-and-retry path
- **Difficulty:** easy
- **Location:** `src/cli/json_envelope.rs:368-377`, `src/cli/batch/mod.rs:2235-2257`
- **Description:** Both sites do `Ok(s) => Ok(s), Err(_) => sanitize-and-retry`. The comment at the call sites says "NaN / Infinity caused this", but `to_writer`/`to_string_pretty` can fail for other reasons (downstream `io::Write` error in the batch case; serde custom Serialize error; recursion limits). When one of those non-NaN errors fires, the sanitize-retry path produces a structurally identical Value, the second serialize fails the same way, and `tracing::warn!` at line 2247 logs only the *second* (post-sanitize) error — not the original. Operator sees a misleading "JSON serialization failed after sanitization" when the real cause was the I/O error on the first attempt.
- **Suggested fix:** Capture the first error: `Err(e) => { let first = e; tracing::debug!(error = %first, "to_writer failed; retrying after float-sanitize"); ... }`. If the second attempt also fails, include both in the warn. Cheap and the I/O-error case becomes diagnosable.

#### EH-V1.38-10: `cli/limits.rs:207-223` `parse_env_usize` / `parse_env_u64` silently accept malformed values when env var is set
- **Difficulty:** easy
- **Location:** `src/cli/limits.rs:207-223`
- **Description:** `parse_env_usize`: `v.parse::<usize>().ok().filter(|n| *n > 0).unwrap_or(default)`. If the user sets `CQS_RERANKER_POOL_SIZE=abc` or `=0`, the env var is silently ignored and the default is used — no warn. Compare with `pipeline/types.rs:99-111`, `gather.rs:155-177`, `trace.rs:355-375`, and dozens of other env-knob helpers in this repo, all of which `tracing::warn!(value = %val, "Invalid X, using default Y")` in the malformed-but-set case. `parse_env_usize`/`parse_env_u64` are unique outliers — and they back at least 6 production knobs (`rerank_pool_size`, `rerank_max_batch_size`, etc.). An operator setting `CQS_RERANK_POOL_SIZE=128 ` (trailing space, copy-paste from a YAML file) gets the default with no signal.
- **Suggested fix:** Add a warn to both helpers: `if !v.is_empty() && v.parse::<usize>().ok().filter(|n| *n > 0).is_none() { tracing::warn!(env = key, value = %v, "Invalid env var value, using default {default}"); }` Mirrors every other env-knob helper in the repo.
