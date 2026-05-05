## Robustness — v1.30.0

Note: prior round (v1.29.0) findings RB-1..RB-10 were filed in `docs/audit-triage-v1.30.0.md` as RB-V1.29-{1..10} and are not repeated here. The findings below are scoped to code added/changed in v1.30.0 (#1090, #1096, #1097, #1101, #1105, #1110, #1117, #1118, #1119, #1120). Read each cited line; many of the surface-level `unwrap()`s in v1.30.0 are guarded by an upstream `Some/Ok` check or live in `#[cfg(test)]`.

#### RB-V1.30-1: Local LLM provider buffers entire HTTP response without size cap
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:97-100, 474-487, 492-499`
- **Description:** `LocalProvider::new` builds a `reqwest::blocking::Client` with only a request timeout — no body cap, and `parse_choices_content` calls `resp.json()` which buffers the entire body before deserializing. A misbehaving (or malicious) local LLM server — particularly one a user pointed `CQS_LLM_API_BASE` at without auditing — can return a multi-GB JSON blob and OOM the cqs process during a `summarize` / `chat` batch. Same risk in `body_preview` via `resp.text().unwrap_or_default()` on the 4xx error path. The retry loop (`MAX_ATTEMPTS = 4`, `RETRY_BACKOFFS_MS` up to 4 s) means each oversize response gets up to four buffering attempts before the item is skipped, multiplying the wasted memory + wall time per item across `local_concurrency()` ≤ 64 worker threads.
- **Suggested fix:** Pre-cap with a streamed read. Replace `resp.json()` with `resp.bytes()` after `Content-Length` inspection, or use a `take(N)` adaptor on the body — pick a 4 MiB cap on summary responses (a 50-token summary is a few hundred bytes; 4 MiB is ~1000× headroom). Apply the same cap to `body_preview` (replace `resp.text()` with a bounded read of ~2 KiB). New env var `CQS_LOCAL_LLM_MAX_BODY_BYTES` if a user wants to opt out.

#### RB-V1.30-2: Slot pointer files (`active_slot`, `slot.toml`) read with unbounded `read_to_string`
- **Difficulty:** easy
- **Location:** `src/slot/mod.rs:207, 323`
- **Description:** `read_active_slot` and `read_slot_model` use `fs::read_to_string(&path)` to load the active-slot pointer and per-slot config. Both files are owned by cqs and expected to be tiny (≤32 chars for the pointer, ~50 bytes for the model config), but a stray editor swap-file collision, a corrupted disk write, or a hostile co-tenant in a shared `.cqs/` directory can leave a multi-GB file behind. `read_to_string` then attempts to allocate the whole thing, OOM-ing every cqs invocation in that project until the user manually inspects `.cqs/`. Because `read_active_slot` runs on every CLI command (it's the slot-resolution fallback), a single bad pointer file effectively bricks the project for cqs.
- **Suggested fix:** Read with a hard cap. E.g.:
  ```rust
  use std::io::Read;
  let mut f = std::fs::File::open(&path)?;
  let mut buf = String::new();
  f.take(4096).read_to_string(&mut buf)?;
  ```
  4 KiB is enough for `slot.toml` (and 100× headroom on the pointer file); an oversize file becomes "treated as missing" with a `tracing::warn!`. Same pattern at both sites.

#### RB-V1.30-3: SystemTime → i64 casts in cache wrap silently after year 2554
- **Difficulty:** easy
- **Location:** `src/cache.rs:349-352, 551-555`
- **Description:** Both `write_batch` and `prune_older_than` do `SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64`. `as_secs()` returns u64 — values above `i64::MAX` (≈ year 2554) wrap to negative and corrupt the `created_at` column / produce a negative cutoff that matches no rows. Latent (we are in 2026), but the `as_secs() as i64` cast pattern is also used elsewhere. None propagate an error — silent wrap is the failure mode.
- **Suggested fix:** `i64::try_from(secs).map_err(|_| CacheError::Internal("clock above i64 cap"))?` at both sites. Defense-in-depth, the deadline is far away.

#### RB-V1.30-4: `migrate_legacy_index_to_default_slot` rollback leaves an undetectable half-state
- **Difficulty:** medium
- **Location:** `src/slot/mod.rs:511-593`, `move_file` at `:628-638`
- **Description:** During slot migration, files are moved one by one. On the second-or-later move failing, the function reverses the inventory and tries to move each successful destination back to its original location. A partial rollback prints `tracing::error!("rollback failed (manual recovery may be needed)")` and continues, leaving the project in a half-migrated state where `.cqs/index.db` is missing AND `.cqs/slots/default/index.db` is missing AND `.cqs/active_slot` may or may not exist. The next cqs invocation sees no legacy index, no slots, returns `Ok(false)` from migration, then errors trying to open a Store on the active slot. There is no way to detect "we are mid-failed-migration" from the user side — the recovery error message looks the same as a fresh project missing an index.
- **Suggested fix:** Write a `.cqs/migration.lock` sentinel file at the start of migration and only remove it on full success; subsequent `migrate_legacy_index_to_default_slot` calls error if the sentinel is found, with a clear `"previous migration failed at $TIME, manually recover then `rm .cqs/migration.lock`"` message. Half-state ambiguity is the actual robustness gap, not the rollback mechanics.

#### RB-V1.30-5: `libc_exdev()` hardcodes 18 — wrong on platforms where EXDEV ≠ 18
- **Difficulty:** easy
- **Location:** `src/slot/mod.rs:644-647`
- **Description:** `libc_exdev` returns the literal `18` to avoid a `libc` dependency. Linux/macOS x86_64/ARM64 all use 18, which is what the comment claims. But Windows (also a release target) uses `ERROR_NOT_SAME_DEVICE = 17`, surfaced through Rust's `io::Error::raw_os_error()` as `Some(17)`. Concretely: a user with `.cqs/` on `D:\` and the legacy `index.db` on `C:\` (junction-mounted via Windows) would hit `move_file`'s `fs::rename` with `ERROR_NOT_SAME_DEVICE`, the EXDEV branch wouldn't match (17 ≠ 18), and the migration would propagate the rename error instead of falling through to copy+remove. The user sees a hard migration failure rather than the documented copy fallback. The doc-comment claims "Windows doesn't surface EXDEV the same way (rename across filesystems just succeeds)" but that is undocumented OS behaviour, not a guarantee.
- **Suggested fix:** Remove the magic-number EXDEV check entirely and fall back to copy+remove on *any* `fs::rename` error after a `fs::metadata().is_some()` check confirming the source file is still readable. Cheaper than getting the constant right per-platform.

#### RB-V1.30-6: `cqs cache prune --older-than DAYS` accepts u32, computes negative cutoff for very large DAYS
- **Difficulty:** easy
- **Location:** `src/cache.rs:548, 551-555`
- **Description:** `prune_older_than` takes a `u32 days` and computes `cutoff = now - days * 86400`. `u32::MAX * 86400 = 3.7e14`, well within i64 — but if `now < days * 86400` (days > current-Unix-seconds / 86400 ≈ 22.5k days = ~62 years), `cutoff` wraps negative. SQLite then prunes all entries (everything's `created_at >= 0 > cutoff`). A user typo `cqs cache prune --older-than 999999999` becomes "prune the entire cache silently". No clamp, no warn.
- **Suggested fix:** Clamp `days` at parse time in the CLI to a sane ceiling (`days.min(36500 /* 100 years */)`), or assert `cutoff >= 0` and refuse the prune with a clear error otherwise.

#### RB-V1.30-7: `local.rs` retry loop unwraps Mutex on every 401/403 — poison cascades the worker pool
- **Difficulty:** medium
- **Location:** `src/llm/local.rs:393, 394, 396`
- **Description:** Each retry attempt does `*auth_attempts.lock().unwrap() += 1;` and `*auth_failures.lock().unwrap() += 1;`. If any worker thread panics while holding the mutex (e.g. via the test-only `on_item` callback panic the file mentions, or via a future regression in `parse_choices_content`), the mutex becomes poisoned. Every subsequent worker thread that hits a 401/403 then panics on `.unwrap()`, cascading into a thundering-herd of panicked rayon workers. The batch surface area is `concurrency` ≤ 64 — they all die, the batch fails with no auth-statistics summary, and the rayon thread pool is left in a partially-joined state.
- **Suggested fix:** Replace `.lock().unwrap()` with explicit poison handling: `*auth_attempts.lock().unwrap_or_else(|p| p.into_inner()) += 1;` — recover from poison and continue counting. The counters are advisory (used to abort the batch when every first-try hit 401/403); a poisoned mutex shouldn't escalate to a panic cascade. Apply at all three sites.

#### RB-V1.30-8: `serve/data.rs` `i64.max(0) as u32` clamp pattern grew from 3 to 8 sites in v1.30
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:299, 300, 587, 588, 777, 778, 993, 994` (8 sites post-v1.30)
- **Description:** `line_start: line_start.max(0) as u32` and matching `line_end` continue to silently clamp DB-corruption / migration-bug `i64` values to `0` then truncate >`u32::MAX` to a low number. v1.30 expanded this pattern to 8 sites (was 3 in v1.29 triage as RB-V1.29-3). The tracking issue is filed but unresolved — the new sites should be folded into the same fix when it lands rather than re-triaged.
- **Suggested fix:** Defer to RB-V1.29-3's resolution; flag here only because the fix scope grew. When the fix lands, `rg 'max\(0\) as u32' src/serve/data.rs` should return zero hits.

#### RB-V1.30-9: HTTP redirect policy disagrees between production (`Policy::none`) and doctor (`Policy::limited(2)`)
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:99` (`Policy::none()`) vs. `src/cli/commands/infra/doctor.rs:578` (`Policy::limited(2)`)
- **Description:** Same OpenAI-compat endpoint reached two ways: production batches refuse all redirects, doctor allows up to 2. A misconfigured local server that 308-redirects from `http://x/v1` to `https://x/v1` then passes the doctor check (limited(2) follows) but every production batch silently fails (`Policy::none` rejects the redirect). User sees `cqs doctor` green and then `summarize` failing with "all attempts hit network errors" — no diagnostic linking the two. Not a panic path, but a robustness/UX gap where the two probes disagree about what counts as a working endpoint.
- **Suggested fix:** Align the two policies. `Policy::limited(2)` in both is the safer choice — a same-origin HTTP→HTTPS redirect on bind-localhost is benign. Alternative: leave `Policy::none()` in production but log a once-per-launch warning in doctor when a redirect was followed during the probe, so the user is told their server is misconfigured before they hit it in batch.

#### RB-V1.30-10: Daemon socket-thread join detaches on timeout but doc-comment claims "joined cleanly"
- **Difficulty:** medium (low impact in practice)
- **Location:** `src/cli/watch.rs:2374-2400`
- **Description:** On daemon shutdown the code polls `handle.is_finished()` for up to 5 s, then breaks out of the loop. If the thread is still running at deadline, `handle_opt` is dropped and the OS thread is detached — its memory and any in-flight Store handle are leaked until the process exits (which happens immediately after shutdown, so practically benign), but a wedged socket thread holding a Store mutex would also leak the mutex's poison state in its `Drop` order. Not a correctness bug because the process is exiting, but the surrounding tracing emits "Daemon socket thread joined cleanly" only on the `is_finished()` path — a deadline-exit is silent. Operators reading logs to confirm a clean shutdown can't tell the difference.
- **Suggested fix:** Add a `tracing::warn!("Daemon socket thread did not exit within 5s; detaching")` log on the deadline-fall-through branch. Optionally, before detaching, force-close the listening socket from outside (`unlink` + `shutdown(2)`) to unblock any `accept()`-blocked socket thread, then re-poll `is_finished()` for another 1 s before detaching.

Summary: 10 v1.30.0-specific findings. RB-V1.30-1, 2, 7 are the highest-impact (all reachable on the new Local LLM provider + slot codepaths). RB-V1.30-4 and 5 are slot-migration robustness — narrow but bricking when they fire. RB-V1.30-3, 6, 8, 9, 10 are defense-in-depth / consistency issues.
