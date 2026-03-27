## Documentation

#### DOC-22: SECURITY.md still lists LoRA model as default
- **Difficulty:** easy
- **Location:** SECURITY.md:40-41
- **Description:** v1.5.0 switched the default model to `intfloat/e5-base-v2`, but SECURITY.md line 40 still says `Default: huggingface.co/jamie8johnson/e5-base-v2-code-search (LoRA fine-tune)` and line 41 says `Fallback: intfloat/e5-base-v2 (via CQS_EMBEDDING_MODEL env var)`. The default/fallback relationship is now inverted — base E5 is the default, and users can override with `CQS_EMBEDDING_MODEL` to use the LoRA model.
- **Suggested fix:** Swap default/fallback: `Default: intfloat/e5-base-v2` and `Override: jamie8johnson/e5-base-v2-code-search (LoRA fine-tune, via CQS_EMBEDDING_MODEL env var)`.

#### DOC-23: PRIVACY.md still lists LoRA model as default
- **Difficulty:** easy
- **Location:** PRIVACY.md:26-27
- **Description:** Same issue as DOC-22. Line 26 says `Default: jamie8johnson/e5-base-v2-code-search (LoRA fine-tune)` and line 27 says `Fallback: intfloat/e5-base-v2`. Inverted since v1.5.0.
- **Suggested fix:** Swap default/override as in SECURITY.md.

#### DOC-24: SECURITY.md/PRIVACY.md model download size stale (~547MB)
- **Difficulty:** easy
- **Location:** SECURITY.md:39, PRIVACY.md:28, src/cli/commands/init.rs:45
- **Description:** The LoRA model was ~547MB. The base E5 ONNX model (now default) is ~438MB based on actual blob size in the HuggingFace cache. SECURITY.md, PRIVACY.md, and `init.rs` all still say "~547MB". Users on metered connections see a misleading size estimate.
- **Suggested fix:** Update all three locations to `~438MB` (or `~440MB` for a round number).

#### DOC-25: README Retrieval Quality table header says "E5-base-v2 LoRA (cqs)"
- **Difficulty:** easy
- **Location:** README.md:574
- **Description:** The Retrieval Quality comparison table header says `E5-base-v2 LoRA (cqs)`. Since v1.5.0, the default model is base E5 (not LoRA). The 92.7% R@1 number in the table is from the enriched hard eval (base E5 + contrastive summaries), not from the LoRA model. The header should reflect the actual model.
- **Suggested fix:** Change header to `E5-base-v2 (cqs)` or `E5-base-v2 + enrichment (cqs)`.

#### DOC-26: ROADMAP.md says "Current: v1.4.2" but Cargo.toml is v1.5.0
- **Difficulty:** easy
- **Location:** ROADMAP.md:3
- **Description:** `ROADMAP.md` line 3 says `## Current: v1.4.2` but `Cargo.toml` is at v1.5.0 and CHANGELOG has a `[1.5.0]` entry dated 2026-03-25. The roadmap was not updated for the v1.5.0 release.
- **Suggested fix:** Update to `## Current: v1.5.0` and add a summary line for v1.5.0 changes.

#### DOC-27: README TL;DR "89.1% Recall@1" ambiguous after model switch
- **Difficulty:** easy
- **Location:** README.md:5, README.md:398, README.md:544
- **Description:** Three places in README say "89.1% Recall@1 on confusable function retrieval (92.7% with full enrichment pipeline)". The 89.1% was the raw hard eval number for both base E5 and LoRA v7. With v1.5.0 shipping base E5 as default, the user-facing number should lead with what the user actually gets out of the box. Since `cqs index --llm-summaries` enables the enrichment pipeline that reaches 92.7%, the raw 89.1% is the baseline. This is technically accurate but could be clearer — consider noting "89.1% raw, 92.7% with LLM enrichment" to distinguish.
- **Suggested fix:** Low priority — the numbers are correct. Optionally clarify that 89.1% is without `--llm-summaries` and 92.7% is with it.

#### DOC-28: Cargo.toml description says "0.965 NDCG@10" — enriched eval shows 0.9624
- **Difficulty:** easy
- **Location:** Cargo.toml:6
- **Description:** Cargo.toml description says `0.965 NDCG@10`. The enriched hard eval (which is the v1.5.0 flagship metric) shows 0.9624 NDCG@10 per CLAUDE.md memory. The 0.965 was the pre-enrichment metric from an older eval run. The full-pipeline eval shows 0.9478 NDCG@10. Neither matches 0.965 exactly. The Cargo.toml metric should match one of the two measured values.
- **Suggested fix:** Use `0.9624 NDCG@10` (enriched hard eval) or `0.9478 NDCG@10` (full-pipeline eval) — pick whichever metric the project wants to lead with in crates.io.

## Error Handling

#### EH-23: `find_contrastive_neighbors` failure silently degrades all summaries to non-contrastive
- **Difficulty:** easy
- **Location:** src/llm/summary.rs:46-52
- **Description:** When `find_contrastive_neighbors` fails, the error is logged at `warn` level and the entire batch falls back to non-contrastive prompts (`neighbor_map` becomes empty HashMap). This is a correct recovery strategy, but the fallback is completely invisible to the caller — `llm_summary_pass` returns `Ok(count)` with no indication that summaries were generated without contrastive context. For a ~$0.38 Haiku batch, the user has no way to know their summaries are lower quality than expected.
- **Suggested fix:** Add a `degraded: bool` field to the return value or log at `warn` level with an actionable message (e.g., "Run `cqs index --llm-summaries` again to retry contrastive generation"). Alternatively, propagate the error and let the caller decide.

#### EH-24: `submit_or_resume` swallows `get_pending` error and returns empty results
- **Difficulty:** medium
- **Location:** src/llm/batch.rs:324-326
- **Description:** In `BatchPhase2::submit_or_resume`, when `batch_items` is empty and `get_pending(store)` returns `Err(e)`, the error is logged at `warn` but the function returns `Ok(HashMap::new())`. This means a store corruption that prevents reading the pending batch ID causes all pending results to be silently lost — the batch was submitted and completed on Anthropic's side, but cqs never fetches the results. The API cost is wasted.
- **Suggested fix:** Propagate the error when `batch_items` is empty and the sole purpose of the call is to check for pending batches. The caller can decide whether to continue. At minimum, escalate to `error!` level since this represents data loss.

#### EH-25: `submit_or_resume` discards pending batch on unknown status without logging the actual status
- **Difficulty:** easy
- **Location:** src/llm/batch.rs:351-358
- **Description:** When `check_batch_status` returns an `Err` or an unrecognized status, the code logs "Pending batch status unknown, submitting fresh" but does not include the actual status or error in the warning. If `check_batch_status` returned `Ok("processing")` (an unexpected but valid status), the user cannot tell from the log why their batch was abandoned.
- **Suggested fix:** Include the actual status or error in the log message. For the `Err` case, log the error. For the `_` match arm, log the status string. Also consider whether unrecognized statuses should default to "wait and retry" rather than "abandon and resubmit" — resubmitting duplicates the API cost.

#### EH-26: `cli/batch/mod.rs` `create_context` silently ignores missing index.db mtime
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:457-459
- **Description:** The `index_mtime` is obtained via `.ok()`, meaning if `std::fs::metadata` or `.modified()` fail, the mtime is `None`. This is used later to detect index staleness — if `index_mtime` starts as `None`, the batch context will never detect that the index was rebuilt mid-session, since it compares `current_mtime != stored_mtime` and `None != Some(t)` is always true, causing unnecessary store reloads on every command.
- **Suggested fix:** Log a warning when the initial mtime cannot be obtained. This is a diagnostic issue — the behavior is safe (over-reloading) but wastes time in batch/chat sessions.

#### EH-27: `store/helpers.rs` `content_hash` uses `try_get().unwrap_or_default()` — masks DB schema issues
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:128
- **Description:** `content_hash: row.try_get("content_hash").unwrap_or_default()` silently produces an empty string if the column is missing or has a type mismatch. Since `content_hash` is used as a cache key for LLM summaries and as an enrichment hash, an empty hash would cause all chunks to appear "already cached" or "already enriched" (empty string matches empty string), silently skipping LLM processing for the entire index.
- **Suggested fix:** Use `row.get("content_hash")` (panics on missing column, which is correct for a required schema field) or propagate the error. The `window_idx` on line 129 correctly uses `unwrap_or(None)` since it's an optional column — `content_hash` is not optional.

#### EH-28: `store/chunks/query.rs` metadata `unwrap_or_default()` hides missing schema version
- **Difficulty:** easy
- **Location:** src/store/chunks/query.rs:97-98
- **Description:** `model_name` and `created_at` use `.unwrap_or_default()` when reading from the metadata HashMap. If these keys are missing (corrupted metadata table), `IndexStats` reports empty strings for model name and creation date. For `model_name` this is benign, but empty `created_at` can confuse downstream logic that uses it for staleness detection.
- **Suggested fix:** Low priority. The `schema_version` parsing on line 103-110 already has proper error logging with `tracing::warn!`. Apply the same pattern to `model_name` and `created_at` for consistency, or accept the current behavior as intentional graceful degradation.

#### EH-29: `cli/display.rs` silently drops `read_context_lines` errors for context display
- **Difficulty:** easy
- **Location:** src/cli/display.rs:136, 163, 299, 327
- **Description:** Four call sites use `if let Ok((before, _)) = read_context_lines(...)` without an else branch. When context line reading fails (file deleted between indexing and display, permission error, etc.), the context is silently omitted. This is correct behavior for display — showing results without context is better than crashing — but there's no diagnostic output to help users understand why context is missing.
- **Suggested fix:** Low priority. Add `tracing::debug!` in the else branch for diagnosability. Not a bug, since omitting context is graceful degradation for a display-only feature.

#### EH-30: `train_data/bm25.rs` `.unwrap_or_default()` on missing document content
- **Difficulty:** easy
- **Location:** src/train_data/bm25.rs:128
- **Description:** In `BM25::top_k_negatives`, when a document hash from the BM25 index doesn't match any document in `self.docs`, the content defaults to an empty string via `.unwrap_or_default()`. This means the returned "hard negative" is an empty document — which is not a useful training example and could degrade fine-tuning quality if not filtered downstream.
- **Suggested fix:** Filter out results where content is empty, or skip hashes that don't match any document. The issue is minor since this code path is only used for training data generation (offline), not production search.

#### EH-31: `gather.rs` bridge search errors silently reduce result quality
- **Difficulty:** easy
- **Location:** src/gather.rs:605-612
- **Description:** In the cross-reference `gather` path, when a bridge search fails for a reference seed, the error is logged at `warn` and the seed is skipped (`None`). If multiple bridge searches fail (e.g., project store is corrupted), the gathered results will contain only reference seeds with no project code — but the caller has no way to know the results are degraded. The `GatherResult` has no degradation flag.
- **Suggested fix:** Add a `degraded: bool` or `bridge_failures: usize` field to `GatherResult` so the CLI layer can warn the user. This is analogous to the EH-18 finding from the v1.4.0 audit (which added `degraded` to `DiffImpactResult`).

## Code Quality

#### CQ-22: `index_pack` in task.rs duplicates `token_pack` in commands/mod.rs
- **Difficulty:** easy
- **Location:** src/cli/commands/task.rs:59-83, src/cli/commands/mod.rs:120-162
- **Description:** `index_pack` (task.rs) and `token_pack` (commands/mod.rs) implement the same greedy knapsack packing algorithm — sort by score descending, pack items until budget exceeded, preserve original order. The only differences: `index_pack` returns indices while `token_pack` returns items directly, and `token_pack` has a first-item-exceeds-budget debug log. Both use the same score+overhead budget logic and original-order preservation. Six calls to `index_pack` in `waterfall_pack` could use a unified implementation.
- **Suggested fix:** Generalize `token_pack` to also return indices (or expose the index-based variant), then delete `index_pack`. Alternatively, rewrite `index_pack` as a thin wrapper around `token_pack`.

#### CQ-23: LLM chunk scanning loop duplicated across summary.rs and hyde.rs
- **Difficulty:** medium
- **Location:** src/llm/summary.rs:54-138, src/llm/hyde.rs:43-114
- **Description:** Both `llm_summary_pass` and `hyde_query_pass` contain near-identical paged chunk scanning loops with the same 4-condition filter (cached, non-callable, too short, windowed), the same `queued_hashes` dedup, the same batch-full check, and the same counter tracking (cached/skipped). The filter conditions also appear a third time in `find_contrastive_neighbors` (summary.rs:206-217, minus the cache check). `doc_comments.rs` applies a similar filter with additional test/source checks. Any change to eligibility criteria must be replicated in 3-4 places.
- **Suggested fix:** Extract a shared `collect_eligible_chunks` iterator or function in `llm/mod.rs` that yields `(ChunkSummary, is_cached)` tuples after applying the common filter. Each pass calls it with its own purpose-specific cache lookup and batch item builder. Reduces ~160 lines of near-identical scanning to a single ~40-line function.

#### CQ-24: `open_project_store` and `open_project_store_readonly` are near-identical
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:27-40, src/cli/mod.rs:47-60
- **Description:** These two functions share identical logic (find root, resolve dir, check exists, error message) differing only in `Store::open` vs `Store::open_light`. 13 lines duplicated for a one-method-call difference. Called from 30+ command files.
- **Suggested fix:** Parameterize with a boolean or enum: `open_project_store_inner(light: bool)` and have the two public functions delegate.

#### CQ-25: LLM-generated doc comments add noise without value on trivial functions
- **Difficulty:** medium
- **Location:** Multiple files — highest density in src/cli/batch/handlers/*.rs, src/cli/commands/task.rs, src/gather.rs
- **Description:** The v1.4.0 audit's SQ-8 doc generation pass added verbose `/// # Arguments` / `/// # Returns` doc comments to many trivial functions where the signature is self-documenting. Examples: `GatherOptions::with_expand_depth(mut self, depth: usize) -> Self` has a 7-line doc comment that says "Sets the maximum depth... # Arguments * `depth` - The maximum nesting level... # Returns Returns `self` to allow method chaining." The batch handlers have ~15-line docs for 5-line dispatch wrappers. Approximately 100+ instances across 43 files. These add ~2000 lines of LLM-generated boilerplate that increases scrolling without aiding comprehension.
- **Suggested fix:** This is cosmetic, not a bug. For future `--improve-all` passes, consider filtering out builder methods and thin dispatch wrappers. Existing comments are harmless and removing them is not worth the churn. Note this for the doc generation heuristics rather than doing a cleanup.

#### CQ-26: `waterfall_pack` in task.rs is 150 lines of repetitive section packing
- **Difficulty:** medium
- **Location:** src/cli/commands/task.rs:125-281
- **Description:** `waterfall_pack` repeats the same pattern 6 times: build text representations, call `count_tokens_batch`, call `index_pack`, subtract from remaining budget. Each section differs only in: which `result` field to iterate, what text to format, what score function to use, and what weight to apply. The function is 150 lines but the core logic is ~15 lines repeated with different field accessors.
- **Suggested fix:** Extract a `pack_section` helper that takes a text builder closure, a score closure, and a budget weight. Would reduce `waterfall_pack` to ~40 lines of section declarations. Medium difficulty because the surplus-forwarding logic (unused budget from section N flows to section N+1) threads through each iteration.

#### CQ-27: `cli/mod.rs` is 2043 lines — CLI definition, dispatch, and tests in one file
- **Difficulty:** medium
- **Location:** src/cli/mod.rs (2043 lines)
- **Description:** `cli/mod.rs` contains the clap `Cli` struct definition (~320 lines of arg definitions), the `Commands` enum (~450 lines of subcommand definitions), the `run_with` dispatch function (~230 lines of match arms), utility functions, and ~1000 lines of CLI parsing tests. The file serves three distinct purposes: argument definition, dispatch routing, and parse testing. This is the largest file in the codebase and grows with every new command.
- **Suggested fix:** Move the `Commands` enum and `run_with` dispatch to a separate `cli/dispatch.rs`. Move tests to `cli/tests.rs` or `cli/mod_tests.rs`. The `Cli` struct, `OutputFormat`, and utility functions stay in `mod.rs`. This would split a 2043-line file into three focused ~600-line files. Medium effort because of the tight coupling between `Cli`, `Commands`, and `run_with`.

## Observability

#### OB-19: `compute_hints_batch` missing tracing span
- **Difficulty:** easy
- **Location:** src/impact/hints.rs:80
- **Description:** `compute_hints_batch` is a public function that performs batch hint computation (caller counts + test reachability) for multiple functions, including a full forward BFS via `test_reachability`. Its sibling `compute_risk_batch` (line 116) has a `tracing::info_span!` with `count = names.len()`, but `compute_hints_batch` has no span at all. Called from `scout.rs` and `cli/commands/impact.rs` where it processes 10+ functions per invocation. Without a span, batch hint computation time is invisible in trace output.
- **Suggested fix:** Add `let _span = tracing::info_span!("compute_hints_batch", count = names.len()).entered();` at function entry, matching the pattern of `compute_risk_batch`.

#### OB-20: `test_reachability` missing tracing span
- **Difficulty:** easy
- **Location:** src/impact/bfs.rs:164
- **Description:** `test_reachability` is a `pub(crate)` function that performs a forward BFS from all test nodes through the call graph — potentially the most expensive single computation in the impact module. It builds equivalence classes from test nodes, runs forward BFS per class, and aggregates counts. Called from both `compute_hints_batch` and `compute_risk_batch`. Neither the function itself nor the per-class BFS iterations emit any tracing. On a large codebase with thousands of tests, this can take seconds with no visibility into where time is spent.
- **Suggested fix:** Add `let _span = tracing::info_span!("test_reachability", test_count = test_names.len(), max_depth).entered();` at entry. Optionally add `tracing::debug!(equiv_classes = equivalence_classes.len(), "Test equivalence classes built");` after the grouping step to aid performance diagnosis.

#### OB-21: `embed_query` missing tracing span
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:433
- **Description:** `embed_query` is a high-traffic public function (called on every user search query) that has `tracing::trace!` for cache hit/miss but no `info_span!` or `debug_span!` at entry. Its companion `embed_documents` (line 411) has `tracing::info_span!("embed_documents", count = texts.len())`. When profiling query latency, `embed_query` time is invisible — only the inner `embed_batch` span appears, without the cache check overhead or the prefix construction being attributed to the query path.
- **Suggested fix:** Add `let _span = tracing::debug_span!("embed_query").entered();` at function entry (after the empty check). Use `debug` level since this is called per-query and `info` would be noisy in batch operations.

#### OB-22: `flush_enrichment_batch` missing tracing span
- **Difficulty:** easy
- **Location:** src/cli/enrichment.rs:268
- **Description:** `flush_enrichment_batch` is called repeatedly during the enrichment pass to embed and store batches of enriched chunks. Each call embeds 64 chunks and writes them to the store. The parent `enrichment_pass` has a span, but the individual flush calls have no sub-spans. When diagnosing slow enrichment, it's impossible to distinguish time spent in embedding vs. store writes within each batch.
- **Suggested fix:** Add `let _span = tracing::debug_span!("flush_enrichment_batch", count = batch.len()).entered();` at function entry. Debug level is appropriate since this is called many times per enrichment pass.

## API Design

#### AD-30: `cosine_similarity` returns `Option<f32>` but `full_cosine_similarity` returns bare `f32`
- **Difficulty:** easy
- **Location:** src/math.rs:13, src/math.rs:35
- **Description:** Two sibling cosine similarity functions in the same module use inconsistent return types for the same category of failure. `cosine_similarity` returns `Option<f32>` (None on dimension mismatch or non-finite result), while `full_cosine_similarity` returns `f32` (0.0 on error). Both functions handle the same error conditions (dimension mismatch, non-finite output) but signal failure differently. Callers must remember which convention each function uses. `full_cosine_similarity` logs at `warn` level on mismatch but `cosine_similarity` returns None silently. The dimension-check logic also differs: `cosine_similarity` requires exactly `EMBEDDING_DIM`; `full_cosine_similarity` only requires `a.len() == b.len()`.
- **Suggested fix:** Make `full_cosine_similarity` also return `Option<f32>`, returning `None` on dimension mismatch or zero-norm. The single caller in `diff.rs:173` already handles the 0.0 case — changing to `.unwrap_or(0.0)` preserves behavior. Alternatively, add a doc comment explaining the intentional divergence and when to use which function.

#### AD-31: `Blame --depth` overloads `-n` short flag with different semantics than all other commands
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:359
- **Description:** `Blame` defines `#[arg(short = 'n', long, default_value = "10")] depth: usize` where `-n` means "max commits to show." In every other command that uses `-n` (`Similar`, `Drift`, `Related`, `Where`, `Scout`, `Plan`, `Task`), it is short for `--limit` (max results). The `Blame` command also names this parameter `depth`, but elsewhere `--depth` means graph traversal depth (`Impact`, `TestMap`, `Onboard`). This is a double overload: `-n` means something different, and `depth` means something different.
- **Suggested fix:** Rename to `--limit` with `-n` short flag (consistent with all other commands) and update the help text to "Max commits to show." The parameter name `depth` should be reserved for graph traversal depth.

#### AD-32: `CQS_API_BASE` env var breaks the `CQS_LLM_*` prefix convention
- **Difficulty:** easy
- **Location:** src/llm/mod.rs:78, src/config.rs:117-118
- **Description:** The three LLM config env vars are: `CQS_LLM_MODEL`, `CQS_LLM_MAX_TOKENS`, and `CQS_API_BASE`. The third breaks the `CQS_LLM_` prefix pattern. The config file fields are consistent (`llm_model`, `llm_api_base`, `llm_max_tokens`) — only the env var is inconsistent. This makes it hard to discover all LLM-related env vars by prefix search.
- **Suggested fix:** Accept `CQS_LLM_API_BASE` as the primary env var, with `CQS_API_BASE` as a deprecated fallback (`CQS_LLM_API_BASE.or(CQS_API_BASE)`). No external users means no deprecation cycle needed — but the SEC-10 audit finding (v1.4.0) validated this env var, so updating tests and docs is required.

#### AD-33: Residual `to_json()` methods on types that already derive `Serialize` with `serialize_path_normalized`
- **Difficulty:** easy
- **Location:** src/impact/types.rs:32 (TestInfo), src/impact/types.rs:184 (RiskScore), src/where_to_add.rs:58 (FileSuggestion)
- **Description:** AD-28 in the v1.4.0 audit fixed the core issue (inconsistent path format between `Serialize` and hand-built `to_json()`). The fix added `#[serde(serialize_with = "crate::serialize_path_normalized")]` to all PathBuf fields. However, the manual `to_json()` methods were not removed — they now duplicate what `serde_json::to_value()` would produce. `TestInfo`, `RiskScore`, and `FileSuggestion` each have both `derive(Serialize)` and a manual `to_json()` that does the same thing. Callers in `task_to_json`, `impact_to_json`, and `diff_impact_to_json` still call `.to_json()` instead of `serde_json::to_value()`. This creates maintenance burden: any field added to these structs must be added in two places.
- **Suggested fix:** Remove `to_json()` from `TestInfo` and `FileSuggestion` (the `Serialize` derive produces identical output now). For `RiskScore`, the `to_json(&self, name: &str)` takes an external `name` parameter — this should become a method on `FunctionRisk` instead (which owns both `name` and `risk`). Update callers (`task_to_json`, `impact_to_json`, `diff_impact_to_json`) to use `serde_json::to_value()`.

#### AD-34: `score_candidate` takes 9 positional parameters with `#[allow(clippy::too_many_arguments)]`
- **Difficulty:** medium
- **Location:** src/search/scoring/candidate.rs:210-221
- **Description:** `score_candidate` takes 9 arguments: `embedding`, `query`, `name`, `file_part`, `filter`, `name_matcher`, `glob_matcher`, `note_index`, `threshold`. Both call sites (search/query.rs:156, search/query.rs:394) pass the same `filter`, `name_matcher`, `glob_matcher`, `note_index`, and `threshold` values for every candidate in a loop — these are effectively "scoring context" that doesn't change per-candidate. The `#[allow(clippy::too_many_arguments)]` suppresses the warning rather than addressing it.
- **Suggested fix:** Group the 5 stable parameters into a `ScoringContext` struct: `{ filter, name_matcher, glob_matcher, note_index, threshold }`. The function signature becomes `score_candidate(embedding, query, name, file_part, ctx)` — 5 args, no clippy suppression needed. Both call sites construct `ScoringContext` once before the loop.

#### AD-35: `TrainDataConfig` and `TrainDataStats` missing standard derives
- **Difficulty:** easy
- **Location:** src/train_data/mod.rs:58, src/train_data/mod.rs:70
- **Description:** `TrainDataConfig` and `TrainDataStats` are plain structs without `Serialize`, `Debug`, or `Clone` derives. The `Triplet` type in the same module has `derive(Debug, Serialize)`. `TrainDataStats` contains useful post-run metrics (total_triplets, repos_processed, commits_skipped, language_counts) that could be output as JSON for pipeline integration. Currently the caller in `cli/commands/train_data.rs` manually formats these with `eprintln!`. `TrainDataConfig` lacks `Debug`, making it invisible in tracing spans.
- **Suggested fix:** Add `#[derive(Debug)]` to both and `#[derive(serde::Serialize)]` to `TrainDataStats`. Not critical since train-data is an offline tool, but it aligns with the convention used by every other result type in the codebase.

#### AD-36: `llm::Client` name collides with common HTTP client naming
- **Difficulty:** easy
- **Location:** src/llm/mod.rs:96
- **Description:** The LLM client is named just `Client`, which collides with `reqwest::blocking::Client` (used internally) and is ambiguous when imported from crate root. Every other major type in cqs has a descriptive name: `Store`, `Parser`, `Embedder`, `Reranker`, `HnswIndex`. A bare `Client` in `cqs::llm::Client` doesn't communicate what it's a client *for*. Currently only used within the `llm` module (not exported in `lib.rs`), so the collision is latent — but `llm` is `pub mod` so external code could access it.
- **Suggested fix:** Rename to `LlmClient`. The module prefix already establishes the namespace, but `LlmClient` is more self-documenting in error messages, tracing spans, and grep results. Internal-only change — no external API breakage since there are no external users.

## Algorithm Correctness

#### AC-16: Waterfall budget `remaining` tracking inconsistent across sections
- **Difficulty:** medium
- **Location:** src/cli/commands/task.rs:156-167
- **Description:** The waterfall budget tracking uses two different formulas to subtract from `remaining`. Section 1 (scout) uses `remaining.saturating_sub(scout_used)` — straightforward, charges actual usage. Section 2 (code) uses `remaining.saturating_sub(code_used.min(code_budget))` — capping the subtraction at the allocated budget. Sections 3 (impact) and 4 (placement) also use `.min(budget)` capping. This inconsistency means: if section 1 overshoots its budget (the `index_pack` first-item guarantee allows a single item to exceed budget), the overshoot correctly reduces `remaining`. But if section 2 overshoots its budget, the overshoot is NOT subtracted from `remaining` because `code_used.min(code_budget)` caps the subtraction. This means the total across all sections can exceed the original budget. Example: budget=1000, code_budget=500, code_used=700 → only 500 subtracted from remaining, but 700 tokens were actually consumed. The `total_used` sum at the end (line 257) will correctly report the overshoot, but downstream sections were allocated budget that was already spent.
- **Suggested fix:** Use the same formula for all sections: `remaining = remaining.saturating_sub(actual_used)`. The surplus forwarding already handles unused budget — it adds `section_budget.saturating_sub(section_used)` to the next section's allocation. Capping subtraction at budget is double-counting: the surplus is forwarded AND the overshoot is not charged.

#### AC-17: `test_reachability` equivalence class loses test-node self-reachability
- **Difficulty:** easy
- **Location:** src/impact/bfs.rs:198-203
- **Description:** When test A calls test B (common in test helpers), the equivalence class optimization skips counting test A as reachable from test B and vice versa. The BFS seeds from the first-hop callees at depth 1, excluding the test node itself from the visited set. In the non-optimized version (pre-PERF-23), each test's own node would not appear in its own BFS either, since forward BFS starts from the test node's callees. So this is actually consistent. However, there's a subtle issue: if two tests A and B have the same callee set {C}, they form one equivalence class of size 2. The BFS visits C and everything C calls. But if test A also appears as a callee of C (cyclic: A -> C -> A), the BFS would count A as reachable, and multiply by class_size=2 — meaning both test A and test B count as reaching test A. In the non-optimized version, only test B's BFS would reach A (through C), giving count=1. The equivalence class incorrectly inflates the count by class_size when cycles exist between test nodes and their callees. In practice this is rare (test functions rarely form cycles), and reachability counts are used for risk scoring ratios capped at 1.0, so the practical impact is minimal.
- **Suggested fix:** Low priority due to rarity. If fixing: after BFS, subtract from counts any test node that appears in the visited set AND belongs to the current equivalence class, adjusting the multiplied count accordingly. Alternatively, document that the equivalence class optimization assumes acyclic test-to-callee graphs.

#### AC-18: `sanitize_fts_query` can produce empty tokens from stripped words
- **Difficulty:** easy
- **Location:** src/store/mod.rs:148-166
- **Description:** `sanitize_fts_query` strips FTS5 special characters (`"*()+-^:`) from each word, then joins with spaces. If a word consists entirely of special characters (e.g., `"+"`, `"**"`, `"--"`), the stripped result is an empty string that still gets added to the output (preceded by a space). This produces double spaces or a leading space. While FTS5 handles extra whitespace gracefully (it tokenizes on whitespace), the output is technically malformed. More importantly, a query like `"++ OR --"` filters out `"OR"` (boolean operator) but the remaining `"++"` and `"--"` strip to empty strings, producing `" "` — a whitespace-only query that gets passed to FTS5 MATCH. FTS5 returns an error on empty/whitespace MATCH queries. The caller in `finalize_results` (query.rs:210) already guards against empty `sanitized` — but the guard checks `sanitized.is_empty()`, which is false for `" "`.
- **Suggested fix:** Trim the output, or skip empty words after stripping: change the extend logic to skip words that are empty after character filtering. A simple fix: after the filter loop, add `.trim()` to the output, or check `if stripped_word_is_empty { continue }` before appending.

#### AC-19: `parent_boost` matches container by name but not by file — cross-file name collisions
- **Difficulty:** easy
- **Location:** src/search/scoring/candidate.rs:82
- **Description:** `apply_parent_boost` looks up the container by checking if `parent_counts.get(&r.chunk.name)` matches. The `parent_type_name` field on child methods contains the unqualified type name (e.g., `"CircuitBreaker"`). The container chunk's `name` field also contains the unqualified name. If two classes in different files have the same name (e.g., `models/CircuitBreaker` and `legacy/CircuitBreaker`), and methods from BOTH appear in search results with `parent_type_name = "CircuitBreaker"`, BOTH container chunks get boosted by the combined child count — even though they're unrelated. The boost formula uses the total count of ALL children with that parent name, regardless of file. A class with 1 method could get a 4-child boost if a same-named class in another file has 3 methods in results.
- **Suggested fix:** Key the `parent_counts` map by `(parent_type_name, file)` instead of just `parent_type_name`. When matching containers, compare both name and file. This requires adding the file to the parent_counts key, which is available from `r.chunk.file`.

## Extensibility

#### EX-23: `doc_format_for` is a standalone match on Language — not part of LanguageDef
- **Difficulty:** medium
- **Location:** src/doc_writer/formats.rs:42-155
- **Description:** `doc_format_for` uses a 100-line `match language` to select the doc comment format (prefix, line prefix, suffix, insertion position). Every other language-specific behavior has been moved into `LanguageDef` fields: signatures, test markers, structural matchers, common types, entry points, etc. Doc format is the last major holdout. Adding language #52 requires editing `formats.rs` in addition to the language module file. The `LanguageDef` struct has 24 fields — this would be #25, keeping the "one line + one file" addition workflow.
- **Suggested fix:** Add a `doc_format: Option<DocFormat>` field to `LanguageDef` (None = default `//`-style). Each language module returns its format in `definition()`. `doc_format_for` becomes a one-liner: `lang.def().doc_format.unwrap_or(DEFAULT_DOC_FORMAT)`. Eliminates the match and centralizes all language config.

#### EX-24: `build_doc_prompt` language-specific appendix is a hardcoded match on 6 language strings
- **Difficulty:** easy
- **Location:** src/llm/prompts.rs:70-78
- **Description:** `build_doc_prompt` has a `match language { "rust" => ..., "python" => ..., "go" => ..., "java" => ..., "csharp" => ..., "typescript" | "javascript" => ..., _ => "" }` for adding language-specific doc conventions to the LLM prompt. This matches on raw strings (not `Language` enum) and only covers 7 of 51 languages. Languages like Ruby (YARD), Elixir (`@doc`), Haskell (Haddock), Perl (POD), Erlang (edoc), R (roxygen2) all have doc conventions but get the empty generic appendix. The knowledge already exists in `doc_format_for` — it's just not connected to the prompt builder.
- **Suggested fix:** Add a `doc_convention_hint: &'static str` field to `LanguageDef` (e.g., `"Use # Arguments, # Returns, # Errors sections as appropriate"` for Rust). The prompt builder reads `lang.def().doc_convention_hint`. Languages without conventions use `""`. Moves knowledge into the language module where it belongs and covers all 51 languages.

#### EX-25: `nl.rs` field extraction and method name extraction hardcoded for 6 languages
- **Difficulty:** medium
- **Location:** src/nl.rs:765-789, src/nl.rs:868-909
- **Description:** Two functions in `nl.rs` have `match language` blocks covering only Rust, Go, Python, TypeScript/JavaScript, and Java/C# — then fall through to `_ => None` for 45+ other languages. `extract_member_fields` (line 765) parses struct/class field names for NL description enrichment. `extract_method_name_from_line` (line 868) parses method declarations for member-method enrichment. Languages like Ruby (`attr_accessor :name`), Kotlin (`val name: Type`), Swift (`var name: Type`), Scala (`val name: Type`), etc. all have field/method patterns but get no NL enrichment. This means struct/class NL descriptions for those languages are less descriptive, reducing search quality.
- **Suggested fix:** Add `extract_field_name: Option<fn(&str) -> Option<&str>>` and `extract_method_name: Option<fn(&str) -> Option<String>>` to `LanguageDef`. The NL module calls `lang.def().extract_field_name` with fallback to the current generic logic. Each language module implements its own field/method patterns. Alternatively, add patterns for the remaining major languages (Ruby, Kotlin, Swift, Scala, Lua, PHP) directly in the match — simpler but doesn't scale.

#### EX-26: LLM module is tightly coupled to Anthropic's API — no provider abstraction
- **Difficulty:** hard
- **Location:** src/llm/mod.rs:55-65, src/llm/batch.rs:1-60
- **Description:** The LLM module hardcodes Anthropic's Messages API and Batches API at every level: the request/response types (`MessagesRequest`, `BatchRequest`, `BatchResponse`), the authentication header (`x-api-key`, `anthropic-version`), the batch ID format (`msgbatch_`), the endpoint paths (`/messages`, `/messages/batches`), and the API version (`2023-06-01`). While `CQS_API_BASE` allows redirecting the base URL, the request format is Anthropic-specific — pointing it at an OpenAI-compatible endpoint would fail. Switching to a different LLM provider (OpenAI, local models) would require rewriting most of the `llm/` module. The `Client` struct has no trait abstraction that a second provider could implement.
- **Suggested fix:** This is a design decision, not necessarily a bug. The project uses a single LLM provider (Anthropic) and the Batches API is Anthropic-specific. An abstraction layer would add complexity for a provider switch that may never happen. If provider flexibility is desired: extract an `LlmProvider` trait with `submit_batch`, `check_batch`, `fetch_results` methods, and implement `AnthropicProvider`. The `Client` becomes generic over the provider. Low priority unless provider switching is a real requirement.

#### EX-27: `EMBEDDING_DIM` is a compile-time constant — model swap requires recompilation
- **Difficulty:** easy
- **Location:** src/lib.rs:215
- **Description:** `pub const EMBEDDING_DIM: usize = 768` is a compile-time constant used in 30+ locations across `embedder`, `hnsw`, `cagra`, `math`, and `store`. The `CQS_EMBEDDING_MODEL` env var allows swapping the ONNX model at runtime, but if the replacement model has a different dimension (e.g., E5-large at 1024-dim, MiniLM at 384-dim), the constant doesn't match and embedding operations will panic or produce garbage. The dimension is validated at embedding creation time (`Embedding::new` checks `data.len() != EMBEDDING_DIM`), so a dimension mismatch is caught — but the error message says "expected 768" regardless of the actual model dimension. There's no runtime detection of the model's actual output dimension.
- **Suggested fix:** Low priority as long as E5-base-v2 is the only supported model. For future model flexibility: read the dimension from the ONNX model metadata at session initialization time and store it on the `Embedder` struct. The compile-time constant becomes a default/assertion target. Store and HNSW validate against the embedder's reported dimension rather than the constant. This is a significant refactor since `EMBEDDING_DIM` is used in 30+ locations including HNSW layer construction.

#### EX-28: 30+ `const BATCH_SIZE` definitions scattered with no central tuning point
- **Difficulty:** easy
- **Location:** Multiple — src/store/chunks/query.rs (4), src/store/calls/query.rs (3), src/store/chunks/staleness.rs (2), src/store/chunks/embeddings.rs (2), src/store/chunks/crud.rs (1), src/store/chunks/async_helpers.rs (2), src/store/calls/related.rs (1), src/store/calls/dead_code.rs (1), src/cli/pipeline.rs (2), src/cagra.rs (1), src/diff.rs (1), src/cli/commands/index.rs (1)
- **Description:** There are 21+ separate `const BATCH_SIZE` definitions across the codebase, each tuned for a specific SQL operation. Values range from 20 (chunk content lookups) to 10,000 (HNSW batch insert, CAGRA build). Each is an inline `const` in the function that uses it, with no central catalog. While the values are individually reasonable (most driven by SQLite's 999-parameter limit: `BATCH_SIZE * params_per_row < 999`), tuning for different hardware profiles (e.g., embedded devices with less memory, or servers with large page caches) requires finding and editing 21+ separate locations.
- **Suggested fix:** Low priority — the values are well-chosen and rarely need changing. If centralization is desired: group related batch sizes into a `mod batch_sizes` in the store module with named constants (e.g., `SQLITE_CHUNK_QUERY: usize = 500`, `SQLITE_CALL_QUERY: usize = 250`, `EMBED_BATCH: usize = 32`). The SQLite ones are genuinely constrained by the 999-parameter limit, so documenting the formula (`batch_size * params_per_row < 999`) in one place would help. Not worth the churn unless tuning is needed.

## Robustness

#### RB-15: `find_contrastive_neighbors` panics on mismatched embedding dimensions
- **Difficulty:** easy
- **Location:** src/llm/summary.rs:252-253
- **Description:** The contrastive neighbor computation sets `dim = valid[0].2.len()` (the embedding dimension of the first chunk) and allocates an `Array2<f32>` of shape `(n, dim)`. The inner loop `for (j, &v) in emb.iter().enumerate() { row[j] = v; }` indexes into the row without checking that `emb.len() == dim`. If any embedding has more elements than the first, `row[j]` panics with an index-out-of-bounds when `j >= dim`. This can happen if the store contains embeddings from a model migration (e.g., some chunks embedded with a 768-dim model, others with a 1024-dim model before the old ones were re-embedded). The `store.get_embeddings_by_hashes()` call does not filter by dimension. While `Embedding::new` enforces `EMBEDDING_DIM` at creation time, corrupted or manually-inserted rows could bypass this.
- **Suggested fix:** Either filter `valid` to only entries where `emb.len() == dim`, or truncate/skip mismatched embeddings with a `tracing::warn!`. A simple guard: `if emb.len() != dim { tracing::warn!(hash, expected=dim, actual=emb.len(), "Skipping mismatched embedding"); continue; }` before the inner loop.

#### RB-16: `search_across_projects` double-unwrap panics if rayon thread pool build fails
- **Difficulty:** easy
- **Location:** src/project.rs:221
- **Description:** `rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().unwrap())` — the fallback builds a default thread pool and `.unwrap()`s. If thread pool construction fails for a systemic reason (e.g., ulimit on threads exhausted, OOM), the fallback will fail for the same reason and panic. The intent is "try 4 threads, fall back to default" but the error path is not actually recoverable — the second `.unwrap()` panics without any diagnostic message.
- **Suggested fix:** Propagate the error: `let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().or_else(|_| rayon::ThreadPoolBuilder::new().build()).map_err(|e| ProjectError::Io(std::io::Error::other(format!("Thread pool: {e}"))))?;`. This lets the caller handle the error instead of panicking.

#### RB-17: `sanitize_fts_query` produces whitespace-only string for special-char-only queries
- **Difficulty:** easy
- **Location:** src/store/mod.rs:148-166
- **Description:** When a query consists entirely of FTS5 special characters (e.g., `"++"`, `"--"`, `"***"`), `sanitize_fts_query` strips them all but still returns a non-empty string (containing only spaces). The caller in `finalize_results` (query.rs:210) guards against empty queries with `if sanitized.is_empty()`, but `" "` is not empty — it passes the guard and gets sent to FTS5 MATCH, which returns an error on whitespace-only input. The error is caught and logged at `warn` level, and the search falls back to no FTS boosting — so this is a benign failure. But it creates unnecessary log noise for queries like symbol lookups (`"++"`).
- **Suggested fix:** Add `.trim()` to the output of `sanitize_fts_query`, or trim before the `is_empty` check in the caller. One-line fix: change `sanitized` to `sanitized.trim().to_string()` at the end of the function.

#### RB-18: `compute_modify_threshold` called with potentially empty `results` slice
- **Difficulty:** easy
- **Location:** src/scout.rs:254, src/scout.rs:357-389
- **Description:** `compute_modify_threshold` is called from `run_scout` with `&results` as input (line 254). Inside, it filters to non-test results and collects scores (line 360-362). If ALL results are test chunks, `scores` is empty, and `scores.first().copied().unwrap_or(f32::MAX)` returns `f32::MAX` — which means every chunk's score is below the threshold, so nothing is classified as a modify target. This is semantically correct (no modify targets when there are only test results) but the threshold value of `f32::MAX` is misleading in the debug log on line 255 (`modify_threshold=inf, "Gap-based threshold computed"`). More importantly, if `results` itself is empty (no search hits), the same `f32::MAX` is returned. The early return in `run_scout` checks for empty results (line 194), so the empty case is guarded — but if that guard were removed or bypassed, the downstream code would silently classify everything as non-modify.
- **Suggested fix:** Low priority — the behavior is correct. For clarity, return a named sentinel (e.g., `f32::INFINITY` with a comment "no modify targets") or add `tracing::debug!("No non-test results, threshold is unbounded")` when `scores` is empty.

#### RB-19: `apply_doc_edits` silently skips functions when file content changes between parse and write
- **Difficulty:** medium
- **Location:** src/doc_writer/rewriter.rs:258-296
- **Description:** `rewrite_file` reads the file content (line 244), re-parses it to get current chunk positions (line 258), then matches edits to chunks by function name (line 270-296). If the file was modified between the original LLM batch submission and this rewrite (e.g., user edited the file during `--improve-docs`), chunk names may no longer match and edits are silently skipped (line 275: `continue` on empty match). The function returns a count of successfully applied edits but there's no indication of how many were skipped or why. If many edits are skipped due to file changes, the user gets partial documentation with no diagnostic — they see "documented 5/20 functions" but don't know the other 15 failed due to stale file content.
- **Suggested fix:** Track skipped edit count and reason. Log at `warn` level when >20% of edits are skipped: `tracing::warn!(applied, skipped, total=edits.len(), "Many doc edits skipped — file may have changed since LLM batch")`. The function already returns `Result<usize>` — could return a struct with `applied` and `skipped` counts instead.

## Platform Behavior

#### PB-22: `is_test_chunk` uses forward-slash-only path splitting — fails on native Windows backslash paths
- **Difficulty:** easy
- **Location:** src/lib.rs:238, src/lib.rs:248-251
- **Description:** `is_test_chunk` extracts the filename via `file.rsplit('/').next()` and checks path patterns with `file.contains("/tests/")` and `file.starts_with("tests/")`. These work correctly in the current deployment (WSL + forward-slash normalized origins), but the function takes `&str` — not `&Path` — and has no normalization. If a caller passes a native Windows path (e.g., `src\tests\foo.rs`), the `/tests/` check fails, `rsplit('/')` returns the entire string as one component, and the filename-based patterns (`_test.`, `.test.`, `.spec.`) match against the full path instead of just the filename. The impact analysis (`impact/analysis.rs:325`) and where_to_add (`where_to_add.rs:762`) both call `is_test_chunk` with `file.to_string_lossy()` from `PathBuf`. On WSL, `PathBuf` uses forward slashes. On native Windows, it would use backslashes. This is a latent bug — currently safe because all callers run on WSL/Linux.
- **Suggested fix:** Either: (a) normalize slashes at the top of `is_test_chunk`: `let file = file.replace('\\', "/");`, or (b) document that `is_test_chunk` requires forward-slash paths and add a `debug_assert!(!file.contains('\\'))` guard (consistent with the PB-17 pattern used in `check_origins_stale`).

#### PB-23: `check_origins_stale` joins forward-slash origin with OS-native `root` — mixed separators on Windows
- **Difficulty:** easy
- **Location:** src/store/chunks/staleness.rs:212
- **Description:** `check_origins_stale` does `let path = root.join(&origin)` where `origin` is a forward-slash relative path from the DB (e.g., `src/lib.rs`). On Unix, `Path::join("src/lib.rs")` works correctly. On native Windows, `Path::join("src/lib.rs")` also works because Windows accepts forward slashes in most contexts. However, the resulting `PathBuf` on Windows is a hybrid: `C:\Projects\cqs\src/lib.rs` (backslash root + forward-slash origin). This hybrid path works for `metadata()` calls (Windows kernel accepts both separators), but if the path were later compared via string equality or used in display output, the mixed separators could cause confusion. Currently not a functional bug, but a latent inconsistency.
- **Suggested fix:** Low priority. Add a comment noting the mixed-separator behavior is intentional and safe for metadata checks. If native Windows support is added, normalize with `dunce::canonicalize` or `path.components().collect()`.

#### PB-24: `prune_missing` compares `PathBuf::from(origin)` with canonicalized `existing_files` — can miss on case-insensitive filesystems
- **Difficulty:** medium
- **Location:** src/store/chunks/staleness.rs:30
- **Description:** `prune_missing` builds `PathBuf::from(origin)` from the DB origin string and checks `existing_files.contains(&PathBuf::from(origin))`. The `existing_files` set comes from `enumerate_files`, which canonicalizes paths via `dunce::canonicalize`. On macOS (case-insensitive HFS+/APFS), canonicalization preserves the filesystem's canonical case: `dunce::canonicalize("src/MyFile.rs")` might return `src/myfile.rs` if the filesystem stored it in lowercase. But the DB origin stores the path as it was at indexing time — which might use different casing. If a file is renamed from `MyFile.rs` to `myFile.rs` (case-only rename on macOS), the old DB origin won't match the new canonical path, causing `prune_missing` to delete the file's chunks even though the file still exists. On Linux/WSL (case-sensitive), this is not an issue. macOS release binary is built in CI (GitHub Actions) but the primary deployment target is Linux/WSL.
- **Suggested fix:** Low priority (macOS edge case, case-only renames). If targeting macOS: canonicalize the DB origin before comparison, or use `std::fs::exists()` as a fallback when the HashSet lookup fails.

#### PB-25: Mtime comparison uses second-precision `as_secs()` — NTFS 100ns granularity can cause missed updates
- **Difficulty:** easy
- **Location:** src/store/chunks/staleness.rs:141, src/cli/pipeline.rs:394
- **Description:** When storing and comparing mtimes, the code uses `duration_since(UNIX_EPOCH).as_secs() as i64`, truncating sub-second precision. NTFS has 100-nanosecond mtime granularity. If a file is written twice within the same second (common with fast code formatters or batch operations), the second write has a different NTFS mtime but the same `as_secs()` value. The staleness check `current > stored` returns false, and the file appears fresh when it's actually stale. This is a practical issue on WSL: `cargo fmt` followed immediately by `cqs index` can produce same-second writes. The `as_secs()` truncation also means that `list_stale_files` uses `current > stored` (strictly greater), not `current != stored` — so even if sub-second precision were stored, a same-second write with different nanoseconds would be missed.
- **Suggested fix:** Store milliseconds instead of seconds: `d.as_millis() as i64`. The SQLite column is `INTEGER` and can hold millisecond timestamps without schema change. Update comparison to `current != stored` for exact mismatch detection. This is a low-risk change since over-detection (reporting fresh files as stale) is harmless but under-detection (missing stale files) causes incorrect search results.

#### PB-26: Watch mode `collect_events` uses `dunce::canonicalize` on potentially deleted files — silent skip
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:345-348
- **Description:** In `collect_events`, every event path is canonicalized via `dunce::canonicalize(path)`. For delete events (`notify::EventKind::Remove`), the file no longer exists, so `dunce::canonicalize` fails and the fallback `path.clone()` is used. The fallback path may not match the canonicalized `cqs_dir` or `notes_path` comparisons on lines 350 and 355, because those were canonicalized at startup. On WSL over NTFS, the non-canonicalized path might have different case or UNC prefix (`\\?\` on Windows, stripped by `dunce` on successful canonicalization). This means delete events for files in the `.cqs` directory might not be filtered out (line 350), and delete events for `notes.toml` might not trigger `pending_notes = true` (line 355). The practical impact is minor: spurious `.cqs` file events just fail extension filtering at line 364, and missing `notes.toml` delete events mean notes aren't re-synced until the next create/modify event.
- **Suggested fix:** For the `.cqs` directory check, also compare the non-canonicalized path against the non-canonicalized `cqs_dir`. Alternatively, check `path.starts_with(root.join(".cqs"))` as a fallback when canonicalization fails.

#### PB-27: `nl.rs` path splitting uses `/` only — NL descriptions degrade on backslash paths
- **Difficulty:** easy
- **Location:** src/nl.rs:694-696
- **Description:** `generate_nl_description` splits the file path on `/` to extract meaningful path components for NL descriptions. The split on line 694 (`s.split('/')`) does not handle backslashes. If a path contains backslashes (e.g., native Windows PathBuf passed via `to_string_lossy()`), the entire path becomes a single component, and the filtering of `skip` directories (tests, vendor, etc.) doesn't work — the "component" is `src\tests\helpers.rs` which doesn't match `"tests"`. The NL description would include the raw backslash path as a single token, degrading embedding quality. Currently latent on WSL (forward-slash paths), but would affect native Windows or any code path that doesn't normalize before calling NL generation.
- **Suggested fix:** Replace `s.split('/')` with `crate::normalize_slashes(&s)` before splitting, or split on both separators: `s.split(['/', '\\'])`.

#### PB-28: `markdown.rs` `extract_link_slug` splits on `/` only — backslash links in Windows-generated markdown
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:633
- **Description:** `extract_link_slug` uses `path_part.rsplit('/').next()` to extract the filename from a markdown link URL. Windows-generated markdown documentation sometimes uses backslash paths in links (e.g., `[Module](src\module.md)`). These would not be split, and the entire `src\module.md` would be treated as the filename. The `.md` suffix check still passes, but the "slug" includes the directory prefix with backslashes, producing an incorrect section name. This affects chunking of markdown files that were authored on Windows.
- **Suggested fix:** Split on both separators: `path_part.rsplit(['/', '\\']).next()`.

## Test Coverage

#### TC-24: `full_cosine_similarity` has zero direct unit tests
- **Difficulty:** easy
- **Location:** src/math.rs:35
- **Description:** `full_cosine_similarity` has no direct unit tests. Its sibling `cosine_similarity` (same module) has 11 tests including NaN/Inf/zero-norm/subnormal adversarial cases. `full_cosine_similarity` differs in three ways: (1) accepts arbitrary dimensions (not just `EMBEDDING_DIM`), (2) computes norms inline rather than relying on pre-normalization, (3) returns `f32` (0.0 on error) rather than `Option<f32>`. These differences are untested. The function is used in `diff.rs:173` for cross-store semantic diff — if NaN vectors reach it, the 0.0 fallback is silent. The comment at `diff.rs:222` claims "full_cosine_similarity tests are in math.rs (canonical location)" but no such tests exist.
- **Suggested fix:** Add tests in `math.rs::tests` for: (1) identical non-normalized vectors, (2) orthogonal vectors, (3) dimension mismatch, (4) empty vectors, (5) NaN/Inf input, (6) zero-norm vector. Mirror the adversarial test coverage of `cosine_similarity`.

#### TC-25: `enrichment_pass` and `compute_enrichment_hash_with_summary` have zero direct tests
- **Difficulty:** medium
- **Location:** src/cli/enrichment.rs:25, src/cli/enrichment.rs:229
- **Description:** The enrichment pass is a critical pipeline stage that re-embeds chunks with call graph context, LLM summaries, and HyDE predictions. `enrichment_pass` (200 lines) and its helper `compute_enrichment_hash_with_summary` (35 lines) have zero direct unit tests. The only test coverage is transitive through `tests/pipeline_eval.rs::test_stress_eval` (integration test requiring ONNX model download + embedding). Key untested behaviors: (1) IDF-based callee filtering (threshold 0.1), (2) enrichment hash determinism (callers/callees sorted before hashing), (3) skip-if-already-enriched path (hash comparison), (4) ambiguous name dedup (`name_file_count > 1` skip), (5) summary/hyde inclusion in hash. If the hash computation is non-deterministic (e.g., unsorted inputs), chunks get needlessly re-embedded on every run.
- **Suggested fix:** Extract `compute_enrichment_hash_with_summary` tests: verify determinism (same inputs = same hash), verify that changing summary/hyde/callers/callees changes the hash, verify callee IDF filtering. For `enrichment_pass`, a store-level integration test with a pre-populated store + mock embedder would cover the skip/re-embed logic.

#### TC-26: `generate_nl_with_call_context_and_summary` has zero direct tests
- **Difficulty:** easy
- **Location:** src/nl.rs:289
- **Description:** The `_and_summary` variant was added in v1.4.0 (SQ-6) to prepend LLM summaries and append HyDE predictions to NL descriptions. It has 72 transitive tests but zero direct tests. The base function `generate_nl_with_call_context` (line 268) is tested transitively through eval tests, but the summary/hyde prepend/append logic is untested in isolation. Specific behaviors that are not directly tested: (1) summary prepended before base NL, (2) empty summary has no effect, (3) hyde predictions appended as "Queries: ...", (4) empty hyde has no effect, (5) both summary and hyde together.
- **Suggested fix:** Add unit tests in `nl.rs::tests` exercising the summary/hyde paths. Use a minimal `Chunk` + `CallContext` and verify that the output contains "Queries:" when hyde is provided, that summary text appears before the base NL, etc.

#### TC-27: `BatchPhase2::submit_or_resume` error paths untested
- **Difficulty:** hard
- **Location:** src/llm/batch.rs:296-369
- **Description:** `submit_or_resume` has three error/fallback paths that are never exercised in tests: (1) `get_pending` returns `Err` (line 324-326) — silently returns empty HashMap, (2) pending batch has unknown status (line 351-358) — submits fresh without logging the actual status, (3) `set_pending` fails during `submit_fresh` (line 428-429) — warns but continues. All tests for this code path go through `llm_summary_pass` → `submit_or_resume`, which requires an API key and live Anthropic endpoint. No mock/stub tests exist for the orchestration logic. The error at (1) is particularly dangerous: if the store is corrupted and can't read the pending batch ID, a completed batch's results are silently lost.
- **Suggested fix:** Hard to test without HTTP mocking. At minimum, add a unit test that creates a `BatchPhase2` with a `get_pending` closure that returns `Err` and verify the behavior (currently returns `Ok(empty)`). If the project adds an HTTP mock layer (e.g., `mockito` or `wiremock`), the full submit/resume/poll cycle can be tested.

#### TC-28: `Bm25Index::select_negatives` missing hash lookup failure test
- **Difficulty:** easy
- **Location:** src/train_data/bm25.rs:122-129
- **Description:** In `select_negatives`, after filtering by hash and content guard, the final `.map()` (lines 122-129) does a linear search `self.docs.iter().find(|(h, _)| h == &hash)` to get content. If this find returns `None` (shouldn't happen in normal operation since the hash came from `self.docs`), `.unwrap_or_default()` returns an empty string. The existing 4 BM25 tests all use small corpora where find always succeeds. No test verifies the behavior when the hash→content lookup could theoretically fail, or when the corpus contains many identical-content documents (only one test for the content hash guard with 2 identical items).
- **Suggested fix:** Add a test with a larger corpus (10+ documents) including multiple identical-content pairs to stress the content hash guard. Also verify that the returned negatives never contain empty-string content (which would indicate a broken hash→content lookup).

#### TC-29: `full_cosine_similarity` zero-norm NaN path untested
- **Difficulty:** easy
- **Location:** src/math.rs:54-56
- **Description:** When both input vectors are all-zero, `denom` is `0.0 * 0.0 = 0.0`, and the function returns `0.0` (line 56). But if one vector is all-zero and the other contains `NaN`, `dot` is `NaN`, `norm_a.sqrt()` is `0.0`, and `denom` is `0.0 * NaN_sqrt = NaN`. The `denom == 0.0` check (line 55) evaluates to `false` for `NaN`, so it falls through to `dot / denom` = `NaN / NaN` = `NaN`. Then the `is_finite()` check on line 59 catches it and returns `0.0`. This path is correct but untested — the `cosine_similarity` adversarial tests exist but `full_cosine_similarity` has none.
- **Suggested fix:** Add tests for `full_cosine_similarity` with: zero-norm vector pair, zero-norm vs NaN, NaN vs normal, Inf vs normal. Each should verify the return is finite (0.0).

#### TC-30: `enrichment_pass` IDF callee filtering threshold untested
- **Difficulty:** easy
- **Location:** src/cli/enrichment.rs:36-43, src/nl.rs:321-325
- **Description:** The enrichment pass computes callee document frequency and filters callees appearing in >=10% of chunks. This 10% threshold (`callee_doc_freq >= 0.10` in `enrichment.rs:247` and `nl.rs:324`) is a critical tuning parameter that determines which callees are treated as "utility noise" (e.g., `unwrap`, `log`, `iter`) and suppressed from NL descriptions. Zero tests verify this filtering — the integration test in `pipeline_eval.rs` uses a tiny corpus where no callee reaches 10%. If the threshold were accidentally changed to 0.01 (filtering everything) or 1.0 (filtering nothing), no test would catch it.
- **Suggested fix:** Create a unit test for `generate_nl_with_call_context_and_summary` with a `callee_doc_freq` map where one callee is at 0.09 (included) and another at 0.11 (excluded). Verify the output contains the first but not the second.

## Security

#### SEC-14: `git_diff_tree` and `git_show` pass unvalidated SHA to subprocess
- **Difficulty:** easy
- **Location:** src/train_data/git.rs:89-96, src/train_data/git.rs:122-130
- **Description:** `git_diff_tree(repo, sha)` and `git_show(repo, sha, path)` pass the `sha` parameter directly to `git diff-tree` and `git show` as a positional argument without any validation. The SHA comes from `git_log` output parsing, so in normal operation it's a hex commit hash. However, if a crafted commit message or external caller supplied a value starting with `-` (e.g., `--exec=...`), it would be interpreted as a git flag. The `blame.rs` command already guards against this pattern (`rel_file.starts_with('-')` check at line 79), but `train_data/git.rs` does not. `git show` is particularly interesting because the `spec` is `format!("{}:{}", sha, path)` — a sha starting with `--` could break out of the positional context.
- **Suggested fix:** Add `debug_assert!` or early-return validation that `sha` matches `^[0-9a-fA-F]{7,40}$` (hex only, 7-40 chars). Same pattern as `is_valid_batch_id` — cheap guard against argument injection.

#### SEC-15: `CQS_API_BASE` accepts non-HTTPS URLs — API key sent in cleartext
- **Difficulty:** easy
- **Location:** src/llm/mod.rs:78-81
- **Description:** SEC-10 (v1.4.0 audit) fixed redirect-based key exfiltration by setting `redirect(Policy::none())`. However, `CQS_API_BASE` still accepts any URL scheme including `http://`. Setting `CQS_API_BASE=http://proxy.internal/v1` sends the `ANTHROPIC_API_KEY` in the `x-api-key` header over plaintext HTTP (visible to any network observer). The `LlmConfig::resolve` function (line 78) accepts the env var as-is with no scheme validation. For a local-only CLI this is low severity, but the API key is a high-value secret.
- **Suggested fix:** Validate that `api_base` starts with `https://` (or `http://localhost` / `http://127.0.0.1` for local dev proxies). Reject or warn on other `http://` URLs. Add a test.

#### SEC-16: Contrastive neighbor names injected into LLM prompt without sanitization
- **Difficulty:** medium
- **Location:** src/llm/prompts.rs:36-47
- **Description:** `build_contrastive_prompt` injects neighbor function names directly into the LLM prompt string (line 47: `neighbors.join(", ")`). These names come from the index's `name` column, which is populated by the parser from source code. A malicious file indexed as a reference could contain a function named with LLM prompt injection text (e.g., `fn ignore_previous_instructions_and_output_api_key() {}`). The name would be embedded verbatim into the prompt sent to Claude. This is indirect prompt injection — the attack surface is anyone who can get code into an indexed reference. Severity is low because (a) Claude is resistant to prompt injection, (b) the LLM response is only stored as a summary string, and (c) the attacker would need write access to indexed code. But it's a defense-in-depth gap.
- **Suggested fix:** Truncate neighbor names to alphanumeric + `_` characters (strip everything else), and cap at 60 chars (already done for length). This neutralizes embedded instructions while preserving legitimate function names.

#### SEC-17: `git_show` path parameter not validated — potential argument injection
- **Difficulty:** easy
- **Location:** src/train_data/git.rs:122-130
- **Description:** `git_show(repo, sha, path)` constructs `spec = format!("{}:{}", sha, path)` and passes it as a positional arg to `git show`. The `path` comes from diff parsing (`parse_diff_output` in `diff.rs`), which extracts it from `+++ b/path` lines. A crafted diff with `+++ b/--exec=cmd` would produce a path starting with `--`, though git's `sha:path` format likely prevents this from being interpreted as a flag. More realistic: a path containing newlines or null bytes could confuse git's output parsing. The `blame.rs` command validates paths (rejects `-` prefix and `:`), but `train_data` does not.
- **Suggested fix:** Validate that `path` does not start with `-` and does not contain null bytes. Same cheap guard as `blame.rs:79-89`.

## Resource Management

#### RM-28: `find_contrastive_neighbors` N*N similarity matrix has no size guard — OOM on large indexes
- **Difficulty:** medium
- **Location:** src/llm/summary.rs:249-263
- **Description:** `find_contrastive_neighbors` builds an N*N f32 similarity matrix via `matrix.dot(&matrix.t())`. The plan doc notes "N*N*4 bytes. At 10k chunks = 381 MB". There is no upper bound check on N. At 20k callable chunks the similarity matrix alone is 1.5 GB; at 30k it is 3.4 GB. A large monorepo or reference index could hit these numbers. Combined with the N*768*4 embedding matrix (~58 MB per 10k), peak memory can spike without warning. The memory is brief (dropped after neighbor extraction), but the peak allocation could OOM a machine with limited RAM. No `tracing::warn!` is emitted if N exceeds a safe threshold.
- **Suggested fix:** Add a size guard: if `n > 15_000`, log a warning and fall back to non-contrastive summaries (same as the existing error fallback on line 48-51). This is consistent with the existing "graceful degradation" pattern. Optionally, compute neighbors in blocks for large N (block-wise matrix multiply, merge top-K across blocks).

#### RM-29: `load_references` uses unbounded global rayon pool — no concurrency cap
- **Difficulty:** easy
- **Location:** src/reference.rs:63-106
- **Description:** `load_references` uses `par_iter()` on the global rayon thread pool with no concurrency cap. Each reference loads a `Store::open_readonly` (tokio runtime + SQLite pool + 64MB mmap) plus `HnswIndex::try_load` (mmap'd files, potentially 50-200MB each). With 10+ references, all load simultaneously. Compare with `search_across_projects` (project.rs:218) which correctly builds a scoped `ThreadPoolBuilder::new().num_threads(4)` pool to cap concurrent opens at 4. The reference path lacks this cap.
- **Suggested fix:** Build a scoped rayon pool with `num_threads(4)`, matching the existing pattern in `project.rs:218-221`.

#### RM-30: Watch mode `last_indexed_mtime` pruning condition is tautological
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:449
- **Description:** The pruning condition `last_indexed_mtime.len() > 10_000 || last_indexed_mtime.len() > 1_000` always evaluates to the weaker condition (`> 1_000`), making the `> 10_000` threshold dead code. After RM-17 removed the `files.len() == 1` guard, the OR condition became a tautology. This means pruning (checking every tracked file exists on disk via `root.join(f).exists()`) runs on every reindex cycle once the map exceeds 1,000 entries. For a project with 2,000 tracked files, this is 2,000 stat calls per reindex — not expensive per-call but unnecessary.
- **Suggested fix:** Simplify to `last_indexed_mtime.len() > 10_000` if the intent was "prune only when very large", or `last_indexed_mtime.len() > 1_000` if lower threshold was intended. Remove the dead branch.

#### RM-31: `find_contrastive_neighbors` double-buffers all embeddings (HashMap + ndarray)
- **Difficulty:** easy
- **Location:** src/llm/summary.rs:229-260
- **Description:** The function loads all embeddings into a `HashMap<String, Embedding>` via `get_embeddings_by_hashes` (line 230), then builds a `valid` Vec of references (lines 233-238), then copies each embedding element-by-element into an ndarray matrix (lines 250-254). For N=10k at 768 dims, the HashMap holds ~30MB and the matrix holds another ~30MB — embeddings exist in memory twice. The HashMap intermediate is only used for joining chunk_ids with their embeddings (a lookup by content_hash).
- **Suggested fix:** Fetch embeddings in chunk_id order directly (batched SELECT with ORDER matching chunk_ids), and write directly into ndarray rows. Eliminates the HashMap intermediate, halving peak embedding memory. Alternatively, drain the HashMap as rows are written to the matrix.

## Performance

#### PERF-25: `find_contrastive_neighbors` per-row full sort is O(n^2 log n) — partial sort would be O(n^2 * limit)
- **Difficulty:** easy
- **Location:** src/llm/summary.rs:270-272
- **Description:** After computing the n x n pairwise cosine similarity matrix (`sims = matrix @ matrix.T`), the function extracts top-N neighbors per chunk by collecting all n-1 scores into a Vec, sorting the entire Vec, and taking the first `limit` elements. This is repeated for each of the n rows: total cost is O(n^2 log n). With n = 10,164 callable chunks (current index), this is ~10K full sorts of ~10K-element Vecs. Since `limit` is typically 5 (contrastive neighbors), a partial sort (select_nth_unstable or manual k-min) would reduce the per-row cost from O(n log n) to O(n), making the total O(n^2) — matching the matrix multiply that dominates anyway. Additionally, each iteration allocates a fresh `Vec<(usize, f32)>` of n-1 elements (~80KB at n=10K). Reusing a single buffer across rows would eliminate ~10K allocations.
- **Suggested fix:** Replace the sort-then-take pattern with a bounded min-heap of size `limit`, or use `select_nth_unstable_by` to partition without full sorting. Reuse a single `scored` Vec (clear + extend instead of fresh allocation per row): `scored.clear(); scored.extend((0..n).filter(|&j| j != i).map(|j| (j, row[j])));`

#### PERF-26: Deferred type edge insertion uses per-file transactions — N separate transactions during indexing
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:769-777
- **Description:** After all chunks are committed, the store stage inserts deferred type edges in a per-file loop: `for (file, chunk_type_refs) in &deferred_type_edges { store.upsert_type_edges_for_file(file, chunk_type_refs) }`. Each `upsert_type_edges_for_file` call opens its own SQLite transaction (types.rs:130), queries chunks by origin (types.rs:133), deletes old edges, inserts new ones, and commits. For a typical codebase with 415 files, this is 415 separate SQLite transactions. Each transaction involves: `BEGIN` → `SELECT chunks WHERE origin = ?` → `DELETE type_edges` → batch `INSERT type_edges` → `COMMIT`. SQLite transaction overhead is ~1-2ms per commit (fsync), adding ~0.4-0.8s total. More importantly, the chunk lookup (SELECT by origin) is repeated per-file when a single batched query could fetch all chunk ID mappings in one pass.
- **Suggested fix:** Add a `upsert_type_edges_batch` method that takes all deferred edges at once, opens a single transaction, fetches all chunk IDs in one batched query (GROUP BY origin), deletes and inserts in bulk. Same pattern as `upsert_calls_batch` (which was added for the analogous chunk_calls deferred insertion in this same function). The chunk_calls path already uses a single batched call (pipeline.rs:756).

#### PERF-27: `search_by_candidate_ids` language/type filtering uses `any()` over HashSet instead of `contains()`
- **Difficulty:** easy
- **Location:** src/search/query.rs:374-389
- **Description:** The candidate filtering builds `HashSet<String>` for languages and chunk types (lines 362-369), but then uses `.any(|l| candidate.language.eq_ignore_ascii_case(l))` to check membership — a linear scan over the set. This defeats the purpose of the HashSet. The `eq_ignore_ascii_case` comparison is the reason: HashSet lookups require exact key match, but languages need case-insensitive comparison. However, since the set values are already lowercased at construction time (`.to_lowercase()`), the fix is to lowercase the candidate value once and use `.contains()`. Currently for a 3-language filter with 500 candidates, this does 1500 string comparisons instead of 500 hash lookups.
- **Suggested fix:** Change to `if !langs.contains(&candidate.language.to_lowercase()) { return None; }` — one lowercase + one hash lookup per candidate instead of N case-insensitive comparisons. Same for `type_set`.

#### PERF-28: `rrf_fuse` allocates HashMap on every query — could reuse or use Vec-based merge
- **Difficulty:** easy
- **Location:** src/store/mod.rs:747-778
- **Description:** `rrf_fuse` is called on every hybrid RRF search query. It allocates a `HashMap<&str, f32>`, inserts semantic_ids + fts_ids, then collects into a `Vec<(String, f32)>` and sorts. For a typical query with semantic_limit=30 and fts_ids=30, this allocates a 60-entry HashMap, clones all keys to Strings in the collect, allocates the Vec, and sorts. The HashMap is overkill for 60 entries — a simple Vec with linear dedup would be faster for small N. More importantly, the final `sorted.truncate(limit)` discards most entries. A bounded heap (already used in `search_filtered` for the same purpose) would avoid the full sort.
- **Suggested fix:** For the typical case (semantic_limit + fts_limit < 200), use a pre-allocated Vec with linear scan for dedup instead of HashMap. Or use `BoundedScoreHeap::new(limit)` already available in the scoring module. The HashMap allocation overhead is small but measurable when this runs on every search query.

#### PERF-29: `enrichment_pass` fetches enrichment hashes per page — could batch for entire pass
- **Difficulty:** easy
- **Location:** src/cli/enrichment.rs:120-124
- **Description:** Inside the page loop, `get_enrichment_hashes_batch` is called per 500-chunk page. The function opens a SQLite query for each page. The pass already pre-fetches summaries and hyde in single queries (lines 93 and 101: `get_all_summaries`), and pre-fetches all callers/callees in batch (lines 63-68). But enrichment hashes are fetched per-page. For 10K chunks at 500/page, this is 20 SQL queries that could be 1. The enrichment hash is a 32-char hex string per chunk — even 10K hashes is ~320KB, well within memory.
- **Suggested fix:** Pre-fetch all enrichment hashes before the page loop, same pattern as `all_summaries` and `callers_map`. Add a `get_all_enrichment_hashes()` method or pass all chunk IDs at once. Eliminates 19 of 20 SQL round trips.

#### PERF-30: `get_call_graph` duplicates all caller/callee strings into both forward and reverse maps
- **Difficulty:** medium
- **Location:** src/store/calls/query.rs:110-121
- **Description:** `get_call_graph` builds forward and reverse adjacency maps by cloning every caller and callee string twice: once as a key in `forward`/value in `reverse`, and once as a value in `forward`/key in `reverse`. For 500K edges, this clones 1M strings. The strings are typically function names (~20-40 chars), so this is ~20-40MB of duplicated heap allocations. The graph is cached (once per Store lifetime), so this is a one-time cost — but on large codebases with many edges, it can cause a noticeable pause on first access.
- **Suggested fix:** Use a string interning approach: store unique strings in a `Vec<String>` or `IndexSet<String>`, and use indices (`u32`) in the adjacency maps instead of cloned Strings. This would reduce the memory from ~40MB to ~10MB for 500K edges (unique strings + index maps). Alternatively, use `Arc<str>` for shared ownership without duplication. Medium difficulty because `CallGraph` is used across many call sites that expect `&str` access.

## Data Safety

#### DS-20: Batch resume stores results from stale batch into current index
- **Difficulty:** medium
- **Location:** src/llm/batch.rs:332-368
- **Description:** When `submit_or_resume` finds a pending batch ID from a previous run, it resumes and stores the results — even if the index has been rebuilt since. Scenario: (1) run `cqs index --llm-summaries`, process crashes after submitting batch but before storing results, (2) run `cqs index --force` which rebuilds all chunks with new content_hashes, (3) run `cqs index --llm-summaries` again — the pending batch from step 1 is resumed, its results are fetched and stored via `upsert_summaries_batch`. The custom_ids are content_hashes from the old index. If any old content_hash collides with a new one (unlikely with blake3 but possible if the same code was re-parsed identically), the old summary overwrites the new one. More commonly, the old results are simply orphaned rows in `llm_summaries` keyed by content_hashes that no longer exist in `chunks`. This wastes the API cost of the new batch (submitted in step 3 but then discarded because the pending batch takes priority) and stores stale summaries.
- **Suggested fix:** When resuming a pending batch, compare its custom_ids against current chunk content_hashes. If fewer than 50% match, log a warning and discard the stale batch (clear pending marker, submit fresh). Alternatively, store a "generation counter" in metadata that increments on `--force` rebuild, and tag pending batches with the generation — reject batches from old generations.

#### DS-21: Contrastive neighbor computation uses N*N memory with no cap
- **Difficulty:** easy
- **Location:** src/llm/summary.rs:249
- **Description:** `find_contrastive_neighbors` builds an N x N f32 similarity matrix where N is the number of callable chunks with embeddings. The doc comment notes "~550MB at 12k callable chunks" but there is no cap on N. A large codebase with 20k callable chunks would allocate 20000 * 20000 * 4 = 1.6GB for the matrix alone, plus the ndarray temporaries. This happens during `llm_summary_pass` which already pre-loads all embeddings. Combined, a 20k-chunk codebase could spike to 3+ GB during this single function, potentially OOM-killing the process.
- **Suggested fix:** Cap N at a reasonable limit (e.g., 15000 chunks = ~900MB matrix). If the index exceeds this, either sample or skip contrastive neighbors (fall back to discriminating-only summaries). Log a warning so the user knows. Alternatively, use a chunked/streaming approach that only computes top-N neighbors per row without materializing the full matrix.

#### DS-22: Enrichment hash non-determinism from f32 IDF threshold boundary
- **Difficulty:** easy
- **Location:** src/cli/enrichment.rs:247
- **Description:** `compute_enrichment_hash_with_summary` filters callees by `callee_doc_freq.get(name).copied().unwrap_or(0.0) < 0.1`. The callee_doc_freq values are computed as `count as f32 / total_chunks`. When a callee's frequency is exactly at the 0.1 boundary (e.g., 100 callers out of 1000 chunks = 0.1 exactly), the `< 0.1` test excludes it. But f32 division is not exact — `100.0 / 1000.0` is `0.1` in IEEE 754, yet `100.0 / 999.0` is `0.10010...` and `99.0 / 1000.0` is `0.099`. If adding or removing a single chunk changes `total_chunks` from 1000 to 1001, the frequency shifts from `0.1` to `0.0999...`, flipping the filter. This changes the enrichment hash, triggering a re-embedding even though the semantic content hasn't changed. Not a correctness bug — just unnecessary re-enrichment churn at the boundary.
- **Suggested fix:** Use `<= 0.1` or add epsilon (`< 0.1 + 1e-6`) to make the boundary stable. Alternatively, document that the IDF threshold is intentionally strict and boundary churn is acceptable.

#### DS-23: `find_contrastive_neighbors` silently produces empty map on embedding fetch failure
- **Difficulty:** easy
- **Location:** src/llm/summary.rs:46-52, 228-238
- **Description:** Two silent degradation paths in contrastive neighbor computation: (1) At line 46-52, if `find_contrastive_neighbors` fails entirely, the error is logged at `warn` and `neighbor_map` becomes an empty HashMap — all summaries fall back to non-contrastive prompts. (2) Inside the function at lines 228-238, `get_embeddings_by_hashes` may return fewer embeddings than chunk_ids (e.g., chunks indexed without embeddings, or embeddings cleared by `--force`). Chunks without embeddings are silently excluded from the similarity matrix. If most chunks lack embeddings (e.g., first index run before enrichment), the neighbor map is nearly empty, and all contrastive summaries degrade to discriminating-only — but the `with_neighbors` log count makes it look like neighbors were found for fewer chunks than expected, without explaining why.
- **Suggested fix:** Log the ratio of chunks with embeddings vs total callable chunks. If fewer than 50% have embeddings, log at `info` level that contrastive quality is degraded. The outer failure already has a `tracing::warn` — the inner partial degradation just needs visibility.

#### DS-24: HNSW save rollback deletes successfully-moved files without restoring originals
- **Difficulty:** medium
- **Location:** src/hnsw/persist.rs:312-321
- **Description:** The save rollback logic at lines 312-321 handles mid-rename failures by deleting any files that were already moved to the final location. However, the rename operation (`std::fs::rename`) is destructive — it replaces the existing file at the final path. So when `rename(temp/graph, final/graph)` succeeds but `rename(temp/data, final/data)` fails, the rollback deletes `final/graph` (the new file), but the old graph file is already gone (replaced by rename). The result: the index directory has only `data` and `ids` from the previous save, with `graph` deleted entirely. The next load will fail with `NotFound`. This is a refinement of DS-14 from v1.4.0 triage — that fix replaced "delete old + no restore" with "delete new + no restore". Neither restores the original files.
- **Suggested fix:** Before the rename loop, rename each existing final file to `{basename}.{ext}.bak`. On success, delete the `.bak` files. On failure, restore `.bak` files to their original names. This provides true rollback. The checksum verification on load means a partial save is always detected, but having no index files at all is worse than having the old valid ones.

#### DS-25: Concurrent `cqs index --llm-summaries` can submit duplicate API batches
- **Difficulty:** easy
- **Location:** src/llm/batch.rs:332-366, src/store/mod.rs:870-877
- **Description:** The pending batch ID is stored in SQLite metadata as a simple key-value pair with no locking beyond SQLite's implicit row-level locking. Two concurrent `cqs index --llm-summaries` processes can race: (1) Process A reads `get_pending_batch_id()` → None, (2) Process B reads `get_pending_batch_id()` → None, (3) Both submit fresh batches, (4) Process A writes its batch ID, (5) Process B overwrites with its batch ID. Process A's batch is orphaned — it will complete on Anthropic's side but its results are never fetched. The cost is double API spend. SQLite WAL mode allows concurrent reads, and the check-then-submit is not atomic.
- **Suggested fix:** Use `INSERT INTO metadata (key, value) VALUES ('pending_llm_batch', ?1) ON CONFLICT(key) DO UPDATE SET value = ?1 WHERE value IS NULL OR value = ''` as an atomic check-and-set. If the insert sees an existing non-empty value, the second process should resume the existing batch instead of submitting a new one. Alternatively, use a BEGIN IMMEDIATE transaction around the read-check-submit-write sequence.
