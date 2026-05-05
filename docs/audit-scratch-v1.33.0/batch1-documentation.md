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
