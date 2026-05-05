## Robustness

#### RB-V1.36-1: `doc_writer::compute_rewrite` / `rewrite_file` read source files unbounded
- **Difficulty:** easy
- **Location:** src/doc_writer/rewriter.rs:251 and src/doc_writer/rewriter.rs:319
- **Description:** Both `compute_rewrite` and `rewrite_file` call `std::fs::read_to_string(path)` with no size guard. They are reachable from `cqs --improve-docs` / `--improve-all` which iterates every project file. A pathological repo (giant generated/SQL/JSON file or symlink to /dev/zero on Linux) drives unbounded heap allocation. The parser path uses `CQS_PARSER_MAX_FILE_SIZE` and the file-read path in `cli/commands/io/read.rs` uses `CQS_READ_MAX_FILE_SIZE`; this site has neither.
- **Suggested fix:** Stat first and bail out (or skip with warn) when `meta.len() > CQS_DOC_WRITER_MAX_FILE_SIZE` (default e.g. 4 MB — the rewriter only ever touches source files small enough for the parser to have ingested them). Wire the same `metadata.len() > max_bytes → bail!` block already used in `convert/mod.rs::read_to_string_with_size_limit`.

#### RB-V1.36-2: `cli::commands::search::query` parent-context read uncapped
- **Difficulty:** easy
- **Location:** src/cli/commands/search/query.rs:899
- **Description:** When parent chunk is missing from the DB, the code falls back to `std::fs::read_to_string(&canonical)` to extract a line range. No size cap. Path is canonicalized + root-restricted (good), but the file itself is still read whole into memory just to pull `lines[start..end]`. A 5 GB file in the project tree (e.g., extracted dataset, debug log) loaded once per parent-context fetch will OOM the search process.
- **Suggested fix:** Either (a) check `metadata.len()` first and skip with a `tracing::warn!` if above a small cap (~4 MB), or (b) replace with line-bounded read using `BufReader::lines().take(line_end + 1)` and discard everything before `start`.

#### RB-V1.36-3: `cli::commands::infra::hook` reads existing git hooks unbounded
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/hook.rs:183, :365, :506
- **Description:** Three sites (`do_install`, `do_uninstall`, `do_status`) read every managed git hook file via `std::fs::read_to_string(&path)` with no size guard. A foreign hook truncated to `/dev/zero` or a multi-GB hook file (rare but possible — corruption, malicious commit hook injection on a shared CI box) will OOM `cqs ci ...`. Less likely than RB-V1.36-1 but no defense at all.
- **Suggested fix:** Stat first; cap at e.g. 1 MB (hooks are normally <10 KB). On overflow, treat as "foreign hook" (so install/uninstall is conservative) and warn. Only `contains(HOOK_MARKER_PREFIX)` is needed — that fits in the first few KB if it's a managed hook, so a streaming `BufReader::read_until(b'\n')` over the first 64 KB is enough.

#### RB-V1.36-4: `slot::ensure_slot_config` reads slot.toml unbounded
- **Difficulty:** easy
- **Location:** src/slot/mod.rs:339
- **Description:** Slot config bootstrap calls `fs::read_to_string(&final_path)` with no size cap. The `Config::load_file` path *does* enforce `MAX_CONFIG_SIZE` (config.rs:720) — slot.toml uses the same TOML parser shape but skips the guard entirely. Less critical than the doc-writer path because slot.toml is owned by cqs, but a hand-edited 10 GB slot.toml triggers OOM rather than the documented "warn + reset to default" path on the line below.
- **Suggested fix:** Mirror the `Config::load_file` size check (read meta first, bail/warn at MAX_CONFIG_SIZE). Same pattern, copy-paste with the slot path.

#### RB-V1.36-5: `store::chunks::staleness::compute_fingerprint` blake3 reads file whole
- **Difficulty:** medium
- **Location:** src/store/chunks/staleness.rs:161
- **Description:** `std::fs::read(path)` followed by `blake3::hash(&bytes)`. No size cap — runs during watch-driven staleness checks. The parser path skips files above `CQS_PARSER_MAX_FILE_SIZE`, but staleness can still be invoked on the same path before the size check downstream (or after a file grows post-index). For a 5 GB SQL dump that grew between index and watch, the watch reload tries to fingerprint it whole.
- **Suggested fix:** Switch to `blake3::Hasher::new()` + `Read` chunked into 64 KB. Same hash output, bounded RSS. Apply the same change at `cli/watch/reindex.rs:662` which also reads-then-hashes.

#### RB-V1.36-6: `train_data::diff::find_changed_functions` `usize` add without saturating
- **Difficulty:** easy
- **Location:** src/train_data/diff.rs:139
- **Description:** `let hunk_end = h.new_start + h.new_count.saturating_sub(1);` — only the inner subtraction is saturating; the outer add is bare. `parse_hunk_header` (line 50/58) parses raw `usize` from the diff header `@@ -a,b +c,d @@`. An attacker-supplied (or `git diff`-emitted-on-corrupt-pack) header `@@ -1,1 +18446744073709551615,2 @@` parses to `new_start = usize::MAX`, `new_count = 2`. `usize::MAX + 1` panics in debug; wraps to 0 in release, which then makes `hunk_end >= func.start_line` accidentally false-negative for every function. Reachable from `cqs review` / `affected` whenever the user pipes diff content through.
- **Suggested fix:** `h.new_start.saturating_add(h.new_count.saturating_sub(1))`. Or reject hunk headers where `new_start > i64::MAX as usize` at parse time (no real diff has line numbers > 2^63).

#### RB-V1.36-7: `where_to_add::compiled_import_regexes` mutex `expect` propagates panics
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:773 and :793
- **Description:** Both lock acquires use `.expect("compiled_import_regexes mutex poisoned")`. If any panic happens while holding the lock (regex compile that recurses into another panic, allocator failure inside `HashMap::get`), every subsequent `cqs where`/`task` call panics permanently for the lifetime of the process — the daemon can't recover without restart. The cache only stores regex Arcs; it's safe to recover from poison.
- **Suggested fix:** Replace both with `.unwrap_or_else(|e| e.into_inner())` (the same pattern already used at `embedder/provider.rs:512` for `ENV_LOCK`). Poison is recoverable here because the cache state isn't invariant-bearing — worst case we recompile a regex.

#### RB-V1.36-8: `language::Language::def` panics on disabled feature flag at runtime
- **Difficulty:** medium
- **Location:** src/language/mod.rs:1113
- **Description:** `Language::def()` calls `try_def()` and unconditionally panics with "Language '...' not in registry — check feature flags" when the registry lookup returns None. `try_def()` exists exactly to support this fallible path, but `def()` is called in many production sites that don't gate on the feature being enabled. If a stored chunk references a `Language` whose feature was compiled out (legitimately possible after a rebuild without `--features all-langs`), the next search/parse panics. The doc comment even calls this out as a deliberate panic but the production call sites don't all guard.
- **Suggested fix:** Audit `def()` callers — switch any non-test caller that touches stored data to `try_def()` and route through `LanguageError`. The bare panic should remain only on hard-coded compile-time language references.

#### RB-V1.36-9: `print_telemetry_text` divide-by-zero when `total == 0` but commands have entries
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/telemetry_cmd.rs:477
- **Description:** Triage v1.33.0 fixed P1-25 (sessions divisor in `format_sessions_line`, line 434), but the same file still has `let pct = (count as f64 / total as f64) * 100.0;` at line 477 where `total = output.events`. If telemetry events are filtered/empty but the commands map carries a stale 0-count entry, `total = 0` and `count` could be 0 → produces NaN, which clippy/serde would format as `NaN%`. Less likely than the sessions case (the canonical path empties commands when total=0) but the guard is one-sided.
- **Suggested fix:** Mirror the `if sessions > 0` guard from line 435: wrap the command-frequency loop in `if total > 0 { ... }`, or compute `pct` as `if total > 0 { count*100/total } else { 0 }`.

#### RB-V1.36-10: `store::sparse::token_dump_paged` casts `f64` weight to `f32` without finite check
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:400
- **Description:** `current_vec.push((token_id as u32, weight as f32))` reads `weight: f64` straight from SQLite. `cache.rs:639` already filters NaN/Inf at insert time for the embedding cache, but the sparse `weight` column has no such guard at read time. A hand-edited DB or a bug in the splade write path that writes Inf (matching the `test_raw_logits_positive_inf_passes_through_as_inf_weight` test that's still passing — see splade/mod.rs:1565) lands in every downstream BM25-style scorer as `f32::INFINITY`, which corrupts every sort that compares NaN-via-Inf+0.
- **Suggested fix:** Add `if !weight.is_finite() { tracing::warn!(...); continue; }` before the push. Cheap; consistent with the cache write path's policy.

DONE
