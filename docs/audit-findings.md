# v1.30.0 Post-Release Audit Findings

Generated: 2026-04-26T20:41:23Z

Total: 170 findings across 16 categories.


## Code Quality

#### `cmd_similar` JSON output emits 3 fields; batch `dispatch_similar` emits 9 — silent parity drop in the canonical SearchResult shape
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers/info.rs:139-148` (batch path), vs `src/cli/display.rs:397-413` (CLI path via `display_similar_results_json`) which routes through `cqs::store::SearchResult::to_json()` at `src/store/helpers/types.rs:143-156`
- **Description:** The CLI `cmd_similar` json branch calls `display_similar_results_json` → `r.to_json()`, which emits the canonical 9-field shape (`file, line_start, line_end, name, signature, language, chunk_type, score, content, has_parent`). The comment at display.rs:401-402 explicitly says "Delegate to SearchResult::to_json() for canonical base keys. Previously missing `type` and `has_parent` (CQ-NEW-5)." The batch path's `dispatch_similar` re-rolls JSON inline as `{name, file, score}` — only 3 fields, missing line numbers, signature, language, chunk_type, content, has_parent. Net effect: agents get a different schema for `cqs similar Foo` depending on whether the daemon is up. CQ-V1.29-3 fixed the *resolution* divergence here; this is the JSON-shape sister bug, same source.
- **Suggested fix:** In `dispatch_similar` at `src/cli/batch/handlers/info.rs:139-148`, replace the manual `serde_json::json!` map with `filtered.iter().map(|r| r.to_json()).collect::<Vec<_>>()`. Same line count, same envelope (`{results, target, total}`), now schema-identical to the CLI path. Add a snapshot test that asserts CLI and batch produce identical key sets for the same query.

#### `Reranker::new` silently ignores the `[reranker]` config section — `resolve_reranker(None)` is the only call site
- **Difficulty:** easy
- **Location:** `src/reranker.rs:127-154` (`Reranker::new`), `src/reranker.rs:61-77` (`resolve_reranker`), `src/reranker.rs:442-446` (`model_paths` calls `resolve_reranker(None)`)
- **Description:** `resolve_reranker` is the entire precedence chain documented at `:59-60`: "CLI → `CQS_RERANKER_MODEL` → `[reranker] model_path` → `[reranker] preset` → hardcoded `ms-marco-minilm`." Its signature accepts `section: Option<&AuxModelSection>` to thread `Config::reranker` (defined at `src/config.rs:228`) into the resolver. But `model_paths` always passes `None`. `Reranker::new()` doesn't accept a config at all; it only reads `CQS_RERANKER_MAX_LENGTH` and `CQS_RERANKER_MODEL` (via env inside resolve_reranker). Net effect: a user who writes `[reranker] preset = "bge-reranker-base"` in `.cqs.toml` gets the default ms-marco-MiniLM with zero error or warning — silent config drop, exactly the class of bug CLAUDE.md flags as "Docs Lying Is P1." The `section` parameter on `resolve_reranker` is also dead code as long as the only caller passes `None`.
- **Suggested fix:** Add `Reranker::with_section(section: Option<&AuxModelSection>)` (or change `new` to accept it) and thread `&config.reranker` through from the two production call sites (`src/cli/batch/mod.rs:1274`, `src/cli/store.rs:276` — both already have `Config` in scope). Inside `model_paths`, store the section on `self` (or pass through) and call `resolve_reranker(self.section.as_ref())` instead of `None`. Same pattern SPLADE already uses.

#### `dispatch_diff` builds a `target_store` placeholder that's never read in its else branch
- **Difficulty:** easy
- **Location:** `src/cli/batch/handlers/misc.rs:354-364, 367-387`
- **Description:** Lines 354-364 set `target_store` via `if/else`. The `if target_label == "project"` arm correctly captures `&ctx.store()`. The `else` arm calls `ctx.get_ref(target_label)?` (caches the reference store in BatchContext), then sets `target_store = &ctx.store()` with a comment "placeholder -- replaced below." But "below" at lines 367-387 is a *second* `if target_label == "project"` that only consumes `target_store` in the project branch. The else branch at line 376-387 never reads `target_store` — it calls `resolve_reference_store` afresh at line 378, opening yet another Store handle and bypassing the `get_ref` cache that line 360 just populated. So: (1) wasted I/O — the `get_ref` cached store is loaded then discarded; (2) dead-variable initialization in else; (3) a duplicate match on `target_label` that's structurally inevitable. Easy to misread and easy to break the cache invariant on the next refactor.
- **Suggested fix:** Collapse to one match. Either: (a) use `get_ref` + `borrow_ref` for both project and ref targets, since `borrow_ref` returns `Option<RefMut<RefIndex>>` and you can take `&store` from inside the borrow's scope — `cqs::semantic_diff` is fully synchronous and the borrow lives long enough; or (b) drop the `get_ref` call entirely from the else branch and let `resolve_reference_store` own the lifetime, removing the placeholder. Either way: one `if target_label == "project"` block, no placeholder variable.

#### Hardcoded "200" in user-facing gather warning lies when `CQS_GATHER_MAX_NODES` is set
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/gather.rs:200` (the println), `src/gather.rs:153-172` (`gather_max_nodes` with env override)
- **Description:** The text-mode warning is `println!("{}", "Warning: expansion capped at 200 nodes".yellow());` — a string literal. The actual cap is `gather_max_nodes()` which honors `CQS_GATHER_MAX_NODES` (logged at gather.rs:161 as "BFS node cap overridden via CQS_GATHER_MAX_NODES"). With `CQS_GATHER_MAX_NODES=500` the user sees "capped at 200" while results were actually capped at 500 — directly contradicts the env var the same module advertises in its tracing message. This is the exact "limit advertised ≠ limit applied" footgun that bites agents debugging "why am I getting only X results."
- **Suggested fix:** Either capture the cap on `GatherResult` (add `pub expansion_cap_used: usize` next to `expansion_capped`) and format the message with the real number, or look up `gather_max_nodes()` again at warn time. Latter is simpler and `gather_max_nodes` already memoizes via OnceLock. Prefer the former because it makes the cap visible in `--json` output too, where agents can react to it.

#### Embedding/Query cache `open_with_runtime` are ~80% copy-paste — 90+ duplicated lines in cache.rs
- **Difficulty:** medium
- **Location:** `src/cache.rs:103-220` (`EmbeddingCache::open_with_runtime`) and `src/cache.rs:1412-1522` (`QueryCache::open_with_runtime`)
- **Description:** Both methods do, in identical order: (1) `info_span!`, (2) `create_dir_all` parent + `#[cfg(unix)] set_permissions(parent, 0o700)` with identical "best effort" warn block (16 lines × 2), (3) Tokio runtime fallback (`if let Some(rt) = runtime { rt } else { Builder::new_current_thread()... }` — 9 lines × 2), (4) `SqliteConnectOptions` + `SqlitePoolOptions::new().max_connections(1).idle_timeout(30s).connect_with(opts)` — only `busy_timeout(5000 vs 2000)` differs, (5) `CREATE TABLE IF NOT EXISTS` (different schemas), (6) `0o600` chmod loop on `["", "-wal", "-shm"]` with identical comment shape (22 lines × 2). Total: ~90 lines duplicated. The two `Drop` impls (`:723-754`, `:1717-1745`) repeat the same panic-message extraction (4 lines × 2). Bug surface: any future hardening of the chmod block, or change to runtime construction, has to be applied twice. PB-V1.29-7 already noticed both blocks separately — the extracted helper is the structural fix.
- **Suggested fix:** Extract three private helpers in `cache.rs` (or `store/helpers/sql.rs` next to `busy_timeout_from_env`): `prepare_cache_dir_perms(parent: &Path)` (parent chmod), `apply_db_file_perms(path: &Path)` (the 0o600 loop), and `connect_cache_pool(path, busy_ms, runtime, schema_sql)` (the 4-step open). Both `EmbeddingCache::open_with_runtime` and `QueryCache::open_with_runtime` collapse to ~30 lines each. The two `Drop` impls share `extract_panic_msg(payload)` — same shape as the existing `cli/pipeline/mod.rs::panic_message` (see next finding).

#### `panic_message` helper duplicated 4 ways across 3 modules — `cli/pipeline/mod.rs::panic_message` and 3 inline copies in Drop impls
- **Difficulty:** easy
- **Location:** `src/cli/pipeline/mod.rs:223-232` (`fn panic_message`), `src/store/mod.rs:1322-1326` (Store::drop), `src/cache.rs:743-747` (EmbeddingCache::drop), `src/cache.rs:1735-1739` (QueryCache::drop)
- **Description:** Four copies of the same `payload.downcast_ref::<&str>().or(downcast_ref::<String>()).unwrap_or("unknown panic")` extraction. The pipeline version is a free function; the three Drop versions inline it. Functions return `String` vs `&str`, but the logic is identical. Anyone tightening the panic-extraction (e.g. adding `Box<dyn Error>` or a `format!` of the type id when both downcasts fail) has to update four sites. Low-risk debt but easy to delete.
- **Suggested fix:** Promote `panic_message` to `pub(crate) fn` in a single common module (`src/lib.rs` next to `temp_suffix`, or a new `src/panic_msg.rs`). Make all four sites use it. The Drop sites currently take `&Box<dyn Any + Send>`; harmonize on that signature so all callers fit. Drop the pipeline-private version.

#### Repeated `match std::env::var("CQS_*") { Ok(v) => v.parse()... }` pattern at 25+ sites — `limits::parse_env_*` helpers exist but are pub(crate)-private
- **Difficulty:** medium
- **Location:** `src/limits.rs:230-260` (`parse_env_f32` / `parse_env_usize` / `parse_env_u64`); duplicated open-coded equivalents in: `src/cli/watch.rs:65,74,100,498,510,766,942,1430` (8 sites), `src/llm/mod.rs:176,315,406,434` (4 sites), `src/cli/pipeline/types.rs:80,98,117,144` (4 sites), `src/hnsw/persist.rs:19,41,63` (3 sites), `src/embedder/models.rs:565,571`, `src/embedder/mod.rs:330`, `src/cache.rs:206,1509`, `src/gather.rs:156`, `src/cli/commands/graph/trace.rs:357`, `src/impact/bfs.rs:16`, `src/reranker.rs:129`
- **Description:** The library limits module (`src/limits.rs:224`) clearly notes "shared parsing helpers" with three carefully-tested fns covering `f32 / usize / u64` shapes. They reject zero, garbage, and non-finite values uniformly, with consistent test coverage. But the helpers are `pub(crate)` — visible only inside `cqs::limits` callers. Every other module that wants the same behavior re-rolls the pattern, frequently with slight variations: some accept `0`, some don't; some warn on bad input, most don't; some clamp, some don't. `EmbeddingCache::open_with_runtime` accepts `CQS_CACHE_MAX_SIZE=0` (silently sets cap to 0 → always-evict mode), while `QueryCache` rejects `0` (`.filter(|&n: &u64| n > 0)`) and falls back to default. Same env-shape, opposite behavior. Standardizing fixes this and shrinks the codebase ~150 lines. (See related finding on `CQS_CACHE_MAX_SIZE` zero-handling below.)
- **Suggested fix:** Move `parse_env_usize` / `_u64` / `_f32` to `src/lib.rs` (or new `src/env.rs`) as `pub fn`, add a `parse_env_duration_secs` variant for `Duration` cases (`local_timeout` etc.), and do a focused sweep replacing the open-coded sites. Drop the `limits.rs` private copies, re-export from there for convenience. One `pub fn` change + ~25 site edits.

#### `EmbeddingCache::open_with_runtime` accepts `CQS_CACHE_MAX_SIZE=0`; `QueryCache::open_with_runtime` rejects it — opposite behavior for sister env vars
- **Difficulty:** easy
- **Location:** `src/cache.rs:206-209` (Embedding, no zero filter), `src/cache.rs:1509-1513` (Query, `.filter(|&n: &u64| n > 0)`)
- **Description:** Side-by-side in the same file, two sibling caches handle their `CQS_*_MAX_SIZE` env knob differently. `EmbeddingCache` does `env::var(...).ok().and_then(|s| s.parse().ok()).unwrap_or(10 * 1024^3)` — `CQS_CACHE_MAX_SIZE=0` parses fine to `0`, sets `max_size_bytes = 0`, and `evict()` then aggressively evicts everything to fit under 0 bytes. `QueryCache` adds `.filter(|&n: &u64| n > 0)` so `CQS_QUERY_CACHE_MAX_SIZE=0` falls back to the 100 MB default. Whichever you intended — uniform fallback, or "0 disables the cache" — picking only one accidentally is the wrong outcome. This is the exact class of inconsistency the proposed shared `parse_env_u64` helper above closes.
- **Suggested fix:** Pick a semantic and document it. If "0 disables": uniformize both to accept 0, document, and have `evict()` short-circuit when `max_size_bytes == 0`. If "0 is invalid, use default": add `.filter(|&n: &u64| n > 0)` to the EmbeddingCache parse at `src/cache.rs:208`. Either way, drive the parse through one helper and write the test once.

#### `cli/commands/resolve.rs::find_reference` and the inline lookup inside `resolve_reference_db` re-roll the same "find ref by name + nice error" twice
- **Difficulty:** easy
- **Location:** `src/cli/commands/resolve.rs:26-39` (`find_reference`), `src/cli/commands/resolve.rs:46-57` (inline `references.iter().find(|r| r.name == name)` with byte-identical error message)
- **Description:** Both functions: (1) call `Config::load(root)`, (2) iterate `config.references`, (3) `.find(|r| r.name == name)`, (4) return `anyhow::anyhow!("Reference '{}' not found. Run 'cqs ref list' to see available references.", name)` on miss. The only difference is `find_reference` returns the full `ReferenceIndex` (loaded via `reference::load_references`), while `resolve_reference_db` only needs the `ReferenceConfig.path`. The error-message duplication is verbatim — change "Run 'cqs ref list'" to anything else and one call site silently lies. Three other call sites across `resolve_reference_store` / `resolve_reference_store_readonly` chain through `resolve_reference_db`, so the bug surfaces on any of them.
- **Suggested fix:** Extract `find_reference_config(config: &Config, name: &str) -> Result<&ReferenceConfig>` returning the typed config row (no IO, just the find + message). `find_reference` then becomes `let cfg = find_reference_config(&config, name)?; reference::load_references(slice::from_ref(cfg)).into_iter().next().ok_or(...)`; `resolve_reference_db` uses the same helper to read `cfg.path`. One source of truth for the error string.

#### `slot::libc_exdev` hardcodes errno 18 with comment claiming `libc` would be "just for this constant" — but `libc` is already a workspace dep
- **Difficulty:** easy
- **Location:** `src/slot/mod.rs:640-647`
- **Description:** Comment (lines 640-643): "We hardcode 18 (Linux) since `libc::EXDEV` would pull in a libc dep just for this constant. macOS also uses 18; Windows doesn't surface EXDEV the same way." `libc = "0.2"` is in `Cargo.toml` and already imported in `src/cli/watch.rs` (`extern "C" fn on_sigterm(_sig: libc::c_int)`). The hardcode is correct in practice (Linux + macOS both define EXDEV=18; FreeBSD too), but the *justification* is wrong, and a future maintainer reading the comment will assume libc isn't available. Worse, on the unlikely platform where EXDEV diverges (some BSD variants in theory), the hardcoded `18` silently mis-classifies the error and the EXDEV → copy+remove fallback in `move_file` skips. Self-documenting fix: use `libc::EXDEV` and delete the comment.
- **Suggested fix:** Replace the `fn libc_exdev() -> i32 { 18 }` shim with `libc::EXDEV` directly at the call site in `src/slot/mod.rs:631`. `#[cfg(unix)]` already gates the EXDEV branch implicitly via `raw_os_error()` returning a Linux/macOS errno; the `libc` dep is unconditionally available on those targets through the existing watch.rs usage. Remove the misleading comment.


## Documentation

#### DOC-V1.30-1: PRIVACY.md and SECURITY.md falsely claim `~/.cache/cqs/query_log.jsonl` is opt-in
- **Difficulty:** easy
- **Location:** `PRIVACY.md:22`, `SECURITY.md:101`, vs `src/cli/batch/commands.rs:371-391` (`log_query`)
- **Description:** **P1: Docs Lying.** Both docs state the per-user query log is "opt-in, written only when `CQS_TELEMETRY=1` or the file already exists." `log_query` in `src/cli/batch/commands.rs` is called unconditionally from `BatchCmd::Search/Gather/Onboard/Scout/Where/Task` dispatch arms (`commands.rs:418, 441, 477, 487, 491, 513`) and uses `OpenOptions::new().create(true).append(true)` — it creates the file on first batch query regardless of env var or prior file presence. Every `cqs chat` / `cqs batch` user is silently building a search-history file at `~/.cache/cqs/query_log.jsonl` despite the privacy/security docs promising opt-in behaviour. Search queries can contain code snippets, identifiers, internal hostnames.
- **Suggested fix:** Either gate `log_query` on `std::env::var("CQS_TELEMETRY") == Ok("1")` plus existing-file check (matching the documented contract and the `cli/telemetry.rs::record` pattern), or rewrite both docs to state that the log is unconditional. The privacy contract is the more defensible direction — fix the code, keep the docs.

#### DOC-V1.30-2: PRIVACY.md claims `query_cache.db` has a 7-day TTL — code only enforces a 100 MiB size cap
- **Difficulty:** easy
- **Location:** `PRIVACY.md:21` vs `src/cache.rs:1536-1606` (`QueryCache::evict`) and `src/cli/batch/mod.rs:1351-1376`
- **Description:** **P1: Docs Lying.** Privacy doc says "recent query embeddings with a 7-day TTL." There is no time-based TTL anywhere in `QueryCache`. `evict()` looks at `CQS_QUERY_CACHE_MAX_SIZE` (100 MiB default) and trims oldest rows by `ts ASC` only when the disk size exceeds the cap. `prune_older_than(days)` exists but is only invoked by the user-typed `cqs cache prune <days>` command, never automatically. A user who runs `cqs` daily for months will retain every unique search query embedding indefinitely up to the size cap, not for "7 days."
- **Suggested fix:** Replace the line in PRIVACY.md with: "recent query embeddings, evicted oldest-first when the DB exceeds `CQS_QUERY_CACHE_MAX_SIZE` (100 MiB default). Prune older entries with `cqs cache prune <DAYS>`."

#### DOC-V1.30-3: CHANGELOG v1.30.0 names `CQS_LLM_ENDPOINT` for local LLM provider — actual env var is `CQS_LLM_API_BASE`
- **Difficulty:** easy
- **Location:** `CHANGELOG.md:19`
- **Description:** v1.30.0 release entry says "`cqs index --llm-summaries` accepts a local OpenAI-compatible endpoint via `CQS_LLM_ENDPOINT`." `CQS_LLM_ENDPOINT` does not exist in the codebase. The actual env vars are `CQS_LLM_PROVIDER=local` plus `CQS_LLM_API_BASE=http://...` (`src/llm/mod.rs:227, 378-385`). README/SECURITY both document `CQS_LLM_API_BASE` correctly. A user copy-pasting the CHANGELOG instructions will get "CQS_LLM_PROVIDER=local requires CQS_LLM_API_BASE" at runtime.
- **Suggested fix:** Rewrite the line as: "...accepts a local OpenAI-compatible endpoint via `CQS_LLM_PROVIDER=local` + `CQS_LLM_API_BASE=...`." Same fix mirrors how the README env-var table phrases it (`README.md:740-745`).

#### DOC-V1.30-4: CONTRIBUTING.md "Adding a New CLI Command" still tells contributors to add a match arm in `dispatch.rs` — that hasn't been the procedure since #1097
- **Difficulty:** easy
- **Location:** `CONTRIBUTING.md:339-355` vs `src/cli/registry.rs:1-29` (header doc)
- **Description:** **P1: Docs Lying.** v1.30.0 #1097/#1114 collapsed five exhaustive matches (`Commands::batch_support`, `variant_name`, dispatch Group A, dispatch Group B, batch classification) into one `for_each_command!` table in `src/cli/registry.rs`. registry.rs:8-21 says: "Adding a new command now means: declare the variant in `definitions.rs::Commands`, add one row to either `group_a` or `group_b` list below, implement the handler. A missing row is a compile error." The contributing checklist still says: "**Dispatch** — match arm in `src/cli/dispatch.rs`" with no mention of `registry.rs`. A new contributor following the checklist will edit dispatch.rs (which now generates from registry) and either fail to compile or paste an arm into the wrong file. Architecture Overview at line 154-158 also lists `dispatch.rs` but never mentions `registry.rs`.
- **Suggested fix:** (1) Replace step 4 with: "**Registry row** — add a `(bind, wild, name, batch_support, body)` row to `group_a` or `group_b` in `src/cli/registry.rs`; the macro generates dispatch + variant_name + batch_support". (2) Add `registry.rs - for_each_command! table; single source of truth for dispatch + variant_name + batch_support` next to `dispatch.rs` in the Architecture Overview block.

#### DOC-V1.30-5: README "Claude Code Integration" command list missing 5 user-facing commands (`ping`, `eval`, `model`, `serve`, `refresh`)
- **Difficulty:** easy
- **Location:** `README.md:467-525`
- **Description:** The `<claude>` integration block tells agents to install this list as their CLAUDE.md command reference. `cqs --help` shows 53 top-level commands; the README list omits: `cqs ping` (daemon healthcheck), `cqs eval` (eval harness, v1.29.x first-class), `cqs model` (show/list/swap embedding model — referenced by `audit-mode.json` doc and CHANGELOG), `cqs serve` (flagship v1.29.0 web UI; DOC-V1.29-2 noted absence in usage section but it's also missing from the agent-facing list), `cqs refresh` / `cqs invalidate` (added v1.30.0 per CHANGELOG line 22). Agents whose CLAUDE.md is bootstrapped from this list won't know these commands exist.
- **Suggested fix:** Append five lines mirroring the existing format, with a one-sentence summary each. `serve` should link to the auth-token launch banner.

#### DOC-V1.30-6: README "Claude Code Integration" lists `cqs cache stats/prune/compact` — actual subcommands are `stats/clear/prune/compact`
- **Difficulty:** easy
- **Location:** `README.md:521`
- **Description:** `cqs cache --help` shows four subcommands: `stats`, `clear`, `prune`, `compact`. README and CLAUDE-Code line says only `stats/prune/compact`. `clear` is the destructive "delete all cached embeddings (or only for a model fingerprint)" — useful and dangerous, should be in agent docs.
- **Suggested fix:** Change `cqs cache stats/prune/compact` to `cqs cache stats/clear/prune/compact` and document `clear --model <fp>` semantics in the trailing sentence.

#### DOC-V1.30-7: README claims "544-query dual-judge eval" in TL;DR — actual eval is 218 queries (109 test + 109 dev)
- **Difficulty:** easy
- **Location:** `README.md:5` (TL;DR) vs `README.md:649` (eval section)
- **Description:** TL;DR headline boasts "**42.2% R@1 / 67.0% R@5 / 83.5% R@20 on a 544-query dual-judge eval against the cqs codebase itself**". The actual Retrieval Quality section twelve hundred lines later says "**Live codebase eval** — 218 queries (109 test + 109 dev)". 544 doesn't appear anywhere else in the codebase or eval scripts; per memory the v3.v2 fixture is 109/109. Per memory + CHANGELOG #1109 "v3.v2 fixture refreshed 2026-04-25", the test R@5 is now 63.3% (74.3% dev) — the 67.0% is also the canonical pre-refresh number, not current. The "544" is likely a hangover from an earlier fixture (v2 had ~272 each split = 544 total).
- **Suggested fix:** Replace "544-query" with "218-query (109 test + 109 dev) v3.v2" and either pin the metrics to a specific commit ("on commit X") or refresh to the current 63.3% / 74.3% (test R@5 / dev R@5). Both numbers should match between TL;DR and the table.

#### DOC-V1.30-8: README "54 languages" claim conflicts with Cargo.toml (lang-elm now in default features) and source code (52 lang variants)
- **Difficulty:** easy
- **Location:** `README.md:5`, `README.md:530-585` (Supported Languages list), `CONTRIBUTING.md:135,187` vs `src/language/mod.rs` (`define_languages!`) and `Cargo.toml:179` (`default = [..., "lang-elm", ...]`)
- **Description:** README repeats "54 languages" three times (TL;DR, Supported Languages header, How It Works step 1). The Supported Languages list itself shows 53 bullet items and never mentions Elm, despite `lang-elm` being in Cargo.toml's default feature list and `Elm => "elm", feature = "lang-elm"` registered in `src/language/mod.rs`. `define_languages!` macro emits 52 language variants when grepped (count includes Markdown but excludes ASPX/Razor/Vue/Svelte/HTML which delegate to other grammars per existing readme prose). Mismatch is small but the README list literally omits Elm — search for "Elm" returns zero hits in README.md.
- **Suggested fix:** Either (a) recount and replace "54" with the audited count (likely 53 or 52 depending on whether Markdown / multi-grammar dispatch counts) and add an Elm bullet to the alphabetical list, or (b) remove `lang-elm` from default if Elm support is not actually shipping. Plumb the count through `language/mod.rs` registry length so it can't drift again.

#### DOC-V1.30-9: SECURITY.md "Read Access" table omits per-project embeddings cache (`<project>/.cqs/embeddings_cache.db`)
- **Difficulty:** easy
- **Location:** `SECURITY.md:65-82`
- **Description:** The Read Access filesystem table lists only the legacy global cache `~/.cache/cqs/embeddings.db`. v1.30.0 (PR #1105) added a per-project cache at `<project>/.cqs/embeddings_cache.db` (`src/cache.rs:91-93::project_default_path`) that is now the primary cache; the global one is consulted on miss for back-compat (PRIVACY.md gets this right at lines 16-20). A security reviewer reading SECURITY.md will think there's only one cache file to consider.
- **Suggested fix:** Add a row to both Read Access and Write Access tables: `<project>/.cqs/embeddings_cache.db` — Per-project embedding cache (#1105, primary; legacy global cache at `~/.cache/cqs/embeddings.db` is fallback) — `cqs index`, search.

#### DOC-V1.30-10: `enumerate_files` doc comment claims "Respects .gitignore" — also layers `.cqsignore` when `no_ignore=false`
- **Difficulty:** easy
- **Location:** `src/lib.rs:542-547` doc comment vs body at `src/lib.rs:566-569`
- **Description:** Public-API doc says: "Respects .gitignore, skips hidden files and files larger than `CQS_MAX_FILE_SIZE` bytes". The body adds `wb.add_custom_ignore_filename(".cqsignore")` when `no_ignore` is false (`lib.rs:567-569`). `.cqsignore` is the headline v1.29.0 feature for excluding files committed to git but never indexed; library consumers calling `cqs::enumerate_files` need to know the function honours it (or reconfigure if they don't want it). Body comment at line 560-565 explains the rationale but the function's outer doc comment doesn't surface it.
- **Suggested fix:** Update the doc comment to: "Respects `.gitignore` and `.cqsignore` (additive on top of `.gitignore`, both disabled by `no_ignore=true`); skips hidden files and files larger than `CQS_MAX_FILE_SIZE` bytes (default 1 MiB — generated code can exceed this)."


## API Design

#### `cqs --json model swap <bad-preset>` emits plain-text error, not envelope JSON
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/model.rs:cmd_model_swap` (the `Unknown preset 'X'.` `anyhow::anyhow!` and the `No index at ...` `bail!`) and the parallel `cmd_model_show` `bail!` for missing-index.
- **Description:** `cqs --json model swap nonexistent-preset` prints `Error: Unknown preset 'nonexistent-preset-zzz'. ...` to stderr and exits non-zero — no JSON envelope. Verified live. Compare to `cqs --json ref remove nonexistent-zzz` which emits `{"data":null,"error":{"code":"not_found","message":"..."}, "version":1}` via `json_envelope::emit_json_error`. Same `--json` global flag, two different error contracts. Agents driving model swaps from CI can't differentiate "bad preset" from "transient I/O failure" because the surface looks identical (stderr text + non-zero exit). `cmd_model_show`'s `No index at ...` `bail!` has the same shape problem on the read path. The v1.30.0 audit fixed this for `ref/project/telemetry`; `model` was missed.
- **Suggested fix:** Wrap the `anyhow::anyhow!`/`bail!` sites in `cmd_model_swap` and `cmd_model_show` so that when `json: bool` is true they route through `json_envelope::emit_json_error` with codes `unknown_preset`, `no_index`, `already_on_target` (the no-op short-circuit), and `swap_failed`. Pattern is already established in `cmd_ref_remove` (`src/cli/commands/infra/reference.rs`).

#### `cqs init`, `cqs index`, `cqs convert` lack `--json` despite being the headline mutation surface
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/init.rs:cmd_init`; `src/cli/commands/index/build.rs:cmd_index` (and `IndexArgs` in `src/cli/args.rs:567+` has no `json` field); `src/cli/commands/index/convert.rs` (no `json` flag).
- **Description:** `cqs init`, `cqs index`, and `cqs convert` are the only path for an agent to bootstrap or refresh an index. None of the three accept `--json`. `cqs index --json` errors with `unexpected argument '--json'`; `cqs init --json` likewise. Every other long-running command (`cqs gc`, `cqs model swap`, `cqs index` peer commands) already supports `--json`. The result is that an automation pipeline that wraps `cqs init && cqs index --llm-summaries` can't capture timing/byte/chunk-count metrics structurally — it has to scrape colored stdout. Particularly painful for `cqs index --llm-summaries --improve-docs` which spends real money and has no machine-readable summary on completion. The CONTRIBUTING "Dry-Run vs Apply" comment in the code already lumps `index` and `convert` as "side-effect commands"; both should also be the `--json`-emitting commands.
- **Suggested fix:** Add `#[arg(long)] pub json: bool` to `IndexArgs`, the new `InitArgs` (currently no args struct — need to introduce one), and `ConvertArgs`. Thread `cli.json || args.json` through `cmd_init` / `cmd_index` / `cmd_convert` and emit a final `{indexed_files, indexed_chunks, took_ms, model, summaries_added?, docs_improved?}` (or analog) envelope via `json_envelope::emit_json` after the work completes. Suppress the per-step progress lines when `json` is set (or route them to stderr — doctor already does this with "Colored human-readable check progress is routed to stderr in this mode").

#### Global `--slot` is silently ignored by every `cqs slot` and `cqs cache` subcommand
- **Difficulty:** easy
- **Location:** `src/cli/definitions.rs` (`pub slot: Option<String>` declared with `#[arg(long, global = true)]`); `src/cli/commands/infra/slot.rs` (`SlotCommand::Create/Promote/Remove` take a positional `name` and never read `cli.slot`); `src/cli/commands/infra/cache_cmd.rs:resolve_cache_path` (no `cli.slot` reference, cache is project-scoped, not per-slot).
- **Description:** `--slot <NAME>` is `global = true` on the top-level `Cli`, so it appears in every subcommand's `--help` (verified: `cqs slot create --help`, `cqs slot promote --help`, `cqs slot remove --help`, `cqs cache stats --help` all advertise `--slot <SLOT>`). For `slot create/promote/remove/active` and the entire `cache` subtree it's a no-op: `slot create foo --slot bar` creates `foo` and ignores `bar` (verified live), `cache stats --slot foo` opens the project-wide cache regardless. The help text actively lies about supported behaviour — agents reading `--help` will conclude they can scope cache ops to a slot. This is the worst kind of API drift: the flag works, parses, and is silently dropped.
- **Suggested fix:** Move `--slot` off `global = true` and onto only the subcommands that actually consume it (every `Commands::*` that ends up calling `cqs::slot::resolve_slot_name` or threads slot into `CommandContext`). For `slot create/promote/remove/active` and `cache *`, keep `--slot` off the surface entirely. Alternative: enforce at dispatch time — `if cli.slot.is_some() && !subcommand_uses_slot { bail!("--slot has no effect on `cqs {subcommand}`") }` — louder than silent-drop, cheap to implement.

#### `cqs refresh` has no `--json`; every other infra command does
- **Difficulty:** easy
- **Location:** `src/cli/definitions.rs:755-761` (`Commands::Refresh` — no JSON arg), and `src/cli/registry.rs` dispatch arm.
- **Description:** `cqs refresh` (alias `invalidate`) is the user-facing surface for the daemon's cache-drop verb. It returns no JSON output — verified, `cqs --json refresh` prints plain text. Compare to `cqs ping --json`, `cqs gc --json`, `cqs cache compact --json`, `cqs telemetry --reset --json`, `cqs slot promote foo --json` — every other infra-mutation/healthcheck supports `--json`. An agent that just promoted a slot needs `refresh` followed by a query; without `--json` it can't confirm "cache invalidated, re-open succeeded" structurally. Was caught in v1.30.0 audit as missing CLI surface (added) but the new entry shipped without the standard JSON contract.
- **Suggested fix:** Add `#[command(flatten)] output: TextJsonArgs` to `Commands::Refresh`, thread into the dispatch arm, and emit `{"refreshed": true, "daemon_running": bool, "caches_invalidated": [...]}` when the flag is set. The daemon-side `dispatch_refresh` (`src/cli/batch/handlers/refresh.rs` or wherever it lives) likely already returns a JSON object — surface it on the CLI path too.

#### List-shape JSON envelopes are inconsistent across `*list` commands
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/reference.rs:cmd_ref_list` (raw array); `src/cli/commands/infra/model.rs:cmd_model_list` (raw array); `src/cli/commands/infra/project.rs:124-140` (object `{projects: [...]}`); `src/cli/commands/infra/slot.rs:slot_list` (object `{active, slots: [...]}`); `src/cli/commands/io/notes.rs:cmd_notes_list` (object `{notes: [...]}`); `src/cli/commands/search/query.rs` (object `{query, results, total}`).
- **Description:** Verified live: `cqs ref list --json` emits `{"data":[{...},{...}], "error":null, "version":1}` (raw array under `data`). `cqs project list --json` emits `{"data":{"projects":[...]}, ...}` (object under `data`). `cqs slot list --json` emits `{"data":{"active":"default","slots":[...]}, ...}`. `cqs model list --json` emits `{"data":[{...}], ...}` (raw array). Five `*list --json` surfaces, three different shape conventions. Agents writing a generic `data.{slots,projects,refs,models,notes}` accessor have to special-case ref and model. The right convention is the wrapping-object form — it leaves room for sibling fields like the slot-list `active` summary or the search `total` count without breaking the schema.
- **Suggested fix:** Standardize on `{"data": {"<plural-name>": [...]}}` for every list-emitting subcommand. Concretely: change `cmd_ref_list` to emit `{"references": [...]}` and `cmd_model_list` to emit `{"models": [...], "current": "<name>"}` (the latter folds the `current: bool` per-row flag into a top-level `current` field — cleaner). Hard rename, no-external-users; ship it as a single PR that touches both call sites + their tests.

#### `cqs cache stats` JSON mixes byte and megabyte units; `cache compact` uses bytes only
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/cache_cmd.rs:cache_stats` (emits both `total_size_bytes` AND `total_size_mb`) vs `cache_compact` (emits `reclaimed_bytes`, `size_after_bytes`, `size_before_bytes`).
- **Description:** Verified: `cqs cache stats --json` emits `total_size_bytes: 1662976, total_size_mb: 1.5859375` — same number, two units. `cqs cache compact --json` emits `reclaimed_bytes / size_after_bytes / size_before_bytes` — bytes only. An agent computing "free space saved" has to special-case which fields are present per command. Mixed-unit fields are also fragile: a future SI-vs-binary correction (1024 vs 1000) silently shifts `*_mb` while `*_bytes` stays correct, producing two-source-of-truth bugs.
- **Suggested fix:** Drop `total_size_mb` from `cache_stats` JSON (keep it in the human text path where the caller wants a digestible number). Bytes are the canonical unit across the rest of cqs (`size_*_bytes` already in `compact`, `chunks` are counts not sizes). For human stdout, format-on-display.

#### `pub use nl::*` leaks dead `generate_nl_with_call_context` wrapper into the lib API
- **Difficulty:** easy
- **Location:** `src/lib.rs:165` (`pub use nl::*`); `src/nl/mod.rs:43-59` (`generate_nl_with_call_context` — five-arg wrapper around the seven-arg `_and_summary` variant).
- **Description:** `generate_nl_with_call_context` exists only as a thin wrapper that hard-codes `summary = None, hyde = None` and calls `generate_nl_with_call_context_and_summary`. Production code has zero callers — verified with `grep -rn 'generate_nl_with_call_context\b' src/ | grep -v '/nl/mod.rs:'` returns empty. Only the tests in `src/nl/mod.rs` use it. It's `pub` and re-exported via `pub use nl::*`, so it's part of the lib's public API surface. Not just dead code — it's dead public API, which is worse because removing it later is a "breaking" change in any consumer that picked it up. Same anti-pattern likely lurks under the other ten `pub use foo::*` glob re-exports in `src/lib.rs`.
- **Suggested fix:** Drop `generate_nl_with_call_context` outright (only the test sites need to be updated to call `_and_summary` with `None, None`). Then audit all eleven `pub use foo::*` lines in `src/lib.rs` (`diff`, `gather`, `impact`, `nl`, `onboard`, `project`, `related`, `scout`, `search`, `task`, `where_to_add`) — replace each with explicit `pub use foo::{...}` lists naming the items the lib actually wants to publish. Glob re-exports are a footgun for surface control: every new `pub fn` in any of those modules joins the public API automatically.

#### `cqs gather --expand <N>` and `cqs --expand-parent` collide on a high-traffic flag name
- **Difficulty:** easy
- **Location:** top-level `src/cli/definitions.rs:Cli::expand_parent` (flag `--expand-parent`, parent-context expansion, `bool`); `src/cli/args.rs:GatherArgs::expand` (flag `--expand`, call-graph BFS depth, `usize`); the v1.30.0 fix renamed `SearchArgs::expand` to `expand_parent` with `--expand` as a visible alias for back-compat.
- **Description:** `cqs gather foo --expand 3` (graph depth) and `cqs foo --expand-parent` (parent-context `bool`) both accept the substring `--expand`. The v1.30.0 fix made `SearchArgs::expand` an alias for `expand_parent`, but `GatherArgs::expand` is still a distinct usize-typed flag. `cqs --expand 2 gather foo` parses `--expand 2` against the *top-level* `Cli` first — and there's no top-level `--expand`, so it errors, but the error doesn't say "did you mean `--expand-parent` (top-level) or did you mean to pass `--expand 2` after `gather`?" The dual semantic (bool parent-context vs usize call-graph depth) under similar flag names is the original API-V1.22-3 footgun that motivated the rename — it's only half-fixed.
- **Suggested fix:** Rename `GatherArgs::expand` to `--graph-depth` (or `--depth` to match `onboard`/`impact`/`test-map` which all already use `--depth` for the same call-graph concept post-v1.30.0). That eliminates the `--expand` collision entirely and aligns the four call-graph-depth-taking commands under one flag name. Ship as hard rename — no external users.

#### `cqs eval` `--save` accepts a path with no `.json` validation; output JSON is hard-coded
- **Difficulty:** easy
- **Location:** `src/cli/commands/eval/mod.rs:EvalCmdArgs::save` (`Option<PathBuf>`).
- **Description:** `cqs eval queries.json --save report` writes a JSON report to a file literally named `report` (no extension). The flag is `Option<PathBuf>` with no validation, no extension default, no warning. `cqs eval` is the eval harness — a sloppy filename in a release-comparison workflow turns into "did I overwrite my baseline?" guessing. Compare `cqs eval --baseline report` which requires the path exist, contrasted with `--save` which just overwrites. Asymmetric input/output validation on a command meant for release-gate comparisons.
- **Suggested fix:** When `--save <path>` lacks an extension, append `.json` automatically (with a one-line stderr note). When it has the wrong extension (`.txt`, `.yaml`), error out — eval reports are JSON-only. Mirrors the validation `cqs train-data --output <path>` should also have but doesn't.

Summary: 0 findings carried forward (all 10 archived findings were fixed in v1.30.0 — verified via direct source reads of `src/cli/commands/infra/{project,reference,telemetry_cmd,model}.rs`, `src/cli/args.rs:NotesListArgs`, `src/cli/batch/handlers/misc.rs`, `src/cli/dispatch.rs:try_daemon_query`, `src/cli/definitions.rs:Refresh`, `src/cli/commands/eval/mod.rs:EvalCmdArgs::limit`); 9 new findings, 10 archived findings dropped as fixed.


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


## Observability

Coverage is broadly excellent in v1.30.0 — the v0.12.1 lesson has been applied across all post-v0.9.7 modules and v1.30.0 additions (`slot`, `cache`, `serve`, embedder/provider). Previously-flagged OB-V1.29-1 through OB-V1.29-7 are now fixed. Below are NEW observability gaps not in `audit-triage-v1.30.0.md`.

#### OB-V1.30-1: Default subscriber drops EVERY `info_span!` event — no spans render at default log level
- **Difficulty:** easy
- **Location:** `src/main.rs:14-32`
- **Description:** The default `EnvFilter` is `"warn,ort=error"`, but every span in the codebase (~150 sites) is `tracing::info_span!` (INFO) or `tracing::debug_span!`. Under default settings none of these match the filter, so the subscriber emits nothing for them. `fmt::init()` also never calls `.with_span_events(FmtSpan::CLOSE)` (or `NEW | CLOSE`), so even if INFO were enabled, span boundaries would not produce log lines on entry/exit — only events emitted *inside* the span would. Consequence: a user running `cqs index` with default config sees no per-batch progress, no SPLADE timing, no UMAP rows-projected count from spans alone — the heavy investment in span instrumentation across `scout`, `gather`, `serve`, `cache`, `slot`, parser, store, and embedder is invisible until someone discovers `--verbose` or `RUST_LOG=info`. There is no runtime way to trace one `cqs serve` request end-to-end without rebuilding the process. Operators of the daemon (`cqs watch --serve`) hit this hardest because the systemd unit inherits empty `RUST_LOG`.
- **Suggested fix:** (a) Bump default to `"cqs=info,warn"` (cqs's own crate at INFO, world at WARN) so existing spans render without third-party noise. (b) Configure span events: `tracing_subscriber::fmt().with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE).with_env_filter(filter)…` so close events emit `latency_ms` automatically. (c) Add a `--log-format=json` flag (and `CQS_LOG_FORMAT=json`) wired to `.json()` on the fmt builder, so daemon journals are structurally consumable.

#### OB-V1.30-2: `auth::enforce_auth` rejects 401 silently — no `tracing::warn!` on auth failure
- **Difficulty:** easy
- **Location:** `src/serve/auth.rs:194-232` (specifically `AuthOutcome::Unauthorized` branch at lines 224-230)
- **Description:** The new per-launch token middleware (#1118, SEC-7) emits zero log output when a request fails authentication. A brute-force scan, expired bookmark, or misconfigured client gets a generic `401 Unauthorized` body and the operator has no journal trail showing it happened, the path attempted, or the source. Asymmetric with `enforce_host_allowlist` at `src/serve/mod.rs:246`, which DOES emit `tracing::warn!(host = %host, "serve: rejected request with disallowed Host header")`. Auth failures are the higher-value signal — host-allowlist failures are usually misconfiguration, while auth failures are the canonical "someone is probing" event.
- **Suggested fix:** Inside the `AuthOutcome::Unauthorized` arm at `src/serve/auth.rs:224`, before returning the 401 response, emit:
  ```rust
  tracing::warn!(
      method = %req.method(),
      path = %req.uri().path(),
      "serve: rejected unauthenticated request",
  );
  ```
  Do NOT log token candidates — even truncated.

#### OB-V1.30-3: Per-request span (TraceLayer) and `build_*` spans are disconnected because `tokio::task::spawn_blocking` doesn't propagate span context
- **Difficulty:** medium
- **Location:** `src/serve/handlers.rs:86, 111, 131, 160, 210, 236` (every `spawn_blocking` call)
- **Description:** The serve router wires `TraceLayer::new_for_http()` (`src/serve/mod.rs:195`) which generates a per-request span (fixes OB-V1.29-5 at the request level). Each handler then spawns its blocking work via `tokio::task::spawn_blocking(move || super::data::build_graph(…))`. The closure runs on a fresh blocking-pool thread which has NO span stack inherited from the caller — `tokio` does not propagate `tracing::Span::current()` into spawn_blocking by default. Inside `build_graph`/`build_chunk_detail`/`build_hierarchy`/`build_cluster`/`build_stats`, the `info_span!` becomes a fresh root span. A JSON capture of one request shows two unrelated trees: TraceLayer's `http_request{method=GET path=/api/graph}` and a separately-rooted `build_graph{file_filter=…}`. Once OB-V1.30-1 is fixed and INFO spans render, the noise will be doubled with no parent linkage.
- **Suggested fix:** Capture `tracing::Span::current()` before the spawn and `instrument` the closure:
  ```rust
  use tracing::Instrument;
  let span = tracing::Span::current();
  let stats = tokio::task::spawn_blocking({
      let store = state.store.clone();
      move || {
          let _entered = span.enter();
          super::data::build_stats(&store)
      }
  }).await…
  ```
  Apply at all six handlers (stats/graph/chunk_detail/search/hierarchy/cluster_2d). Alternative: drop the per-handler `tracing::info!` lines (80, 100, 126, 149, 175, 201, 231) and rely solely on the inner `build_*` span entry — they're redundant once the inner span emits CLOSE events.

#### OB-V1.30-4: `cqs eval` runner uses `eprintln!` for progress instead of `tracing::info!`
- **Difficulty:** easy
- **Location:** `src/cli/commands/eval/runner.rs:163-168`
- **Description:** The eval runner emits progress (`[eval] 50/109 queries (12.3 q/s)`) via `eprintln!` at line 167. Every other progress signal in the codebase (`build.rs` SPLADE batches at 546, UMAP at `umap.rs:142`, daemon GC at `watch.rs:1217`) uses `tracing::info!` with structured fields. Using `eprintln!`: (a) prevents JSON-redirect of eval output for downstream comparison tooling, (b) fires unconditionally even with `RUST_LOG=error` (no quiet mode), (c) the q/s number can't be filtered or summed from journal logs.
- **Suggested fix:** Replace line 167 with:
  ```rust
  tracing::info!(done, total = total_queries, qps, "eval progress");
  ```
  Keep `eprintln!` only behind a `--quiet=false` CLI gate if interactive feedback is required, or rely on the `cqs=info` default after OB-V1.30-1.

#### OB-V1.30-5: `nl/mod.rs` public NL generators have zero spans — generated text shapes the embedding for every chunk
- **Difficulty:** easy
- **Location:** `src/nl/mod.rs:43, 65, 189, 209` (`generate_nl_with_call_context`, `generate_nl_with_call_context_and_summary`, `generate_nl_description`, `generate_nl_with_template`)
- **Description:** None of the four `generate_nl_*` public functions have entry spans, despite being the canonical text-shaping path that determines what every chunk's embedding sees. When eval drops 5pp recall after a model swap, there is no way to bisect from "which NL template rendered which chunk" — an operator has to add spans by hand and rebuild. (The fts/markdown helpers running in tight indexing loops are reasonable to skip; the 4 NL generators are NOT inner-loop, they run once per chunk during enrichment.)
- **Suggested fix:** Add a single `tracing::debug_span!("generate_nl", template = ?template, chunk_kind = ?chunk.chunk_type, len = chunk.content.len())` at the top of `generate_nl_with_template` (line 209). The other three call into it transitively, so one span covers all four entry points. `debug_span!` (not info) keeps it off by default.

#### OB-V1.30-6: `embed_documents` / `embed_query` lack completion fields — entry span has `count`/`text` but no result.len, dim, time
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:683, 722`
- **Description:** Both entry spans are minimal: `info_span!("embed_documents", count = texts.len())` and `info_span!("embed_query")`. There is no "embed_documents complete" event with the produced batch size, embedding dim, or tokenization stats. `FmtSpan::CLOSE` (OB-V1.30-1) would partially fix this, but even with CLOSE events the only structured field would be the entry-time `count`, not output sizes / dim / tokenization stats. Indexing 100k chunks today produces ~200 `embed_batch` enter events and zero "I produced N embeddings of dim D in T ms" events.
- **Suggested fix:** At the bottom of `embed_documents` (after the loop completes), add:
  ```rust
  tracing::info!(
      total = embeddings.len(),
      dim = self.embedding_dim(),
      input_count = texts.len(),
      "embed_documents complete"
  );
  ```
  Same pattern in `embed_query` at `tracing::debug!` level.

#### OB-V1.30-7: `Reranker::rerank_with_passages` swallows `passages.len() != results.len()` mismatch with no log — silent semantic corruption
- **Difficulty:** easy
- **Location:** `src/reranker.rs:200-220`
- **Description:** `rerank_with_passages` accepts `passages: &[&str]` independent of `results: &mut Vec<SearchResult>`. The doc says "passages must have the same length as results" but the implementation does not assert, log, or take a tracing event when the lengths diverge. If a caller mis-zips (e.g. filters results post-fetch but forgets to re-trim passages), `compute_scores` either panics on out-of-bounds slicing or scores arbitrarily-paired text, and the operator sees nothing in the journal. Semantically silent: ranks shift, neither error nor warning fires.
- **Suggested fix:** At line 213 (after the entry span, before the early-return), add:
  ```rust
  if passages.len() != results.len() {
      tracing::warn!(
          passages = passages.len(),
          results = results.len(),
          "rerank_with_passages: length mismatch — caller bug, results will be corrupted",
      );
      return Err(RerankerError::InvalidArguments(format!(
          "passages.len()={} != results.len()={}",
          passages.len(),
          results.len()
      )));
  }
  ```
  Add the `InvalidArguments` variant if it doesn't exist; warn-only also acceptable but less safe.

#### OB-V1.30-8: `train_data` git subprocess wrappers don't log non-zero exit codes — silent failure on shallow clones / missing SHAs
- **Difficulty:** easy
- **Location:** `src/train_data/git.rs:65-242` (`git_log`, `git_diff_tree`, `git_show`)
- **Description:** Each function has an entry `tracing::info_span!` (good), but on `output.status.success()` failure the exit code and stderr are bundled into a `TrainDataError` and returned — no structured log. When `cqs train-data --max-commits 1000` walks a shallow repo and 50% of `git_diff_tree` calls fail with `fatal: bad revision`, the user sees a single aggregated count at the end and has no way to reconstruct WHICH SHAs failed without re-running with `RUST_LOG=trace`. The `is_shallow` probe at line 241 is the only one that handles a missing-SHA case gracefully.
- **Suggested fix:** In each subprocess wrapper, on `!output.status.success()` add before the error return:
  ```rust
  tracing::warn!(
      sha,
      exit = output.status.code(),
      stderr = %String::from_utf8_lossy(&output.stderr).trim(),
      "git_diff_tree failed",
  );
  ```
  Apply consistently to `git_log` (line 65), `git_diff_tree` (line 131), `git_show` (line 173).

#### OB-V1.30-9: Format-string-interpolated `tracing::info!` calls — structural fields are lost
- **Difficulty:** easy
- **Location:** `src/hnsw/build.rs:78, 236`; `src/hnsw/persist.rs:210, 638, 771`; `src/reference.rs:220`; `src/cli/commands/train/export_model.rs:76`; `src/audit.rs:85, 93`; `src/embedder/provider.rs:149`
- **Description:** OB-V1.29-4 specifically called out `verify_hnsw_checksums` for this pattern; the audit fixed that one site but the broader pattern persists across HNSW build/persist and several other modules. Lines like `tracing::info!("Building HNSW index with {} vectors", nb_elem)` produce a single rendered string instead of structured `count = nb_elem` fields. Once OB-V1.30-1 lands JSON formatting, these lines remain un-queryable: jq-extracting the vector count from "Building HNSW index with 178432 vectors" needs a regex per line, while `count: 178432` is a JSON field. Friction with no offsetting benefit — `tracing` natively supports both forms.
- **Suggested fix:** Convert each site to structured form. Examples:
  ```rust
  // src/hnsw/build.rs:78
  tracing::info!(count = nb_elem, "Building HNSW index");
  // src/hnsw/persist.rs:210
  tracing::info!(dir = %dir.display(), basename, "Saving HNSW index");
  ```
  Sweep the 9 sites in one pass. Pure-mechanical change, no behavior delta.

#### OB-V1.30-10: `cqs serve` cluster_2d emits no warn when corpus has chunks but zero UMAP rows — operators see only the empty payload
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:901, 1020` (`build_cluster`); handler at `src/serve/handlers.rs:227-242`
- **Description:** When the user runs `cqs serve` against a corpus indexed without `cqs index --umap` (the v1.30.0 schema-v22 default — UMAP is opt-in), the cluster-3d view returns `{nodes: [], skipped: N}` with N = total chunks. The frontend renders a "run cqs index --umap" hint (per the doc comment at handlers.rs:226), but the backend emits no `tracing::warn!` to surface this state in the journal — the operator who runs `cqs serve` over SSH and gets a blank cluster view has no log to point at. Neighboring `build_hierarchy` at `data.rs:638` DOES log `tracing::info!(root_id, "build_hierarchy: root chunk not found")` for its empty-result case.
- **Suggested fix:** At the point inside `build_cluster` where `coords` is empty but `total_chunks > 0`, add:
  ```rust
  if response.nodes.is_empty() && response.skipped > 0 {
      tracing::warn!(
          skipped = response.skipped,
          "build_cluster: corpus has chunks but no UMAP coordinates — run `cqs index --umap`",
      );
  }
  ```

---

## Triage notes

- **OB-V1.30-1** is the highest-leverage finding — fixing it unlocks the value of every span already in the codebase. P1 by impact, easy by effort.
- **OB-V1.30-2** is the only one tied directly to the new auth surface (#1118); SEC-7 shipped without the warn-on-reject side, so the security event is silent.
- **OB-V1.30-3** is the only "medium" — it requires understanding tokio's blocking-pool span propagation. The other nine are all easy.
- **OB-V1.30-9** is bundled because the prior audit (OB-V1.29-4) only patched one site of an idiomatic-but-stale pattern that persists across ~9 lines.
- I did NOT report module-wide gaps in `scout`, `where_to_add`, `gather`, `staleness`, `impact`, or `slot`/`cache`/`serve` — every public function in those modules now has an entry span, confirming the v0.12.1 lesson has been applied. The current observability bar in cqs is high; the remaining gaps are at the rendering / correlation / completeness edges, not the "no spans exist" tier.


## Test Coverage (adversarial)

Audit scope: v1.30.0. Skipped findings already triaged in `docs/audit-triage-v1.30.0.md` (the v1.29.0 carryover triage). Focus is on surfaces *added or substantially changed* in v1.29.1..v1.30.0 — slots/cache (#1105), local LLM provider (#1101), per-launch serve auth (#1118), non-blocking HNSW rebuild (#1113), `nomic-coderank` preset (#1110), execution-provider feature split (#1120) — plus pre-existing untested adversarial paths.

#### TC-ADV-1.30-1: `LocalProvider` body-size DoS — `body_preview` and `parse_choices_content` both buffer entire HTTP response before truncating
- **Difficulty:** medium
- **Location:** `src/llm/local.rs:474-488` (`parse_choices_content`), `src/llm/local.rs:490-500` (`body_preview`). Tests in `src/llm/local.rs:595+`.
- **Description:** `parse_choices_content` calls `resp.json::<Value>()` which reads the entire response body into memory before parsing. `body_preview` calls `resp.text()` which does the same, then char-truncates to 256 bytes. The `reqwest::blocking::Client` constructed at `src/llm/local.rs:97-100` sets `timeout` and `redirect` only — there is **no body-size limit**. A misconfigured / hostile / panicked OpenAI-compat server (or any server reachable at the configured `api_base`) can return an arbitrarily large 200-OK or 4xx body and OOM the daemon's blocking-pool thread. The local LLM concurrency knob clamps at 64, so up to 64 concurrent unbounded reads can race for memory before any one completes. Existing test `long_response_not_truncated` at `:826-847` only exercises the 100 KB happy path and *requires* the full body to come through — locking in the unbounded read as a contract.
- **Suggested fix:** Add to the `tests` module in `src/llm/local.rs`:
  - `test_oversized_response_body_capped_at_5mb` — mock a 200-OK with a 50 MB JSON body; assert the worker either errors out (preferred) or reads at most a documented cap (e.g. 5 MB). Implementation: wrap the response with `Response::bytes_stream` / `take` or set `reqwest::Client::builder().…response_body_limit(…)` (helper exists via `read_to_end_with_limit`).
  - `test_4xx_with_large_body_does_not_buffer_entire_body` — mock a 400 with a 50 MB body; assert `body_preview` returns ≤ 256 bytes *and* never allocates more than e.g. 4 KB by checking with a tracking allocator or measuring `resp.bytes_stream().next().await`.
  - Recommended alongside: cap response body via `reqwest::Body::limit` or `Response::bytes_stream` chunking. Without a fix, an A6000 box's 64-thread blocking pool × N-MB body = trivial OOM crater.

#### TC-ADV-1.30-2: `EmbeddingCache::write_batch` and `QueryCache::put` accept NaN/Inf embeddings — cross-process cache poisoning
- **Difficulty:** easy
- **Location:** `src/cache.rs:332-407` (`EmbeddingCache::write_batch`), `src/cache.rs:1677-1699` (`QueryCache::put`). Tests in the `tests` mod (`src/cache.rs:756+`) and `shared_runtime_tests` (`:1748+`).
- **Description:** `write_batch` validates `embedding.len() == dim` and `!embedding.is_empty()` (lines 360-373) but never checks `embedding.iter().all(|f| f.is_finite())`. Same hole in `QueryCache::put` (line 1677): bytes are flat-mapped from `embedding.as_slice()` and inserted with no finiteness check. Once a poisoned NaN/Inf entry lands on disk, *every subsequent process* that reuses the project's `embeddings_cache.db` (now project-scoped per #1105 — so cross-slot too) reads it back via `read_batch` (line 282-296) which also does not gate on finiteness, and the corrupt floats flow into HNSW search (which has a `is_finite` guard at `src/hnsw/search.rs:82`) and the brute-force / reranker scoring paths (which mostly do not). #1105 made this *worse* by making the cache long-lived per-project (was per-build before): a single bad embedder run permanently poisons the cache until the user runs `cqs cache prune`.
  - Triage note: TC-ADV-1.29-1 / TC-ADV-1.29-2 (P3) called this out at the embedder boundary; this is the *cache* boundary, which is independent and currently the only line of defense if a future embedder swap (Phase B/C of #956 — CoreML/ROCm) happens to ship NaN-on-overflow behavior different from CUDA today.
- **Suggested fix:** In `src/cache.rs::tests`:
  - `test_write_batch_rejects_nan_embedding` — write `vec![1.0, f32::NAN, 0.5, …]`; assert it is either dropped with a `tracing::warn!` or returned as a 0-count, and that a subsequent `read_batch` returns no row for that hash.
  - `test_write_batch_rejects_inf_embedding` — same for `f32::INFINITY` / `f32::NEG_INFINITY`.
  - `test_query_cache_put_rejects_non_finite` — sibling test in `query_cache_tests` (or a new mod). Build an `Embedding` via `Embedding::new` (the unchecked path; `try_new` already rejects), `put` it, then `get` it and assert the embedding either does not appear or comes back as a documented sentinel.
  - Implementation should sit alongside the existing `embedding.len() != dim` warn/skip blocks: one extra `if embedding.iter().any(|f| !f.is_finite())` early-skip with a `tracing::warn!` carrying the hash prefix.

#### TC-ADV-1.30-3: `slot_create` / `slot_remove` TOCTOU — concurrent slot operations corrupt `.cqs/slots/<name>/`
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/slot.rs:219-266` (`slot_create`), `:299-350` (`slot_remove`). Tests at `:391-516` cover sequential happy paths only.
- **Description:** Two scenarios with no test today:
  1. **Concurrent `cqs slot create foo`**: line 224 checks `dir.exists()`, line 233 calls `fs::create_dir_all(&dir)` (idempotent — does not fail). Both processes proceed past the existence check, both write `slot.toml` via `write_slot_model`. The second writer wins via the temp+rename atomicity inside `write_slot_model`, but both processes report success and the user has no signal that one was a no-op.
  2. **`cqs slot remove foo` racing `cqs index --slot foo`**: `slot_remove` calls `fs::remove_dir_all(&dir)` (line 335) without any lock. The indexer holds an open SQLite connection on `slot/foo/index.db`; on Linux the unlinking of in-use files is silent (Windows would error). The indexer continues writing to phantom inodes and, on next open, sees an empty slot. Worse: the auto-promotion at line 331 (`write_active_slot(…, &all[0])`) can flip the active pointer mid-index, so `cqs search` after the remove starts hitting an unrelated slot.
  3. **`cqs slot remove`** while daemon is serving from that slot: daemon's read-only `Store` keeps the file alive on Linux but the on-disk directory is gone — the daemon's HNSW save path (any `set_hnsw_dirty` write) silently writes to a deleted-but-open inode. After daemon restart the slot is gone.
- **Suggested fix:** Add to `src/cli/commands/infra/slot.rs::tests`:
  - `test_slot_create_concurrent_same_name` — spawn 2 threads racing `slot_create(…, "foo", Some("bge-large"), …)`, then `slot_create(…, "foo", Some("e5-base"), …)`; assert at most one returns `Ok` *or* the slot ends up with a deterministic model (whichever contract is chosen).
  - `test_slot_remove_during_open_index_db` — open `index.db` via `Store::open_readonly_pooled`, then call `slot_remove` from another thread; assert the open store either keeps working OR the remove returns an error (currently neither is guaranteed).
  - Recommend adding `flock` on `<slot>/index.db.lock` (or a new `slot.lock` in `.cqs/slots/<name>/`) acquired by both `slot_remove` and the indexer, so the second to-arrive blocks or errors instead of corrupting.

#### TC-ADV-1.30-4: Non-blocking HNSW rebuild — no test for thread panic, dim drift, or store-open failure
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:965-1042` (`spawn_hnsw_rebuild`), `:1058+` (`drain_pending_rebuild`). Existing tests at `:3979-4115` cover `delta` replay, dedup, error-clears-pending, in-flight stays-pending.
- **Description:** The rebuild thread runs `cqs::Store::open_readonly_pooled`, then `build_hnsw_index_owned`, then `build_hnsw_base_index`. Several adversarial paths are not exercised:
  - **Thread panic mid-build**: the closure at `:980-1030` is *not* wrapped in `std::panic::catch_unwind`. A panic inside `build_hnsw_index_owned` (e.g., from `unwrap` on a corrupt vector during `id_map.clone()`) unwinds the thread, which means the `let _ = tx.send(result)` at line 1029 *never runs* — the receive side hits `TryRecvError::Disconnected` on the next `drain_pending_rebuild` poll. That path *does* clear pending (line 1066-1068), so the daemon recovers. But the *delta* (chunks captured during the rebuild window) is **silently dropped** — those chunks are never inserted into any index until the next full rebuild trigger. No test covers this drop.
  - **Store dim drift**: line 985-991 explicitly checks `store.dim() != expected_dim` and bails. No test exercises this — the test fixture at `:3979` builds `state` directly, not via `spawn_hnsw_rebuild`, so the dim-drift bail path has zero coverage. After #1105 (named slots), this is *the* defense against `cqs slot promote` flipping the active slot to a different-dim model mid-rebuild.
  - **Store open failure**: line 984 `cqs::Store::open_readonly_pooled` can fail with `SQLITE_BUSY` (concurrent migration), `SQLITE_CANTOPEN` (slot dir gone — see TC-ADV-1.30-3), or schema mismatch. All collapse to `RebuildOutcome::Err`, which `drain_pending_rebuild` clears (line 1100+ in the `Err(_)` arm). Untested.
  - **Spawn-failure path** (line 1031-1036): a `Builder::spawn` failure (resource exhaustion) returns a `PendingRebuild` whose `rx` is a *fresh, unsent channel* — `try_recv` returns `Empty` forever, NOT `Disconnected`. The pending state is leaked: the watch loop thinks a rebuild is in flight indefinitely and never spawns a follow-up. **This is a real bug, not just a coverage gap** — comment says "channel will hang up on first poll" but `tx` is dropped at end of `spawn_hnsw_rebuild`'s scope only because the closure was never moved into a thread. Actually re-reading: `tx` was moved into the closure that failed to spawn, so it's dropped, and `rx.try_recv()` would return `Disconnected`. OK so the leak is closed — but the test that pins this should exist.
- **Suggested fix:** Add to `src/cli/watch.rs::tests` (alongside `drain_pending_rebuild_*`):
  - `test_spawn_hnsw_rebuild_dim_mismatch_clears_pending` — set up a `Store` with `dim=768`, call `spawn_hnsw_rebuild(…, expected_dim=1024, …)`, then `drain_pending_rebuild`; assert pending is cleared and the watch state has no dangling channel.
  - `test_spawn_hnsw_rebuild_thread_panic_drops_delta_loudly` — wrap the rebuild closure or use a temporary feature flag to inject a panic; assert the delta is *not* silently dropped (current behavior). The fix is to wrap the closure in `catch_unwind` and replay the delta into `state.hnsw_index` on panic.
  - `test_spawn_hnsw_rebuild_store_open_fails_clears_pending` — point at a non-existent index path; assert `drain_pending_rebuild` clears pending after the receiver sees the error.
  - `test_spawn_hnsw_rebuild_failure_to_spawn_disconnect_path` — synthesize spawn failure (rlimit on threads, or stub `Builder::spawn` via injection); assert a follow-up `drain_pending_rebuild` clears via `Disconnected`.

#### TC-ADV-1.30-5: `serve` auth — `strip_token_param` does not handle case variants, percent-encoded `token`, or leading `?token=` after a `?`-less path
- **Difficulty:** easy
- **Location:** `src/serve/auth.rs:101-115` (`strip_token_param`). Existing tests at `:269-291`.
- **Description:** Three cases not pinned:
  - **Case mismatch**: `?Token=abc` (capital T) — `pair.starts_with("token=")` is false, kept in the redirect URL. Browsers preserve case; some CLIs lowercase. The token won't match `ct_eq` either (token is URL-safe base64, mixed case-sensitive), so this never authenticates — but the leftover `?Token=…` in the redirect URL leaks the token into the address bar, defeating the SEC-7 design goal.
  - **Percent-encoded `token`**: `?%74oken=abc` (`%74` = `t`) — same issue, the percent-encoded prefix fails the literal `starts_with` check and the redirect carries the (still-bad) param through. RFC 3986 says `%74` and `t` are equivalent at the URI level; clients may normalize at random.
  - **Empty value `?token=`**: not tested. `pair.strip_prefix("token=")` returns `Some("")`; `ct_eq("", expected)` is false, so it falls through to `Unauthorized`. But no test pins this — a future refactor could accept empty tokens.
  - **Trailing `&` / double `&`**: `?token=abc&&depth=3` — `query.split('&')` produces an empty string between the `&&`. The empty pair fails `starts_with("token=")` and survives into the rejoined query. The redirect URL has `?depth=3` (since join uses `&`), but if the *only* other param were such an empty pair the redirect emits a stray empty query slot. Cosmetic but untested.
  - The deeper concern: `check_request` at `:158-172` only matches *literal* `token=…` in the query string, by the same `starts_with` check. So a percent-encoded `token` query param is silently treated as no token — a user pasting a URL into a browser that just happens to percent-encode `t` (Safari does this in some flows for non-ASCII surrounding context) gets a 401 for what should be a valid token. Untested.
- **Suggested fix:** In `src/serve/auth.rs::tests` (extend the existing `strip_token_param_*` set):
  - `test_strip_token_param_case_insensitive` — pin behavior on `?Token=…` (ideally: also stripped, since the auth check should be case-insensitive on param name to match HTTP convention).
  - `test_check_request_rejects_percent_encoded_token_key` — `%74oken=…` — assert 401 (current behavior) or fix to decode.
  - `test_strip_token_param_handles_double_ampersand` — `?token=abc&&depth=3` — pin redirect output.
  - Recommend a one-line fix: percent-decode the *key* using `percent_encoding::percent_decode_str` (already in the dep tree) before the prefix check, and lowercase the key for comparison. Token *value* stays exact-match for `ct_eq`.

#### TC-ADV-1.30-6: `validate_slot_name` accepts names that the OS or shell will misinterpret
- **Difficulty:** easy
- **Location:** `src/slot/mod.rs:159-178`, `src/slot/mod.rs:661+` (existing tests).
- **Description:** Pure `[a-z0-9_-]+` with max 32 chars seems safe, but two concrete cases bite:
  - **Names starting with `-`**: `validate_slot_name("-foo")` returns `Ok(())`. When passed to `cqs index --slot -foo`, clap's positional/long-flag parser will treat `-foo` as a flag, not a value to `--slot`. The user gets a confusing clap error far from the slot module. Untested.
  - **Names ending with `-`**: `"foo-"` passes. Cosmetically fine on Linux/macOS but `cqs slot remove foo-` — `cqs slot remove` accepts the name OK, but `fs::remove_dir_all(slot_dir(…, "foo-"))` then operates on `.cqs/slots/foo-/`. On Windows, trailing dashes in directory names are legal, no issue. The real problem: shell completion / `gh` URL passes / Slack mentions silently strip trailing dashes from many copy-paste paths.
  - **Names that are integer-shaped**: `"42"` passes. Then `cqs slot create 42 --model bge-large` is fine, but a future consumer that does `--slot $(cat foo)` where `foo` contains numeric data is fine — yet some shell pipelines pass numeric strings through to flag parsers as positional args by accident. Documenting that integer-shaped names are valid is enough; no current bug, but pinning the contract is cheap.
  - **Pure-underscore names**: `"_"`, `"__"`, `"_____"` all pass. `cqs slot create _` is legal. UI prints `*  _   chunks=0 …`. Cosmetic, untested. Pinning behavior with a test means future column-alignment / log-parsing changes don't silently break.
- **Suggested fix:** Add to `src/slot/mod.rs::tests`:
  - `test_validate_rejects_leading_dash` — `validate_slot_name("-foo")` should error (because of clap collision), or document and pin if intentionally accepted. Recommend rejecting: a name starting with `-` cannot be passed as a `--slot` argument value without `--slot=-foo`, which most users won't know.
  - `test_validate_rejects_trailing_dash` — pin behavior; recommend rejecting for consistency.
  - `test_validate_accepts_pure_underscore_or_underscore_only` — pin current behavior so a future "must contain alphanumeric" tightening surfaces as a test break, not silent UX change.

#### TC-ADV-1.30-7: `slot::migrate_legacy_index_to_default_slot` rollback path is untested
- **Difficulty:** medium
- **Location:** `src/slot/mod.rs:511-593` (migration), specifically the rollback loop at `:561-582`. No test for partial-failure rollback in `src/slot/mod.rs::tests` (`:850+` covers happy path + idempotency only).
- **Description:** The migration moves N files (typically `index.db` + `index.db-wal` + `index.db-shm` + 4 HNSW files + SPLADE = 8 candidates) from `.cqs/` to `.cqs/slots/default/`. If `move_file` fails on file K (cross-device, permission denied, EBUSY on Windows because `index.db-wal` is still open by a concurrent reader), the rollback loop at line 562-571 reverses the moves of files 1..K-1. Failure modes:
  - **Rollback itself fails**: line 564-569 logs but does not abort. After a failed rollback, files 1..K-1 are in `slots/default/`, file K is in the original `.cqs/` directory, and the active_slot pointer is **never written** (line 585 only runs on success). The next migration call hits the `slots_dir.exists()` check at line 523 and returns `Ok(false)` — *the project is now permanently broken* with files split across two locations. Untested.
  - **`fs::remove_dir(&dest)` at line 573 fails** because `slots/default/` contains rolled-back files that haven't been moved out. The cleanup is skipped silently (`let _ =`) so the next run sees `slots/` exist and does nothing. Same broken-project state. Untested.
  - **EBUSY on Windows for `index.db-wal`** specifically: this is the realistic trigger. A daemon running the watch loop holds the WAL open. A user runs `cqs index` from another shell, which does `Store::open` → triggers migration. WAL is locked. Migration step 2 (`-wal`) fails. Step 1 (`index.db`) succeeded — already moved into `slots/default/`. Rollback moves it back. Cleanup tries to `remove_dir(&dest)`. Now there's only one outcome: the project is fine. *Unless* the rollback `move_file(slots/default/index.db, .cqs/index.db)` itself fails (e.g., something raced to create a file at the target path) — then we have split state, no rollback, no test.
- **Suggested fix:** Add to `src/slot/mod.rs::tests`:
  - `test_migrate_rollback_on_second_file_failure` — plant `index.db` and `index.db-wal`, make `index.db-wal` fail to move (e.g., open it with an exclusive handle on Linux via `flock`, or remove read perms on the parent dir mid-test). Assert the rollback restores `index.db` to `.cqs/`, `slots/` is fully cleaned up, and the next migration call still works.
  - `test_migrate_rollback_failure_leaves_loud_signal` — make rollback itself fail (chmod the source dir read-only after the first move). Assert the migration returns `Err(SlotError::Migration(…))` AND the user-visible state contains a *single* known signal (not silent split state). Recommend writing a `.cqs/migration_failed` marker file the daemon checks at startup, or refuse to start when `slots/` and `.cqs/index.db` both exist.

#### TC-ADV-1.30-8: `LocalProvider` accepts non-HTTP `api_base` URLs and unbalanced `concurrency` configurations
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:88-121` (`LocalProvider::new`), `:128-312` (`submit_via_chat_completions`), `:153` (channel sized at `concurrency.max(8) * 2`).
- **Description:** Three under-tested edge cases on the new (#1101) provider:
  - **Non-HTTP scheme**: `api_base = "file:///etc/passwd"` or `"gopher://example/"`. `Client::post(&url)` accepts the URL; `reqwest` errors at request time with a generic "URL scheme is not allowed" but the error is downgraded to `LlmError::Http` via `?` and retried 4× (each retry sleeping 500ms→4s). 7.5s wasted per item × N items, no specific signal that the user typo'd `file://` for `http://`. Untested.
  - **`api_base` with trailing slash**: `"http://x/v1/"` → `format!("{}/chat/completions", self.api_base)` → `"http://x/v1//chat/completions"`. Most servers handle the doubled slash; some (strict nginx configs, some llama.cpp builds) 404. The user's mock at `make_config(&format!("{}/v1", server.base_url()), …)` always uses no trailing slash. No regression test.
  - **`api_base` without `/v1`**: `"http://x"` → `"http://x/chat/completions"`. Some servers expect it (vLLM defaults to `/v1`); others reject. The doc at line 80-82 says "any server that speaks `/v1/chat/completions`" but the code does not enforce or normalize. No test pins the contract.
  - **`concurrency=64, items=1`**: line 153 sizes the bounded channel at `concurrency.max(8) * 2 = 128` for a single item. 64 worker threads spin up, 63 immediately exit on closed-channel after the single item is consumed. Wasteful but harmless — until you note it does this for *every* `submit_*` call inside the same daemon, and worker-thread allocation under heavy churn is non-trivial on glibc. No test verifies that small batches don't spawn full-concurrency thread pools.
- **Suggested fix:** Add to `src/llm/local.rs::tests`:
  - `test_non_http_api_base_fails_fast` — `make_config("file:///tmp/foo", …)`, `submit_batch_prebuilt(&items, …)`. Assert error returns within ~100ms (no 7.5s × N retry stall). Recommend adding a one-line URL scheme check in `LocalProvider::new`: bail if `Url::parse(&api_base).scheme() != "http" && != "https"`.
  - `test_api_base_with_trailing_slash_works` — pin behavior so the doubled-slash case either succeeds (current httpmock probably accepts) or normalizes.
  - `test_concurrency_clamped_to_item_count_when_smaller` — recommend `let workers = self.concurrency.min(items.len()).max(1);` at line 166. Test that for `items.len()=1`, only 1 worker thread is spawned (verify via thread name enumeration or a counter in a custom worker-spawn hook).

#### TC-ADV-1.30-9: Embedder `provider::ort_runtime_search_dir` and `find_ld_library_dir` — no test for malformed `/proc/self/cmdline` or pathological `LD_LIBRARY_PATH`
- **Difficulty:** easy
- **Location:** `src/embedder/provider.rs:67-83` (`ort_runtime_search_dir`), `:115-123` (`find_ld_library_dir`), `:34-62` (`ensure_ort_provider_libs`). No `#[test]` block in this file at all — provider detection paths are exercised only end-to-end via `Embedder::new`.
- **Description:** This file moved out of `embedder/mod.rs` in #1120 (Phase A) and gained the explicit symlink-and-search logic. It now reads `/proc/self/cmdline`, parses up to the first NUL, decodes UTF-8, and joins against CWD. Untested adversarial inputs:
  - **`/proc/self/cmdline` empty / single NUL**: `cmdline.iter().position(|&b| b == 0)` returns `None` (if no NUL) or `Some(0)` (if first byte is NUL). For `Some(0)`: `argv0 = ""`, `argv0.starts_with('/')` is false, falls through to `current_dir().ok()?.join("")` → `current_dir`. Then `abs_path.parent()` returns `Some(current_dir.parent())`. The symlink directory becomes the *parent* of CWD. If CWD is `/home/user/proj/.cqs`, ORT search dir becomes `/home/user/proj/`. Probably benign (no provider .so files there). But if CWD is `/`, parent is `None` and `ensure_ort_provider_libs` silently does nothing — provider activation fails on the first call, caller falls back to CPU with no diagnostic. Untested.
  - **Non-UTF8 argv[0]**: `std::str::from_utf8(&cmdline[..argv0_end]).ok()?` returns `None`. Function returns `None`. Same silent-CPU-fallback. Untested. While Linux argv[0] *should* be UTF-8, container runtimes occasionally inject non-UTF-8 bytes via `exec` for sandboxing / launchers.
  - **`LD_LIBRARY_PATH` with empty entries**: `"::/foo:"` — `ld_path.split(':')` produces `["", "", "/foo", ""]`. Filter `!p.is_empty()` rejects all empties. Then `Path::new("/foo").is_dir()` decides. Untested for the `LD_LIBRARY_PATH=":"` corner case.
  - **`LD_LIBRARY_PATH` containing the ORT cache itself** (`:ort_lib_dir`): the filter `!ort_cache_str.starts_with(p)` excludes *prefixes* of the ORT cache. If `ort_lib_dir = /home/u/.cache/ort.pyke.io/dfbin/x86_64-…/v1.x` and `LD_LIBRARY_PATH = /home/u/.cache`, the filter drops `/home/u/.cache` because `ort_cache_str` does start with it. Probably correct intent. But `LD_LIBRARY_PATH = /home/u/.cache/ort.pyke.io/dfbin/x86_64-unknown-linux-gnu/v0.x` (sibling cache version) is *not* a prefix of the active ORT lib dir, so it passes the filter and gets symlinks pointed *into* a stale ORT version's cache. Symlink overwrite of the user's other ORT install. Untested.
- **Suggested fix:** Create `src/embedder/provider.rs::tests` (currently empty):
  - `test_ort_runtime_search_dir_handles_empty_cmdline` — write a file at a temp path that begins with NUL; inject via test-only env var override or test-only feature flag. Assert returns `None` or a documented sentinel.
  - `test_find_ld_library_dir_skips_empty_entries` — `LD_LIBRARY_PATH=":/tmp:"`, assert it picks `/tmp` and not `""`.
  - `test_find_ld_library_dir_does_not_pick_sibling_ort_version_dir` — set up two cache dirs `…/v0.x` and `…/v1.x`, point `LD_LIBRARY_PATH` at `…/v0.x`, assert the symlink target is *not* `…/v0.x` (since that would corrupt the older cache). Recommend changing the prefix-check to a path-containment check via `Path::ancestors()`.

#### TC-ADV-1.30-10: `cache::EmbeddingCache::insert_many` — `blake3_hex_or_passthrough` accepts non-hex content_hash bytes that look like hex
- **Difficulty:** easy
- **Location:** `src/cache.rs:709-721` (`blake3_hex_or_passthrough`), `:674-703` (`insert_many`). No test for the passthrough/encode branch boundaries.
- **Description:** The function checks: if the bytes are valid UTF-8, length 64, and all ASCII hex digits → pass through; otherwise hex-encode each byte. Edge cases not tested:
  - **64-byte UTF-8 with NUL bytes that happen to be in ASCII hex range**: NUL (0x00) is not in `0..='9' | 'a'..='f' | 'A'..='F'`, so the all-hex check fails and we fall through to encode. Good. But not tested.
  - **64-byte UTF-8 that is all hex but uppercase**: `"ABCDEF…"` — `b.is_ascii_hexdigit()` accepts uppercase. So the cache stores uppercase hex for some entries and lowercase (the encoded path) for others. **Two writes for the same content_hash with different case produce two different cache rows.** PRIMARY KEY enforces uniqueness at the row level, but `(content_hash, model_fingerprint)` rows differ by case — so a chunk hashed once via passthrough (uppercase) and once via encode (lowercase) duplicates and both linger. Untested.
  - **Non-64-byte hex strings**: `"abc"` (3 bytes UTF-8, all hex). Length check fails (3 != 64). Falls into encode → `"616263"` is stored. Each call with the same input is deterministic, but a caller passing a 32-char hex string gets a 64-char hex of the *bytes of that hex string*, not its decoded value. Surprising. Untested.
  - The deeper issue: this function exists because `Chunk::content_hash` is *already* a hex string (lowercase, 64 chars), and the cache schema declares the column as `TEXT`. The passthrough is a fast path. Anything that doesn't fit the fast-path contract collides into a parallel hex encoding. Two different inputs can produce the same output: `b"\xab\xcd…"` (64 bytes) → hex-encode → 128 ascii chars; vs the literal string `"abcd…"` (64 chars, valid hex) → passthrough → 64 chars. These don't collide at length 64 vs 128, but they do at any matching length. *Specifically*: passing a 64-byte raw input where every byte happens to be `0x30` (ASCII '0') — that's UTF-8, hex-only, 64 chars long → passthrough as `"00…0"`. Vs another caller passing the literal 32-byte hex string `"00000000000000000000000000000000"` (32 chars, hex) → fails length check → encode → `"3030…30"` (64 chars). Different outputs. OK no collision. But an attacker controlling content_hash could craft inputs that hit the passthrough boundary intentionally; a more conservative fix is to *always* hex-encode and never trust passthrough.
- **Suggested fix:** Add to `src/cache.rs::tests`:
  - `test_blake3_hex_or_passthrough_uppercase_hex_passthrough` — pin: uppercase 64-char hex passes through unchanged. If we want lowercase invariant, normalize.
  - `test_blake3_hex_or_passthrough_short_hex_string_gets_encoded` — pin: 32-char hex string is encoded, not passed through (the surprising case).
  - `test_insert_many_does_not_dup_when_caller_alternates_passthrough_and_encode` — write the same logical chunk twice, once with hex string bytes and once with raw blake3 bytes; assert exactly one row in the DB after both inserts. This currently fails. Recommend: always normalize via blake3 hash of input bytes if length is not exactly 64 lowercase hex; or kill the passthrough fast path entirely (cost: one allocation per write — negligible).

## Summary

10 findings. Highest-impact gaps cluster around v1.30.0's *new* surfaces:
1. **#1101 LocalProvider** — unbounded body reads (TC-ADV-1.30-1, TC-ADV-1.30-8) DoS the daemon on a single hostile or panicked LLM endpoint. Trivial reqwest config fix.
2. **#1105 Slots+Cache** — TOCTOU in concurrent slot ops (TC-ADV-1.30-3), untested rollback in legacy migration (TC-ADV-1.30-7), and *cross-slot* cache poisoning via NaN/Inf passthrough (TC-ADV-1.30-2) which #1105 made worse by extending the cache lifetime.
3. **#1113 Background HNSW rebuild** — silent delta loss on thread panic (TC-ADV-1.30-4) is a real bug masked by an absent `catch_unwind`, plus dim-drift bail untested under concurrent slot promote.
4. **#1118 Serve auth** (TC-ADV-1.30-5) — case-sensitivity and percent-encoding gaps in `strip_token_param` cause real SEC-7 leakage (token survives in URL bar after redirect).
5. **#1120 Provider feature split** (TC-ADV-1.30-9) — silently falls back to CPU on malformed `/proc/self/cmdline` or pathological LD_LIBRARY_PATH; symlink target picking can corrupt sibling ORT cache versions.

Lowest-priority but cheap to add: slot-name validation edges (TC-ADV-1.30-6), cache passthrough/encode duality (TC-ADV-1.30-10).


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


## Scaling & Hardcoded Limits

#### [SHL-V1.30-1]: CAGRA `itopk_size = (k * 2).clamp(itopk_min, itopk_max)` can produce `itopk_size < k` on small indexes — silent zero-result regression
- **Difficulty:** medium
- **Location:** `src/cagra.rs:359` (computation), `:166-170` (`cagra_itopk_max_default`)
- **Description:** cuVS CAGRA requires `itopk_size >= k` as a hard constraint — when violated, the call to `params.set_itopk_size(itopk_size)` followed by `Index::search` is the historical cause of the documented `topk=500 > itopk_size=480` failure mode (per `MEMORY.md`: "CAGRA fails at limit≥100 via `topk=500 > itopk_size=480` — keep eval at limit=20"). The current code computes `itopk_size = (k * 2).clamp(itopk_min, itopk_max)` where `itopk_max = (log2(n_vectors) * 32).clamp(128, 4096)`. For a small index (n_vectors = 1000 → itopk_max ≈ 320) and a user request of `k = 500` (e.g., `cqs search --limit 500`), the computation becomes `(1000).clamp(128, 320) = 320 < 500 = k`. The constraint is violated, no guard is present, and the eval-time workaround ("keep eval at limit=20") is the only thing protecting users from this. There is no `if itopk_size < k { fall back to HNSW }` branch, no error to the caller, and no clamp on `k` itself. Anyone wiring `cqs search --rerank --limit 200` (legitimate to give the reranker a 200-candidate pool) on a corpus that hasn't grown past ~13k chunks will silently produce undefined cuVS behavior.
- **Suggested fix:** Either (a) clamp `itopk_size = itopk_size.max(k)` after the existing clamp, then re-check that the result `<= itopk_max` and degrade to HNSW above that (cleanly returning a typed error from `search_impl`), or (b) clamp `k` itself to `itopk_max - 1` at search-entry and surface a `tracing::warn!` so the caller knows the limit was reduced. A `// CONSTRAINT: itopk_size >= k (cuVS hard requirement)` comment on the constant declaration would prevent regressions during refactors.

#### [SHL-V1.30-2]: `nl::generate_nl_with_template` char_budget defaults to 512 even when the active model has `max_seq_length=2048` — nomic-coderank silently truncates at 25% of capacity
- **Difficulty:** easy
- **Location:** `src/nl/mod.rs:222-229`
- **Description:** Section-chunk NL generation reads `CQS_MAX_SEQ_LENGTH` once into a `OnceLock` with default 512: `let max_seq = *MAX_SEQ.get_or_init(|| std::env::var("CQS_MAX_SEQ_LENGTH").ok().and_then(|v| v.parse().ok()).unwrap_or(512));`. The actual model's `max_seq_length` is encoded in `ModelConfig` (E5/v9-200k/BGE: 512; **nomic-coderank: 2048** per `embedder/models.rs:366`). Switching to nomic-coderank via preset/config without setting `CQS_MAX_SEQ_LENGTH` silently caps the per-section content preview at ~1800 chars instead of ~7800 chars — every section chunk gets truncated to 23% of what the model can accept. The comment at line 220-221 even says "Larger models (8192 → ~32000 chars) get more context", acknowledging the relationship but not wiring it through `ModelConfig`. This is the same "convenience wrapper hardcoded 768-dim while default model was 1024" pattern flagged in `MEMORY.md` as a historical bug.
- **Suggested fix:** Plumb the active `Embedder`'s `model_config.max_seq_length` into `generate_nl_with_template` (it's currently a free function with no embedder access). Either pass it as an argument from the caller in `pipeline/parsing.rs`, or move section-NL into an `Embedder::generate_nl` method. The env var should be a fallback override, not the source of truth.

#### [SHL-V1.30-3]: `MAX_BATCH_SIZE = 10_000` in LLM module hardcoded — silently truncates summary/HyDE passes on large corpora
- **Difficulty:** easy
- **Location:** `src/llm/mod.rs:192`, used at `summary.rs:58,92`, `hyde.rs:39-41`, `doc_comments.rs:271`
- **Description:** `const MAX_BATCH_SIZE: usize = 10_000;` is the per-pass cap on Anthropic Batches API submissions. For cqs's own ~17k-chunk corpus this fits in one pass; for any monorepo with >10k callable chunks the user gets a silent truncation — `summary.rs:92-97` does emit a `tracing::info!("Batch size limit reached, submitting partial batch")` but downstream a user must rerun the same `cqs index --improve-docs` or `cqs index --improve-summaries` repeatedly to make progress, with no flag to raise the cap and no visible "X chunks remain unprocessed" hint at CLI exit. Anthropic's actual Batches API hard limit is 100,000 requests per batch — cqs uses 10% of that. No env override, no config section, no CLI flag. With nomic-coderank/CodeRankEmbed enabling much larger corpora to be useful, this becomes a real ceiling. Also note that `hyde.rs:41` also uses this for HyDE expansion which has very different cost characteristics.
- **Suggested fix:** Move `MAX_BATCH_SIZE` to `src/limits.rs` with an env resolver (`CQS_LLM_MAX_BATCH_SIZE` default 10000, capped at 100000 to honor Anthropic's hard limit). Surface "X chunks remain — rerun to continue" at CLI exit when the cap is hit (today's `tracing::info!` is invisible without `RUST_LOG=info`).

#### [SHL-V1.30-4]: `serve::ABS_MAX_GRAPH_NODES = 50_000` and `ABS_MAX_CLUSTER_NODES = 50_000` hardcoded — graph/cluster views silently cap at arbitrary 25% subset of large monorepos
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:17,24`
- **Description:** Both `pub(crate) const`. The cluster query (`build_cluster`) sorts by `id ASC LIMIT effective_cap` — at 200k chunks, the cluster view shows 25% of the corpus chosen by id-string lexical order, which is essentially random with respect to topology or coverage. The graph view has the same arbitrary truncation. The comment at line 14-16 ("Prevents a single unauth request from materialising the full chunks table (millions of rows)") is a SEC-3 motivation, but the *value* 50k is a guess, not derived from anything (RAM ceiling, JSON serialization budget, browser rendering capacity). No env override; no config; no `?max_nodes` ceiling for trusted operators. A user running `cqs serve` on a 500k-chunk monorepo opens the graph view and sees an unspecified slice with no warning. Cytoscape's practical render ceiling is around 5k-10k nodes anyway, so 50k is too high for the UI side and too low for a "show me everything" power-user query.
- **Suggested fix:** Keep the security cap, but add `CQS_SERVE_MAX_GRAPH_NODES` and `CQS_SERVE_MAX_CLUSTER_NODES` env overrides (with a hard ceiling, e.g., `1_000_000`, that even env can't exceed). Better: derive from `chunk_count` so small projects ship the whole graph and large projects ship the top-N by `n_callers_global` (already an indexed column). The graph endpoint also needs a documented "show me a focused subgraph around node X" mode for monorepos.

#### [SHL-V1.30-5]: `build_chunk_detail` callers/callees/tests `LIMIT 50/50/20` hardcoded SQL constants — silent truncation in the chunk-detail UI on hot functions
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:505,542,571`
- **Description:** Three SQL queries inside `build_chunk_detail` — `callers_rows` (`LIMIT 50`), `callees_rows` (`LIMIT 50`), `tests_rows` (`LIMIT 20`) — pin the chunk-detail sidebar to fixed truncation. A heavily-called function on a large corpus (e.g., `Store::query` with 200+ callers) shows the first 50 by `(origin, line_start)` and silently drops the rest with no "showing 50 of 247" indicator. The 20-test cap is even more painful for popular utilities — `cqs::Embedding::new` may have 100+ test references. No env override, no SQL parameter binding, no JSON `truncated: true` flag in the response. Inconsistent with `build_graph` which honors `max_nodes` from the request.
- **Suggested fix:** Bind these as `?` parameters (already done via `LIMIT ?` for `build_graph`, just not here), accept `?max_callers` / `?max_callees` / `?max_tests` query params, and emit `truncated: true` in the response when the cap is hit. Source the defaults from `src/limits.rs` so the same value drives the UI, the test, and any future CLI consumer.

#### [SHL-V1.30-6]: `embed_batch_size()` default 64 doesn't scale with model dim or available VRAM — RTX 4060 OOMs while A6000 idles
- **Difficulty:** medium
- **Location:** `src/cli/pipeline/types.rs:143-160`, `src/embedder/mod.rs:685-689`
- **Description:** Embedding batch size defaults to 64 regardless of model (768-dim E5 vs 1024-dim BGE-large), regardless of `max_seq_length` (512 BGE vs 2048 nomic), regardless of GPU VRAM. Forward-pass activations scale roughly linearly in `batch * seq_len * hidden_dim`. With BGE-large + 512 seq + 64 batch on the embedder-default ORT path: ~64 × 512 × 1024 × 4 bytes ≈ 130 MB just for one tensor — fits a 4060 8GB. With nomic-coderank + 2048 seq + 64 batch: ~512 MB per tensor × multiple intermediate states → OOM on consumer GPUs. The comment at line 139-141 even says: "Was 32 (backed off from 64 after an undiagnosed crash at 2%). Restored to 64 with debug logging" — meaning we've already had a silent crash on this exact knob, and the resolution was "raise it back and add tracing" rather than make it dim/seq-aware. The eval-only `CQS_EMBED_BATCH_SIZE` env override exists but no auto-tuning.
- **Suggested fix:** When `CQS_EMBED_BATCH_SIZE` is unset, compute `64 * (768 / model_config.dim) * (512 / model_config.max_seq_length).max(0.25)` rounded to a power of 2. Or query GPU VRAM via `nvml-wrapper` (already a transitive dep via cuVS) and target ~25% of free VRAM. At minimum, document the dim/seq sensitivity in the const's docstring so future operators know to tune.

#### [SHL-V1.30-7]: `diff::EMBEDDING_BATCH_SIZE = 1000` doesn't scale with model dim — 2.6× memory variance between presets
- **Difficulty:** easy
- **Location:** `src/diff.rs:158`
- **Description:** Comment at line 156-157: "For 20k pairs at ~12 bytes/dim * model_dim, each batch is ~9-12 MB instead of ~240 MB total." The math is correct only for ~1024-dim BGE-large. For 384-dim presets, batch is ~4.6 MB; for 1024-dim it's ~12 MB. More critically, the batch *count* scales with model dim because the 1000 figure was set assuming ~1KB/embedding. With future presets shipping 1536-dim or 2048-dim (or the common case of stacking SPLADE sparse vectors alongside dense), the same batch is 18 MB or 24 MB — still OK, but not what the comment promises. No env override. Worse: the user-visible behavior of `cqs diff` (latency, memory) silently changes when you swap the model preset, with no log or doc.
- **Suggested fix:** Compute `EMBEDDING_BATCH_SIZE = max(100, 12_000_000 / (model_dim * 12))` (target 12 MB per batch regardless of dim). Or expose `CQS_DIFF_EMBEDDING_BATCH_SIZE` env override and document the dim relationship.

#### [SHL-V1.30-8]: `CagraIndex::gpu_available()` checks only that cuVS resources can be created — no VRAM ceiling, OOMs on 8GB GPUs at ~200k chunks
- **Difficulty:** medium
- **Location:** `src/cagra.rs:262-264`
- **Description:** `pub fn gpu_available() -> bool { cuvs::Resources::new().is_ok() }` returns true on any CUDA-capable GPU. Once `chunk_count >= 5000` (the CAGRA threshold), the build path runs `Index::build(&resources, &build_params, &dataset)` where dataset is `Array2::from_shape_vec((n_vectors, dim), flat_data)`. For a 200k-chunk × 1024-dim corpus this is 200k × 1024 × 4 = 819 MB on host, then copied to GPU plus graph overhead (~64 edges × n_vectors × 4 = 51 MB) plus working memory. On an 8GB GPU shared with the embedder model (~1.3 GB BGE-large) and OS/window overhead (~1-2 GB), the build OOMs. The CPU-side `cagra_max_bytes()` (default 2GB) gates host array allocation, but doesn't model GPU VRAM at all. The user reads the `MEMORY.md` line "GPUs: A6000 48GB (training), RTX 4000 8GB (inference)" — cqs is shipped for an A6000 workstation but published to crates.io for everyone else.
- **Suggested fix:** Query free GPU VRAM via cuVS / cuda-rs / nvml at `gpu_available()` time. Compute estimated build memory (`n_vectors * dim * 4 + graph_degree * n_vectors * 4 + slack`) and return false if it exceeds 80% of free VRAM. Surface as a `tracing::warn!("GPU has X MB free, CAGRA build needs Y MB — falling back to HNSW")`. Add a `CQS_CAGRA_MAX_GPU_BYTES` override for users who want to force-try.

#### [SHL-V1.30-9]: Daemon `worker_threads = min(num_cpus, 4)` hardcoded with no env override — caps shared-runtime parallelism on large machines
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:115-119`
- **Description:** `let worker_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(4);` — the shared tokio runtime that powers Store, EmbeddingCache, and QueryCache caps at 4 worker threads. On a 24-core workstation (per `MEMORY.md`), this means the daemon's SQLx pool can only meaningfully drive 4 connections worth of concurrent work even if the SQLite pool is sized larger. The hardcoded ceiling came from "the heaviest of the three" pre-#968 default. With an 8-core laptop on battery, `min(8, 4) = 4` is fine; on a 32-core EPYC server, `min(32, 4) = 4` leaves 28 cores idle. No env override.
- **Suggested fix:** Read `CQS_DAEMON_WORKER_THREADS` (default `min(num_cpus, 4)` to preserve existing behavior). Document at the constant's docstring that this is the shared-runtime size for the daemon process specifically.

#### [SHL-V1.30-10]: `train_data::MAX_SHOW_SIZE = 50 MB` for `git show` hardcoded — silently drops large files from training-data extraction
- **Difficulty:** easy
- **Location:** `src/train_data/git.rs:167`
- **Description:** `const MAX_SHOW_SIZE: usize = 50 * 1024 * 1024;` — `git_show` returns `Ok(None)` (treated as "skip this file") when stdout exceeds 50 MB. Used during training-data extraction (referenced in `~/training-data/` per memory). Generated SQL bundles, vendored deps, large docs, and minified web assets routinely exceed 50 MB. There's no log line at the call site; the caller (`pub fn git_show -> Result<Option<String>>`) treats `None` as "binary or too large", losing the distinction. No env override, no `--max-show-size` flag. The `crate::limits` module already houses similar caps (`PARSER_MAX_FILE_SIZE`, `parser_max_file_size()`) — this one was missed.
- **Suggested fix:** Move `MAX_SHOW_SIZE` to `src/limits.rs` with `train_data_git_show_max_bytes()` reading `CQS_TRAIN_GIT_SHOW_MAX_BYTES`. Distinguish "too large" from "binary" in the return type so callers can warn-log truncations explicitly.


## Algorithm Correctness

#### `semantic_diff` sort by similarity has no secondary tie-breaker — non-deterministic "most changed" ordering across runs
- **Difficulty:** easy
- **Location:** `src/diff.rs:202-207` (and the parallel test helper at `src/diff.rs:298-303`)
- **Description:** `semantic_diff` populates `modified: Vec<DiffEntry>` by iterating a `HashMap` (process-seed-randomized order) and sorts with only one key:
  ```rust
  modified.sort_by(|a, b| match (a.similarity, b.similarity) {
      (Some(sa), Some(sb)) => sa.total_cmp(&sb),
      (Some(_), None) => std::cmp::Ordering::Less,
      (None, Some(_)) => std::cmp::Ordering::Greater,
      (None, None) => std::cmp::Ordering::Equal,
  });
  ```
  Two modified entries with identical similarity (e.g., both 0.73 — common for small, nearly-identical refactors) sort into arbitrary relative order across process invocations because `sort_by` is stable w.r.t. the (HashMap-derived, random) input order, not the data. `cqs diff` and `cqs drift` JSON output will reorder identical rows between runs, defeating diff-the-diff comparisons, breaking test determinism, and making eval-flake hard to reproduce. All other score-sorting sites in the codebase carry a full `(file, name, line_start)` tie-break cascade — this one was missed in the v1.25.0 wave-1 sweep that fixed the rest.
- **Suggested fix:** Replace the `Equal` fallbacks with a cascade on the stable identity fields `DiffEntry` already carries:
  ```rust
  fn cmp_entries(a: &DiffEntry, b: &DiffEntry) -> std::cmp::Ordering {
      match (a.similarity, b.similarity) {
          (Some(sa), Some(sb)) => sa.total_cmp(&sb),
          (Some(_), None) => std::cmp::Ordering::Less,
          (None, Some(_)) => std::cmp::Ordering::Greater,
          (None, None) => std::cmp::Ordering::Equal,
      }
      .then_with(|| a.file.cmp(&b.file))
      .then_with(|| a.name.cmp(&b.name))
      .then_with(|| a.chunk_type.cmp(&b.chunk_type))
  }
  ```
  Apply to both production (line 202) and the test at line 298 so they don't drift. Add a `proptest!`-style shuffling test that asserts the sort is stable across shuffled inputs.

#### `is_structural_query` keyword probe uses `format!(" {} ", kw)` and misses keywords at end-of-query
- **Difficulty:** easy
- **Location:** `src/search/router.rs:787-789`
- **Description:**
  ```rust
  STRUCTURAL_KEYWORDS
      .iter()
      .any(|kw| query.contains(&format!(" {} ", kw)) || query.starts_with(&format!("{} ", kw)))
  ```
  Covers keywords preceded by whitespace and surrounded by whitespace (via `" {} "`) or at the very start (via `"{} "`), but **not keywords at the end of the query**. Concrete failure trace for `"find all trait"` (3 words):
  - `is_identifier_query`: `"all"` is in `NL_INDICATORS` → returns false.
  - `is_cross_language_query`: no two language names → false.
  - `extract_type_hints`: "trait" isn't in the chunk-type hint table (which is phrases like "all traits") → none returned.
  - `is_structural_query`: `STRUCTURAL_PATTERNS_AC` doesn't match; keyword loop with `kw="trait"` → `query.contains(" trait ")` false (no trailing space), `query.starts_with("trait ")` false. **All keywords fail** → false.
  - `is_behavioral_query`: no behavioral verb word-match, no "code that"/"function that" → false.
  - `is_conceptual_query`: `words.len() == 3 <= 3`, `"all"` is NL-indicator match, `!is_structural_query` → **true**.
  - Routes to `Conceptual` (α=0.70), should have been `Structural` (α=0.90).

  Same pattern for `"show me all trait"`, `"find every impl"`, `"list all enum"`, `"all class"`, `"find enum"`, etc. — i.e., the common NL pattern where a user ends their query with the type they're looking for. This shifts SPLADE α from 0.90 → 0.70 for every such query (≈20% heavier SPLADE weight than intended on Structural), and the strategy enum shifts from `DenseWithTypeHints` → `DenseDefault`, bypassing the type-boost path entirely. Also allocates a `String` per (keyword × probe) iteration on every classify. The adjacent structural-pattern check uses Aho-Corasick — the keyword path should too.
- **Suggested fix:** Replace with a word-boundary check over the pre-computed `words` vec (same approach already used for `NEGATION_TOKENS`):
  ```rust
  pub fn is_structural_query(query: &str) -> bool {
      if STRUCTURAL_PATTERNS_AC.is_match(query) { return true; }
      // words is computed once upstream; pass it through instead of re-splitting
      let words: Vec<&str> = query.split_whitespace().collect();
      STRUCTURAL_KEYWORDS.iter().any(|kw| words.iter().any(|w| w == kw))
  }
  ```
  Add regression tests: `"find all trait"` → Structural, `"all class"` → Structural, `"find enum"` → Structural. No allocation, correct at EOL, matches the pattern the rest of the router uses.

#### `bfs_expand` processes BFS seeds in HashMap iteration order — non-deterministic `name_scores` when `max_expanded_nodes` cap is reached mid-expansion
- **Difficulty:** easy
- **Location:** `src/gather.rs:317-320` (seed enqueue from `name_scores.keys()`) and `src/gather.rs:326,338` (cap checks)
- **Description:**
  ```rust
  let mut queue: VecDeque<(Arc<str>, usize)> = VecDeque::new();
  for name in name_scores.keys() {
      queue.push_back((Arc::from(name.as_str()), 0));
  }
  while let Some((name, depth)) = queue.pop_front() {
      // ...
      if name_scores.len() >= opts.max_expanded_nodes && visited.len() > initial_size {
          expansion_capped = true;
          break;
      }
      // expand neighbors
  }
  ```
  `name_scores` is a `HashMap<String, ...>`, so `name_scores.keys()` iterates in seed-randomized order. When the BFS hits `max_expanded_nodes` mid-expansion (common on dense graphs — default `max_expanded_nodes` = 50 for onboard callers BFS, see `src/onboard.rs:165`), which seeds got expanded and which got cut off depends entirely on which order the iterator handed them out. Different runs of `cqs gather`, `cqs task`, `cqs onboard` on the same corpus/query produce different expanded graphs, different score maps, different final chunk lists after dedup+truncate. This is exactly the class of non-determinism the v1.25.0 tie-break sweep targeted, but it sits one layer up in the pipeline (BFS graph seeding, not result sorting).
- **Suggested fix:** Enqueue seeds in a deterministic order — easiest is a sort by `(initial_score desc, name asc)` before push:
  ```rust
  let mut seeds: Vec<(&String, (f32, usize))> =
      name_scores.iter().map(|(k, v)| (k, *v)).collect();
  seeds.sort_by(|a, b| {
      b.1.0.total_cmp(&a.1.0)               // higher score first
          .then_with(|| a.0.cmp(b.0))        // tie on name asc
  });
  for (name, _) in seeds {
      queue.push_back((Arc::from(name.as_str()), 0));
  }
  ```
  This respects the "process higher-scoring seeds first" intent (the old code happened to do this only by coincidence of HashMap hashing), and makes the cap-at-50 cutoff deterministic. Add a test that seeds two equally-scored entries, caps at a small `max_expanded_nodes`, and asserts the same `name_scores` on 100 re-runs.

#### `llm::summary::contrastive_neighbors` top-K selection sorts by score alone — non-deterministic neighbor choice when similarities tie
- **Difficulty:** easy
- **Location:** `src/llm/summary.rs:263,265,267`
- **Description:** Three sibling sorts all use `b.1.total_cmp(&a.1)` with no tie-break:
  ```rust
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));           // line 263
  candidates.select_nth_unstable_by(limit - 1, |a, b| b.1.total_cmp(&a.1));  // line 265
  candidates.truncate(limit);
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));           // line 267
  ```
  `candidates` is `Vec<(usize, f32)>` where `usize` is an index into `valid_owned`. When multiple candidates have identical similarity (common at low precision — f32 embeddings clamp to the same bit pattern for very close vectors, especially for L2-normalized embeddings over the same reindex cohort), `select_nth_unstable` can pick any of them, and the final neighbor set for a given seed is non-deterministic. This propagates into the prompt sent to the LLM for contrastive summary generation, so the *same* corpus + *same* seed chunk produces different summaries on different runs. Contrastive summary caching by content_hash then either caches the first random result forever (good) or wastes Batches API credits regenerating when the cache misses (bad — ~$0.38/run Haiku).
- **Suggested fix:** All three sort calls need the index as a secondary key. `candidates: Vec<(usize, f32)>` already carries the index:
  ```rust
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
  candidates.select_nth_unstable_by(limit - 1, |a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
  candidates.truncate(limit);
  candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
  ```
  Same cascade the rest of the codebase applies everywhere else.

#### `--name-boost` CLI arg accepts negative / >1 values — negative embedding weight, out-of-range fusion
- **Difficulty:** easy
- **Location:** `src/cli/args.rs:57-58` (arg declaration); `src/search/scoring/candidate.rs:286` (consumer)
- **Description:** CLI argument validation:
  ```rust
  #[arg(long, default_value = "0.2", value_parser = parse_finite_f32)]
  pub name_boost: f32,
  ```
  `parse_finite_f32` only rejects NaN/Infinity; any other `f32` value passes through. Consumer in `apply_scoring_pipeline`:
  ```rust
  (1.0 - ctx.filter.name_boost) * embedding_score + ctx.filter.name_boost * name_score
  ```
  A user calling `cqs search "foo" --name-boost 5.0` gets `(1.0 - 5.0) * embedding = -4.0 * embedding_score`, i.e., the embedding signal is **negated** — identical semantic matches get ranked last. Symmetrically, `--name-boost -1.0` gives `2.0 * embedding - 1.0 * name_score`, over-weighting embedding past its natural [0,1] range. The `.clamp(0.0, 1.0)` that the config-file path applies at `src/config.rs:370-371` is not mirrored on the CLI-flag path, so a config that looked safe can be overridden into a search-breaking regime via a stray flag. Most eval scripts set `--name-boost` explicitly, so a typo is one `bash` run away.
- **Suggested fix:** Replace the argument parser with a clamped variant. Either add a helper:
  ```rust
  fn parse_name_boost(s: &str) -> std::result::Result<f32, String> {
      let v = parse_finite_f32(s)?;
      if (0.0..=1.0).contains(&v) { Ok(v) } else {
          Err(format!("name_boost must be in [0.0, 1.0], got {v}"))
      }
  }
  ```
  and use `value_parser = parse_name_boost` at line 57. Or enforce the clamp at `SearchFilter` construction so config and CLI paths converge. Same fix applies to any other weight/threshold-style f32 flag.

#### `reranker::compute_scores_opt` — `batch_size * stride` unchecked multiplication hides shape errors; `data[i * stride]` can panic on overflow
- **Difficulty:** easy
- **Location:** `src/reranker.rs:368-387`
- **Description:**
  ```rust
  let stride = if shape.len() == 2 { shape[1] as usize } else { 1 };
  if stride == 0 { /* return error */ }
  let expected_len = batch_size * stride;              // <-- unchecked mul
  if data.len() < expected_len { /* return error */ }
  let scores: Vec<f32> = (0..batch_size).map(|i| sigmoid(data[i * stride])).collect();
  ```
  `shape[1]` is `i64` from ORT. The zero-guard landed after the prior audit (RB-8) but the negative-dim and overflow guards are still missing:
  - `shape[1] = -1` → `(-1_i64 as usize) = usize::MAX` (on 64-bit).
  - `batch_size * usize::MAX` wraps to a small value; `data.len() < expected_len` passes with that small wrapped value.
  - Inside the loop, `i * stride` also wraps, indexing `data` at an arbitrary position. If the wrapped index exceeds `data.len()`, **Rust bounds-checks and panics** in the middle of a hot inference call — aborting the entire search pipeline.
  A malicious / corrupted ONNX file (or a new reranker with an unusual output tensor layout) is the reachable source of a negative or pathologically-large `shape[1]`.
- **Suggested fix:** Guard the cast and the multiply:
  ```rust
  if shape.len() == 2 && shape[1] <= 0 {
      return Err(RerankerError::Inference(format!(
          "reranker output has non-positive dim 1: {}", shape[1]
      )));
  }
  let stride = if shape.len() == 2 { shape[1] as usize } else { 1 };
  if stride == 0 { /* existing error */ }
  let expected_len = batch_size.checked_mul(stride).ok_or_else(|| {
      RerankerError::Inference(format!(
          "reranker expected_len overflows: batch_size={batch_size} stride={stride}"
      ))
  })?;
  if data.len() < expected_len { /* existing error */ }
  ```
  Same pattern fixes the SPLADE six-site parallel in `splade/mod.rs` (see prior audit RB-9). The `data[i * stride]` indexing can stay as-is once the upstream `expected_len` check is sound.

#### `llm::doc_comments::select_uncached` sort has no tie-break beyond content length — non-deterministic selection when `max_docs` truncates
- **Difficulty:** easy
- **Location:** `src/llm/doc_comments.rs:222-229,242`
- **Description:**
  ```rust
  uncached.sort_by(|a, b| {
      let a_no_doc = a.doc.as_ref().is_none_or(|d| d.trim().is_empty());
      let b_no_doc = b.doc.as_ref().is_none_or(|d| d.trim().is_empty());
      b_no_doc.cmp(&a_no_doc)
          .then_with(|| b.content.len().cmp(&a.content.len()))
  });
  // ...
  uncached.truncate(uncached_cap);
  ```
  Two chunks with the same `has-doc` status and the same content-length byte count collide on the compare; `sort_by` is stable w.r.t. the input `uncached` vec's order, which is fed by a DB scan that may return duplicates-by-size in any order depending on index layout. When `--improve-docs --max-docs N` trips the truncate (line 242), which rows get documented vs skipped is non-deterministic across runs. For a Claude Batches API call (≈ $0.38 / run Haiku), that means the set of chunks that eat budget is non-reproducible. Between the enrichment re-run and the contrastive-summaries batcher this is the third "tie-break missing" site in `llm/*.rs`.
- **Suggested fix:** Append a stable tertiary key — chunk id is always unique and carried by `ChunkSummary`:
  ```rust
  .then_with(|| b.content.len().cmp(&a.content.len()))
  .then_with(|| a.id.cmp(&b.id))
  ```

#### `token_pack` breaks on first oversized item — drops smaller items that would fit, undershoots budget
- **Difficulty:** easy
- **Location:** `src/cli/commands/mod.rs:398-417` (greedy loop in `token_pack`)
- **Description:** The greedy knapsack loop treats budget overflow as a hard stop:
  ```rust
  for idx in order {
      let tokens = token_counts[idx] + json_overhead_per_item;
      if used + tokens > budget && kept_any {
          break;          // <-- should be `continue;`
      }
      // ...
      used += tokens;
      keep[idx] = true;
  }
  ```
  Once a single item fails to fit, the loop exits — every lower-scored item is dropped, even items that would comfortably fit in the remaining budget. Concrete repro: budget = 300, items sorted by score descending = `[A=250 tokens, B=100 tokens, C=40 tokens]`. After `A` is packed (used=250), `B` fails (`350 > 300`) → `break` → `C` is silently dropped, even though `used + 40 = 290 ≤ 300`. With `continue`, `C` would land in the result and the function would return `(2 items, 290 tokens)` instead of `(1 item, 250 tokens)`. Hits every consumer of `--tokens` — `cqs context`, `cqs explain`, `cqs scout`, `cqs gather`, `cqs task`, the CLI/batch search packers, etc. — under the realistic mix where one large chunk is followed by smaller chunks in the score-ordered list. Particularly bad for code search where high-relevance fixtures (whole modules) often outweigh the per-symbol chunks that would otherwise round out the response.
- **Suggested fix:** Replace `break` with `continue` so the loop keeps probing for fits, and drop the now-redundant `kept_any` short-circuit on the break (the `kept_any && tokens > budget` check is still needed for the "include at least one" branch). Add a regression test with score-sorted items `[oversized, fits, fits]` asserting the two `fits` survive.

#### `map_hunks_to_functions` returns hunks in HashMap iteration order — non-deterministic `cqs impact-diff` JSON across runs
- **Difficulty:** easy
- **Location:** `src/impact/diff.rs:38-106` (`map_hunks_to_functions` outer loop), and the downstream truncation at `src/impact/diff.rs:154-168`
- **Description:** Two layered determinism bugs in the diff-impact pipeline:
  1. `by_file: HashMap<&Path, Vec<&DiffHunk>>` is iterated at line 66 (`for (file, file_hunks) in &by_file`). HashMap iteration is process-seed-randomized, so the order of `functions: Vec<ChangedFunction>` produced is run-to-run random for any diff that touches more than one file.
  2. The `analyze_diff_impact_with_graph` cap at line 165 uses `changed.into_iter().take(cap)` (default cap = 500). When the input exceeds 500 functions, *which* 500 survive depends on the random Vec order from step 1 — so on a real "big refactor" diff (>500 changed functions), `cqs impact-diff` output is nondeterministically truncated. Two runs against the same diff give different `changed_functions`, different caller batches, different reverse-BFS results, different test sets, different `via` attributions.
- **Suggested fix:** Sort `changed` by `(file_path, line_start, name)` after `map_hunks_to_functions` builds it, before the cap takes effect:
  ```rust
  let mut changed = map_hunks_to_functions(...);
  changed.sort_by(|a, b| {
      a.file.cmp(&b.file)
          .then(a.line_start.cmp(&b.line_start))
          .then(a.name.cmp(&b.name))
      });
  ```
  Or build `by_file` as a `BTreeMap`/`Vec<(&Path, …)>` sorted by path. Add a regression test with a diff spanning 3 files and assert `functions` is identical across 100 calls.

#### `drain_pending_rebuild` dedup against rebuild-thread snapshot drops fresh embeddings for chunks whose content changed during the rebuild window
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:1077-1105` (`drain_pending_rebuild`, the `known` filter)
- **Description:** The non-blocking HNSW rebuild added in #1113 streams a snapshot of `(id, embedding)` from a read-only Store handle in a worker thread, while the watch loop continues capturing newly upserted `(id, embedding)` pairs into `pending.delta`. On swap, the code dedups via:
  ```rust
  let known: HashSet<&str> = new_index.ids().iter().map(String::as_str).collect();
  let to_replay: Vec<(String, Embedding)> = pending.delta
      .into_iter()
      .filter(|(id, _)| !known.contains(id.as_str()))
      .collect();
  ```
  `known` contains every chunk id the rebuild thread saw at its snapshot moment. If the watch loop *re-embedded* a chunk during the rebuild window (file edit while rebuild was in flight — exactly the case the non-blocking rebuild was added to handle), the new `(id, Embedding)` pair lands in `pending.delta`, but `known` already contains the id with the *old* embedding. The filter drops the fresh embedding, and the swapped-in HNSW carries the stale vector until the next threshold rebuild. For an editor save loop this means up to 100 saves' worth of stale vectors against the freshly-modified file, exactly defeating the rebuild's purpose. Search for the modified file returns hits on the pre-edit content.
- **Suggested fix:** Dedup must compare the embedding payload, not just the id. Cleanest: have the rebuild thread return `Vec<(String, blake3_hash)>` alongside the index, and replay any delta whose `(id, hash)` differs from the snapshot. Cheaper alternative: use the chunk's `content_hash` from the store at delta-capture time — `pending.delta` becomes `Vec<(String, Embedding, ContentHash)>`, and the dedup filter checks the content hash matches the rebuilt vector's hash. Add a test that mid-rebuild-window upserts of an existing id produce the *new* embedding in the swapped-in index.

#### `search_reference` weighted threshold filters using post-weight comparison but pre-weight `limit` — multi-ref ranking truncates valid candidates
- **Difficulty:** easy
- **Location:** `src/reference.rs:231-258` (`search_reference`, `apply_weight = true` branch)
- **Description:** The flow is:
  ```rust
  let mut results = ref_idx.store.search_filtered_with_index(
      query_embedding, filter, limit, threshold, ref_idx.index.as_deref(),
  )?;
  if apply_weight {
      for r in &mut results { r.score *= ref_idx.weight; }
      results.retain(|r| r.score >= threshold);
  }
  ```
  Two coupled algorithm bugs:
  1. `search_filtered_with_index` is asked for the top `limit` results that satisfy `score ≥ threshold`. With `weight = 0.8` and a `threshold = 0.5`, a chunk that scored 0.62 (above raw threshold) will pass into `results`, then become 0.50 after `*= weight`. A chunk that scored 0.40 (below raw threshold) is dropped by the underlying search — but `0.40 * 1/weight = 0.50` would have been the boundary. The reference therefore *systematically* under-samples its own corpus when `weight < 1.0`: the underlying top-`limit` cap is computed against unweighted scores, missing valid post-weight survivors.
  2. The post-weight `retain` then re-applies the same `threshold` on weighted scores, double-filtering: a 0.51 raw score (passes raw threshold) becomes 0.408 weighted (fails weighted threshold), so it's dropped *after* the underlying search already spent the cycle on it. The user pays for the search but gets a smaller result set than `limit` even when more candidates exist.
- **Suggested fix:** When `apply_weight` is true, query the underlying store with a relaxed threshold (`threshold / weight` for weight > 0) and an over-fetch limit, then weight + filter + sort + truncate to `limit` in caller-side code:
  ```rust
  let raw_threshold = if apply_weight && ref_idx.weight > 0.0 {
      threshold / ref_idx.weight
  } else { threshold };
  let raw_limit = if apply_weight {
      // 2x or 3x over-fetch leaves headroom for the weighted retain step
      (limit as f32 * 2.0).ceil() as usize
  } else { limit };
  let mut results = ref_idx.store.search_filtered_with_index(
      query_embedding, filter, raw_limit, raw_threshold, ref_idx.index.as_deref())?;
  if apply_weight {
      for r in &mut results { r.score *= ref_idx.weight; }
      results.retain(|r| r.score >= threshold);
      results.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.chunk.id.cmp(&b.chunk.id)));
      results.truncate(limit);
  }
  ```
  Same fix shape applies to `search_reference_by_name` at line 265-285 (which has the threshold-then-weight ordering inverted, hiding the same bug).

#### `find_type_overlap` chunk_info dedup picks `(file, line)` from random HashMap iteration — non-deterministic `cqs related` per-result file attribution
- **Difficulty:** easy
- **Location:** `src/related.rs:131-147` (build of `chunk_info`); `src/related.rs:155-157` (final sort, no tie-break on name)
- **Description:** Two algorithm bugs in the same loop:
  1. `chunk_info: HashMap<String, (PathBuf, u32)>` uses `or_insert(...)` to remember the first `(file, line)` seen for each function name. The outer iteration `for chunks in results.values()` walks `results: HashMap<String, Vec<ChunkSummary>>` in process-seed-random order. When a function name appears across multiple type result lists (common — a function uses several types and so shows up in each type's user list), the `or_insert` retains the first arrival, which depends on which type's bucket happens to be first in the random iteration. For a function defined in one file but with overloads or test fixtures in another, the result row's `file` field flips run-to-run.
  2. The final sort at line 155-157 is `sorted.sort_by_key(|e| Reverse(e.1))` (count only) followed by `truncate(limit)`. Counts in this domain are tiny integers (1, 2, 3) and equal counts are the rule, not the exception. Truncate then picks arbitrary names from a HashMap-ordered Vec.
  3. Earlier at line 59-65, `type_names` is computed via `HashSet → into_iter().collect::<Vec>()` — also random order — though this only affects bind ordering downstream, not result identity.
  Net effect: `cqs related <fn>` returns different `shared_types` lists across runs, with different `(file, line)` attribution per result. Defeats `cqs related <fn>` reproducibility for evals or cached agent prompts.
- **Suggested fix:** (a) Sort `type_counts.into_iter()` results into a deterministic Vec before the count-sort: `sorted.sort_by(|a, b| Reverse(a.1).cmp(&Reverse(b.1)).then(a.0.cmp(&b.0)))` so equal counts break by name asc. (b) For `chunk_info`, walk `results` in sorted-by-key order so the first `or_insert` is deterministic — or store *all* `(file, line)` candidates per name and pick `min` by `(file, line)` after the loop. (c) Convert the HashSet collect at line 63-65 into a sort: `let mut type_names: Vec<_> = ...; type_names.sort(); type_names.dedup();`.

#### CAGRA `search_with_filter` silently under-fills when `included < k` — caller cannot distinguish "few matching candidates" from "filter too restrictive"
- **Difficulty:** medium
- **Location:** `src/cagra.rs:520-598` (`search_with_filter`); `src/cagra.rs:344-486` (`search_impl`)
- **Description:** When the caller asks for `k` results filtered by a predicate, the bitset path does:
  ```rust
  let mut included = 0usize;
  for (i, id) in self.id_map.iter().enumerate() {
      if filter(id) { bitset[i / 32] |= 1u32 << (i % 32); included += 1; }
  }
  if included == n { return CagraIndex::search(self, query, k); }
  if included == 0 { return Vec::new(); }
  // else: ask CAGRA for k — but only `included` slots can ever be filled
  ```
  When `included < k` (e.g., `cqs search "foo" --include-type Function --lang rust` over a corpus where the Function/Rust subset has 12 vectors but `k = 20`), CAGRA receives `topk = 20` and writes valid `(neighbor, distance)` pairs into the first `included` slots; the remaining `k - included` slots stay at the `INVALID_DISTANCE` sentinel. The `!dist.is_finite()` check at line 473 correctly drops those slots, but the caller above this layer (e.g., `Store::search_filtered_with_index`) sees `Vec<IndexResult>` of length `min(included, k)` with no signal that under-fill happened. Downstream paging / pagination logic that assumes "got fewer than k → end of results" is correct, but a user who set `--limit 20` and gets 12 has no way to distinguish "this filter combination has only 12 hits" from "CAGRA itopk_size cap silently truncated".

  Worse, when `included < k` AND `k > itopk_max` (#988 reported `itopk_size_max=480` while `k=500` failed), CAGRA returns an error from `gpu.index.search(...)` and `search_impl` logs at error then returns `Vec::new()` — silently zeroing out a query that, with `k = included`, would have succeeded. The path doesn't try a fallback with `k = included.min(k)`.
- **Suggested fix:** Cap `k` at `included` before invoking `search_impl` so CAGRA is always asked for a feasible top-K:
  ```rust
  let effective_k = k.min(included);
  // … then
  self.search_impl(&gpu, query, effective_k, Some(&bitset_device))
  ```
  Add a debug log when `effective_k < k` so eval scripts can see the truncation. As a follow-on, `search_filtered_with_index` should propagate a `degraded` / `truncated` boolean upward when the underlying index returned `< k` results so that the JSON envelope can carry it (matches the pattern used in `analyze_diff_impact_with_graph` for `truncated: bool`).

#### Hybrid SPLADE fusion: `alpha == 0` branch produces unbounded `1.0 + s` scores that mix into a [-1, 1] cosine pool — magic-constant cliff at the SPLADE boundary
- **Difficulty:** easy
- **Location:** `src/search/query.rs:649-672` (the fusion lambda inside `splade_fuse_with_dense`)
- **Description:** The `alpha <= 0.0` branch ("pure re-rank mode") emits:
  ```rust
  let score = if alpha <= 0.0 {
      if s > 0.0 { 1.0 + s } else { d }
  } else {
      alpha * d + (1.0 - alpha) * s
  };
  ```
  - `s` is normalized to `[0, 1]` by dividing by `max_sparse` (line 614), but `1.0 + s` is in `[1.0, 2.0]`.
  - `d` (dense cosine) is in `[-1, 1]` (`cosine_similarity` is not clamped here — `apply_scoring_pipeline`'s `.max(0.0)` runs *later*, after this fusion).
  - A SPLADE-found chunk with `s = 0.001` (barely-there sparse signal — possible when its single shared subword token barely fires) gets `1.0 + 0.001 = 1.001`, which beats *every* SPLADE-unknown chunk regardless of how relevant they are by dense cosine (best possible 1.0 unclamped).
  - A SPLADE-found chunk with `s = 0` is *not* in `sparse_scores` because `sparse_scores.insert(&r.id, normalized)` runs even when `normalized = 0` only if the source had `r.score > 0` upstream — confirmed (and `max_sparse > 0` gate at line 614). So the `s > 0` test does what it says, but the cliff at the threshold (any positive sparse hit, no matter how small, dominates dense) is a hidden gotcha. Real-world impact: setting `--alpha 0` (re-rank mode) inverts the result list when a sparse-only weak match exists.
- **Suggested fix:** Use a calibrated additive boost rather than a magic constant — e.g., `let boost = 1.0 + s * 0.1;` would still place SPLADE-found chunks above any non-found candidate while preserving their dense ordering relative to each other within the boost band. Or, as the cleaner option, treat `alpha == 0` symmetrically with the linear path: `0.0 * d + 1.0 * s = s`, and rely on an `s > d` post-filter to bias toward sparse hits. Either way, drop the `1.0 + s` magic constant — it inflates SPLADE matches into a band the dense path can never reach. Add a doc-test repro: dense pool `[(A, 0.95)]`, sparse pool `[(B, 0.001 normalized)]`, `alpha = 0.0` → expect `A` first, but the current code returns `[B, A]` with `B@1.001`.

#### `apply_scoring_pipeline` / hybrid path drops embedding-score sign without clamping — negative cosine inflates negatives via `name_boost` sign-flip
- **Difficulty:** easy
- **Location:** `src/search/scoring/candidate.rs:283-298` (the `(1.0 - name_boost) * embedding_score + name_boost * name_score` blend)
- **Description:**
  ```rust
  let base_score = if let Some(matcher) = ctx.name_matcher {
      let n = name.unwrap_or("");
      let name_score = matcher.score(n);
      (1.0 - ctx.filter.name_boost) * embedding_score + ctx.filter.name_boost * name_score
  } else { embedding_score };
  // …
  let mut score = base_score.max(0.0) * ctx.note_index.boost(file_part, chunk_name);
  ```
  `embedding_score` here is raw cosine which is in `[-1, 1]` for un-normalized or oddly-normalized vectors and even for unit-norm vectors when the chunk and query are anti-correlated. The blend `(1 - nb) * e + nb * ns` with `e = -0.3, ns = 0.0, nb = 0.2` produces `-0.24`. The subsequent `.max(0.0)` clamps to `0.0`, which then misses the `score >= threshold` test (`threshold` defaults > 0). So far so good for the negative case.
  But when `name_boost > 1` is supplied (CLI accepts arbitrary finite `f32`, see the still-pending AC-V1.29-5 from triage), `(1 - nb)` is negative, multiplying `e = 0.9` (a great match) by `-0.5` and adding `nb * ns`. The `.max(0.0)` then turns this into `0.0`, silently demoting good matches to zero. Net effect: an out-of-range `--name-boost` flag does not just mis-weight — it deletes good results. Compounds with the same finding in the existing scratch (#5).
- **Suggested fix:** Clamp `name_boost` to `[0.0, 1.0]` at `SearchFilter` construction (single fix point that closes both the CLI and config paths), and clamp `embedding_score` to `[0.0, 1.0]` *before* the blend so the linear interpolation is always between two numbers in the same range and never produces a sign-flip:
  ```rust
  let embedding_score = embedding_score.clamp(0.0, 1.0);
  let nb = ctx.filter.name_boost.clamp(0.0, 1.0);
  let base_score = if let Some(matcher) = ctx.name_matcher {
      let name_score = matcher.score(name.unwrap_or(""));
      (1.0 - nb) * embedding_score + nb * name_score
  } else { embedding_score };
  ```
  Add a property test asserting `apply_scoring_pipeline` output ∈ `[0.0, ∞)` for *any* `name_boost`, `embedding_score`, `name_score` ∈ `f32::finite()`.



## Extensibility

> Note: every original v1.29.0 finding (EX-V1.29-1 through EX-V1.29-9, archived in `docs/audit-triage-v1.30.0.md`) was substantially closed by #1101 (Local LLM provider), #1105 (slots/cache), #1114 (single-registration command registry), the `define_aux_presets!` table, the `define_chunk_types!` macro, the `define_query_categories!` macro, the `ConfigSection` trait + `ArgMatches::value_source()` shift, the `VisibilityRule::{SigStartsTriage, RegexImportSet, NameCase}` extensions, the `NotesListArgs` flatten, and the `approx_download_bytes` field on `ModelConfig`. The findings below are the residual / new pressure points after that wave.

#### Adding a third score signal requires touching two parallel fusion code paths
- **Difficulty:** medium
- **Location:** `src/store/search.rs:182-229` (`Store::rrf_fuse(semantic_ids: &[&str], fts_ids: &[String], limit) -> Vec<(String, f32)>`), `src/search/query.rs:511-720` (`search_hybrid` linear-α dense+sparse), `src/search/query.rs:399` (call site uses 2-list `rrf_fuse`)
- **Description:** Today there are two completely separate ways to combine ranked candidate lists into a final score:
  1. `Store::rrf_fuse(semantic_ids, fts_ids, limit)` — RRF formula `1/(k+rank)`, hardcoded to **exactly two** input lists (semantic embedding + FTS keyword). The function signature has named `semantic_ids` and `fts_ids` parameters; adding a third signal (a name-fingerprint signal, a type-overlap signal, the SPLADE sparse list, or the cross-encoder rerank score) requires changing the signature plus every caller.
  2. `Self::search_hybrid` — a separate dense+sparse linear-interpolation path with its own α knob (`splade_alpha`), its own min-max normalization, its own dedup loop. SPLADE blends here, not via RRF, because RRF is locked to two lists.
  
  Concrete symptom: SPLADE is built and persisted at index time, but cannot be one of the inputs to RRF — it lives on a parallel `search_hybrid` path with different normalization (linear interp, not reciprocal-rank). The "type boost" added at `query.rs:449-454` is a *third* score-mutation site (multiplicative post-fusion) that's neither RRF-fused nor α-blended; it's just a hardcoded multiplier applied after the fact. Adding the planned reranker score as a fusion input (rather than the current "rerank top-K post-hoc" mode) would force a fourth bespoke blend path.
- **Suggested fix:** Generalize `rrf_fuse` to `fn rrf_fuse_n(ranked_lists: &[&[&str]], limit: usize) -> Vec<(String, f32)>` so any number of ranked sources contribute. Optionally introduce `trait ScoreSignal { fn rank(&self, query: &Query) -> Vec<&str>; fn weight(&self) -> f32; }` and a `FusionPipeline` that owns an ordered list of signals — semantic, FTS, SPLADE, name-fingerprint, type-boost all become uniform participants. Either fix collapses two parallel code paths into one and removes the "RRF can't take SPLADE" architectural quirk.

#### `BatchCmd::is_pipeable` is a separate exhaustive match outside the command registry
- **Difficulty:** easy
- **Location:** `src/cli/batch/commands.rs:325-364` (`BatchCmd::is_pipeable(&self) -> bool`), `src/cli/batch/commands.rs:413-538` (`fn dispatch(ctx, cmd) -> Result<Value>`), `src/cli/registry.rs:55-720` (the `for_each_command!` registry that already classifies commands)
- **Description:** Issue #1097 (PR #1114) collapsed five Group-A/B exhaustive matches in `dispatch.rs` + `definitions.rs` (`batch_support`, `variant_name`, two dispatch matches, plus the legacy `BatchSupport::Cli/Daemon` decision) into a single `for_each_command!` row per command. But the **batch-side** `BatchCmd` enum was not lifted into the same registry — it still has two exhaustive matches that must be hand-edited per command:
  - `is_pipeable()` (39-line two-arm match: pipeable vs non-pipeable)
  - `dispatch()` (130-line single-arm-per-variant match calling `handlers::dispatch_*`)
  
  And the batch enum itself lists 31 variants alongside the `Commands` enum — same surface, different file. Verified by reading `cli/batch/commands.rs:413-538` (the dispatch is a per-variant `BatchCmd::Foo { args, .. } => handlers::dispatch_foo(ctx, &args.x, &args.y, …)` chain; pure mechanical mapping, exhaustive, no compile-time link to the `Commands` enum, no link to the `for_each_command!` registry). Reduces #1114's "one row per command" promise to "one row, one batch enum variant, two batch matches".
- **Suggested fix:** Either (a) drive `BatchCmd` from `for_each_command!` so adding a row in the registry generates the `BatchCmd` variant + `is_pipeable` arm + dispatch arm + handler stub, with the row optionally carrying a `is_pipeable: true` flag and the handler ident. Or (b) merge `Commands` and `BatchCmd` into a single enum (clap derive supports this) with `is_pipeable` derived from a registry attribute. Either path eliminates the parallel batch-side fan-out.

#### `LlmProvider` resolver and `create_client` factory hardcode two providers — no registry, despite already needing one
- **Difficulty:** easy
- **Location:** `src/llm/mod.rs:200-205` (`LlmProvider` enum: 2 variants), `:284-304` (`match std::env::var("CQS_LLM_PROVIDER")` matches the strings `"anthropic"` / `"local"` literally), `:362-398` (`create_client` matches `LlmProvider::Anthropic` / `::Local` and constructs the per-provider client + writes the per-provider config-validation error message)
- **Description:** `create_client` now returns `Box<dyn BatchProvider>` (closing the bulk of EX-V1.29-3), and `LocalProvider` is a real second impl (#1101). But the **registration** of providers is still hand-coded in three places:
  1. `LlmProvider` enum in `mod.rs:200-205` (one variant per provider)
  2. `resolve()` env-var match in `mod.rs:284-304` (`Some("anthropic") | None => …, Some("local") => …, Some(other) => warn + default`) — adding a third provider requires another arm here. The env-var-name-to-variant table is implicit in this match.
  3. `create_client()` factory in `mod.rs:362-398` (one match arm per provider; each arm reads its own env vars — `ANTHROPIC_API_KEY`, etc. — and validates its own preconditions)
  
  The `API_BASE` (`https://api.anthropic.com/v1`), `API_VERSION` (`2023-06-01`), and `MODEL` (`claude-haiku-4-5`) constants in `mod.rs:167-169` are still file-level Anthropic defaults; the same constants double-duty as "is this user configured for local?" sentinels at `:381-394` (`if llm_config.api_base == API_BASE { return Err("Local requires CQS_LLM_API_BASE") }`). Add a third provider (e.g. OpenAI cloud, Gemini) and you must add its own sentinel-detection arm too.

  And `batch.rs:64-261` carries 5 hardcoded `header("anthropic-version", API_VERSION)` + `header("x-api-key", &self.api_key)` calls — the `LlmClient` impl bakes Anthropic's auth scheme. A new "OpenAI cloud" provider can't reuse `LlmClient`; it must be a fresh `BatchProvider` impl (which is fine — the trait is set up for it) but the env-var resolver and the factory both still need editing.
- **Suggested fix:** Introduce `trait ProviderRegistry { fn name(&self) -> &'static str; fn from_env(&self, cfg: &Config) -> Result<Box<dyn BatchProvider>>; }` and an inventory-style `static REGISTRY: &[&dyn ProviderRegistry]`. `resolve()`'s env-var match and `create_client`'s factory both walk the slice. Adding a provider then means: (1) impl `ProviderRegistry` for the new struct, (2) add it to the slice. The `API_BASE / MODEL / API_VERSION` constants move into the Anthropic-specific impl where they belong.

#### Vector index backend selection is a `#[cfg]`-gated hand-coded if/else chain — no `IndexBackend` trait
- **Difficulty:** medium
- **Location:** `src/cli/store.rs:423-540` (`build_vector_index_with_config<Mode>`)
- **Description:** Today the function is a 120-line `#[cfg(feature = "cuda-index")]` block with three explicit branches: (1) "chunk_count >= cagra_threshold AND gpu_available → CAGRA, try persisted, else build, persist", (2) "chunk_count < cagra_threshold → HNSW", (3) "GPU unavailable → HNSW". Each branch hand-codes the persistence path (`index.cagra` literal, `delete_persisted` cleanup, magic-number checks, fallback-on-failure). Adding a third backend requires:
  - A new `cagra_threshold`-style `CQS_FOO_THRESHOLD` env var
  - A new branch in this if/else chain
  - A new persisted-path literal + load-then-rebuild fallback
  - A new structured `tracing::info!(backend = "foo", …)` log line per code path
  - A new `gpu_available()` (or equivalent) gate
  - A new `delete_persisted` cleanup hook
  
  All of this is mechanical; nothing in `index::VectorIndex` lets a backend declare its own threshold / persistence path / availability gate. `VectorIndex` itself is a clean trait (HNSW and CAGRA both implement it), but the **selector** isn't trait-driven. Concrete pressure: the v1.30.0 release ships the `cuda-index` feature split (#956 Phase A) which already names "future Metal / ROCm" backends; each addition will edit this chain again.
- **Suggested fix:** Extend `VectorIndex` with `fn try_open(cqs_dir: &Path, store: &Store<Mode>) -> Option<Box<dyn VectorIndex>>` (where `None` means "I'm not the right backend for this config") and `fn priority(store: &Store) -> i32` (higher wins). Build a `&[&dyn IndexBackend]` slice; selection iterates highest-priority-first and returns the first non-None `try_open`. The `cagra_threshold` lives inside CAGRA's `try_open`, the `gpu_available` lives inside CAGRA's `priority`, the `index.cagra` filename lives inside CAGRA's persistence. HNSW becomes the always-priority-zero fallback. New backends are pure additions to the slice.

#### Tree-sitter query files are wired by `include_str!` per-row — no runtime / startup self-test that registry → query files are consistent
- **Difficulty:** easy
- **Location:** `src/language/queries/*.scm` (109 files), `src/language/languages.rs` (each row uses `chunk_query: include_str!("queries/<lang>.chunks.scm")`), no test that asserts coverage
- **Description:** Each `LanguageDef` row in `languages.rs` literally embeds its query strings via `include_str!("queries/<name>.<kind>.scm")`. A typo in the path is a build error — that part is fine. But:
  - There's no test that iterates `REGISTRY.all()` and asserts every code-carrying language has a non-empty `chunk_query` (an empty .scm file `include_str!`s as `""` and compiles to a no-op tree-sitter query — the language silently emits zero chunks).
  - There's no test that scans `src/language/queries/*.scm` and asserts every file is referenced by at least one language row (orphan .scm files don't break anything but are pure dead weight, and a *partially wired* language — chunks.scm but no calls.scm — won't error at compile time even though the runtime impact is "calls graph never populated for this language").
  - Adding a new query kind (e.g. a hypothetical `docs.scm` for docstring extraction) means editing every `definition_*` function to add an `include_str!` arm, with no compile-time assertion that "every language with a grammar declared a docs.scm".
  
  Verified by walking `language/queries/`: 109 files, no `tests/queries_consistent.rs` or similar self-test. The 54 supported languages × up to 3 query kinds = 162 cells, ~109 files filled — the gap (53 missing-by-design) is invisible without a registry walk.
- **Suggested fix:** Add a single `#[test] fn registry_coverage() { for lang in REGISTRY.all() { if has_grammar(lang) { assert!(!lang.chunk_query.is_empty(), "{lang:?} chunk_query empty"); } } }` in `language/mod.rs`. Catches the silent-empty-query trap with a single test. For new query kinds, layer a `phf`-style `static QUERIES: phf::Map<(&str, QueryKind), &str>` so adding a kind requires editing the map (one place) instead of every `definition_*` function.

#### `ScoringOverrides` adds a knob → must edit 4 sites (struct, defaults, env-var resolver, consumer)
- **Difficulty:** medium
- **Location:** `src/config.rs:153-172` (`ScoringOverrides` struct, 11 `Option<f32>` fields), `src/search/scoring/*.rs` (`ScoringConfig::DEFAULT` consts), `src/store/search.rs:11-42` (`RRF_K_CONFIG_OVERRIDE` + `rrf_k()` env-var-or-config resolver — one of these per knob), wherever the consumer reads it
- **Description:** Each scoring knob (`name_exact`, `parent_boost_cap`, `splade_alpha`, `rrf_k`, …) requires editing four places: (1) the struct, (2) a `pub const DEFAULT_*: f32 = …` sibling, (3) the per-knob env-var resolver function (e.g., `fn rrf_k() -> f32`), (4) the consumer that reads the resolved value. There's no shared resolver. Concrete: adding the queued "type boost factor" knob (`CQS_TYPE_BOOST` is already an env override at `query.rs:449-454`, but it doesn't appear in `ScoringOverrides` so it's environment-only — the `.cqs.toml` `[scoring]` section can't set it). New scoring knobs added by the same author have already drifted out of the `[scoring]` section schema.
- **Suggested fix:** Make `ScoringOverrides` a `HashMap<&'static str, f32>` (or a `serde(flatten)` wrapper) plus a `static SCORING_KNOBS: &[ScoringKnob]` table where each row is `(name, env_var, default, kind)`. Resolver becomes `fn resolve_knob(name: &str) -> f32` driven by the table. Adding a knob = one row. Drops the four-site fan-out to one. Bonus: enables `cqs config show` to dump every knob and its source (env / config / default) without keeping a hand-maintained list.

#### `NoteEntry` has no kind / tag taxonomy — sentiment-only, can't filter for a class
- **Difficulty:** medium
- **Location:** `src/note.rs:41-67` (`NoteEntry { sentiment: f32, ... }`, `Note { sentiment: f32, ... }`), `src/note.rs:79-89` (sentiment → "Warning: " / "Pattern: " prefix is the only kind taxonomy)
- **Description:** Notes carry a single `sentiment: f32` axis (CLAUDE.md documents 5 discrete values: `-1, -0.5, 0, 0.5, 1`). The only "kind" is implicit: sentiment < `-0.3` → "Warning:", sentiment > `+0.3` → "Pattern:", else neutral. There's no formal `kind` field. Adding a separate retrievable note class (e.g. `kind = "todo"` / `"design-decision"` / `"deprecation"` / `"known-bug"` — all of which appear in MEMORY.md and PROJECT_CONTINUITY.md as ad-hoc text patterns the user wants searchable) requires:
  - Schema migration: new `notes.kind TEXT` column
  - `NoteEntry` struct change
  - TOML serialization roundtrip update (`docs/notes.toml` parser)
  - `cqs notes add --kind …` flag
  - Search filter: `cqs notes list --kind …`
  - Sentiment-prefix logic at `note.rs:79-89` becomes kind-prefix
  
  Today the workaround is "encode the kind in the note text" (e.g. `"TODO: rebuild HNSW after migration"`), which is unsearchable as structured data. The schema-version churn for adding a column is the friction — CLAUDE.md "always do things properly" doesn't currently extend to notes shape.
- **Suggested fix:** Add a `kind: Option<String>` field on `NoteEntry` (free-string, not enum, so it's not a 50-language-style registry problem). Add a `notes.kind` SQLite column at the next schema bump. Treat `sentiment_to_prefix` as the legacy fallback for entries with `kind = None`. Filter shape: `cqs notes list --kind todo` becomes a column filter, not a regex on `text`.

#### `LanguageDef::structural_matchers` is per-language `Option<&[(name, fn)]>` — no shared library of common matchers
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:191` (`type StructuralMatcherFn = fn(&str, &str) -> bool;`), `:345` (`pub structural_matchers: Option<&'static [(&'static str, StructuralMatcherFn)]>`), `src/structural.rs:93-98` (lookup site)
- **Description:** Structural matchers exist for one language so far (Rust, by reading `structural_matchers_default_none` at `language/mod.rs:2391-2395` — every other language returns `None`). The shape `Option<&'static [(&str, fn)]>` is fine for one language but doesn't scale: adding the same "swallow exceptions" or "async hot path" patterns for Python/JS/Go means rewriting the same predicate function bodies in each `definition_*` row, with no shared library. The Patterns enum at `structural.rs:44` already encodes language-agnostic ideas (`SwallowedException`, `AsyncIO`, `Mutex`, `Unsafe`) — but the matchers are per-language fn pointers, not a `(Pattern, Language) -> bool` table. The cross-language tests at `structural.rs:319-388` already cover Python/JS/Go/Rust for the *fallback regex matcher*, but the language-tuned matchers only exist for Rust.
- **Suggested fix:** Move structural matchers to a `(Pattern, Language) -> Option<&'static dyn Fn(&str, &str) -> bool>` table (or expand `LanguageDef` rows to declare a `structural_matchers: &[(Pattern, fn)]` slice referencing shared functions). Adding the same pattern across 5 languages becomes 5 table rows pointing at one function, not 5 separate fn definitions per `definition_*`.

#### `find_project_root` markers list is hardcoded — language-grammar registry has no link to "where would a project of this language root live"
- **Difficulty:** easy
- **Location:** `src/cli/config.rs:155-162` (hardcoded 6-marker list with embedded comment "EX-5: These markers are intentionally NOT derived from LanguageDef")
- **Description:** The `markers` array `["Cargo.toml", "package.json", "pyproject.toml", "setup.py", "go.mod", ".git"]` is stable — the inline comment argues the alternative ("project_root_markers on LanguageDef") would dilute the language registry. Granted. But the *current shape* is a fixed array whose authoritative semantics are "these are the 5 languages with a unambiguous canonical project file, plus `.git` as fallback." It is documented as an intentional decision. The pressure point: if cqs ever grows first-class support for Maven (`pom.xml`), Gradle (`build.gradle`/`build.gradle.kts`), .NET solutions (`*.sln`), Bazel (`MODULE.bazel`/`WORKSPACE`), Mix (`mix.exs`), or Cargo workspaces with non-`Cargo.toml` markers — the list grows here, not in `languages.rs`. The decision "where does project-root-detection live" is settled, but the 6-marker list itself is documented as sufficient for now via a comment, not as data the registry actually owns.
- **Suggested fix:** Convert to a `static PROJECT_ROOT_MARKERS: &[(&str, &str)] = &[("Cargo.toml", "rust"), …, (".git", "fallback")]` table at module level. The inline comment becomes the table's doc. Adding Maven (`pom.xml`, "java") is one row, not an `if current.join("pom.xml").exists()` arm. The "is this a workspace root?" Cargo-specific branch at `:167-172` keeps its dedicated logic but stops being in the middle of a `for marker in &markers` loop.

#### Embedder constructor coupling: `define_embedder_presets!` rows generate `pub fn <variant_fn>(&self) -> Self` constructors but no per-preset config knob extension hook
- **Difficulty:** easy
- **Location:** `src/embedder/models.rs:163-300` (`define_embedder_presets!` macro), `:313-374` (the four shipped preset rows)
- **Description:** Each preset row generates a method like `ModelConfig::bge_large(&self) -> Self` that fully populates `ModelConfig { repo, dim, max_seq_len, normalize_embeddings, query_prefix, doc_prefix, approx_download_bytes, pad_id, … }`. Adding a *cross-cutting* preset attribute (e.g. "this preset requires GPU because dim >= 1024", or "this preset's tokenizer expects BOS/EOS even though the default doesn't") means: editing `ModelConfig` (new field), editing the macro `@build_arm` to plumb the field through, editing every preset row to add the attribute. The 4 row × N field matrix is fine at 4 rows, but the preset-additive case still has a real fan-out per attribute. Verified by counting attributes per row: 9 fields per preset × 4 presets = 36 cells; one new field is 4 row-edits + 2 macro-edits.
  
  Lower priority than the others — the macro pattern actually *contains* this fan-out to one file. Flagging because the alternative (a `HashMap<&'static str, Value>` per row) would let adding a new attribute be a no-op for old presets that don't set it.
- **Suggested fix:** Optional. If new preset attributes start landing frequently, extend the macro grammar with an `extras: { gpu_only = true, expects_bos = true }` block per row that maps to a `HashMap<&'static str, ModelAttr>` field on `ModelConfig`. Keeps required fields in the row's main shape, optional/sparse ones in `extras`. Skip if presets are stable.

Summary: 9 extensibility findings. The original v1.29.0 9 findings have largely closed (registry refactor #1114, define_aux_presets, define_chunk_types, define_query_categories, ConfigSection, VisibilityRule extensions, NotesListArgs flatten, approx_download_bytes, Local LLM provider). Residual pressure now concentrates on (1) score-signal pluggability — RRF locked to two lists while SPLADE lives on a parallel α-blend path, (2) batch-side dispatch — `BatchCmd` enum + `is_pipeable` + dispatch matches did not get lifted into the registry alongside `Commands`, (3) provider/index backend selectors that still hand-code the "match on enum kind" factory shape despite having clean trait-objects on the consumer side, (4) implicit registries (project-root markers, structural matchers) that work fine today but document themselves as "intentionally not data" while bearing the structural-data shape.


## Platform Behavior

#### `cqs serve` shutdown_signal handles only Ctrl-C — `systemctl stop` skips graceful drain on Linux
- **Difficulty:** easy
- **Location:** src/serve/mod.rs:253-260
- **Description:** `shutdown_signal()` awaits only `tokio::signal::ctrl_c()`. On Linux when `cqs serve` is run under systemd or any supervisor that issues `SIGTERM` (the default for `systemctl stop`), axum never sees the signal — it keeps serving until systemd escalates to `SIGKILL`. The watch daemon explicitly installs a SIGTERM handler via `libc::signal` (src/cli/watch.rs:132-148), but the serve binary does not. On macOS `launchd` ALSO sends SIGTERM by default. Result: the "press Ctrl-C to stop" banner is the only documented graceful shutdown, and any service-manager wrapper sees forced kills with no graceful_shutdown future polled.
- **Suggested fix:** On `cfg(unix)` race `tokio::signal::ctrl_c()` against `tokio::signal::unix::signal(SignalKind::terminate())` via `tokio::select!`. On Windows also accept `ctrl_break()` and `ctrl_close()`.

#### `EmbeddingCache::default_path` and `QueryCache::default_path` hardcode `~/.cache/cqs/...` on Windows
- **Difficulty:** easy
- **Location:** src/cache.rs:80-84, 1399-1403; src/cli/batch/commands.rs:373-376
- **Description:** Three paths hardcode `dirs::home_dir().join(".cache/cqs/...")`. On Windows this materializes as `C:\Users\X\.cache\cqs\embeddings.db` / `query_cache.db` / `query_log.jsonl`. The native conventions place caches under `%LOCALAPPDATA%\cqs\` (which `dirs::cache_dir()` returns). This is the same defect class as triaged PB-V1.29-8 (HF cache), but PB-V1.29-8 covers HF only; embedding/query caches and the daemon query-log are independent code paths still using the hardcoded layout. Result: Windows users get a hidden `.cache` folder in their home dir that backup tools / antivirus scans don't expect, and dual cqs installs can't share caches with HF tooling that does honor `%LOCALAPPDATA%`.
- **Suggested fix:** Use `dirs::cache_dir().unwrap_or_else(|| dirs::home_dir().join(".cache")).join("cqs")` for all three paths, mirroring `aux_model::hf_cache_dir`'s fallback chain.

#### `dispatch_drift` JSON `file` field is normalized but `dispatch_diff` JSON file fields are not (PB-V1.29-5 partial dupe — additional unfixed sites)
- **Difficulty:** easy
- **Location:** src/suggest.rs:101 (`dead.chunk.file.display().to_string()`), src/store/types.rs:220 (`file_display = file.display().to_string()`)
- **Description:** PB-V1.29-5 covers `dispatch_drift`/`dispatch_diff` in `cli/batch/handlers/misc.rs`. There are at least two more sites that emit Windows backslashes the same way and aren't on the triage list: `suggest::dead_code` returns `Suggestion.file` via `dead.chunk.file.display().to_string()` (rendered into JSON), and `store::types` uses `file_display` for log messages tied to type-edge upserts. Both leak `\` separators into agent-visible output on Windows.
- **Suggested fix:** Replace `.display().to_string()` with `crate::normalize_path(...)` in both sites; add a clippy lint or a doc-tested helper to make the convention discoverable.

#### `serve::open_browser` on Windows passes URL to `explorer.exe` — drops query string / token
- **Difficulty:** medium
- **Location:** src/cli/commands/serve.rs:89-104
- **Description:** `cmd_serve --open` invokes `explorer.exe <url>` on Windows. `explorer.exe` does not interpret a URL argument as a navigation target the way `xdg-open`/`open` do — it tries to open the URL as a path, frequently noops or pops a "Windows can't find" dialog, and on success may strip the `?token=...` query string when handed off to the default browser through DDE. With #1096 auth on by default the token is mandatory; users on Windows lose the one-click experience documented at line 67-82. The other two arms (`xdg-open`, `open`) correctly forward.
- **Suggested fix:** Use `cmd /C start "" "<url>"` on Windows (the empty title is required so `start` parses the URL as the target, not the title). Alternative: use the `opener` crate which already encodes this behavior across platforms.

#### `find_ld_library_dir` splits on `:` — incorrect on Windows / wrong env var name
- **Difficulty:** easy
- **Location:** src/embedder/provider.rs:115-123
- **Description:** `find_ld_library_dir` is `cfg(target_os = "linux")`-gated, so this is currently dormant. But the `ensure_ort_provider_libs` helper has only a Linux arm — there is no equivalent for Windows or macOS. Consequence: when ORT ships on Windows targets the only fallback for finding provider DLLs is the system loader's PATH search, with no logging of where it actually looked. Documenting the gap in the function header and adding a Windows arm that walks `PATH` (split on `;`, looking for `onnxruntime_providers_*.dll`) makes the cross-platform CUDA story explicit instead of "Linux works, others get whatever ORT happens to do."
- **Suggested fix:** Either add `#[cfg(target_os = "windows")]` arms to `ensure_ort_provider_libs` / `find_ort_provider_dir` that walk `PATH` with `;` separator and look for `.dll`, or add a top-level doc comment stating the Windows path resolution is delegated entirely to ORT and confirming the release CI tests catch the failure mode.

#### `ProjectRegistry` doc claims `~/.config/cqs/projects.toml` but `dirs::config_dir()` returns macOS-specific path
- **Difficulty:** easy
- **Location:** src/project.rs:1-3, 176-179
- **Description:** Module-level doc says "Maintains a registry of indexed projects at `~/.config/cqs/projects.toml`". On macOS `dirs::config_dir()` returns `~/Library/Application Support/`, so the actual file lives at `~/Library/Application Support/cqs/projects.toml`. On Windows it lives at `%APPDATA%\cqs\projects.toml`. macOS and Windows users following the doc to find / edit the registry will look in the wrong place. The path is constructed correctly via `dirs::config_dir()` — only the doc is lying. (Per memory note `feedback_docs_lying_is_p1.md`: docs lying about a path users will run `ls`/`open` on is a P1 correctness bug, not "just docs".)
- **Suggested fix:** Update both the module doc and the `load`/`save` doc comments to enumerate the three platform paths, e.g. "Linux: `~/.config/cqs/`, macOS: `~/Library/Application Support/cqs/`, Windows: `%APPDATA%\cqs\`". Mention `dirs::config_dir()` as the source of truth.

#### `index.lock` `flock` is advisory on Linux but mandatory on Windows — different failure modes for cross-tooling
- **Difficulty:** medium
- **Location:** src/cli/files.rs:120-213
- **Description:** `acquire_index_lock` uses `std::fs::File::try_lock` (introduced in Rust 1.89, MSRV 1.93+). On Linux this maps to `flock(LOCK_EX|LOCK_NB)` — purely advisory; non-cqs writers (e.g. an editor saving the DB after a crash, an external SQLite tool) will silently corrupt the index. On Windows it maps to `LockFileEx` which is mandatory and prevents *any* other process from opening the file with a conflicting share mode — including a benign `sqlite3.exe` or backup tool that opens with `FILE_SHARE_READ` but no `FILE_SHARE_WRITE`. The function-level doc covers the WSL `/mnt/c` case but does not document the Linux-vs-Windows mandatory-vs-advisory difference, and `is_wsl_drvfs_path` is not consulted before deciding to trust the lock. Result: same code, two very different concurrency contracts that callers cannot distinguish at runtime.
- **Suggested fix:** Add a `tracing::warn!` once at startup on Windows noting that the lock is mandatory and that opening `index.db` from another process while the lock is held will fail with sharing violation. Document the Linux/Windows split in the `acquire_index_lock` doc-comment alongside the existing WSL paragraph.

#### `is_wsl_drvfs_path` only matches single-letter drive mounts — misses `wsl.localhost` and explicit-uppercase mounts
- **Difficulty:** easy
- **Location:** src/config.rs:92-101
- **Description:** The pattern requires exactly `/mnt/<lowercase letter>/`. WSL2 also exposes Windows drives under `//wsl.localhost/<distro>/mnt/c/...` and (when accessed from the Windows side) `\\wsl$\<distro>\mnt\c\...`. Additionally, `wsl.conf` `automount.options=case=force` allows uppercase drive letters. The `cli/watch.rs::create_watcher` code at line 1483-1489 already explicitly checks for `//wsl` and `is_under_wsl_automount`, but the *shared* helper used by config / project / hnsw doesn't, so those three sites still treat WSL DrvFS paths reached via UNC as native Linux. They'll then warn about world-readable perms (line 497-503) on a path where Linux-side perms are meaningless.
- **Suggested fix:** Extend `is_wsl_drvfs_path` to also match `//wsl.localhost/`, `//wsl$/`, and uppercase drive letters. Test via `daemon_translate` style tests that fix `WSL_DISTRO_NAME`.

#### `git_file = rel_file.replace('\\', "/")` only normalizes one direction — Windows-origin chunk IDs slip through
- **Difficulty:** easy
- **Location:** src/cli/commands/io/blame.rs:113-115
- **Description:** Comment says "PB-3: Windows compat" and the `replace('\\', "/")` covers the common case where chunk.file came from `cqs::normalize_path` (forward-slash). But `chunk.file` can also be a `PathBuf` whose components include the verbatim `\\?\` prefix when the chunk was inserted from a path that bypassed `normalize_path` (e.g. a partial / pre-DS2-1 fix path). The replace would emit `//?/C:/Projects/...` to git, which git rejects with "ambiguous argument". A symmetric strip via `crate::normalize_slashes(&rel_file)` (which calls `strip_windows_verbatim_prefix` first) is what the rest of the codebase uses.
- **Suggested fix:** `let git_file = crate::normalize_slashes(&rel_file);` — covers both backslash conversion and `\\?\` strip in one call, matching the convention established in src/lib.rs:420.

#### `daemon_socket_path` falls back to `std::env::temp_dir()` on `XDG_RUNTIME_DIR` unset — different parent-dir trust on macOS
- **Difficulty:** medium
- **Location:** src/daemon_translate.rs:179-188
- **Description:** Linux desktops set `XDG_RUNTIME_DIR=/run/user/<uid>` (mode 0700, owned by the user). When unset (headless servers, container minimal images, macOS where `XDG_RUNTIME_DIR` is not standard), the code falls back to `std::env::temp_dir()` — `/tmp` on Linux, `/var/folders/.../T` on macOS. macOS's `/var/folders/...` is per-user-and-bootstrap and reasonably private (mode 0700), but Linux `/tmp` is mode 1777. The umask wrap at watch.rs:1626 narrows the bind window, and the explicit `chmod 0o600` at watch.rs:1637 is the actual access gate. Still, the silent fallback hides a meaningful trust boundary: on a Linux multi-user system without `XDG_RUNTIME_DIR`, the socket lives in a directory where another local user can `unlink` it (or `mkfifo` over its name during the bind race). The doc comment notes the issue (line 1615-1622 SEC-D.6) but `daemon_socket_path` itself doesn't log when the fallback fires.
- **Suggested fix:** When `XDG_RUNTIME_DIR` is unset on Linux, log `tracing::info!("XDG_RUNTIME_DIR unset — daemon socket falls back to temp_dir; consider setting XDG_RUNTIME_DIR=/run/user/$(id -u)")` once per process. On macOS the fallback is fine — gate the warning on `cfg(target_os = "linux")` so it's only emitted where `/tmp` is actually shared.

#### NTFS mtime resolution is 100ns but Windows-side editors update mtime at 2s granularity in some configurations — `prune_last_indexed_mtime` watermark too tight
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:551-560 (and wider mtime-keyed change-detection in `should_reindex`)
- **Description:** `last_indexed_mtime` is a `HashMap<PathBuf, SystemTime>` and the watcher decides "skip unchanged mtime" via exact `SystemTime` equality. NTFS file timestamp resolution is documented as 100ns, but FAT32 (still mounted on USB sticks, recovery partitions, and some `/mnt/<letter>/` paths) has 2-second resolution on writes. WSL DrvFS exposes the underlying NTFS mtime, but Windows-side `notepad.exe` saves can lose sub-second precision when the underlying filesystem is FAT32. Two saves within 2s on a FAT32 mount will therefore collide on the same mtime and the watch loop will skip the second — a real correctness gap on `/mnt/d` if D: is a USB stick. There's a 1s debounce auto-bump on WSL DrvFS (line 1495-1500) that masks most of this, but the equality check against the cached mtime doesn't.
- **Suggested fix:** When `is_wsl_drvfs_path(file)` is true, treat mtime equality with `<` instead of `==` over a 2-second buckets, OR fall back to content-hash comparison (already computed for parser ingest) when mtime equality is suspicious. Document the FAT32 caveat in the function header.

#### `serve::enforce_host_allowlist` accepts missing Host header — dev-only ergonomic leaks into production
- **Difficulty:** easy
- **Location:** src/serve/mod.rs:230-251
- **Description:** Comment at lines 230-233 explains the bypass: "A missing `Host:` header passes through — HTTP/1.1 requires one and hyper always provides one on real traffic, but unit tests built via `Request::builder()` without a `.uri()` that includes a host don't get one synthesized, and we'd rather not break that ergonomic." That's a unit-test ergonomic baked into the production middleware. A non-browser HTTP/1.0 client (or HTTP/2 client that uses `:authority` but routes through a proxy that strips it) reaches the handler with no Host header, bypassing the DNS-rebinding allowlist that SEC-1 closes for browser traffic. The auth token (#1096) covers this in default config, but `--no-auth` exposes it.
- **Suggested fix:** In production code reject missing Host with 400, and in `tests.rs` add `Host: localhost` to the `Request::builder()` fixtures (cheap one-liner). Or gate the bypass on `cfg(test)`.


## Security

> All eight previously-listed findings (DNS-rebinding host-allowlist,
> XSS via `body.slice()` → `innerHTML`, IN-list / SQL-LIMIT DoS, LIKE-wildcard
> injection in `file` and `tests-cover`, `cmd_serve` / `xdg-open` URL hazard,
> unauthenticated serve) are already in `docs/audit-triage-v1.30.0.md` as
> SEC-1 … SEC-8. SEC-7 (no-auth) is fixed by #1118. The findings below are
> NEW exposures introduced by the v1.30.0 surface (auth token plumbing,
> tracing, host-header edge cases) that the prior pass did not catch.

#### Auth token leaked into tracing spans by `TraceLayer::new_for_http()`
- **Difficulty:** easy
- **Location:** `src/serve/mod.rs:195` (`TraceLayer::new_for_http()`), interacts with `src/serve/auth.rs:194-232` (`enforce_auth`) and `src/cli/commands/serve.rs:73-76` (token-bearing URL passed to `xdg-open`)
- **Description:** `auth.rs` is meticulous about not leaking the token: constant-time compare, `HttpOnly; SameSite=Strict` cookie, redirect that strips `?token=` from the address bar, and an explicit comment at `auth.rs:226-228` claiming "Tracing happens once per launch (banner) and never per-request — auditors can grep for the count of 401s without seeing tokens." That contract is silently broken by `TraceLayer::new_for_http()`, which is wired as the outermost layer (`mod.rs:195`) and thus runs **before** `enforce_auth`. The default `MakeSpan` of `tower-http` 0.6's TraceLayer records `http.uri` (full path + query string) on every request span. So the very first browser navigation `GET /?token=<43 chars>` lands the token in the span at DEBUG, and any operator running with `--log-level=debug`, `RUST_LOG=tower_http=debug`, or `RUST_LOG=info,tower_http=debug` (commonly recommended for diagnosing serve issues) writes the token to journald / log files / pipes that long-outlive the per-launch token. The same applies to `xdg-open`: the URL passed to `Command::new("xdg-open")` (`serve.rs:97`) typically ends up in shell history, in xdg-open's own logging, and in the browser's session-restore database — but those are user-local and arguably the intended cost of `--open`. The TraceLayer leak is the surprise: it converts a defensible "token visible to your local browser" model into "token visible to anyone who can read your logs." Coupled with the loud-warning banner that prints the token to stdout (`mod.rs:113-116`), captured stdout in CI / systemd `StandardOutput=` ends up in journald too. Severity: medium — token rotates per-launch, so leaked tokens are bounded by uptime, but a 30-day journal retention easily covers a long-running daemon.
- **Suggested fix:** Customize TraceLayer's `MakeSpan` to scrub the query string before recording: `TraceLayer::new_for_http().make_span_with(|req: &Request<_>| { let path = req.uri().path(); tracing::info_span!("http_request", method = %req.method(), path) })`. Alternatively, redirect-then-trace by reordering the layers so the auth redirect runs before TraceLayer sees the URI — but that swaps the ordering invariant the comment relies on, and breaks the "401 responses still get traced" guarantee. The MakeSpan override is the surgical fix.

#### `enforce_host_allowlist` lets a missing-`Host:` request bypass DNS-rebinding protection
- **Difficulty:** easy
- **Location:** `src/serve/mod.rs:234-251` (specifically the `None => Ok(next.run(req).await)` branch at line 240)
- **Description:** The allowlist middleware passes through when `Host:` is absent: `match req.headers().get(header::HOST) { None => Ok(next.run(req).await), … }`. The doc-comment justification at `mod.rs:230-233` is "HTTP/1.1 requires one and hyper always provides one on real traffic, but unit tests built via `Request::builder()` without a `.uri()` that includes a host don't get one synthesized, and we'd rather not break that ergonomic." This privileges test ergonomics over runtime safety. (1) HTTP/1.0 does not require Host — a hand-crafted `GET /api/chunk/<id> HTTP/1.0\r\n\r\n` from `nc 127.0.0.1 8080` reaches the handler with zero Host header and zero allowlist check. With `--no-auth` the entire DNS-rebinding protection is bypassed for that request shape. With auth, the auth layer still gates it (so the immediate damage is bounded), but the v1 spec explicitly relies on the host allowlist as defense-in-depth, and this hole quietly disables it. (2) Browsers in error states (eg. `XMLHttpRequest` against a non-standard scheme handler) have historically dropped the Host header. (3) An axum middleware quirk: the middleware sees the raw header map; some HTTP libraries emit `Host:` with empty value (`Host: \r\n`), which `.to_str().unwrap_or("")` collapses to `""`, but `to_str()` on an empty `HeaderValue` actually returns `Ok("")`, which then fails the allowlist check (good). But a request with a non-ASCII byte in Host (`Host: \xff`) fails `.to_str()` → `unwrap_or("")` → empty → rejected. So the only real escape is the literal-missing case at line 240 — which is the most exploitable variant. The "don't break test ergonomics" excuse is also questionable: tests inside the crate can call `enforce_host_allowlist_with_default_allowed` or stamp a Host header in fixtures (`Request::builder().header(HOST, "127.0.0.1:8080").uri("/foo").body(...)`), which is a one-line change in the existing test file.
- **Suggested fix:** Reject missing-Host as a malformed request: replace `None => Ok(next.run(req).await)` with `None => Err((StatusCode::BAD_REQUEST, "missing Host header"))`. Update tests in `src/serve/tests.rs` to set a Host header explicitly — those tests are already exercising the real auth + host paths; adding a header is consistent with what hyper does on real traffic and removes a real bypass.

#### `--bind 0.0.0.0` allows LAN attackers to hit the server but the host-allowlist breaks the legitimate browser path — operator footgun pushes them to `--no-auth`
- **Difficulty:** medium
- **Location:** `src/serve/mod.rs:207-218` (`allowed_host_set`), `src/cli/commands/serve.rs:27-37` (warning gating)
- **Description:** With `cqs serve --bind 0.0.0.0`, the allowlist ends up containing `{localhost, localhost:8080, 127.0.0.1, 127.0.0.1:8080, [::1], [::1]:8080, 0.0.0.0:8080, 0.0.0.0}`. A LAN client navigating to `http://192.168.1.5:8080/` sends `Host: 192.168.1.5:8080`, which is NOT in the allowlist → 400 "disallowed Host header". So `--bind 0.0.0.0` is effectively unusable from any non-loopback peer despite the operator having explicitly opted into LAN binding. The expected operator response is to add `--no-auth` (which doesn't help — the host check is in front of auth) or to dig into the source and discover that `--bind <actual-IP>` is the real path. Worse: the only "bind-without-auth-is-loud" warning at `serve.rs:27-37` triggers only when `bind != "127.0.0.1" && bind != "localhost" && bind != "::1"`, but treats `0.0.0.0` as non-localhost-y and fires the warning — pushing operators toward `--no-auth` to "make it work." The result is a UX that punishes the secure path (auth on + bind 0.0.0.0 → 400) and rewards the insecure path (`--no-auth` → works for LAN). Threat model: an operator legitimately needs `cqs serve` reachable from a teammate's laptop on the same VLAN, sees auth-on returns 400 on the laptop, falls back to `--no-auth`, exposes the entire indexed corpus.
- **Suggested fix:** When `bind == 0.0.0.0` (or `::`), the host allowlist should accept any RFC-1918 / loopback / link-local Host plus the bind port. Cleaner: when binding to a wildcard, populate the allowlist from `nix::ifaddrs` / `if_addrs` so the actual interface IPs are accepted. Even simpler: when `bind_addr.ip().is_unspecified()`, skip the host-header check entirely (auth is the only gate) and emit a one-line stderr warning at startup explaining that the host-header DNS-rebinding protection is disabled in wildcard mode.

#### Token printed to stdout is captured by systemd `journald` — long-lived servers leak their token to a 30-day-retention log
- **Difficulty:** easy
- **Location:** `src/serve/mod.rs:111-117` (`println!("cqs serve listening on http://{actual}/?token={}")`)
- **Description:** The auth comment at `auth.rs:226-228` claims tokens never reach the structured log stream. The startup banner does write the token to stdout, intentionally, so a copy-paste from the operator's terminal lands in an authenticated session. But a long-running `cqs serve` deployed under systemd (`StandardOutput=journal`, the default) or via container (`docker run` capturing stdout into the container log driver) immediately persists the token banner into a log retention store the user never thinks about. The user's mental model is "the token only shows up in my terminal scrollback"; the reality is "anyone with `journalctl -u cqs-serve` (or the container log endpoint) can read the token until log rotation." Combined with that the token doesn't rotate during a long-lived launch, a 30-day-old banner is a 30-day-valid credential. Severity: low without a public binding (loopback only); medium with `--bind` to a LAN IP and any user on the box that has `journalctl -u`. Note: systemd `cqs-watch.service` already runs `cqs watch --serve`, not the HTTP server, but a future `cqs-serve.service` (or anyone wrapping `cqs serve` in systemd) inherits this trap silently.
- **Suggested fix:** Two complementary mitigations: (1) print the token banner only to a controlling-tty-detected stderr (`atty::is(Stream::Stderr)`), or to a write-only file (`stdout` redirected to `/dev/null` when not interactive), so it doesn't leak to journald by default; (2) document in the listening banner that the token is per-launch and the user should rotate by restarting after disclosure. Optional: write the token to a `0o600`-permissioned file in `$XDG_RUNTIME_DIR` and print only the file path, with a `cqs serve token` subcommand that re-emits it on demand — same pattern Jupyter Lab uses since 2018.

#### Auth state ignored by `quiet=true` callers — internal API lets tests build a router that omits auth without auditing
- **Difficulty:** easy
- **Location:** `src/serve/mod.rs:78-83` (`run_server` signature accepts `auth: Option<AuthToken>`), `src/serve/mod.rs:154-178` (`build_router`)
- **Description:** `run_server` and `build_router` both take `auth: Option<AuthToken>`. Passing `None` silently disables auth — there is no `AuthRequired` enum, no `Either<NoAuth, Auth>` type. The callsite in `cmd_serve` (`serve.rs:61-65`) correctly defaults to `Some(random())` and only opts into `None` on `--no-auth`, but the type signature does not constrain future internal callers (e.g. an embedded `cqs serve` for a doctor-style smoke test, an alternate CLI surface, or a feature-gated dev mode) from passing `None` and quietly disabling auth. There is no compile-time gate or runtime warning when the auth-disabled path is taken; the only signal is the `eprintln!` at `mod.rs:120-123`, which is only emitted when `quiet == false`. So a future `run_server(store, addr, true, None)` ships a fully open server with zero output — a regression that nothing in the type system catches. The pattern matters because the equivalent invariant in `cqs ref add` is enforced via type and validate-by-construction, but here the "default secure" property lives only in convention.
- **Suggested fix:** Replace `Option<AuthToken>` with a `enum AuthMode { Required(AuthToken), Disabled { ack: NoAuthAcknowledgement } }` where `NoAuthAcknowledgement` is a `pub` zero-sized type only constructable inside `cmd_serve` after the `--no-auth` flag is parsed. Future internal callers physically cannot instantiate the disabled variant without intentionally importing the proof type. Alternative: keep `Option<AuthToken>` but add a `tracing::error!` (not warn) in the `None` branch of `build_router` so any test or future caller that disables auth shows up loudly in logs.

#### `AuthToken::from_string` is `#[cfg(test)]` but exposed via `pub(crate)` — a future code path can construct a token with CR/LF and crash the server
- **Difficulty:** hard
- **Location:** `src/serve/auth.rs:75-78` (`from_string`), `src/serve/auth.rs:218-220` (`HeaderValue::from_str(&cookie).expect(…)`)
- **Description:** `AuthToken::from_string` is gated on `#[cfg(test)]` so it's not callable from production code today. But the contract of `AuthToken` ("alphabet is URL-safe base64; HeaderValue construction infallible" — `auth.rs:215-218`) is enforced only by the docstring on `random()`, not by the type. If a future refactor lifts the cfg-gate (e.g. for a "fixed token from env var" feature, which is a reasonable ask for scripted automation that wants stable tokens across launches), an attacker who controls that env var can write `CR/LF` into the token and crash the worker on every redirect — `HeaderValue::from_str` rejects bytes that aren't valid HTTP header bytes, and the `.expect(…)` at line 218 panics. The crash is per-request (axum catches handler panics into a 500), so it's not a server-killer, but it's a guaranteed 500 for any request that hits the query-param redirect path. More subtly: a token containing `;` or `,` would split the cookie syntax and let an attacker who knows the format inject a second cookie pair, e.g. token = `validbase64; admin=true; ` — then on the redirect `Set-Cookie: cqs_token=validbase64; admin=true; ; Path=/; HttpOnly; SameSite=Strict`. The cookie would be malformed and most browsers would reject the whole header, but the principle of "only ever construct a HeaderValue from validated bytes" is broken.
- **Suggested fix:** Make the alphabet a structural property of `AuthToken`: have `random()` return `AuthToken(String)` where the wrapped string is verified at construction time to be `[A-Za-z0-9_-]+` and panic on construction (not on use) if it's not. For `from_string`, do the same validation; tests that want to build a deterministic token still get one, but they cannot smuggle invalid bytes in. With the alphabet enforced at the type level, the `.expect(…)` at `auth.rs:218` becomes a real safety proof rather than a fragile docstring.

#### `cqs serve` request body is unbounded — POST or chunked uploads can exhaust memory; no `RequestBodyLimitLayer`
- **Difficulty:** easy
- **Location:** `src/serve/mod.rs:154-196` (`build_router` — no body-limit layer in the chain)
- **Description:** The router declares only `GET` routes (`routes.rs:160-168`), and axum returns 405 for non-GET on those routes. But axum still BUFFERS the request body before dispatching — and for a route declared as `get(...)`, axum 0.7 reads the body into memory before deciding to 405. There's no `tower_http::limit::RequestBodyLimitLayer` in the chain. So an attacker (after passing host + auth) can `POST /api/stats` with `Content-Length: 99999999999` and `Transfer-Encoding: chunked`, and axum will buffer up to the OS-level read limit before responding 405. With the auth layer running *outside* the body read, the 405 path can be reached by any authenticated client. Memory pressure proportional to body size, repeated forever. Since auth is in front, an external attacker without a token can't do this — but a multi-user box where one user has the token (per the `journald` finding above) can. Also: for the GET-only routes that DO expect query strings (e.g. `/api/search?q=…`), there is no `Content-Length` cap on the body that some clients might send anyway. axum forgivingly ignores the body for GET, but not before reading it into memory.
- **Suggested fix:** Add `RequestBodyLimitLayer::new(64 * 1024)` to the layer chain (sits inside CompressionLayer, outside auth so the limit applies to even rejected requests). 64 KiB is plenty for query strings and cookies; legitimate clients never approach it. Tower-http already has the layer; no new dep.

#### `Path=/` cookie scope plus 127.0.0.1 sharing — multiple `cqs serve` instances on the same host share cookies and can hijack each other
- **Difficulty:** hard
- **Location:** `src/serve/auth.rs:211-214` (`Set-Cookie: cqs_token={token}; Path=/; HttpOnly; SameSite=Strict`)
- **Description:** Two `cqs serve` instances on the same machine, on different ports (e.g. project A on 8080, project B on 8081), both share the same cookie origin from the browser's perspective: cookies on `localhost`/`127.0.0.1` are scoped by host but NOT by port. Both servers set `cqs_token=...` with `Path=/`. A user authenticates to project A, gets cookie `cqs_token=A_TOKEN`. They later visit `http://127.0.0.1:8081/?token=B_TOKEN` for project B. The redirect sets `cqs_token=B_TOKEN`, OVERWRITING the project A cookie in the browser jar (same name + host + path). Now the user's tab on project A is silently logged out and (worse) any link they click through to project A sends `cqs_token=B_TOKEN`, which fails ct_eq and 401s. Browsers do scope cookies by `Path=/` but not by port, so this is a fundamental browser-cookie limitation, not a server bug. It's still a real footgun for any user running two `cqs serve` instances, plus a downgrade vector: an attacker who can make the user visit their own attacker-controlled `cqs serve` on a port they control — say, by phishing the user into running `cqs serve --bind 127.0.0.1 --port 8081` against a malicious project — replaces the legitimate project's cookie. Combined with SameSite=Strict-bypass via top-level navigation (the user *is* navigating top-level), the attacker can drop any cookie they like into the victim's localhost cookie jar. Mitigation note: the comment at `auth.rs:42-47` ("Pinned to `cqs_token` so a future second-server instance running in another tab on the same host uses a different cookie path") explicitly acknowledges the issue but does not actually solve it.
- **Suggested fix:** Use `__Host-` cookie prefix (`__Host-cqs_token`) per RFC 6265bis — requires `Path=/`, `Secure`, and forbids `Domain`. The `Secure` requirement means the cookie won't stick on plain HTTP, which is a problem for localhost. Alternative: include the bind port in the cookie name (`cqs_token_8080`) so two instances don't collide. Best alternative: set `Path=/api/__cqs_<port>/` and have the auth layer rewrite the request path — heavy. Pragmatic: set the cookie name from a hash of `(bind_addr, launch_time)` so two instances don't collide and a new launch invalidates the old cookie automatically. The knob count rises but the multi-instance footgun goes away.

## One-line summary

Eight new findings on top of SEC-1 … SEC-8 (already triaged). All are in the v1.30.0 auth surface (#1118): TraceLayer leaks the token via URI logging despite the careful HttpOnly/redirect handoff; missing-Host requests bypass DNS-rebinding protection; `--bind 0.0.0.0` is broken in a way that pushes operators to `--no-auth`; the launch banner persists tokens to journald for log-retention lifetime; `Option<AuthToken>` permits silently building a no-auth router; `from_string` is cfg-gated but the alphabet invariant relies on a docstring rather than a type; no request-body-limit layer; cookies aren't port-scoped on localhost so two instances stomp each other.


## Data Safety

#### Migration restore_from_backup overwrites live DB while pool holds open connections
- **Difficulty:** medium
- **Location:** `src/store/backup.rs:171-180` (called from `src/store/migrations.rs:106-128`)
- **Description:** When a migration step fails, `migrate()` calls `restore_from_backup(db_path, bak)` which invokes `copy_triplet` → `copy_file_atomic` → `crate::fs::atomic_replace` to rename the backup over the live `db_path`. But the SQLite `pool` from `migrate()`'s caller is STILL holding open file descriptors against the old inode. After the atomic_replace, `db_path`'s inode is the backup's; existing connections in the pool see the *unlinked old* inode, while any subsequent open via the same path sees the new one. Worse, the loop then copies `-wal` and `-shm` over what the pool's open connections believe are *their* sidecars — but those copies land on the new inode while the pool's mapped sidecars (mmap'd) belong to the old inode. The result is silent two-state divergence: in-process queries can read stale rows from the old WAL while readers from new processes see the restored DB. SQLite's documented restore pattern requires closing all connections first (or using the online backup API). Tests in `src/store/migrations.rs:tests` use a fresh temp DB and a single transaction, so the divergence never surfaces. In production a daemon already has a long-lived `Store::open(...)` that owns the pool; if a fresh CLI invocation triggers a migration and fails on a buggy step, the daemon then serves queries from a phantom inode.
- **Suggested fix:** Drop / close the pool before calling `restore_from_backup` (e.g. take `pool` by value, `.close().await`, then run the restore, then re-open). At minimum, `PRAGMA wal_checkpoint(TRUNCATE)` and force a connection close on every pooled connection before the file replace; document that callers must hold no other Store handles to `db_path` during the restore.

#### `stream_summary_writer` bypasses `WRITE_LOCK` — concurrent writer can collide with reindex
- **Status:** RESOLVED in #1126 PR (write-coalescing queue + `Store::flush_pending_summaries` API).
- **Difficulty:** medium
- **Location:** `src/store/chunks/crud.rs:504-545` (pre-fix); now `src/store/summary_queue.rs` + `src/store/chunks/crud.rs`.
- **Description:** Every other write path in `Store<ReadWrite>` acquires the in-process `WRITE_LOCK` mutex via `begin_write()`. `stream_summary_writer` instead executes `INSERT OR IGNORE INTO llm_summaries ...` directly against `&self.pool` from a captured `Arc<SqlitePool>` callback that fires from LLM provider streaming threads. Two concrete races:
  1. A background `cqs llm summary` (or `--improve-docs`) batch is streaming results while the user runs `cqs index` on the same project. The streaming write and `upsert_chunks_and_calls` both contend for SQLite's exclusive lock without the in-process serialization that `WRITE_LOCK` provides. With WAL mode and `busy_timeout=5s` either side can SQLITE_BUSY and abort.
  2. Multiple in-flight LLM streams (Haiku + doc-comments + hyde concurrently) each fire INSERT OR IGNORE per item; without `begin_write()`, sqlx auto-wraps each statement in its own implicit transaction and commits it individually. That's 1 fsync per row instead of one per batch — already a perf bug, but the data-safety angle is that if the process is killed mid-stream, the partial writes are visible to readers immediately (no transactional grouping).
- **Resolution:** Added a per-`Store<ReadWrite>` `PendingSummaryQueue` (`src/store/summary_queue.rs`). The streaming callback now enqueues into the queue; the queue flushes synchronously when either the row threshold (default 64) or the time interval (default 200 ms) is crossed, OR when callers (LLM passes, `cmd_index`) call `Store::flush_pending_summaries` explicitly. Flushes drain every queued row inside one `WRITE_LOCK`-guarded transaction with a single multi-row `INSERT OR IGNORE`, restoring the invariant that all `index.db` writes serialize through the same in-process mutex. See `docs/design/1126-1127-lock-topology.md` for the full design rationale.

#### Chunk content change does not invalidate `umap_x` / `umap_y` — cluster view serves stale positions
- **Difficulty:** easy
- **Location:** `src/store/chunks/async_helpers.rs:339-362` (UPSERT) + `src/cli/commands/index/umap.rs:38-228`
- **Description:** v22 added nullable `umap_x`/`umap_y` columns; `cqs index --umap` runs a UMAP projection over current embeddings and writes coords back via `update_umap_coords_batch`. The `cqs serve` cluster view at `src/serve/data.rs:920-1003` reads these coords directly. Problem: the chunk UPSERT in `batch_insert_chunks` lists every column it overwrites on conflict — embedding, embedding_base, parser_version, etc. — but it does NOT touch `umap_x` or `umap_y`. So when content changes (`WHERE chunks.content_hash != excluded.content_hash`), the embedding gets refreshed but the UMAP coords stay frozen. The cluster view then displays the chunk at a position computed from the old embedding, potentially landing it in a wrong cluster (e.g. function rewritten end-to-end keeps its old coords until the user remembers to run `cqs index --umap`). Worse, `cqs serve` has no way to surface the staleness — the `umap_x IS NOT NULL` filter only catches the all-NULL case. Memory rule: invalidation counters / staleness must be enforced at the schema layer; relying on the user to re-run `--umap` is exactly the call-site instrumentation pattern the project explicitly rejected.
- **Suggested fix:** Add `umap_x = NULL, umap_y = NULL` to the ON CONFLICT UPDATE clause when content_hash differs. The cluster view's `IS NOT NULL` filter then correctly reports "needs reprojection." Optionally add a metadata `umap_generation` counter that bumps on chunks delete/insert (mirroring `splade_generation`) so a future `cqs serve` warning can fire when generation > umap_generation_at_projection.

#### `slot_remove` race: read active_slot → list_slots → remove_dir_all is TOCTOU on concurrent promote
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/slot.rs:299-350`
- **Description:** `slot_remove` reads `active_slot` (line 312), reads `list_slots` (line 313), then `fs::remove_dir_all(&dir)` (line 335). Between any two of those steps a concurrent `cqs slot promote` (or another `slot remove`) can mutate `.cqs/active_slot`. Trigger sequence:
  1. Process A enters `slot_remove("foo", force=false)`. `foo` is NOT the active slot per its read at line 312 — read says active is "default".
  2. Process B runs `cqs slot promote foo`, atomically rewriting `active_slot` to "foo".
  3. Process A proceeds past the active-slot guard (line 316) since its snapshot says active="default", and runs `fs::remove_dir_all(slot_dir(... "foo"))`.
  4. The system is now pointing `active_slot` at a slot directory that no longer exists. Subsequent commands hit `SlotError::Empty("foo")` until the user manually runs `cqs slot promote default`.
  Same race with two concurrent `slot_remove` calls competing to remove the only remaining slot. There is no file lock around `.cqs/slots/` lifecycle — `slot_dir` operations rely on plain filesystem semantics.
- **Suggested fix:** Take an exclusive lock on a `.cqs/slots.lock` file at the top of every `slot_*` operation (mirroring the `notes.toml.lock` pattern in `src/note.rs:209-228`). Hold it for the entire read-validate-mutate sequence. Same pattern in `slot_promote` and `slot_create`.

#### Slot legacy migration moves live `index.db-wal` / `-shm` instead of checkpointing first
- **Difficulty:** medium
- **Location:** `src/slot/mod.rs:511-624`
- **Description:** `migrate_legacy_index_to_default_slot` runs idempotently on every `Store::open`. If a legacy `.cqs/index.db` is present and `.cqs/slots/` is absent, it moves `index.db`, `index.db-wal`, and `index.db-shm` (alongside HNSW + SPLADE files) into `.cqs/slots/default/`. But `index.db-wal` may contain uncommitted pages from a *prior* daemon run that crashed before checkpointing. SQLite recovers WAL-mode databases by replaying the WAL on next open — but only if the WAL sits next to the DB on the same inode lineage. After the migration moves all three files atomically the WAL replay still works, but if the moves are NON-atomic (cross-device fallback at `move_file:631-637` does `fs::copy + fs::remove_file`), an interrupt between copying `index.db` and copying `index.db-wal` leaves the new `slots/default/index.db` without its WAL. SQLite reopens that DB and silently truncates / discards uncommitted WAL pages — data loss for any writes that were in flight when the crash occurred.
- **Suggested fix:** Before the migration, open the legacy `index.db` once with `PRAGMA wal_checkpoint(TRUNCATE)` so the WAL is drained into the main file and the sidecars are empty/absent. Only then move files. This makes the multi-file move's failure modes recoverable: the worst case is a partially-moved index.db, which on restart is detected by the legacy path still existing.

#### `model_fingerprint` fallback uses Unix timestamp — every restart misses cache, breaks cross-slot copy invariant
- **Difficulty:** medium
- **Location:** `src/embedder/mod.rs:435-465`
- **Description:** `model_fingerprint()` is the cache key for the cross-slot embeddings cache (per memory: "cross-slot copy by content_hash before A/B reindex saves ~$1-5 in API spend"). The fingerprint is normally a blake3 of the ONNX file. But four error branches fall back to `format!("{}:{}", self.model_config.repo, ts)` where `ts = SystemTime::now()`. Every process restart that hits a fallback writes cache rows under a NEW timestamp, and subsequent reads with a different timestamp miss them. Worse, the cache `(content_hash, model_fingerprint)` PRIMARY KEY treats different timestamps as different models — cross-slot copy by `content_hash` would silently match WRONG embeddings if two slots happened to use the timestamp fallback at different moments (the timestamp fallback for both slots gives different fingerprints, so the cross-slot copy queries would miss the cache entirely; even more concerning is that the fingerprint is used as cache identity across writes, so every fallback embedding becomes orphan, accumulating). The fingerprint is also used in PRAGMA-style metadata records — a stale fingerprint stored in `metadata.embedding_model_fp` will never match a cache write made under the current timestamp.
- **Suggested fix:** Make the fallback deterministic: `format!("{}:fallback:size={}", repo, file_size_or_zero)` with NO time component. A fallback fingerprint that's stable across restarts is strictly better than a "unique" fallback that fragments the cache. Log loudly at `warn!` so users notice the missing-file path was taken; failing the embedder open is a defensible alternative.

#### `write_slot_model` and `write_active_slot` skip parent-dir fsync after rename
- **Difficulty:** easy
- **Location:** `src/slot/mod.rs:237-277` (`write_slot_model`), `src/slot/mod.rs:363-406` (`write_active_slot`)
- **Description:** Both functions write to a temp file, fsync the file (only `write_active_slot` does this — `write_slot_model` only `f.sync_all()`s; both do), then `fs::rename`. Neither fsyncs the parent directory after the rename. On power loss between rename and the next inode/dirent flush, the rename can be lost (returning the user to the previous active_slot or a missing slot.toml). `src/note.rs:304` and `src/audit.rs:149` correctly use `crate::fs::atomic_replace`, which fsyncs both file AND parent dir. The slot writers are the odd ones out — same code shape, weaker guarantees. For `active_slot`, this matters because a `cqs slot promote foo` followed by a power cut can crash the system into seeing the OLD slot active even though `cqs slot promote` returned success — the user re-runs commands assuming the new slot, gets stale results.
- **Suggested fix:** Replace the bespoke temp+rename in `write_slot_model` and `write_active_slot` with a call to `crate::fs::atomic_replace` (the helper already exists for `notes.toml` and `audit-mode.json`). Removes ~20 lines from each function and gives durable rename semantics.

#### Daemon serializes ALL queries through one `Mutex<BatchContext>` — a slow query (LLM batch fetch, large gather) blocks every other reader
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:1775` (mutex setup), `src/cli/watch.rs:1853-1858` (per-connection thread that takes the mutex)
- **Description:** `cqs watch --serve` wraps the BatchContext in `Arc<Mutex<...>>` and per-connection threads acquire it for the entire `handle_socket_client` dispatch (line 1856 → `handle_socket_client(stream, &ctx_clone)`). All `cqs <cmd>` invocations from the user's shell — search, callers, scout, gather, even `notes list` — block on this single mutex. A slow path (e.g. `cqs llm summary --batch ...` triggering a Claude Batch poll, or a `cqs gather` BFS over a hostile-shape graph that takes 30+ seconds) blocks every other CLI invocation including `cqs --version` if it goes through the daemon. From a data-safety angle the issue is that with the mutex held, any background job that needs to *write* (like `stream_summary_writer` callbacks fired from a parallel LLM thread inside the BatchContext) waits behind reads. With `WRITE_LOCK` held inside the mutex while the daemon's outer mutex is also waiting, a deadlock surface emerges: thread A holds outer mutex + WRITE_LOCK, thread B (LLM stream) wants WRITE_LOCK but can't proceed; if thread A's transaction is waiting on a rayon pool that thread B's host thread serves, A and B both stall.
- **Suggested fix:** Two options that match the project's conventions:
  1. Replace the outer `Mutex<BatchContext>` with `RwLock<BatchContext>`; reader paths take `read()`, the few mutator paths (sweep_idle_sessions, reload notes) take `write()`. Lets concurrent reads parallelize.
  2. Push the mutex inside `BatchContext` to per-resource locks (one for sessions, one for notes cache, etc.) and reach in only for the specific field a query needs.
  Either way, audit `stream_summary_writer` carefully — it must NOT be reachable from inside the daemon mutex without going through the `WRITE_LOCK` discipline.

#### `embedding_cache` schema is identical for both `embedding` and `embedding_base` columns — no separation by purpose
- **Difficulty:** medium
- **Location:** `src/cache.rs:159-171` (schema), `src/store/chunks/async_helpers.rs:319` (writes)
- **Description:** The cache stores `(content_hash, model_fingerprint) → embedding`. v18 added `embedding_base` (raw NL embedding before enrichment) but the cache schema does not record WHICH of the two dual-index columns the cached blob represents. The cache is read at the top of every embed batch (in `Embedder::embed_batch_with_cache` or similar) — if the lookup is for "embedding" but the row was written for "embedding_base" (or vice versa), the wrong vector is returned. In practice today both columns are seeded identically on the initial insert (line 319), so it works; but PR #1040's enrichment pass overwrites `embedding` only, leaving `embedding_base` intact. The next reindex hits the cache by content_hash + fingerprint and gets back... whichever row was written last. There's no `purpose` discriminator. If a future change ever caches the post-enrichment embedding, the cache becomes non-deterministic between purposes.
- **Suggested fix:** Add a `purpose TEXT NOT NULL DEFAULT 'embedding'` column to the cache schema, include it in the PRIMARY KEY and all reads/writes. Costs one migration and one extra bind per query; eliminates the implicit assumption that the same content_hash + fingerprint can only have one meaning.

#### `update_umap_coords_batch` uses TEMP TABLE shared across concurrent calls — DELETE may clear another session's data mid-flight
- **Difficulty:** easy
- **Location:** `src/store/chunks/crud.rs:392-450`
- **Description:** Inside the write transaction, the function does `CREATE TEMP TABLE IF NOT EXISTS _update_umap (...)` then `DELETE FROM _update_umap` (line 401-403). TEMP tables in SQLite are *connection-scoped*, not transaction-scoped: they persist across statements on the same connection. Because the sqlx pool may hand out the same connection to a future `update_umap_coords_batch` call, the second call's `DELETE FROM _update_umap` sees rows from the first call's TEMP table (or the table simply persists across multiple invocations until the connection is dropped). Today this is masked because `WRITE_LOCK` serializes calls. But if a concurrent error path leaves the table populated (e.g. `INSERT INTO _update_umap` succeeds, then the `UPDATE chunks ... FROM _update_umap` fails and the transaction rolls back), the CREATE IF NOT EXISTS sees the leftover, and DELETE clears it — but a DROP TABLE IF EXISTS at the end of the function (line 435-437) is INSIDE the transaction that was just rolled back, so the leftover survives. The next call's INSERT-then-UPDATE then operates on the right shape but the TEMP TABLE is now persistently dirty.
- **Suggested fix:** Either (a) DROP the TEMP TABLE before CREATE so each call starts clean, regardless of prior state; or (b) use a uniquely-named TEMP TABLE (suffix with random u64) and DROP at end-of-function via a Drop guard so failures still clean up. Option (a) is simpler; SQLite TEMP DROP is cheap and the error path is the rare case.

#### Cache `evict()` size-then-DELETE is in a transaction but the per-row `write_batch()` it competes with is NOT under the same lock
- **Difficulty:** medium
- **Location:** `src/cache.rs:408-460` (evict tx with `evict_lock`), `src/cache.rs:354-398` (`write_batch` tx, no `evict_lock`)
- **Description:** The DS2-5 fix moved the `(SELECT size, AVG, DELETE)` triplet inside one transaction with an in-process `evict_lock` mutex serializing concurrent evicts. Good. But `write_batch` runs in its own transaction without acquiring `evict_lock`. Under WAL mode the evict's BEGIN takes a snapshot at evict-start; a concurrent `write_batch` can commit AFTER the SELECT-size step but BEFORE the DELETE step. The evict's DELETE then deletes some rows just inserted, with no signal to the writer. From the writer's perspective, the embedding write succeeded; from a cross-session read it's a cache miss, and the next embed pass repeats the (potentially expensive) embedding computation. Not corruption — but a confusing perf footgun.
- **Suggested fix:** Hold `evict_lock` (or the Tokio equivalent) across writes too — every cache mutation goes through one of the two paths, both acquire the same mutex. Costs a per-batch lock acquire (cheap) and eliminates the silent re-embed loop.


## Performance

#### [PF-V1.30-1]: `reindex_files` watch path double-parses calls per file (parse_file_all then extract_calls_from_chunk per chunk)
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:2815, 2930-2939`
- **Description:** The watch reindex calls `parser.parse_file_all(&abs_path)` at line 2815 — this returns `(file_chunks, calls, chunk_type_refs)`, where `calls` is the file-level call graph. The `calls` value is upserted at line 2851 via `store.upsert_function_calls`, then **silently discarded for chunk-level call mapping**. Lines 2930-2939 then loop every chunk and call `parser.extract_calls_from_chunk(chunk)` — which re-runs tree-sitter over the chunk content to extract the same call sites a second time. The bulk pipeline already fixed this in P2 #63 by using `parse_file_all_with_chunk_calls` (returns a fourth `chunk_calls: Vec<(chunk_id, CallSite)>` value from the same Pass 2). The docstring at `src/parser/mod.rs:447-451` explicitly notes "Watch (`src/cli/watch.rs`) still uses `parse_file_all` and runs its own `extract_calls_from_chunk` per chunk; collapsing that into this method is a separate refactor." That refactor never landed. With ~14k chunks per repo-wide reindex (parser.rs note) and one tree-sitter parse per chunk, this is an extra 14k tree-sitter parses per `cqs index` (when the daemon is the indexer) or per touched file's chunks per watch event.
- **Suggested fix:** Switch the watch path from `parse_file_all` to `parse_file_all_with_chunk_calls`. The fourth tuple element is `Vec<(String, CallSite)>` keyed by absolute-path chunk id; rewrite the ids using the same prefix-strip the watch path already does for `chunk.id` at line 2834, then replace the `for chunk in &chunks { extract_calls_from_chunk(chunk) }` loop with a `HashMap` populated from the returned chunk_calls. Single-line API switch + ~10 lines of id rewriting; cuts reindex CPU roughly in half on the watch path.

#### [PF-V1.30-2]: `reindex_files` watch path bypasses the global EmbeddingCache (slot/cross-slot benefit lost on file edits)
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:2876-2887` vs `src/cli/pipeline/embedding.rs:39-62`
- **Description:** PR #1105 added the per-project `.cqs/embeddings_cache.db` keyed by `(content_hash, model_id)` so a chunk re-embedded after a model swap or a new slot can hit cache instead of going through the GPU. The bulk index path (`prepare_for_embedding`) checks both `global_cache.read_batch` and `store.get_embeddings_by_hashes`. The watch reindex hot path (`reindex_files`) at line 2877 only calls `store.get_embeddings_by_hashes(&hashes)` — it never sees `EmbeddingCache`. Net effect: every file change in watch mode goes through the embedder for any chunk whose content_hash isn't in the *current slot's* `chunks.embedding` column, even if the same hash was already computed in another slot or in a prior model that lives in the global cache. The watch loop is the highest-frequency embedder consumer (every file save during active development); missing the global cache here costs the most.
- **Suggested fix:** Plumb `global_cache: Option<&EmbeddingCache>` through `cmd_watch` → `reindex_files`. Replace lines 2876-2887 with a call to the same `prepare_for_embedding` helper the bulk pipeline uses (it already handles the `global cache → store cache → embed` fallback chain, including the dim mismatch guard). Eliminates the diverging cache-check code and makes the watch path benefit from #1105 the way the bulk path already does.

#### [PF-V1.30-3]: `reindex_files` allocates N empty `Embedding` placeholders then overwrites each
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:2918-2924`
- **Description:** `let mut embeddings: Vec<Embedding> = vec![Embedding::new(vec![]); chunk_count];` allocates `chunk_count` placeholder `Embedding` structs (each with an empty inner `Vec<f32>`), then immediately overwrites every slot via the cached + new-embedding loops at 2919-2923. Even setting aside the constructor cost, the `Embedding` type holds normalized-state metadata; the placeholders may need `Embedding::try_new(vec![])` validation in a future refactor and silently produce zero-norm vectors today. Allocation pattern is also wasteful — for a 100-file batch with 3000 chunks, that's 3000 `Embedding::new(vec![])` calls with discarded results.
- **Suggested fix:** Build `embeddings` directly from the (cached, new) iterators rather than placeholder-then-overwrite. Either: (1) sort `cached` and `to_embed` indices and merge in order, or (2) build a `HashMap<usize, Embedding>` and `(0..chunk_count).map(|i| map.remove(&i).unwrap_or_else(...))` — but better is to refactor the same way the bulk pipeline does (`create_embedded_batch` at `src/cli/pipeline/embedding.rs:127-143`): zip cached + (to_embed/new_embeddings) in original order without ever materializing a placeholder Vec. This is the same pattern the bulk path already proved.

#### [PF-V1.30-4]: `prepare_for_embedding` always issues store-cache query even when global cache fully satisfies the batch
- **Difficulty:** easy
- **Location:** `src/cli/pipeline/embedding.rs:64-82`
- **Description:** `prepare_for_embedding` first queries the global `EmbeddingCache` (line 47) populating `global_hits`, then UNCONDITIONALLY queries the store cache (line 68) for the same `hashes` slice. On the warm-cache path (e.g. reindex after `cqs slot promote`, or any reindex where chunks are unchanged), the global cache hit-rate approaches 100% and every store query is wasted DB work. The store query at `get_embeddings_by_hashes` is one SELECT but with O(n) bind variables and a JOIN against the `chunks` table — non-trivial latency on big batches. The fix is to filter the second query to only hashes the global cache missed.
- **Suggested fix:** Compute `let missed_hashes: Vec<&str> = hashes.iter().filter(|h| !global_hits.contains_key(*h)).copied().collect()` and pass `&missed_hashes` to `store.get_embeddings_by_hashes`. When all chunks hit global cache, the store query is skipped entirely. When none do, behaviour is identical to today. Additional comment at line 84 about the `global cache > store cache > embed` precedence is already correct; the implementation just doesn't act on it for the second query.

#### [PF-V1.30-5]: `wrap_value` deep-clones the entire payload via `serde_json::to_value(Envelope::ok(&payload))`
- **Difficulty:** medium
- **Location:** `src/cli/json_envelope.rs:160-176`
- **Description:** `wrap_value(&serde_json::Value)` constructs `Envelope::ok(payload)` (which holds `&Value`), then serializes-and-parses the whole envelope via `serde_json::to_value`. For `serde_json::Value` the `Serialize` impl visits every node and rebuilds an identical tree — a deep clone disguised as a re-serialization round trip. The function is called once per daemon dispatch via `crate::cli::batch::write_json_line` and once per CLI emit, so every `cqs gather --tokens 50000` (which can be 50KB+ of nested objects), every `cqs scout`, every `cqs review` output pays the cost. The header comment at line 153-155 acknowledges "shallow clone of the payload (necessary because `serde_json::json!` macro takes ownership)" — but this isn't shallow, the serde_json round trip walks the whole tree and reallocates every Map and Vec. For a typical 30KB gather payload, that's ~30KB of allocator churn per query; on a busy daemon at 100 QPS that's ~3MB/s of pointless allocator pressure plus the CPU walking the tree.
- **Suggested fix:** Build the envelope as a `serde_json::Value::Object` directly without a typed-struct round trip. `serde_json::Map::from_iter([("data", payload.clone()), ("error", Value::Null), ("version", Value::Number(1.into()))])`. Single shallow clone of the payload's outer enum tag (the inner Map/Vec stays owned) instead of a tree walk. Even better: change the contract so callers pass an *owned* `serde_json::Value` and `wrap_value` moves it in — `Map::insert("data", payload)` doesn't allocate a copy at all. Most call sites (`batch/mod.rs::write_json_line`) already produce the value just-in-time; switching to by-value is a per-site noop.

#### [PF-V1.30-6]: Daemon socket handler walks the args array twice (validation pass + extraction pass)
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:266-297`
- **Description:** `handle_socket_client` first scans `request.get("args")` to collect indices of non-string elements (`bad_arg_indices`, lines 267-274), and if the array is clean does a SECOND pass via `arr.iter().filter_map(|v| v.as_str().map(String::from))` (lines 291-296) to materialize the `Vec<String>`. Each daemon query thus walks the `serde_json::Value::Array` twice. Cheap individually but it's literally the request entry point — every daemon query at 100+ QPS pays this. Combine the two passes: do the strict-string validation while building the `Vec<String>` and bail out the moment a non-string is observed.
- **Suggested fix:** Fold both passes into one:
```rust
let mut args = Vec::new();
let mut bad_arg_indices = Vec::new();
if let Some(arr) = request.get("args").and_then(|v| v.as_array()) {
    for (i, v) in arr.iter().enumerate() {
        match v.as_str() {
            Some(s) => args.push(s.to_string()),
            None => bad_arg_indices.push(i),
        }
    }
}
if !bad_arg_indices.is_empty() { /* reject */ }
```
One pass instead of two; preserves the existing reject-with-indices error message.

#### [PF-V1.30-7]: `build_graph` correlated subquery for n_callers — N rows × per-row COUNT(*) instead of one GROUP BY
- **Difficulty:** medium
- **Location:** `src/serve/data.rs:234-264`
- **Description:** The node-fetch SQL in `build_graph` includes `COALESCE((SELECT COUNT(*) FROM function_calls fc WHERE fc.callee_name = c.name), 0) AS n_callers_global` as a correlated subquery in the SELECT. SQLite executes the subquery once per row scanned. With `idx_callee_name` present the per-row cost is O(log M) where M = function_calls row count (~30k+ in this repo), and N is the cap'd graph size (`ABS_MAX_GRAPH_NODES`, currently 5000). That's 5000 × log(30k) ≈ 75k index probes for one `/api/graph` request. A single `LEFT JOIN (SELECT callee_name, COUNT(*) AS n FROM function_calls GROUP BY callee_name)` aggregates once and joins by name — one full scan + one hash join, O(M + N), independent of N. On larger projects (the cqs serve /api/graph endpoint is the biggest data fetch in the new web surface) the difference is several hundred ms vs single-digit ms.
- **Suggested fix:** Replace the correlated subquery with a JOIN against an aggregated subselect:
```sql
SELECT c.id, c.name, c.chunk_type, c.language, c.origin, c.line_start, c.line_end,
       COALESCE(cc.n, 0) AS n_callers_global
FROM chunks c
LEFT JOIN (SELECT callee_name, COUNT(*) AS n FROM function_calls GROUP BY callee_name) cc
  ON cc.callee_name = c.name
WHERE 1=1 ... ORDER BY n_callers_global DESC, c.id ASC LIMIT ?
```
Same result, single aggregation pass. Also benefits `build_hierarchy` which has a similar shape (`src/serve/data.rs:670-754`).

#### [PF-V1.30-8]: `build_graph` edge-dedup HashSet keys clone (file, caller, callee) per row even on dedup miss
- **Difficulty:** easy
- **Location:** `src/serve/data.rs:367-373`
- **Description:** The edge dedup loop builds `let key = (file.clone(), caller.clone(), callee.clone())` for every row regardless of whether the row will be kept, then `seen.insert(key)` — three String clones per fetched row. With `ABS_MAX_GRAPH_EDGES` typical at tens of thousands, that's tens of thousands of extra String allocations per `/api/graph` request, most of them duplicating work the row decode already did (`row.get("file")` already returned an owned String). The pattern was lifted from a deduplicating insert in another module but here the strings are small and the surrounding loop bound is ABS_MAX_GRAPH_EDGES so the cost compounds.
- **Suggested fix:** Two options. (1) Skip the dedup entirely — the SQL `LIMIT` + the symmetric `IN (...)` twice over already over-fetches; deduping at the resolver step at line 396 is enough since the resolver is a `HashMap` lookup that naturally collapses duplicates by ignoring them. (2) Keep the dedup but switch to a hash-of-bytes key:
```rust
use std::collections::hash_map::DefaultHasher;
let mut h = DefaultHasher::new();
file.hash(&mut h); caller_name.hash(&mut h); callee_name.hash(&mut h);
let hash_key = h.finish();
if seen.insert(hash_key) { accum.push((file, caller_name, callee_name)); }
```
Hash collisions on a `u64` keyed `HashSet<u64>` are negligible at <1M edges. Cuts allocations from 3N+1 strings to ~zero.

#### [PF-V1.30-9]: `extract_imports` uses `HashSet<String>` — allocates a `String` per candidate line even on duplicate rejection
- **Difficulty:** easy
- **Location:** `src/where_to_add.rs:258-276`
- **Description:** `extract_imports` iterates every line of every chunk, and for every line that matches a prefix it calls `seen.insert(trimmed.to_string())`. The HashSet stores `String` so insertion always allocates, even when the value is rejected as a duplicate (HashSet still hashes its borrowed key, but the caller materialized the String first). For a Rust file with ~50 chunks × ~30 lines/chunk × 5 prefixes, that's ~7500 `to_string` calls per `cqs where`/`cqs task` invocation — most of which are non-import lines that matched the prefix loosely or duplicate imports already seen. Lines borrowed from `chunks` are valid for the lifetime of the function so a borrowed-key HashSet works.
- **Suggested fix:** Switch `seen` to `HashSet<&str>` with the same lifetime as `chunks`:
```rust
let mut seen: HashSet<&str> = HashSet::new();
let mut imports: Vec<String> = Vec::new();
for chunk in chunks {
    for line in chunk.content.lines() {
        let trimmed = line.trim();
        for &prefix in prefixes {
            if trimmed.starts_with(prefix) && imports.len() < max && seen.insert(trimmed) {
                imports.push(trimmed.to_string());  // Allocate only on accept
                break;
            }
        }
    }
}
```
Allocation now happens only for accepted imports (capped at `max=5`), not per candidate line. ~1500× fewer String allocations on a typical Rust file.

#### [PF-V1.30-10]: Watch `reindex_files` cached embedding clone via `existing.get` instead of `.remove`
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:2879-2887`
- **Description:** The cached-embedding loop:
```rust
for (i, chunk) in chunks.iter().enumerate() {
    if let Some(emb) = existing.get(&chunk.content_hash) {
        cached.push((i, emb.clone()));   // clone every cached Embedding
    } else {
        to_embed.push((i, chunk));
    }
}
```
Every cache hit clones the `Embedding` (inner `Vec<f32>`, dim=1024 default = 4KB allocation per hit). For a 100-file save that touches 500 chunks with 80% cache hit rate, that's ~400 × 4KB = 1.6MB of allocator churn per watch event — and watch events fire on every save in active development. The `existing` map is consumed only by this loop and discarded afterward, so we can `.remove()` to take ownership instead.
- **Suggested fix:** Make `existing` mutable (already is — `let mut`isn't there but the binding owns the map) and use `existing.remove(&chunk.content_hash)` to take ownership:
```rust
let mut existing = store.get_embeddings_by_hashes(&hashes)?;
let mut cached: Vec<(usize, Embedding)> = Vec::new();
let mut to_embed: Vec<(usize, &cqs::Chunk)> = Vec::new();
for (i, chunk) in chunks.iter().enumerate() {
    if let Some(emb) = existing.remove(&chunk.content_hash) {
        cached.push((i, emb));
    } else {
        to_embed.push((i, chunk));
    }
}
```
Eliminates every Embedding clone on the cache-hit path. Mirrors the `global_hits.remove` pattern already used at `src/cli/pipeline/embedding.rs:97`. P3 #126-style fix the watch path missed.


## Resource Management

#### [RM-V1.30-1]: Background HNSW rebuild thread is detached — daemon shutdown cannot wait for it
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:965-1042` (`spawn_hnsw_rebuild`)
- **Description:** PR #1113 spawns a background HNSW rebuild via `std::thread::Builder::new().name(...).spawn(...)` and stores only the mpsc `Receiver` in `PendingRebuild` (line 1037-1041). The `JoinHandle` is dropped immediately, so the thread is detached. On `systemctl stop cqs-watch` (SIGTERM) the daemon's main loop exits and `WatchState` drops, but the rebuild thread (which holds its own `Store::open_readonly_pooled` handle plus a CUDA build pipeline at `commands::build_hnsw_index_owned`) keeps running until it sends on the channel — even though the receiver is already gone. On the user's A6000 a full 17k-chunk CUDA HNSW build is ~10-15s, plus another ~10-15s for the base index. Worst case: a `systemctl restart cqs-watch` triggers a fresh rebuild on the new daemon while the old daemon's orphaned rebuild thread is still spending GPU memory + CUDA streams on the now-discarded result. Two CUDA HNSW builds running concurrently against the same `index.db` snapshot is exactly the contention pattern that leads to `cuda_runtime` API errors (cuvs is process-global, not per-context). In addition, `index.hnsw.lock` may be re-acquired by the new daemon while the old thread still has it open via `Owned::save` — leading to a half-written graph file.
- **Suggested fix:** Hold the `JoinHandle` inside `PendingRebuild` next to `rx`. On daemon shutdown (the `loop` exit in `cmd_watch`), call `pending_rebuild.take().map(|p| p.handle.join())` with a bounded timeout (e.g. 2s) before letting the daemon exit. Better: have `spawn_hnsw_rebuild` accept a `Arc<AtomicBool>` cancellation flag, check it inside `build_hnsw_index_owned` between batches, and bail with `RebuildOutcome::Err(anyhow!("cancelled"))` so the GPU work stops promptly.

#### [RM-V1.30-2]: `pending_rebuild.delta` grows unbounded during a long rebuild window
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:611,623-626,2667-2674,2740-2741`
- **Description:** While a background HNSW rebuild is in flight (10-30s), every reindex cycle appends every newly-embedded chunk into `pending.delta: Vec<(String, Embedding)>` (line 2674). Each `Embedding` is `dim × 4 bytes` ≈ 4 KB on BGE-large (1024-dim). A bulk file operation during the rebuild — e.g. `git checkout` of a feature branch touching 5k files, `find -name '*.rs' -exec sed -i ...`, or a `cargo fix` pass — can push tens of thousands of chunks into the delta before the rebuild completes. 30k entries × 4 KB = 120 MB held in `WatchState`, on top of the rebuild thread's own working memory and the in-memory `state.hnsw_index`. There is no cap, no warn-on-overflow, no spill-to-disk fallback, and no early-cancel of the in-flight rebuild even when delta has already invalidated the snapshot it's building from.
- **Suggested fix:** Cap `delta` at e.g. `MAX_PENDING_REBUILD_DELTA = 5_000` entries (~20 MB on 1024-dim). On overflow: `tracing::warn!`, drop oldest entries (or all of them — the next `threshold_rebuild` will pick the chunks up from SQLite anyway), and mark the rebuild as `state.pending_rebuild = None` to short-circuit the swap entirely (the new in-memory index would be ~useless given the volume of misses). Add a structured event so operators can see when the daemon entered "delta saturation" mode.

#### [RM-V1.30-3]: `Embedder::new` opens a fresh `QueryCache` SQLite + 7-day prune on every CLI subcommand
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:355-366`
- **Description:** `Embedder::new` unconditionally calls `QueryCache::open(&QueryCache::default_path())` followed by `c.prune_older_than(7)` (line 358-361), even on commands that will never call `embed_query` (e.g. `cqs notes list`, `cqs read`, `cqs slot list`, `cqs cache stats`, `cqs explain --no-embed`). The `QueryCache` is constructed lazily *during the embedder*, but the embedder itself is constructed eagerly by every CLI handler that takes a `cli.try_model_config()` (16 call sites in `Bash` cross-check above). Each open is: `std::fs::create_dir_all` + `chmod 0o700` + new tokio `current_thread` runtime + SqlitePool with `max_connections=1` + 4 PRAGMA + WAL setup + `prune_older_than(7)` (a `DELETE` against `query_cache`). Cold start of `cqs --help` (which already lazy-loads the embedder for completion) pays this; a hot `cqs notes list` run via daemon doesn't, but a CLI bypass (`CQS_NO_DAEMON=1`) does. On WSL DrvFS this is ~30-50ms of completely wasted I/O per invocation.
- **Suggested fix:** Lazy-open the disk cache the first time it's actually used inside `embed_query`. Store it as `OnceLock<Option<QueryCache>>` rather than constructing eagerly in `new_with_provider`. The in-memory `LruCache` stays as-is; only the SQLite-backed half pays the cold open. Bonus: cli subcommands that are pure SQL (notes, slot, cache, telemetry) no longer touch `~/.cache/cqs/query_cache.db` at all.

#### [RM-V1.30-4]: `LocalProvider::stash` retains all submitted batch results until provider drop
- **Difficulty:** medium
- **Location:** `src/llm/local.rs:74,304-309,542-547`
- **Description:** `LocalProvider::submit_via_chat_completions` (PR #1101) stores each submitted batch's results in `self.stash: Mutex<HashMap<String, HashMap<String, String>>>`, keyed by batch_id. The only path that removes entries is `fetch_batch_results` (line 545: `stash.remove(batch_id)`). If a caller submits multiple batches and crashes (or panics) between submit and fetch, all unfetched batches stay resident for the lifetime of the `LocalProvider`. Even on the happy path, a long-running `cqs index --llm-summaries` over a 5k-chunk corpus with `concurrency=4` may submit several batches sequentially: each batch ~250 successful items × 500-byte summary text ≈ 125 KB stashed at a time, but if a batch fails the entries-so-far are retained anyway because the function returns `Err(LlmError::Api{...})` after inserting partial results into the stash (line 304-309 happens unconditionally before the auth-fail bail at 286). Worst case for a 50k-chunk doc-comments pass with 50% timeout failures: ~25k summaries × ~500 bytes = ~12 MB of dead text held in the provider until the CLI exits. There is no LRU, no TTL, no "drain everything older than N" sweep.
- **Suggested fix:** (1) Move the stash insert past the auth-fail bail so failed batches don't leak partial results. (2) Add `LocalProvider::drain_old_batches(&self, max_keep: usize)` called from the outer `llm_summary_pass` after each `wait_for_batch + fetch_batch_results` cycle. (3) On the auth-fail Err arm at line 286, explicitly clear the stash entry. (4) Cap total stash size: if it exceeds e.g. 1000 batches or 100 MB cumulative, evict oldest by insertion order.

#### [RM-V1.30-5]: Daemon never checks `fs.inotify.max_user_watches` — silently drops events on large monorepos
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:1947-1949`
- **Description:** `RecommendedWatcher::new(...).watch(&root, RecursiveMode::Recursive)` walks the tree and registers an inotify watch per directory. Linux's `fs.inotify.max_user_watches` defaults to 8192 on older distros, 65536-1048576 on newer ones. On a project with deep `node_modules/`, `target/`, `vendor/`, or `dist/` directories — none of which are filtered before `watch()` because `notify` registers everything before our gitignore filter sees it — the limit can be silently exceeded. `notify` returns `Err` on registration failure for individual sub-directories but the `watch(root, Recursive)` call at line 1949 only returns the *root* registration error. Per-subdir failures are swallowed, leading to "watch isn't picking up changes in src/foo/" with zero diagnostic output. The `--poll` fallback exists but the daemon never auto-detects exhaustion to suggest it.
- **Suggested fix:** At daemon startup, read `/proc/sys/fs/inotify/max_user_watches` and `/proc/<pid>/status` (Inotify count). If the project's directory count is within 90% of the limit, log a `tracing::warn!` recommending `--poll` or a `sysctl fs.inotify.max_user_watches=524288` bump. For inotify-watcher, replace the direct `watch(&root, Recursive)` with a manual descent that respects the gitignore matcher: only watch directories that wouldn't be ignored anyway (avoids registering target/, node_modules/, .git/). This both reduces inotify pressure and matches the user's expectations about which paths the daemon should react to.

#### [RM-V1.30-6]: `select_provider()` triggers CUDA/TensorRT probe + symlink ops for every CLI process, including no-embed commands
- **Difficulty:** medium
- **Location:** `src/embedder/provider.rs:171-248`, `src/embedder/mod.rs:312-313`
- **Description:** PR #1120 (Phase A) refactored execution-provider detection but kept the call-site contract: `Embedder::new` → `select_provider()` → `detect_provider()` → `ensure_ort_provider_libs()`. Even with `CACHED_PROVIDER: OnceCell` memoizing the result, the *first* call within a process always: walks `~/.cache/ort.pyke.io/dfbin/<triplet>/`, sorts directory entries, reads `/proc/self/cmdline`, joins multiple `PathBuf`s, calls `dunce::canonicalize` on each library, and `std::os::unix::fs::symlink`s up to 3 `.so` files into both `ort_search_dir` and the first writable `LD_LIBRARY_PATH` entry. On a CLI invocation that never reaches `embed_query` (e.g. `cqs notes list`, which constructs an embedder via `try_model_config`), this is pure waste — the CUDA probe `ort::ep::CUDA::default().is_available()` itself can take 200-500ms because it lazily loads `libcudart.so` and pings the driver. Phase A's `ep-coreml` / `ep-rocm` cfg-gates are correct in spirit but they're cfg-gates over branches that today are dead `tracing::warn!` arms — the `ensure_ort_provider_libs` always fires regardless.
- **Suggested fix:** Move the provider probe + symlink work into a `LazyLock<ExecutionProvider>` that fires only inside `Session::create_session(model_path, ...)` — i.e. on the first actual ONNX inference, not on every `Embedder::new`. Construction-time should know the *target* provider (from CLI flag / env) but defer side effects until needed. The `Mutex<Option<Session>>` already supports lazy session init; making provider detection equally lazy keeps no-embed commands free of the GPU probe + symlink overhead.

#### [RM-V1.30-7]: `LocalProvider::submit_via_chat_completions` worker threads use default 2 MB stack — `concurrency=64` allocates 128 MB just for the fan-out
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:163-256` (`std::thread::scope` in `submit_via_chat_completions`)
- **Description:** `local_concurrency()` clamps `CQS_LOCAL_LLM_CONCURRENCY` at `[1, 64]`. At the upper end, `std::thread::scope` spawns 64 worker threads via `s.spawn(...)` (line 176) with the platform default stack (2 MB on glibc) — 128 MB of stack space just for batch fan-out, on top of the per-thread `reqwest` blocking client state and JSON parsing buffers. The worker body is shallow (channel recv → reqwest send → JSON parse → mutex insert → callback) so 256 KB or 512 KB would suffice. The cap is also surprisingly high: 64 concurrent HTTPS connections to a local llama.cpp server is going to thrash the GPU, not parallelize — the practical sweet spot for `vllm` on an RTX 4000 is 4-8.
- **Suggested fix:** Use `std::thread::Builder::new().stack_size(512 * 1024).name(format!("cqs-llm-worker-{i}"))` inside a manual scope (or `scope.builder()` if one stabilizes — currently scoped threads use `Scope::spawn` without a stack-size hook, so swap to a `std::thread::Builder` + manual join Vec). Drop the upper clamp from 64 to 16; at >16 the local server is the bottleneck anyway.

#### [RM-V1.30-8]: `serve` handlers spawn one `tokio::task::spawn_blocking` per HTTP request, default blocking pool is 512 threads (~1 GB stack)
- **Difficulty:** medium
- **Location:** `src/serve/handlers.rs:86-89,100+,...` (every handler has a `spawn_blocking`), `src/serve/mod.rs:92-95`
- **Description:** Every `cqs serve` route handler wraps its `Store` call in `tokio::task::spawn_blocking(...)` (six occurrences; at lines 86, ~100, ~140, ~180, ~210). The runtime is built via `Builder::new_multi_thread().enable_all().build()` (mod.rs:92-95) which uses tokio's defaults: `worker_threads = num_cpus` (already noted in RM-V1.29-6) and `max_blocking_threads = 512`. A hostile or buggy client opening 512 parallel `/api/graph` requests (loading a 50k-node graph response each) can saturate the blocking pool — each blocking thread holds the default 2 MB Linux stack + the SQL working set (`HashMap::with_capacity(rows.len())` for up to 50k rows = several MB). Worst case: 512 × (2 MB stack + ~10 MB working set) ≈ 6 GB. The fact that v1.30.0 added per-launch auth (#1118) closes the trivial unauthenticated DoS vector, but an authenticated user (or a malicious browser tab on the same host once they've grabbed the cookie) can still launch this.
- **Suggested fix:** `.max_blocking_threads(8)` on the `Builder` — 8 concurrent SQL queries is more than enough for an interactive single-user UI. Combined with `.worker_threads(4)` from RM-V1.29-6 the daemon's max steady-state thread count is bounded at 12, vs. 512+num_cpus today. Optionally also wrap the inner `Store::rt.block_on` calls with a `tokio::time::timeout(30s, ...)` so a stuck SQL query doesn't pin a blocking thread indefinitely.

#### [RM-V1.30-9]: `LocalProvider::http` uses default reqwest connection pool (no idle cap, no per-host limit)
- **Difficulty:** easy
- **Location:** `src/llm/local.rs:97-100`
- **Description:** `Client::builder().timeout(timeout).redirect(Policy::none()).build()` — no `pool_max_idle_per_host`, no `pool_idle_timeout`, no `tcp_keepalive`. reqwest's blocking client defaults to `pool_max_idle_per_host = usize::MAX` and `pool_idle_timeout = 90s`. On a long-running `cqs index --llm-summaries` that submits batches over hours, the connection pool to the local server can grow without bound — particularly bad against vLLM, which is single-process and sees each idle keep-alive as a held slot in its connection table. If the daemon embeds `LocalProvider` (it doesn't today, but the LLM path is converging on daemon execution), pool growth becomes permanent.
- **Suggested fix:** `Client::builder().pool_max_idle_per_host(self.concurrency).pool_idle_timeout(Duration::from_secs(30)).timeout(timeout)...`. Caps the idle pool at the worker count (extra idle connections beyond `concurrency` are by definition unused) and recycles after 30s of idle.

#### [RM-V1.30-10]: `Mutex<Option<Arc<Tokenizer>>>` pattern in `Embedder` keeps a stale tokenizer Arc alive on `clear_session` if any inference is in flight
- **Difficulty:** medium
- **Location:** `src/embedder/mod.rs:261,808-823`
- **Description:** `clear_session` sets `*self.tokenizer.lock() = None` (line 820-821), but the comment on line 256-258 explicitly says *"Arc holds a strong ref so in-flight inference that grabbed an Arc clone before this call continues with its own copy."* This is correct for safety, but the implication is that if a long-running inference (e.g. a batch of 1000 chunks at 4ms each = 4s of work) is mid-flight when `sweep_idle_sessions` fires, the tokenizer Arc stays in memory until that batch completes — the BGE-large tokenizer is ~10 MB, larger BPE vocabularies are ~20 MB. Concurrently, the next inference call after `clear_session` lazy-reloads the tokenizer (10-30 ms), so for a brief window the daemon holds *two* tokenizer copies. Combined with the parallel session reload (~500 MB), the actual peak memory during the "clear → next-use" handoff is ~1 GB rather than the documented ~500 MB. This is documented behavior, but the actual lifecycle isn't surfaced anywhere — the idle-eviction path looks like a clean drop-and-reload to the operator.
- **Suggested fix:** Either (a) wait for in-flight inference before clearing — adds a `RwLock` around the tokenizer Arc and `clear_session` takes the write lock, blocking until inference releases its read lock; or (b) document the doubled-memory window in the `clear_session` doc comment and surface it via a `tracing::info!(stage = "clear_during_inference", ...)` event when `Arc::strong_count(&tok) > 1` at clear time so operators can correlate memory spikes. Option (b) is the lower-risk fix since (a) extends the inference critical section.

## Summary
Found 10 resource-management issues new in v1.30.0:
- (1) PR #1113 detached HNSW rebuild thread can outlive daemon shutdown and contend on GPU/index lock with the next daemon's rebuild;
- (2) sibling issue: `pending_rebuild.delta` is unbounded — bulk git operations during a rebuild window can pin 100MB+;
- (3) `Embedder::new` opens a fresh `QueryCache` SQLite + 7-day prune on every CLI subcommand even when no embedding ever happens;
- (4) PR #1101's `LocalProvider::stash` retains all submitted batch results until provider drop, with no LRU/TTL/cap;
- (5) inotify watcher silently drops events on large monorepos — daemon never checks `fs.inotify.max_user_watches` or warns;
- (6) PR #1120's `select_provider()` still fires CUDA probe + symlink work eagerly via `Embedder::new` even on no-embed commands;
- (7) `LocalProvider` worker threads use default 2MB stack, allocating 128MB at `concurrency=64`;
- (8) `cqs serve` handlers `spawn_blocking` without `max_blocking_threads` cap — authenticated user can pin 512 threads × ~10MB working set;
- (9) `LocalProvider::http` reqwest client has no pool_max_idle / idle_timeout, can leak idle connections to local server over a long indexing run;
- (10) `Embedder::clear_session` documents but doesn't surface the doubled-memory window when in-flight inference holds a tokenizer Arc concurrent with a session reload.


## Test Coverage (happy path)

#### TC-HAP-1.29-1: `cqs serve` data endpoints (`build_graph`, `build_chunk_detail`, `build_hierarchy`, `build_cluster`) never tested with populated data
- **Difficulty:** medium
- **Location:** `src/serve/data.rs:192` (`build_graph`), `:452` (`build_chunk_detail`), `:586` (`build_hierarchy`), `:825` (`build_cluster`), `:933` (`build_stats`). All tests are in `src/serve/tests.rs` and use `fixture_state()` at line 25 which creates an empty init-only store.
- **Description:** The entire `cqs serve` subsystem (new in v1.29.0) has 14 endpoint tests, but every test runs against an empty store. Result: `graph_returns_empty_for_fresh_store` asserts `nodes.len() == 0`, `chunk_detail_unknown_id_returns_404` asserts 404, `hierarchy_unknown_root_returns_404` asserts 404, `cluster_returns_empty_for_fresh_store` asserts `nodes.len() == 0`. The test file comment at line 417-419 (`graph_returns_empty_for_fresh_store`) even admits this: *"Real graph rendering is exercised by manual smoke against the cqs corpus; an in-process test would need a populated fixture (~few hundred LOC of chunk inserts) which is more setup than the shape-check is worth at this stage."* The SQL queries in `build_graph` (cf. `src/serve/data.rs:219-260` correlated subquery for n_callers, edge resolution at `:300-440`), the BFS in `build_hierarchy` (`:620-655`), UMAP-coord lookup in `build_cluster` (`:829-924`), and the caller/callee enrichment in `build_chunk_detail` (`:452-577`) are all untested with actual chunks. A mistake in any of these (e.g., filter-by-file bug, max_nodes clamp off-by-one, direction=callers/callees confusion) ships silently — there is no positive test that asserts "if you index 3 chunks with 2 call edges, `/api/graph` returns 3 nodes and 2 edges".
- **Suggested fix:** Add a module in `src/serve/tests.rs` (or new file) that uses `common::InProcessFixture`-style chunk seeding to populate the store with a small call graph (e.g. `process_data` → `validate` → `format_output`, plus a test chunk). Then assert:
  - `build_graph` returns 3 data nodes + 2 call edges; `max_nodes=1` truncates; `file_filter` narrows; `kind_filter="function"` excludes tests.
  - `build_chunk_detail(store, "process_data_chunk_id")` returns `callers.len()==0, callees.len()==2, tests.len()==1`.
  - `build_hierarchy(store, root, Direction::Callees, depth=5)` returns the 3-node subtree; direction=Callers returns just the root; depth=1 truncates.
  - `build_cluster` returns N nodes with coords when UMAP coords are populated; returns `skipped=N` when they are not.

#### TC-HAP-1.29-2: Batch dispatch handlers (`dispatch_gather`, `dispatch_scout`, `dispatch_task`, `dispatch_where`, `dispatch_onboard`, `dispatch_callers`, `dispatch_callees`, `dispatch_impact`, `dispatch_test_map`, `dispatch_trace`, `dispatch_similar`, `dispatch_explain`, `dispatch_context`, `dispatch_deps`, `dispatch_related`, `dispatch_impact_diff`) have zero tests
- **Difficulty:** medium
- **Location:** `src/cli/batch/handlers/misc.rs:15, 131, 173, 209` (gather/task/scout/where); `src/cli/batch/handlers/graph.rs:24, 63, 103, 143, 233, 292, 375, 392` (deps/callers/callees/impact/test_map/trace/related/impact_diff); `src/cli/batch/handlers/info.rs:46, 100, 168, 302` (explain/similar/context/onboard).
- **Description:** Only `dispatch_search` has inline tests (5 of them in `src/cli/batch/handlers/search.rs:528-742`). The other **16** batch dispatch functions have zero tests. These are the daemon-hot-path handlers every agent hits via `cqs batch` and the socket path — a shape change to the JSON output or a regression in a chunk resolver bubbles silently until an agent notices. Grep confirms: no test file in `tests/` references any of these dispatch fns (only `real_eval_callgraph.json` mentions `dispatch_trace` as a pattern label). Batch 1 triage item P2 #48 addressed `dispatch_review/dispatch_diff/dispatch_drift/dispatch_blame/dispatch_plan`, but the read-only graph/info/search surface was not included.
- **Suggested fix:** Add a single integration file `tests/batch_handlers_test.rs` that uses `common::InProcessFixture` to seed a small corpus + call graph, then calls each handler through `BatchContext::dispatch_line("<cmd> <args>", &mut sink)` and asserts the JSON envelope's `data` field has the expected keys + a non-empty results array. One test per handler is enough. Example: `ctx.dispatch_line("callers process_data", &mut sink)` → JSON has `callers: [...]` with the seeded caller. Follows the exact same pattern `dispatch_search` tests already use (`create_test_context`, seeded chunks).

#### TC-HAP-1.29-3: `Reranker::rerank` and `Reranker::rerank_with_passages` have no tests
- **Difficulty:** medium
- **Location:** `src/reranker.rs:160` (`rerank`), `:190` (`rerank_with_passages`). Only `sigmoid` (a scalar helper) has tests (lines 450+).
- **Description:** `rerank`/`rerank_with_passages` are the two public entry points of the cross-encoder re-ranking subsystem — they take a `Vec<SearchResult>` + query and rescore via ORT. Zero tests pin their contract. The only callers in `tests/` are `eval_harness.rs:527` and `model_eval.rs:1417`, both of which use the reranker as a black box inside an evaluation loop and neither asserts behaviour of a specific (query, passages) pair. A regression in the pair-encoding (batch concat, attention mask), the sigmoid mapping to scores, or the ORT session call would surface as "eval scores moved slightly" rather than a unit-test failure. Given the reranker was flagged in recent audits as a correctness-critical scoring component (see P3 #100 "reranker over-retrieval pool ... duplicated 4x"), its own surface needs at least one happy-path pin.
- **Suggested fix:** Add two tests (likely `#[ignore]`-gated because they need the reranker model on disk — same shape as the existing ignored model tests):
  - `test_rerank_preserves_input_set_reorders_by_score`: seed 3 passages with obviously different relevance to a query ("rust async await" → ["tokio runtime docs", "how to bake sourdough", "rust futures trait"]); assert all 3 are returned and the baking passage ranks last.
  - `test_rerank_with_passages_empty_input_returns_empty_output`: pin the no-op shortcut. (This is the one test a non-model-loading run could cover.)

#### TC-HAP-1.29-4: `cmd_project { Register, Remove, Search }` — only `Register/List/Remove` has a CLI test; `Search` has no CLI integration test
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/project.rs:70` (`cmd_project`). Existing CLI tests: `tests/cli_surface_test.rs:170` `project_register_list_remove_round_trips`, `:252` `project_remove_nonexistent_succeeds_quietly`, `tests/cli_envelope_test.rs:133` `cqs --json project search anything`.
- **Description:** The `Search` subcommand (the one that does actual cross-project semantic search via `search_across_projects`, lines 119-164) runs `Embedder::new` → `embed_query` → `search_across_projects` → emit. The only existing test `tests/cli_envelope_test.rs:133` invokes `cqs --json project search anything` but asserts **only** the envelope shape, not that results were returned. There is no test that registers two projects, indexes both, runs `project search <query>`, and asserts results from both projects are interleaved and tagged with their project name. Inline tests in `src/cli/commands/infra/project.rs:178-207` only exercise `ProjectSearchResult` JSON serialization, not the end-to-end path. This is the ONLY cross-project-search surface — a regression (wrong project_name tag, dedup across projects collapsing results, weight application across indexes) ships silently.
- **Suggested fix:** Add `tests/cli_project_search_test.rs` with one test that uses `InProcessFixture` to create two temp stores (projects), writes distinct content in each, invokes `cqs project register` twice, then `cqs project search "<term>"`, and asserts: (a) at least one result comes from each project, (b) the `project` field on each result matches the registered name, (c) exit code 0. Can share the cross-project fixture from `tests/cross_project_test.rs`.

#### TC-HAP-1.29-5: `cmd_ref_add`, `cmd_ref_list`, `cmd_ref_remove`, `cmd_ref_update` (CLI) have no end-to-end test; only library-level helpers tested
- **Difficulty:** medium
- **Location:** `src/cli/commands/infra/reference.rs:88, 187, 320, 350` (add/list/remove/update). Tests in `tests/reference_test.rs` (397 lines, 11 tests) exercise `merge_results`, `search_reference`, `validate_ref_name` — all library functions. `tests/cli_drift_diff_test.rs:114` calls `cqs ref add` as setup for drift/diff tests but asserts only the drift output, not the ref add shape.
- **Description:** The 4 CLI subcommands under `cqs ref` do real work: `add` runs `enumerate_files` + `run_index_pipeline` + `build_hnsw_index` + `add_reference_to_config` (see lines 128-179); `list` reads every reference's DB for chunk counts (lines 187-280); `remove` validates existence + rewrites config (lines 320-340); `update` re-indexes from source. None have a happy-path CLI test. Impact: a regression in the TOML config round-trip (`add_reference_to_config` at `src/config.rs`), or in HNSW-per-reference path, or in the chunk-count aggregation shown by `list`, would ship unnoticed. The drift/diff tests use `ref add` but only as setup — they don't validate its output shape.
- **Suggested fix:** Add `tests/cli_ref_test.rs` with 4 tests:
  - `ref_add_then_list_shows_reference_with_chunk_count` — add a tiny reference (2 files), then `ref list --json` and assert `[{name, path, chunks: >=2}]`.
  - `ref_remove_deletes_from_config_and_disk` — add, then remove, then `ref list` returns empty.
  - `ref_update_reindexes_source_content` — add, modify a source file, `ref update`, assert chunk count changed.
  - `ref_add_weight_rejects_out_of_range` — pin `0.0..=1.0` contract (already tested at library level via `validate_ref_name` but not CLI-level).

#### TC-HAP-1.29-6: `cqs batch` daemon socket (`handle_socket_client`) — no happy-path test that a valid command round-trips through the socket
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:160` (`handle_socket_client`). Referenced by Batch 1 TC-ADV-1.29-3 for adversarial cases; this finding is the happy-path complement. `tests/daemon_forward_test.rs` has 9 tests but only covers the CLI-forwarding translator (`translate_cli_args_to_batch`) and `ping` — not a real dispatch-line round trip.
- **Description:** `handle_socket_client` is the daemon hot-path — every systemd-deployed `cqs watch --serve` instance serves ALL agent queries through it. The 9 tests in `daemon_forward_test.rs` cover only the pre-socket translation layer + `ping`. There is no test that sends `{"command":"search","args":["foo"]}` down a `UnixStream` pair, reads the response, and asserts `{data: {results: [...], total: N}}`. A regression in the framing layer (newline vs length-prefix, NUL-termination, gzip wrap), the JSON envelope wrap (P2 #28 already overlapped this for CLI; socket path is distinct), or the per-command dispatch registration in `dispatch_socket_command` would not surface until an agent noticed a broken batch response. The existing `tests/daemon_forward_test.rs:321` `test_mock_socket_round_trip_for_daemon_command` is labelled "mock socket" — it's actually a one-sided harness; see the file for details.
- **Suggested fix:** Add `tests/daemon_socket_roundtrip_test.rs` that creates a `tokio::net::UnixStream::pair()`, spawns `handle_socket_client` against the server half with a minimal `BatchContext` + seeded store, writes a newline-terminated JSON request, reads the response, and asserts envelope shape + payload. Cover one read-only command (e.g. `stats` — no embedder needed) to avoid model load cost.

#### TC-HAP-1.29-7: `cmd_similar` (CLI) has no integration test; only inline serialization tests and module-level `find_similar` tests
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/similar.rs:41` (`cmd_similar`). Inline tests at line 280+ test output struct serialization only (5 tests, none exercise `cmd_similar`). Library `find_similar`/`find_related` is tested in `tests/related_impact_test.rs`.
- **Description:** `cmd_similar` is the CLI entry point for "find functions similar to X by embedding distance". No test invokes it with real input. The library `find_similar` is tested but `cmd_similar` adds: target-function lookup (`store.search_by_name`), embedding fetch, pattern filter application (line 72+), and typed-output JSON build. Each is a regression surface. Very cheap to add because `InProcessFixture` already has the scaffolding and `find_similar` can use `MockEmbedder`.
- **Suggested fix:** Add one test in `src/cli/commands/search/similar.rs::tests` using in-process fixture: seed 3 functions, call `cmd_similar(&ctx, "foo")`, capture stdout via a `Write` sink (the fn currently uses `println!`; if that makes it hard, add to the JSON path only — `--json` sinks via `emit_json`). Assert output contains the other 2 as similar results.

#### TC-HAP-1.29-8: `cmd_ci` happy path — library `ci::analyze_diff` tested, CLI function only tested in error paths
- **Difficulty:** easy
- **Location:** `src/cli/commands/review/ci.rs:9` (`cmd_ci`). Tests in `tests/ci_test.rs` cover library `ci::analyze_diff` (happy path: yes); `tests/cli_train_review_test.rs` has CLI tests for `cmd_ci` but they are error-path only (P2 #46 deliverable — see the batch 1 audit summary).
- **Description:** The CLI surface for `cqs ci` (which produces the GitHub-Actions-style review comment) runs: diff parsing → `analyze_diff` → `cqs_format` / `markdown_format` output → exit code assignment. `cli_train_review_test.rs` (line 154+) has a comment "P2 #46 (b) — `cmd_task`" and covers only failure modes. There is no test that feeds a real diff into `cmd_ci` and asserts the output markdown contains the expected sections ("High-risk changes", "Tests"). A regression in markdown formatting or exit-code mapping (lines 47-95 in ci.rs) ships silently.
- **Suggested fix:** Add one happy-path test in `tests/cli_train_review_test.rs` or a new `cli_ci_test.rs`: feed a real unified-diff string (modify a known indexed function) to `cmd_ci`, assert stdout contains "High-risk" when the diff touches a hotspot and the exit code matches the severity level set at `cli/commands/review/ci.rs:62-87`.

#### TC-HAP-1.29-9: `cmd_gather` (CLI) untested; only library `gather()` tested
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/gather.rs:77` (`cmd_gather`). Inline tests at bottom cover output shape; `tests/gather_test.rs` tests library `gather()`. CLI-level `cmd_gather` has zero integration tests.
- **Description:** `cmd_gather` adds BFS depth / max_files clamping, content injection via `inject_content_into_gather_json` (unique to CLI path, not library), token budget trimming via `apply_token_budget`, and the JSON envelope wrap. A regression in any of these silently alters the output agents rely on for `/investigate` and `cqs task`. Library tests don't exercise these CLI-only steps.
- **Suggested fix:** Add one test in `tests/cli_gather_test.rs` (or reuse `tests/gather_test.rs` harness with `cqs()` spawning): seed a 3-function corpus, run `cqs gather "foo" --json --max-files 2`, assert the response envelope has `results.len() == 2` (clamp worked), `tokens_in/tokens_out` fields present when `--tokens N` passed, content injected into each file_group.

#### TC-HAP-1.29-10: `dispatch_line` has no happy-path test that verifies a valid command produces a correct JSON response
- **Difficulty:** easy
- **Location:** `src/cli/batch/mod.rs:557` (`dispatch_line`). Tests at lines 1908 (`bumps_query_counter` with a `bogus` input — error), 2124 / 2161 / 2198 (all adversarial — NUL bytes, unbalanced quotes).
- **Description:** Every existing `dispatch_line` test uses either a parse-failure input (`bogus`) or an adversarial malformed input. There is no test that sends a valid `search foo` / `stats` / `dead` down `dispatch_line`, reads the `Vec<u8>` sink, parses the JSON, and asserts envelope shape. The tests pin error_count/query_count invariants but not the actual output — a regression that produced the error envelope where it should have produced a success envelope would still pass the counter checks. Relatedly, `dispatch_search` is well-covered at the **handler** level but the **dispatch-line** wrap (clap parsing → handler routing → envelope serialization → newline emission) is not.
- **Suggested fix:** Add one test in `src/cli/batch/mod.rs::tests`: `test_dispatch_line_stats_emits_success_envelope` — run `ctx.dispatch_line("stats", &mut sink)` against an init-only store, parse the `sink` bytes as JSON, assert `json["data"]["total_chunks"].is_number()` and `json["error"].is_null()`. Small. Would catch P2-class regressions (envelope-shape changes, stats serialization drift).

#### TC-HAP-1.30-1: `spawn_hnsw_rebuild` and `drain_pending_rebuild` (non-blocking HNSW rebuild, #1113) ship in v1.30.0 with zero tests
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs::spawn_hnsw_rebuild` (around line ~925, added in PR #1113), `drain_pending_rebuild` (same file). Plus the `PendingRebuild` struct fields `rx`, `delta`, `started_at` and the `RebuildOutcome` channel type (defined in WatchState section ~line 580-600).
- **Description:** The flagship daemon fix in v1.30.0 — `cqs watch --serve` no longer blocks editor saves for 10-30s during full HNSW rebuilds — has **zero** automated test coverage. The change spawns a background thread that opens its own read-only `Store` on the slot's `index.db`, builds enriched + base HNSWs from scratch, sends the `Owned` index back through a channel, and the watch loop's `drain_pending_rebuild` replays any (chunk_id, embedding) deltas captured during the rebuild window into the new index before the swap (deduping against `new_index.ids()`). This is both correctness-critical (delta replay / dedup) and concurrency-critical (TOCTOU between rebuild snapshot and channel `recv`). A regression — e.g., delta vector dropped on swap, dedup miscompare leaking duplicates into HNSW, or rx-channel miss leaving the daemon stuck on the old index — would surface as silently-stale search results in the daemon, not as a test failure. The PR commit message explicitly calls out two failure modes (post-restart Loaded/Owned mismatch, every-100 threshold trigger) but no test was added for either.
- **Suggested fix:** Add `tests/watch_hnsw_rebuild_test.rs` (or a `#[cfg(test)] mod` in `src/cli/watch.rs`) with three tests using `InProcessFixture`-style setup:
  - `rebuild_completes_and_swaps_owned_index` — seed 50 chunks, call `spawn_hnsw_rebuild(...)`, wait on the channel, assert the returned `Owned` HNSW has `len() == 50` and contains all chunk ids.
  - `delta_replayed_on_swap` — seed 50, spawn rebuild, while it's running call `WatchState::record_delta` with 5 new (id, embedding) pairs, then drain; assert the post-swap HNSW has `len() == 55` and the 5 deltas are searchable.
  - `delta_dedup_avoids_double_insert` — seed 50, spawn rebuild, push a delta whose chunk_id already exists in the rebuild's snapshot; assert post-swap `len() == 50` (no duplicate insert) and the embedding matches the delta's value (winner is the most-recent write).
  These can run with a tiny dim (e.g., 16) to keep the test fast — the rebuild path doesn't care about embedding semantics.

#### TC-HAP-1.30-2: `for_each_command!` registry macro and the four `gen_*` emitters (#1114) ship with zero compile-fail or behavioral tests
- **Difficulty:** medium
- **Location:** `src/cli/registry.rs:61` (`macro_rules! for_each_command!`), `src/cli/definitions.rs:850` (`gen_batch_support_impl`), `:897` (`gen_variant_name_impl`), `src/cli/dispatch.rs:51` (`gen_dispatch_group_a`), `:83` (`gen_dispatch_group_b`). Test counts: `registry.rs` = 0, `dispatch.rs` = 0, `definitions.rs` = 20 (but those test other things — clap arg parsing, `BatchSupport`, etc.).
- **Description:** v1.30.0 collapsed five exhaustive matches over `Commands` (~675 LOC of dispatch matches) into a single `for_each_command!` table consumed by four macro emitters. The PR comment notes "Adding a variant without a registry row is a compile error in four places" — but **no test verifies this contract**. The macros also encode group-A vs group-B classification (no-store/lifecycle/mutation vs store-using) which controls whether a command runs before or after `CommandContext::open_readonly`. A registry-row classification mistake (e.g., new mutation command added to `group_b:` instead of `group_a:`) would compile fine but silently open a read-only store before the mutation — a subtle correctness bug. Beyond classification, there is no test that walks every `Commands` variant and asserts `BatchSupport::for_command(v)` and `variant_name(v)` return non-default values, which is the only guard against a future edit silently regressing the macro emitter to a default-arm fallback.
- **Suggested fix:** Add `src/cli/registry.rs::tests` with two tests:
  - `every_command_variant_has_batch_support_entry` — iterate the `Commands` discriminants (use `strum::EnumIter` or a hand-rolled list), call `BatchSupport::for_command(&v)` for each; assert no variant returns the macro's default `_ => BatchSupport::None` arm by checking against an explicit allowlist of "should be None" variants. Same for `variant_name`.
  - `group_a_variants_disjoint_from_group_b` — derive the two sets from the registry rows (or from a debug introspection helper) and assert their intersection is empty. The `unreachable!("Group A variant `{name}` handled …")` arms in `dispatch.rs` enforce this at runtime; a test enforces it at compile-time-of-tests.
  Plus a `compile_fail` doc-test that adds a variant without a registry row, expecting a compile error (use `trybuild` if already in dev-deps, otherwise skip).

#### TC-HAP-1.30-3: `select_provider` / `detect_provider` (embedder ExecutionProvider feature split, #1120) untested
- **Difficulty:** easy
- **Location:** `src/embedder/provider.rs:171` (`select_provider`), `:188` (`detect_provider`), `:258` (`build_session_with_provider`). Test count in this file: **0**.
- **Description:** v1.30.0 PR #1120 (#956 Phase A) renamed `gpu-index` → `cuda-index` and split execution-provider selection out of `embedder/mod.rs` into `embedder/provider.rs`. The new module has zero tests. `select_provider` caches the result of `detect_provider` in a `OnceCell<ExecutionProvider>`, and `detect_provider` walks a feature-flag-conditional priority list (TensorRT > CUDA > CoreML > ROCm > CPU). A regression in the cache-on-first-call invariant, in the priority ordering, or in the `cfg(feature = "cuda-index")` gating could ship without anyone noticing — the prod path runs a single time per process and a wrong provider just shows up as "embeddings ran on CPU when CUDA was supposed to be enabled". The `Display for ExecutionProvider` impl at `src/embedder/mod.rs:207` is also untested.
- **Suggested fix:** Add `src/embedder/provider.rs::tests` with three tests (gated as needed by feature flags):
  - `cpu_when_no_features` (no feature gates, always runs) — under a thread-local override path, force `detect_provider()` into the CPU branch and assert it returns `ExecutionProvider::CPU`.
  - `cuda_selected_when_feature_enabled` (`#[cfg(feature = "cuda-index")]`) — assert that on a CUDA-enabled build, `detect_provider` returns `CUDA { device_id: 0 }` if the runtime env reports a GPU; pin via mock or by reading `select_provider()` once and checking the variant.
  - `select_provider_caches_first_call` — call `select_provider()` twice, assert both return the same enum value (the `OnceCell` invariant); flip an env var between calls and assert it's still cached.

#### TC-HAP-1.30-4: `build_hnsw_index_owned` and `build_hnsw_base_index` — core index helpers used by both `cqs index` and the new `spawn_hnsw_rebuild` — have zero direct tests
- **Difficulty:** medium
- **Location:** `src/cli/commands/index/build.rs:848` (`build_hnsw_index_owned`), `:880` (`build_hnsw_base_index`). Test count for `index/build.rs`: **0**. Both fns are `pub(crate)` and called from at least 3 sites: `run_index_pipeline`, `spawn_hnsw_rebuild` (#1113), and the post-pipeline finalizer.
- **Description:** These are the two functions every full-rebuild path goes through. `build_hnsw_index_owned` returns the enriched HNSW (used by the dual-index router); `build_hnsw_base_index` returns the base (non-enriched) HNSW (used as router fallback). The functions wrap embedding fetch + HNSW construction + on-disk save. Their callers (`cmd_index`, `spawn_hnsw_rebuild`) are integration-tested only end-to-end through `cqs index` invocations. No test asserts: (a) the returned `Owned` has the right `len()` matching chunk count, (b) the saved file at the right path can be loaded back, (c) `build_hnsw_base_index` returns `Ok(None)` when `embedding_base` rows are empty (its current shortcut), (d) the dim of the saved index matches `store.dim()`. With #1113 making these critical to the daemon-rebuild path, the absence of unit-level pins is a fresh risk.
- **Suggested fix:** Add `src/cli/commands/index/build.rs::tests` with three tests using `InProcessFixture`:
  - `build_hnsw_index_owned_returns_index_with_chunk_count` — seed 10 chunks with 16-dim embeddings, call `build_hnsw_index_owned(&store, &cqs_dir)`, assert `Some(idx)` returned and `idx.len() == 10`.
  - `build_hnsw_base_index_returns_none_when_no_base_rows` — fresh store, call `build_hnsw_base_index`, assert `Ok(None)`.
  - `build_hnsw_index_owned_round_trips_through_disk` — build, save, then `HnswIndex::load_with_dim(...)` from the saved path; assert the loaded index has the same `len()` and an arbitrary chunk-id is present in `ids()`.

#### TC-HAP-1.30-5: `hyde_query_pass` and `doc_comment_pass` (LLM passes shipping in `cqs index --hyde` / `--improve-docs`) have zero tests
- **Difficulty:** medium
- **Location:** `src/llm/hyde.rs:11` (`hyde_query_pass`, full file 98 LOC, **zero** tests), `src/llm/doc_comments.rs:135` (`doc_comment_pass`). Compare with `src/llm/summary.rs::llm_summary_pass` which has 4+ end-to-end tests in `tests/local_provider_integration.rs:113-280`.
- **Description:** Three LLM batch passes ship in production: `llm_summary_pass`, `doc_comment_pass`, and `hyde_query_pass`. Only `llm_summary_pass` has integration tests against the local mock server. Both `doc_comment_pass` and `hyde_query_pass` go through identical machinery — `collect_eligible_chunks` filter → `BatchPhase2::submit_or_resume` → store write — but their specific filter (e.g., `needs_doc_comment` for doc_comments, the HyDE eligibility predicate at `src/llm/hyde.rs`) and their result-purpose tags (`"hyde"` vs `"summary"` vs `"doc_comment"`) are unique. A regression in either one's filter or in the `set_pending_*_batch_id` resumption path (HyDE uses `s.get_pending_hyde_batch_id` / `s.set_pending_hyde_batch_id`) could ship — the existing `llm_summary_pass` tests don't cover them. Both passes are billed surfaces (real Anthropic dollars in the prod path); a regression that double-submits or fails resumption costs real money.
- **Suggested fix:** Extend `tests/local_provider_integration.rs` with two parallel tests mirroring the existing `llm_summary_pass` tests:
  - `hyde_query_pass_round_trips_through_mock_server` — seed 3 chunks, mock the local server's `/v1/chat/completions` to return canned predictions, run `hyde_query_pass`, assert `count == 3` and `store.get_summaries_by_purpose("hyde")` has 3 rows.
  - `doc_comment_pass_skips_already_documented_functions` — seed 3 chunks where 1 has a doc comment per `needs_doc_comment`'s predicate, run `doc_comment_pass`, assert `count == 2` (the 1 already-documented chunk skipped). Verifies the unique-to-doc_comments filter.

## Summary

15 findings filed (10 from initial v1.29 audit + 5 v1.30.0-specific).

The most impactful gaps in the v1.30.0-specific batch:
- **TC-HAP-1.30-1** — flagship #1113 fix (non-blocking HNSW rebuild) has **zero** tests despite shipping concurrency-critical delta/dedup logic in the daemon hot path
- **TC-HAP-1.30-2** — #1114 single-registration command registry's classification correctness (group_a vs group_b) is enforced only at compile time of the dispatch matches, not at test time; a misclassified mutation command would silently open a read-only store
- **TC-HAP-1.30-4** — `build_hnsw_index_owned` / `build_hnsw_base_index` are now critical to both `cqs index` and `spawn_hnsw_rebuild` and have no direct unit tests

The original v1.29 findings remain valid; the most impactful is still #1 — the entire `cqs serve` subsystem has 14 tests but all run against an empty store. Several of these are cheap adds (~1 test each) because the in-process fixture already exists.

