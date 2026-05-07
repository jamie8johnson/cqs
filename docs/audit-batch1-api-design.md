# API Design audit — batch 1 (post-v1.38.0)

Audit cuts: CLI argument shape consistency, trait method shape, top-level `Cli` global flags, error variant hygiene. Skips items already addressed in #1505/#1506/#1507/#1501/#1500/#1470 per task scope. All paths absolute.

## Findings

#### API-V1.38-1: `ModelCommand` and `HookCommand` use inline `json: bool` instead of shared `TextJsonArgs`
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/cli/commands/infra/model.rs:122-145` (`ModelCommand::{Show,List,Swap}`), `/mnt/c/Projects/cqs/src/cli/commands/infra/hook.rs:55-97` (`HookCommand::{Install,Uninstall,Fire,Status}`)
- **Description:** Project / Ref / Slot / Cache / Notes / Init / Doctor / Stats / Affected / Brief / Refresh / Ping / Status / Convert all flatten `TextJsonArgs` (audit IDs API-V1.22-2, API-V1.29-1/2, P3-25/26/30). Model and Hook are the holdouts and still ship inline `json: bool` per-variant. Two consequences: (1) every new shared output knob (e.g. `--pretty`, `--ndjson`) becomes a multi-file edit instead of one-line, (2) the top-level `cqs --json model show` propagation has to be hand-merged in the dispatcher (`cli.json || *json` per arm) instead of riding the shared resolver.
- **Suggested fix:** Replace the inline `json: bool` fields on every variant of `ModelCommand` and `HookCommand` with `#[command(flatten)] output: TextJsonArgs`, then collapse the per-arm `cli.json || *json` to read `cli.json || output.json`. Non-breaking — the user-facing `--json` flag stays.
- **Tag:** non-breaking

#### API-V1.38-2: `ProjectCommand::Search` duplicates `query/limit/threshold` instead of flattening `SearchArgs`
- **Difficulty:** medium
- **Location:** `/mnt/c/Projects/cqs/src/cli/commands/infra/project.rs:85-97`
- **Description:** `ProjectCommand::Search` defines its own bare `query: String`, `limit: usize`, `threshold: f32` fields. CQ-V1.25-1/4 already extracted `SearchArgs` as the single source of truth for every search knob (21 fields: `--rrf`, `--name-boost`, `--include-type`, `--exclude-type`, `--pattern`, `--include-docs`, `--reranker`, `--splade*`, `--expand-parent`, `--no-demote`, `--no-stale-check`, `--lang`, `--path`). Cross-project search silently drops every one of those — agents who learn `cqs scout foo --reranker onnx` and reach for `cqs project search foo --reranker onnx` get an "unexpected argument" error, with no signal that cross-project search is a different surface. Item 2 of #1459 ("project / ref verb consolidation") is the umbrella but the field-level duplication is the concrete bug. **Bonus**: `threshold` here lacks `value_parser = parse_finite_f32`, so NaN/Inf bypass the validator that protects every other threshold flag (AC-V1.29-5).
- **Suggested fix:** Replace inline fields with `#[command(flatten)] args: SearchArgs` (the same pattern `Commands::Search { args }` already uses). Pipe `args` through `search_across_projects` so cross-project search honors filters, name boost, RRF, reranker, etc. If full parity is too risky in one PR, at minimum (a) wire `value_parser = parse_finite_f32` on `threshold`, (b) add the missing `--lang` / `--include-type` / `--exclude-type` / `--rrf` / `--reranker` knobs as a quick-fix.
- **Tag:** non-breaking (additive flags); breaking if shared `SearchArgs` semantics differ at handler

#### API-V1.38-3: `BatchProvider` trait lacks `Send + Sync` bounds while sibling traits require them
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/llm/provider.rs:115`
- **Description:** `pub trait BatchProvider {` has no auto-trait bounds, but every sibling trait does: `VectorIndex: Send + Sync` (`/mnt/c/Projects/cqs/src/index.rs:32`), `IndexBackend: Send + Sync` (`index.rs:127`), `Reranker: Send + Sync` (`reranker.rs:187`). The result is asymmetric: `Arc<dyn Reranker>` can move across threads, `Box<dyn BatchProvider>` (the actual return type from `create_client`, see `llm/mod.rs:415`) can't. The Anthropic provider is fine across threads (HTTP client is `Send + Sync`); the Local provider's worker pool fan-out already implies `Send + Sync` at the impl level. Today nothing tries to hold the trait object across an `await` or `spawn`, but the next async-batch / parallel-validation refactor will — and the failure will be a confusing object-safety error rather than an explicit compiler complaint.
- **Suggested fix:** `pub trait BatchProvider: Send + Sync {`. Both shipping impls (`LlmClient` in `llm/batch.rs` and `LocalProvider` in `llm/local.rs`) already satisfy it; `MockBatchProvider` in `llm/provider.rs:186` is `#[cfg(test)]` and trivially `Send + Sync`.
- **Tag:** non-breaking

#### API-V1.38-4: `LocalProvider::submit_batch` skips `validate_model` despite the trait contract
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/llm/local.rs:830-860` vs trait doc at `/mnt/c/Projects/cqs/src/llm/provider.rs:160-164`
- **Description:** `BatchProvider::validate_model`'s docstring (EXT-V1.36-1 / P3) says it "is called from `submit_batch` *before* the API roundtrip so a wrong-provider/model combo (e.g. `--provider anthropic --model gpt-4o`) fails fast with the offending name in the error instead of surfacing as an opaque API error." `LlmClient::submit_batch` (`llm/batch.rs:362`) honors this: `self.validate_model(&self.llm_config.model)?;` is the first line. `LocalProvider::submit_batch` doesn't call it at all — the dispatch table goes straight into `submit_via_chat_completions`. A future provider-validation tightening (e.g. local provider rejecting empty model name) will silently fall on the floor for every Local user.
- **Suggested fix:** Either (a) add `self.validate_model(&self.config.model)?;` as the first line of `LocalProvider::submit_batch`, or (b) hoist the `validate_model` call into a default `submit_batch_validated` template method in the trait that impls override only for the actual submission, removing the foot-gun entirely.
- **Tag:** non-breaking

#### API-V1.38-5: `Cli::resolved_model` is `pub` instead of `pub(super)`
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/cli/definitions.rs:314-315`
- **Description:** The field is `#[arg(skip)] pub resolved_model: Option<cqs::embedder::ModelConfig>` — set by `dispatch::resolve_model`, read by handlers via `cli.try_model_config()`. The `pub` visibility is broader than necessary: every other `Cli` field is `pub` because `clap::Parser` requires it, but `resolved_model` is `#[arg(skip)]` and nothing outside this binary crate touches it. The comment "set by dispatch, not CLI" implies the author knew it shouldn't be poked externally; the type system isn't enforcing that. (Lower-stakes than the inline-`json` arms because `Cli` itself is binary-only.)
- **Suggested fix:** Change to `pub(super) resolved_model: Option<...>` so only `cli/dispatch.rs` and `cli/definitions.rs` can write it; readers go through `try_model_config()` which is already `pub`. Confirm `cli/dispatch.rs` is in the same `super` (it is — `crate::cli`).
- **Tag:** non-breaking

#### API-V1.38-6: Top-level `Cli` search flags silently ignored when subcommand is given
- **Difficulty:** medium
- **Location:** `/mnt/c/Projects/cqs/src/cli/definitions.rs:155-289` (limit/threshold/name_boost/lang/include_type/exclude_type/path/pattern/name_only/rrf/include_docs/reranker/splade/splade_alpha/expand_parent/ref_name/include_refs/tokens/no_stale_check/no_demote/model)
- **Description:** Top-level Cli has 20+ search-shaped flags that exist for the bare `cqs <query>` shorthand. They're NOT marked `global = true` (only `--slot` is). Two failure modes: (1) `cqs scout foo --rrf` → clap rejects "unexpected argument" because `ScoutArgs` doesn't carry `--rrf`. (2) `cqs --rrf scout foo` → parses successfully, scout's handler doesn't read `cli.rrf`, the flag is silently dropped. I just verified mode (2): scout's `cmd_scout` (`cli/commands/search/scout.rs:42`) takes `task / limit / json / max_tokens` only — every other top-level search flag is dead air. Multiple commands have the same shape: `gather`, `where`, `task`, `plan`, `onboard`, `related` all have their own `*Args` structs that exclude `--rrf`, `--name-boost`, `--include-docs`, `--reranker`, etc. An agent who learns the bare-query flags can't transfer them to any command that wraps a search.
- **Suggested fix:** Two options. **(a)** Mark search-shaping flags `global = true` and have all search-wrapping commands' inner `cmd_*` read them from `cli` instead of args (this matches how `--slot` works today). **(b)** Promote the `--rrf` / `--include-docs` / `--reranker` / `--name-boost` / `--no-demote` knobs into a shared `SearchKnobsArgs` struct and flatten it into `ScoutArgs / GatherArgs / WhereArgs / TaskArgs / PlanArgs / OnboardArgs / RelatedArgs`. Path (b) is the cleaner long-term shape but bigger surface; (a) closes the silent-drop today. Even just an explicit warn-on-ignored at handler boundary would prevent silent-drop.
- **Tag:** non-breaking (additive)

#### API-V1.38-7: `ExportModel.dim` is `Option<u64>` while every other dim field is `usize`
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/cli/definitions.rs:781-782`
- **Description:** `ExportModel { dim: Option<u64>, ... }` — every other place dim is plumbed it's `usize`: `ModelConfig.dim: usize` (`embedder/models.rs:157`), `EmbeddingConfig.dim: Option<usize>` (`models.rs:863`), `VectorIndex::dim() -> usize`, `Embedding::len() -> usize`. The `u64` here serves no purpose — embedding dim is a small integer (768/1024 today, not even close to `u32::MAX`) and the export-model command writes it into a TOML file as a string. Passing through `as usize` at the handler boundary obscures the fact that the rest of the codebase agrees on the type.
- **Suggested fix:** Change `dim: Option<u64>` → `dim: Option<usize>` and drop the `as usize` casts at the handler. Compatible — `--dim 768` parses identically.
- **Tag:** non-breaking

#### API-V1.38-8: Two flags for the same semantic — `--wait-secs` (status) vs `--require-fresh-secs` (eval)
- **Difficulty:** easy
- **Location:** `/mnt/c/Projects/cqs/src/cli/definitions.rs:892-895` (`Status.wait_secs`), `/mnt/c/Projects/cqs/src/cli/commands/eval/mod.rs:94-95` (`EvalCmdArgs.require_fresh_secs`), comment at `definitions.rs:889-891` already notes the duplication
- **Description:** Both flags wait for `cqs watch --serve` to report `state == fresh` and bound the wait. `cqs status --wait-secs 30` and `cqs eval --require-fresh-secs 600` use different spellings for the same semantic. The comment "Note: `cqs eval --require-fresh-secs` has the same semantics; default differs by use case" acknowledges this — but two flag names is exactly the muscle-memory cost #1459 / `--depth` vs `--commits` vs `--max-depth` was filed to clean up. Agents that learn one don't transfer to the other.
- **Suggested fix:** Pick one canonical spelling (`--wait-secs` is shorter and matches the `cqs status --wait` semantic of "block until ready"; `--require-fresh-secs` is more self-documenting in CI logs). Add the other as a `visible_alias` so existing scripts keep working. Defaults can stay command-specific (30 for status, 600 for eval).
- **Tag:** non-breaking (with alias)

#### API-V1.38-9: `EmbedderError::HfHub(String)` and `RerankerError::ModelDownload(String)` are sibling stringified errors
- **Difficulty:** medium
- **Location:** `/mnt/c/Projects/cqs/src/embedder/mod.rs:56-57`, `/mnt/c/Projects/cqs/src/reranker.rs:142-143`
- **Description:** Two error variants in two separate enums both wrap a String description of "the HuggingFace fetch failed". Both flow through `aux_model::resolve` (the shared resolver). The reranker variant is named `ModelDownload`, the embedder variant `HfHub` — same root cause, different display string ("Model download failed: …" vs "HuggingFace Hub error: …"), different name. Keeps the two error pipelines from sharing rendering / retry logic. Smaller pain than the API-V1.36 `IndexBackendError` collapse but the same shape.
- **Suggested fix:** Promote `aux_model::ResolveError` (which already exists internally per the `.map_err` calls) to a `pub` shared error and have both `EmbedderError` and `RerankerError` `#[from]` it: `#[error(transparent)] AuxModel(#[from] crate::aux_model::ResolveError)`. Both display strings collapse to the inner error's. Drop both stringly-typed variants.
- **Tag:** breaking (variant rename, but `pub use` aliases can preserve the name in display strings)

#### API-V1.38-10: `LimitArg` flattened in 6 places, but inline `limit: usize` still exists in 7 sister args
- **Difficulty:** easy
- **Location:** Inline: `SearchArgs.limit` (`args.rs:122`), `GatherArgs.limit` (`args.rs:271`), `ScoutArgs.limit` (`args.rs:315`), `RelatedArgs.limit` (`args.rs:473`), `WhereArgs.limit` (`args.rs:521`), `PlanArgs.limit` (`args.rs:530`), `TaskArgs.limit` (`args.rs:546`). Flattened: `ImpactArgs / TraceArgs / OnboardArgs / ExplainArgs / TestMapArgs / DepsArgs / CallersArgs` — all use `#[command(flatten)] limit_arg: LimitArg`.
- **Description:** Task A3 (`args.rs:101-106`) defined `LimitArg` to "standardise `--limit` across every graph subcommand" but stopped at the graph commands. The 7 search-shaped commands still inline `#[arg(short = 'n', long, default_value = "5")] pub limit: usize`. The default is consistent (5 across all), the field is identical — but a future change to the cap (e.g. adding `value_parser` to reject `0`, or bumping default to 10) needs a 7-file edit instead of one-line. **Concrete bug**: only `SearchArgs.limit` would benefit from a `value_parser = parse_nonzero_usize` (search with `--limit 0` is meaningless), but adding it requires editing all 7 inline copies.
- **Suggested fix:** Replace inline `limit: usize` in the 7 search-shaped args with `#[command(flatten)] limit_arg: LimitArg`, update handlers from `args.limit` → `args.limit_arg.limit`. While there, add `value_parser = parse_nonzero_usize` to `LimitArg.limit` so `--limit 0` is rejected at parse time across all 13 commands at once.
- **Tag:** non-breaking (handler-internal field rename)

## Summary

API-V1.38-1 (model/hook envelope) and API-V1.38-10 (LimitArg fan-out) are pure cleanup: low risk, immediately delete code, complete patterns that are already 80% rolled out. API-V1.38-2 (ProjectCommand::Search) and API-V1.38-6 (top-level search-flag ignore) are the highest-impact items — both surface the gap between "agent learns one search command" and "agent learns N near-twins"; both close real silent-drop / parse-rejection failure modes. API-V1.38-3 (BatchProvider Send+Sync) and API-V1.38-4 (validate_model) are trait-shape hygiene with one-line fixes; today nothing depends on them but the next async refactor will. API-V1.38-7 (`u64` dim) and API-V1.38-5 (`pub` resolved_model) are scope-tightening with no behavior change. API-V1.38-8 (`--wait-secs` vs `--require-fresh-secs`) is a renamed-flag muscle-memory item, alias-friendly. API-V1.38-9 (HF error variants) is medium-effort but matches the IndexBackendError collapse pattern that #1501 just landed — same architectural argument applies.

Most are **non-breaking**; only API-V1.38-9 is breaking (variant rename), and even that can be aliased. Stable order if doing a single PR: 1, 10, 7, 5, 3, 4, 8, 6, 2, 9.
