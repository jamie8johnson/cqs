# Audit Findings — v1.7.0

Audit date: 2026-03-27

## API Design

#### AD-37: `--model` flag ignored by all commands except `doctor`
- **Difficulty:** medium
- **Location:** `src/cli/dispatch.rs:46`, `src/cli/commands/query.rs:70`, and ~20 other call sites
- **Description:** The `--model` CLI flag is defined in `definitions.rs:179` and parsed into `cli.model`, but `dispatch.rs` only threads it to `cmd_doctor(cli.model.as_deref())`. Every other command that creates an `Embedder` calls `ModelConfig::resolve(None, None)`, ignoring the CLI flag entirely. A user running `cqs "query" --model bge-large` would still search with the default e5-base model. The flag exists on the top-level `Cli` struct (not on a subcommand), so it applies to the implicit search command and should be respected.
- **Suggested fix:** Thread `cli.model.as_deref()` through to every `ModelConfig::resolve()` call site. The simplest approach: store the resolved `ModelConfig` once early in `run_with()` and pass it (or a reference) to command handlers that need an embedder. This avoids 20+ `resolve(None, None)` → `resolve(cli_model, config_embedding)` edits and eliminates redundant resolution.

#### AD-38: `export_model` template uses wrong field name `tokenizer` instead of `tokenizer_path`
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs:50`
- **Description:** The generated `model.toml` template writes `tokenizer = "tokenizer.json"`, but `EmbeddingConfig` (which parses `[embedding]` sections) defines the field as `tokenizer_path`. A user who copies this template verbatim into `cqs.toml` will get the tokenizer path silently ignored by serde (unknown fields are skipped by default), falling back to the default `"tokenizer.json"`. In this case the default happens to match, so the bug is latent — but for any model with a non-standard tokenizer path, it would be a confusing silent failure.
- **Suggested fix:** Change line 50 from `tokenizer = "tokenizer.json"` to `tokenizer_path = "tokenizer.json"`.

#### AD-39: `BatchProvider` trait uses opaque 4-tuple instead of named struct
- **Difficulty:** medium
- **Location:** `src/llm/provider.rs:19` — `items: &[(String, String, String, String)]`
- **Description:** All four `submit_*` methods on `BatchProvider` take `items: &[(String, String, String, String)]` where the fields are (custom_id, content, field3, language). The comment explains `field3` is "chunk_type or signature depending on the prompt builder" but this is not enforced by the type system. Callers must know the positional convention, and the `submit_batch_prebuilt` path ignores fields 2 and 3 entirely (passing them through as dead data). A named struct like `BatchItem { custom_id, content, context_field, language }` would make call sites self-documenting and prevent silent positional errors.
- **Suggested fix:** Define a `BatchSubmitItem` struct with named fields, use it across all `submit_*` methods. The `submit_batch_prebuilt` variant can document that `context_field` is unused (or take a narrower type).

#### AD-40: `embedding_to_bytes` returns `Result` while `embedding_slice`/`bytes_to_embedding` return `Option`
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:896`, `src/store/helpers.rs:914`, `src/store/helpers.rs:932`
- **Description:** Three sibling functions handle embedding serialization but use inconsistent error conventions. `embedding_to_bytes` returns `Result<Vec<u8>, StoreError>` on dimension mismatch. `embedding_slice` returns `Option<&[f32]>` on mismatch (trace-level log). `bytes_to_embedding` returns `Option<Vec<f32>>` on mismatch (warn-level log). All three validate the same invariant (dimension match) but callers must handle errors differently. Additionally, the logging levels are inconsistent: `embedding_slice` uses trace, `bytes_to_embedding` uses warn, for the same condition.
- **Suggested fix:** Align on one convention. Since these are called on hot paths where the caller already handles the failure mode (skip the embedding), `Option` is appropriate for all three. Or if error context matters, return `Result` for all three. Also align logging: both `embedding_slice` and `bytes_to_embedding` should use the same level (trace for hot paths, or warn for corruption detection — pick one).

#### AD-41: Three independent definitions of the default model name
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:99` (`MODEL_NAME`), `src/store/helpers.rs:27` (`DEFAULT_MODEL_NAME`), `src/embedder/models.rs:34` (inline in `e5_base()`)
- **Description:** The default model repo ID `"intfloat/e5-base-v2"` is defined in three places: `store::MODEL_NAME` (pub), `store::helpers::DEFAULT_MODEL_NAME` (pub(crate)), and inline in `ModelConfig::e5_base()`. `check_model_version()` compares against `DEFAULT_MODEL_NAME`, `doctor.rs` uses `store::MODEL_NAME`, and `ModelConfig::e5_base()` has its own copy. If the default model changes, all three must be updated in sync or validation will silently break (e.g., `check_model_version` rejects the correct model because its constant wasn't updated).
- **Suggested fix:** Single source of truth: `ModelConfig::e5_base().repo` (or a `const` on `ModelConfig`). `DEFAULT_MODEL_NAME` and `MODEL_NAME` should either be removed (callers use `ModelConfig::e5_base().repo`) or defined as `pub const DEFAULT_REPO: &str = ...` in one place, referenced everywhere else. The `check_model_version()` no-arg variant should use the runtime-resolved model, not a compile-time constant — otherwise multi-model support is broken at the validation layer.

#### AD-42: `Store::dim` is `pub` — exposed mutable field on a core type
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:208`
- **Description:** `Store::dim` is `pub dim: usize`, allowing any code to mutate it after construction. All other `Store` fields are `pub(crate)` or private. `dim` is set once during `open_with_config()` from metadata and should be immutable. External code reads `store.dim` (e.g., `cagra.rs`, `async_helpers.rs`), but no code outside the `store` module should set it. A caller accidentally writing `store.dim = 1024` would corrupt all embedding operations without any error.
- **Suggested fix:** Change to a private field with a public getter: `pub fn dim(&self) -> usize { self.dim }`. This is a library crate, and even without external users, `pub` on a mutable field is a code smell that invites bugs.

#### AD-43: `check_model_version()` validates against compile-time constant, not runtime model
- **Difficulty:** medium
- **Location:** `src/store/metadata.rs:93-94`
- **Description:** `Store::open()` calls `check_model_version()` which hardcodes `DEFAULT_MODEL_NAME` (`"intfloat/e5-base-v2"`). If a user configures `bge-large` via `CQS_EMBEDDING_MODEL` or config file, `Store::open()` will reject their index because the stored model name (`"BAAI/bge-large-en-v1.5"`) doesn't match the hardcoded default. The `check_model_version_with(expected)` variant exists but is unused by `open()`. This means multi-model support (the entire point of `ModelConfig::resolve()`) is broken at the store layer — any non-default model index cannot be reopened.
- **Suggested fix:** `Store::open()` (or `open_with_config`) should accept an optional expected model name, or `check_model_version()` should read the resolved model from `ModelConfig::resolve()`. The cleanest approach: `open()` skips model validation (dimension is already validated via `Store::dim`), and model mismatch is checked at index-time only (when embeddings are actually written). Alternatively, add a `model_name: Option<&str>` parameter to `open()`.

## Observability

#### OB-23: `detect_provider` and `create_session` have zero tracing — silent GPU provider selection
- **Difficulty:** easy
- **Location:** `src/embedder/provider.rs:214-233` (detect_provider), `src/embedder/provider.rs:236-265` (create_session)
- **Description:** `detect_provider()` silently selects between CUDA, TensorRT, and CPU without logging which provider was chosen. `create_session()` creates an ONNX session without logging the model path or provider. When debugging "why is inference slow?" or "is GPU being used?", the only way to tell is to check the `Embedder.provider` field externally. The result is cached in a `OnceCell`, so the decision happens exactly once per process and is invisible in logs. The caller (`embedder_session_init` span) logs "Embedder session initialized" but not which execution provider was used.
- **Suggested fix:** Add `tracing::info!(provider = ?provider, "Execution provider selected")` at the end of `detect_provider()` before the return. Add `tracing::info!(provider = ?provider, model_path = %model_path.display(), "Creating ONNX session")` at the top of `create_session()`. These fire once per process (cached) and are high-value for debugging GPU issues.

#### OB-24: `LlmConfig::resolve` has no tracing span
- **Difficulty:** easy
- **Location:** `src/llm/mod.rs:173-199`
- **Description:** `LlmConfig::resolve()` resolves API base, model, and max_tokens from env vars, config file, and defaults, but has no tracing span. The HTTPS warning (line 181) is logged but the resolution decision chain is not. Callers (e.g., `llm_summary_pass`, `doc_comment_pass`, `hyde_query_pass`) each log the resolved config after calling `resolve()`, so the final result IS visible. However, the resolution logic itself is silent — if `CQS_LLM_API_BASE` falls back to `CQS_API_BASE` (deprecated alias), there is no log of which env var was used, unlike `ModelConfig::resolve()` which logs `source = "cli"/"env"/"config"/"default"` at each step.
- **Suggested fix:** Add a span and log the resolution source for each field, matching the pattern in `ModelConfig::resolve()`. At minimum: `tracing::debug!(api_base_source = "env:CQS_LLM_API_BASE"|"env:CQS_API_BASE"|"config"|"default", ...)` so the deprecated alias usage is visible.

#### OB-25: `generate_nl_with_call_context_and_summary` — zero tracing on a key indexing function
- **Difficulty:** easy
- **Location:** `src/nl.rs:289-364`
- **Description:** This function assembles the final NL string that gets embedded for every chunk during the enrichment pass. It combines the base NL description, caller/callee context (IDF-filtered), LLM summaries, and HyDE predictions. It is called once per chunk during indexing (thousands of times) and any issue with NL generation (empty summary, missing callees, IDF filtering too aggressive) is completely silent. The parent function `generate_nl_description` and the helper `extract_field_names` both lack tracing too, but those are hot-path functions where per-call tracing would be noise. However, `generate_nl_with_call_context_and_summary` is the integration point where debuggability matters — at minimum, a `tracing::trace!` with the input sizes (callers count, callees count, has_summary, has_hyde) would help diagnose enrichment quality issues.
- **Suggested fix:** Add `tracing::trace!(callers = ctx.callers.len(), callees = ctx.callees.len(), has_summary = summary.is_some(), has_hyde = hyde.is_some(), "Generating enriched NL")` at the top. This is trace-level so it won't create noise in normal operation but is available with `RUST_LOG=cqs::nl=trace`.

#### OB-26: `export_model` Python dependency check logs no details on failure
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs:11-19`
- **Description:** When the Python dependency check fails (line 14), the function bails with a static message ("Missing Python dependencies"). The stderr output from the failed `python3 -c "import optimum; import sentence_transformers"` command is discarded. If the import fails due to a version mismatch or partial install (e.g., `optimum` present but wrong version), the user gets no diagnostic information. Compare with the ONNX export step (line 37) which correctly captures and includes stderr.
- **Suggested fix:** Capture and log stderr from the dependency check, same pattern as lines 37-39: `let stderr = String::from_utf8_lossy(&check.stderr); tracing::warn!(stderr = %stderr, "Python dependency check failed");` before the bail message. Also include it in the bail: `"Missing Python dependencies (stderr: {stderr}). Install with: ..."`.

#### OB-27: `stored_model_name` silently swallows store errors via `.ok()`
- **Difficulty:** easy
- **Location:** `src/store/metadata.rs:131-136`
- **Description:** `stored_model_name()` chains `.ok().flatten().filter(...)`, converting any `StoreError` from `get_metadata_opt` into `None`. If the metadata table is corrupt or the database is locked, this function silently returns `None` (no model name), which callers interpret as "fresh database / pre-model index". This could mask real store issues. The function is `pub` and called by `doctor.rs` for display — a corrupt database showing "no model configured" instead of an error is misleading.
- **Suggested fix:** Either propagate the error (`-> Result<Option<String>, StoreError>`) or log on error: `match self.get_metadata_opt("model_name") { Err(e) => { tracing::warn!(error = %e, "Failed to read stored model name"); None } Ok(v) => v.filter(|s| !s.is_empty()) }`.

## Error Handling

#### EH-32: `export_model` conflates missing Python with missing Python packages
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs:11-19`
- **Description:** Line 11 runs `Command::new("python3").args(["-c", "import optimum; ...]).output()?`. The `?` on `.output()` propagates the OS error (e.g., "No such file or directory" if python3 is not installed), but the error message provides no context — the user sees a raw `std::io::Error` about a missing executable. If python3 IS installed but the imports fail, the error message on line 16 says "Missing Python dependencies" — correct but doesn't distinguish "python3 not found" from "packages not found". A user without Python installed gets an unhelpful low-level OS error instead of the actionable "install python3" suggestion.
- **Suggested fix:** Check for the python3 binary explicitly first with a descriptive error: `Command::new("python3").arg("--version").output().map_err(|_| anyhow!("python3 not found. Install Python 3 first."))?`. Then separately check the package imports.

#### EH-33: `CQS_LLM_MAX_TOKENS` parse failure silently falls back to default
- **Difficulty:** easy
- **Location:** `src/llm/mod.rs:193-195`
- **Description:** `std::env::var("CQS_LLM_MAX_TOKENS").ok().and_then(|s| s.parse().ok())` — if the user sets `CQS_LLM_MAX_TOKENS=abc`, the parse fails silently and the default (100) is used. No warning, no error. The user thinks they configured a custom value but gets the default. This is inconsistent with `ModelConfig::resolve()` which logs a warning on unknown values for `CQS_EMBEDDING_MODEL`, and with `CQS_API_BASE` which validates the URL scheme. The LLM max_tokens path is the only env var resolution that silently swallows parse errors.
- **Suggested fix:** Log a warning when the env var is set but fails to parse: `if let Ok(s) = std::env::var("CQS_LLM_MAX_TOKENS") { match s.parse::<u32>() { Ok(v) => ..., Err(e) => tracing::warn!(%s, %e, "CQS_LLM_MAX_TOKENS not a valid u32, using default") } }`.

#### EH-34: `resume` returns unfiltered results — caller counts include stale entries
- **Difficulty:** easy
- **Location:** `src/llm/batch.rs:584`
- **Description:** `BatchPhase2::resume()` performs DS-20 validation (filtering stale content_hashes), stores only `valid_results` to DB, but returns the original unfiltered `results` on line 584. Callers use the return value for counting: `llm_summary_pass` reports `api_generated = api_results.len()` (line 126) and `hyde_query_pass` does the same. After a `--force` rebuild, these counts are inflated — e.g., "LLM summary pass complete: api_generated=50" when only 30 were actually stored (20 were stale). The inaccurate count is a diagnostic issue, not data loss.
- **Suggested fix:** Return `valid_results` instead of `results` from `resume()`. This makes the caller's count accurate and avoids confusion in logs.

#### EH-35: `submit_fresh` swallows `set_pending` failure — batch ID lost on crash
- **Difficulty:** medium
- **Location:** `src/llm/batch.rs:602-604`
- **Description:** After successfully submitting a batch to the Anthropic API (line 601), `submit_fresh` attempts to store the pending batch ID in the database (line 602). If this fails (e.g., disk full, WAL checkpoint failure), the error is logged at `warn` level and execution continues. The batch ID is returned and used for polling in the same process, so the current run completes. However, if the process crashes between submission and result fetching (e.g., OOM during wait, power loss), the batch ID is permanently lost — there's no pending marker in the DB, so `submit_or_resume` on the next run won't find it. The API cost for that batch is wasted. This is the same class of issue as EH-24 (now fixed for the read side) but on the write side.
- **Suggested fix:** Propagate the error. If we can't persist the batch ID, it's safer to fail early and let the user retry than to proceed and risk losing the batch on crash. The batch was already submitted so the cost is sunk, but at least with an error the user knows to check Anthropic's dashboard for orphaned batches.

#### EH-36: `Store::open` silently defaults corrupt dimension metadata to EMBEDDING_DIM
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:404-408`
- **Description:** When reading the `dimensions` metadata key, `s.parse::<u32>().ok()` on line 405 silently converts a corrupt value (e.g., "not_a_number", empty string) to `None`, which `.unwrap_or(EMBEDDING_DIM)` maps to the default 768. No warning is logged. A corrupted dimension value is a sign of database damage. If the actual stored embeddings are 1024-dim (BGE-large) but the metadata says "garbage", the store opens with dim=768, and all searches produce wrong results (dimension mismatch in cosine similarity). Compare with `check_schema_version` which correctly returns `StoreError::Corruption` for unparseable schema versions.
- **Suggested fix:** Add a `tracing::warn!` when the dimension string is present but fails to parse, preserving the fallback behavior but making the corruption visible. Alternatively, return `StoreError::Corruption` to force a `--force` rebuild, matching the schema_version behavior.

#### EH-37: `stored_model_name()` swallows DB errors via `.ok()`
- **Difficulty:** easy
- **Location:** `src/store/metadata.rs:132-134`
- **Description:** `stored_model_name()` calls `self.get_metadata_opt("model_name").ok().flatten()`. The `.ok()` converts any `StoreError` (including `Database`, `Corruption`) to `None`, making the function unable to distinguish "no model stored" from "database is broken". Callers interpret `None` as "fresh database" and proceed normally. If the database is genuinely corrupted and `get_metadata_opt` fails, the caller may attempt to index into a corrupt database. Note: this is the same root cause as OB-27.
- **Suggested fix:** Return `Result<Option<String>, StoreError>` instead of `Option<String>`, or at minimum log a warning in the error path.

#### EH-38: `ModelConfig::resolve` custom model accepts `dim: 0` as valid
- **Difficulty:** easy
- **Location:** `src/embedder/models.rs:112-127`
- **Description:** When parsing a custom model config with `has_repo && has_dim`, a config file with `dim = 0` produces a `ModelConfig` with `dim: 0`. A zero-dimension model causes: `embedding_to_bytes` produces a 0-byte buffer, `embedding_slice` always returns `None` (0 != any byte length), and HNSW build returns `DimensionMismatch`. The error surfaces eventually but deep in the stack — the user sees a cryptic "Embedding dimension mismatch: expected 0, got 768" instead of "invalid model configuration: dim must be positive".
- **Suggested fix:** Add a minimum dimension check in the custom model path: `let dim = embedding_cfg.dim.unwrap_or(768); if dim == 0 { tracing::warn!("Custom model has dim=0, falling back to default"); return Self::e5_base(); }`.

#### EH-39: `resume` stores all results on hash validation failure, blocking future re-generation
- **Difficulty:** medium
- **Location:** `src/llm/batch.rs:536-538`
- **Description:** When `get_all_content_hashes()` fails (line 530-533), `valid_hashes` is an empty set and line 536 takes the "Couldn't fetch hashes" branch, storing ALL results including stale ones. This means a store error during hash validation causes stale summaries to be committed to the DB. On next run, those stale summaries are found by `collect_eligible_chunks` as "cached" for their content_hash, preventing re-generation. If a chunk's content changed but kept the same hash (impossible with blake3, but possible if content_hash is empty due to EH-27), the stale summary persists indefinitely. More practically, after a `--force` rebuild that changes content, a transient store error during the validation step permanently commits stale summaries.
- **Suggested fix:** When `get_all_content_hashes()` fails, either propagate the error (fail the batch) or skip storage entirely and log at `error` level. The current "store everything on error" path is the worst outcome — it commits potentially stale data that blocks future correct processing.

## Code Quality

#### CQ-28: `Embedder::new` and `Embedder::new_cpu` are near-identical constructors
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:245-288`
- **Description:** `Embedder::new()` (lines 245-264) and `Embedder::new_cpu()` (lines 270-288) differ only in how `provider` is set: `select_provider()` vs `ExecutionProvider::CPU`. The remaining 15 lines (LRU cache creation, struct initialization) are identical. This is the kind of duplication that drifts — if a new field is added to `Embedder`, both constructors must be updated.
- **Suggested fix:** Consolidate into a single private `fn new_with_provider(config, provider)` and have `new()` call it with `select_provider()` and `new_cpu()` call it with `ExecutionProvider::CPU`.

#### CQ-29: `upsert_type_edges_for_file` logic duplicated inside `upsert_type_edges_for_files`
- **Difficulty:** medium
- **Location:** `src/store/types.rs:108-220` (single-file) vs `src/store/types.rs:227-329` (batch)
- **Description:** The batch method `upsert_type_edges_for_files` contains an exact copy of the per-file logic from `upsert_type_edges_for_file`: chunk ID resolution via SQL query, `name_to_id` HashMap construction, edge collection with unresolved-chunk warning, batched DELETE, batched INSERT. The only difference is transaction scope — single-file wraps each file in its own transaction, batch wraps all files in one. This is ~120 lines of duplicated async SQL code with 4 separate `HashMap`, `Vec`, and SQL builder constructions that must be kept in sync.
- **Suggested fix:** Extract the per-file core into an async helper that takes `&mut Transaction` instead of `&self.pool`. Both `upsert_type_edges_for_file` and `upsert_type_edges_for_files` call this helper within their own transaction management.

#### CQ-30: `normalize_for_fts` contains duplicated token-streaming block
- **Difficulty:** easy
- **Location:** `src/nl.rs:131-139` and `src/nl.rs:152-160`
- **Description:** The "stream tokens from `tokenize_identifier_iter` into result string" block appears twice in `normalize_for_fts` — once inside the main loop (for words separated by non-alphanumeric characters) and once after the loop (for the trailing word). Both blocks are identical 8-line sequences: `first_token` flag, iterator loop, conditional space insertion, `push_str`.
- **Suggested fix:** Extract into a local closure or inline helper: `let mut append_tokens = |word: &str| { for token in tokenize_identifier_iter(word) { if !result.is_empty() || ... { result.push(' '); } result.push_str(&token); } }`. Call it in both places.

#### CQ-31: `strip_prefixes` allocates `format!("{} ", prefix)` on every loop iteration
- **Difficulty:** easy
- **Location:** `src/nl.rs:795-796`
- **Description:** Inside a `while changed` loop, `strip_prefixes` calls `format!("{} ", prefix)` for each prefix on every iteration. Since prefixes are `&'static str` from `LanguageDef`, and the function is called once per content line per struct/enum chunk, this creates many small heap allocations. The prefixes don't change between iterations — the formatted strings can be computed once before the loop.
- **Suggested fix:** Pre-compute the suffixed versions: `let plist: Vec<String> = prefixes.split_whitespace().map(|p| format!("{} ", p)).collect();` sorted by length. Then the inner loop uses `result.strip_prefix(p.as_str())` with no per-iteration allocation.

#### CQ-32: `should_skip_line` hardcodes language keywords, inconsistent with data-driven `FieldStyle`
- **Difficulty:** medium
- **Location:** `src/nl.rs:742-763`
- **Description:** `should_skip_line` has 12 hardcoded `starts_with` checks for language-specific declaration keywords (`pub struct`, `data class`, `sealed class`, `case class`, `defstruct`, `@property`, etc.). This is the opposite of the data-driven approach used by `FieldStyle` where each language defines its own `strip_prefixes` and `separators`. The hardcoded list misses some languages (e.g., Haskell `data`, OCaml `type`, Elixir `defmodule`) and includes Rust-specific patterns (`pub struct`, `pub enum`) that don't apply to other languages. It also checks Python's `#` as a comment prefix, which misses that `#` is a valid attribute prefix in Rust (though this is benign since attributes aren't field declarations).
- **Suggested fix:** Add a `skip_prefixes: &'static [&'static str]` field to `LanguageDef` containing the line prefixes that indicate non-field lines (headers, comments) for that language. Move the hardcoded checks into language-specific definitions. Keep the universal checks (empty, `//`, `/*`, `*`, braces) in the function.

#### CQ-33: `nl.rs` at 2055 lines — growing monolith with 5 distinct responsibilities
- **Difficulty:** medium
- **Location:** `src/nl.rs`
- **Description:** `nl.rs` handles five distinct concerns: (1) FTS normalization (`normalize_for_fts`, `tokenize_identifier`, iterator), (2) NL description generation (`generate_nl_description`, `generate_nl_with_template`, `generate_nl_with_call_context_and_summary`), (3) field/method extraction (`extract_field_names`, `extract_member_method_names`, `should_skip_line`, `strip_prefixes`, `validate_field_name`), (4) markdown stripping (`strip_markdown_noise`, 6 compiled regexes), (5) JSDoc parsing (`parse_jsdoc_tags`). The file has grown from ~1200 lines (v1.0) to 2055 lines with the FieldStyle field extraction (383 line diff). Tests are 700+ lines at the bottom. Each responsibility is internally cohesive but has no coupling to the others — FTS normalization is used by store search, markdown stripping by section NL, field extraction only by struct/enum NL.
- **Suggested fix:** Split into `nl/mod.rs` (re-exports + NL generation), `nl/fts.rs` (FTS normalization + tokenizer), `nl/fields.rs` (field/method extraction), `nl/markdown.rs` (markdown stripping). This follows the same pattern as the `store/` and `hnsw/` splits already done. Each file would be 300-500 lines.

## Documentation

#### DOC-29: ROADMAP.md says "Current: v1.6.0" — stale for v1.7.0
- **Difficulty:** easy
- **Location:** `ROADMAP.md:3`
- **Description:** `ROADMAP.md` line 3 says "Current: v1.6.0" and the summary describes v1.6.0 features. v1.7.0 is the current version (per `Cargo.toml`). Additionally, the "Next -- Embedding Model Options" section (lines 10-13) lists items that were completed in v1.7.0: `ModelConfig` registry, BGE-large as configurable alternative, `cqs export-model`. These should be checked off or moved to a "Done" section.
- **Suggested fix:** Update the header to "Current: v1.7.0", add a v1.7.0 summary (configurable embedding models, `export-model` command, workflow skills), and check off the completed embedding model items.

#### DOC-30: CONTRIBUTING.md `llm/` architecture listing missing `provider.rs`
- **Difficulty:** easy
- **Location:** `CONTRIBUTING.md:209`
- **Description:** The architecture overview lists `llm/` submodule files as: `mod.rs, batch.rs, doc_comments.rs, hyde.rs, prompts.rs, summary.rs`. The actual directory also contains `provider.rs` (the new `BatchProvider` trait, added in v1.6.0 #681). This file defines a key abstraction (`pub trait BatchProvider`) referenced in the CHANGELOG and is the extension point for adding non-Anthropic LLM providers.
- **Suggested fix:** Add `provider.rs (BatchProvider trait, Anthropic implementation)` to the llm/ listing on line 209.

#### DOC-31: README "Training Data" section mentions "LoRA fine-tuning triplets" — stale reference
- **Difficulty:** easy
- **Location:** `README.md:324`
- **Description:** Line 324 says "Generate fine-tuning training data from git history (LoRA fine-tuning triplets)". The default model switched to base E5 in v1.5.0 and LoRA models are no longer the primary training target. The v1.6.0 changelog explicitly states "stale LoRA references updated to base E5" but this one was missed. The training data command generates triplets usable for any fine-tuning approach (LoRA, full fine-tune, etc.), not specifically LoRA.
- **Suggested fix:** Change to "Generate fine-tuning training data from git history:" — drop the parenthetical.

#### DOC-32: CLI help text for `TrainData` says "LoRA fine-tuning"
- **Difficulty:** easy
- **Location:** `src/cli/definitions.rs:614`
- **Description:** The `TrainData` command's doc comment says `/// Generate training data for LoRA fine-tuning from git history`. This appears in `cqs --help` output. Same stale LoRA reference as DOC-31.
- **Suggested fix:** Change to `/// Generate training data for fine-tuning from git history`.

#### DOC-33: `embedder/mod.rs` comment mentions LoRA — stale
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:27`
- **Description:** Line 27 says `// blake3 checksums — empty to skip validation (model changes with LoRA updates)`. LoRA models are no longer the default. The checksums are empty because configurable models (v1.7.0) can be any ONNX model — checksums are model-specific and cannot be hardcoded for an arbitrary model.
- **Suggested fix:** Change to `// blake3 checksums — empty to skip validation (configurable models have different checksums)`.

#### DOC-34: Hardcoded "768-dim E5-base-v2" in doc comments — stale with configurable models
- **Difficulty:** easy
- **Location:** `src/index.rs:25`, `src/cagra.rs:329`, `src/cli/batch/mod.rs:55`
- **Description:** Three doc comments hardcode "768-dim E5-base-v2" as if the dimension is fixed: (1) `index.rs:25` says `query` is "768-dim E5-base-v2", (2) `cagra.rs:329` says "Vectors are 768-dim unit-norm E5-base-v2 embeddings", (3) `batch/mod.rs:55` says "~3 KB per vector (768-dim x 4 bytes)". Since v1.6.0, embedding dimension is runtime-configurable (768 for E5-base, 1024 for BGE-large, arbitrary for custom). The VectorIndex trait is model-agnostic by design — its doc comment should not assume a specific model.
- **Suggested fix:** `index.rs:25` change to "Query embedding vector (dimension depends on configured model)". `cagra.rs:329` change to "Vectors are unit-norm embeddings". `batch/mod.rs:55` change to "~3-4 KB per vector (768-1024 dim x 4 bytes, depending on model)".

#### DOC-35: README config example `.cqs.toml` doesn't show `[embedding]` section
- **Difficulty:** easy
- **Location:** `README.md:130-151`
- **Description:** The "Configuration" section shows a `.cqs.toml` example with `limit`, `threshold`, `name_boost`, etc., but omits the `[embedding]` section entirely. The "Embedding Model" section (lines 54-71) shows `[embedding] model = "bge-large"` but no other fields. A user wanting to configure a custom model would need to read the source code (`EmbeddingConfig` struct in `models.rs`) to discover the available fields (`repo`, `onnx_path`, `tokenizer_path`, `dim`, `max_seq_length`, `query_prefix`, `doc_prefix`). The `export-model` command generates a template, but that template has the wrong field name (AD-38).
- **Suggested fix:** Add an `[embedding]` section to the config example showing all available fields with comments, similar to the doc comment on `EmbeddingConfig`. At minimum, show the custom model fields since preset usage is already documented.

#### DOC-36: ROADMAP test count says "1993 tests" — stale
- **Difficulty:** easy
- **Location:** `ROADMAP.md:5`
- **Description:** The v1.6.0 summary says "1993 tests" but v1.7.0 added new tests (model config, export model, etc.). The actual count should be verified and updated.
- **Suggested fix:** Run `cargo test --features gpu-index` and update the test count in ROADMAP.md.

#### DOC-37: README Claude Code integration command list missing `export-model` and `doctor`
- **Difficulty:** easy
- **Location:** `README.md:433-478`
- **Description:** The suggested CLAUDE.md command reference (lines 433-478) lists 38 commands but omits `cqs export-model` (new in v1.7.0) and `cqs doctor` (which gained model consistency checking in v1.7.0). These are operational commands that an agent would use when setting up or debugging model configuration issues.
- **Suggested fix:** Add `- \`cqs export-model --repo <id>\` - export HuggingFace model to ONNX for custom model use` and `- \`cqs doctor\` - check index health, model consistency, schema version` to the command list.

## Test Coverage

#### TC-31: Zero integration tests for `--model` flag end-to-end pipeline
- **Difficulty:** hard
- **Location:** `src/cli/dispatch.rs:46`, `src/embedder/models.rs`, `src/store/metadata.rs:93`
- **Description:** The `--model` flag -> `ModelConfig::resolve()` -> `Store::open()` -> HNSW pipeline has zero integration tests. AD-37 (flag ignored) and AD-43 (`check_model_version()` hardcodes default) are both confirmed bugs, yet no test exercises this path. Every `ModelConfig::resolve()` call in production passes `(None, None)` except `cmd_doctor`. Every test uses `ModelConfig::resolve(None, None)` or `ModelConfig::e5_base()`. There is no test that: (1) creates a store with model X, (2) reopens it with `check_model_version_with("X")`, (3) verifies the stored dim matches X's dim. The `tests/common/mod.rs:46` has `with_model(&ModelInfo)` but it's only called with `ModelInfo::default()`.
- **Suggested fix:** Add integration tests that: create a store with `ModelInfo::new("BAAI/bge-large-en-v1.5", 1024)`, reopen it, verify dim=1024 and `stored_model_name()` returns the BGE repo. Then test `check_model_version()` failure: open with BGE, call `check_model_version()` (which hardcodes E5), assert `ModelMismatch` error. This test alone would have caught both AD-37 and AD-43.

#### TC-32: `batch.rs` has zero unit tests (608 lines)
- **Difficulty:** medium
- **Location:** `src/llm/batch.rs`
- **Description:** `batch.rs` (608 lines) has no `#[cfg(test)]` module and zero `#[test]` functions. It contains `BatchPhase2` (the batch lifecycle manager: submit_or_resume, wait, fetch, validate, store), `submit_fresh`, the DS-20 hash validation logic (line 530-538), and the `clear_pending` helper. The resume error path (TC-27 in v1.5.0 triage, still open) swallows errors that should be tested. The hash validation branch (line 536: "Couldn't fetch hashes, storing all results") is the root cause of EH-39 and has no test. The `submit_fresh` set_pending failure path (EH-35) is also untested. The only code exercising batch.rs is the full LLM pipeline integration (which requires API keys and is never run in CI).
- **Suggested fix:** Add a test module with a mock `BatchProvider` (just a struct implementing the trait with preset responses). Test: (1) `submit_or_resume` with no pending ID calls submit, (2) `submit_or_resume` with pending ID calls poll+fetch, (3) resume with stale hashes filters results (DS-20), (4) resume with `get_all_content_hashes` failure stores all results (current bug -- test documents the behavior for when EH-39 is fixed), (5) `submit_fresh` stores pending ID.

#### TC-33: `Embedding::try_new` has zero tests
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:122-136`
- **Description:** `Embedding::try_new()` is the validated constructor that rejects empty vectors and NaN/Inf values. It has zero direct tests. The only tests are for `Embedding::new()` (the unchecked constructor) and property tests for `normalize_l2`. No test verifies: (1) `try_new` with empty vec returns `Err`, (2) `try_new` with NaN values returns `Err`, (3) `try_new` with Inf values returns `Err`, (4) `try_new` with valid data returns `Ok`. This is the safety boundary for embedding data quality -- if its behavior changes, nothing catches it.
- **Suggested fix:** Add 4 tests: `try_new_empty_rejects`, `try_new_nan_rejects`, `try_new_inf_rejects`, `try_new_valid_accepts`. Each is 3-4 lines.

#### TC-34: `export_model` has zero tests
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs`
- **Description:** `cmd_export_model` (62 lines) shells out to `python3` and `optimum-cli` with zero test coverage. The function has two confirmed bugs (AD-38: wrong field name in template, EH-32: conflated error messages). While the actual Python subprocess calls are hard to unit test, the generated `model.toml` template (lines 42-54) is pure string construction and trivially testable. The template has the `tokenizer` vs `tokenizer_path` bug (AD-38) that a test would catch immediately.
- **Suggested fix:** Extract the template generation into a separate function `fn model_toml_template(repo: &str) -> String` and test it: verify it contains `tokenizer_path` (not `tokenizer`), verify `repo` is interpolated, verify it contains `[embedding]` section. The subprocess calls can be tested via a `#[ignore]` integration test that requires Python.

#### TC-35: `ModelConfig::resolve` with `dim: 0` custom config not tested
- **Difficulty:** easy
- **Location:** `src/embedder/models.rs:112-127`
- **Description:** EH-38 identified that `ModelConfig::resolve` accepts `dim: 0` for custom models. The 19 tests in `models.rs` cover presets, env vars, CLI overrides, and custom models with valid dims (384), but none tests the boundary: `dim: 0`, `dim: 1` (minimum valid), or `max_seq_length: 0`. A custom config with `dim: Some(0)` silently produces a `ModelConfig { dim: 0 }` which causes downstream failures in `embedding_to_bytes` (empty buffer) and HNSW build (division by zero in `data.chunks(dim)`).
- **Suggested fix:** Add tests: `test_resolve_custom_dim_zero` (verify it either rejects or falls back), `test_resolve_custom_dim_one` (minimum valid), `test_resolve_custom_max_seq_zero`. These document the expected behavior and prevent regression when EH-38 is fixed.

#### TC-36: `Config::validate` NaN/Inf not tested
- **Difficulty:** easy
- **Location:** `src/config.rs:192-230`
- **Description:** `Config::validate()` clamps `threshold`, `name_boost`, and `weight` to `[0.0, 1.0]`. The existing tests cover out-of-bounds values (1.5, -0.1, -0.5) but no test passes NaN or Infinity. TOML does not allow NaN/Inf literals, but programmatic config construction could produce them. The `clamp` function on NaN returns NaN (`NaN.clamp(0.0, 1.0) == NaN`), so a NaN threshold would pass validation unchanged and propagate to search scoring where it produces zero results. The `validate_finite_f32` function in `definitions.rs:70` handles this at the CLI layer, but config-file values bypass it.
- **Suggested fix:** Add a test that constructs a `Config` with `threshold: Some(f32::NAN)` and calls `validate()`, verifying the NaN is caught (if adding a NaN check to `clamp_config_f32`) or documenting current pass-through behavior.

#### TC-37: `Store::open` dimension parse with edge values not tested
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:404-408`, `src/store/metadata.rs:483-498`
- **Description:** `tc17_corrupt_dimension_defaults_to_embedding_dim` exists (metadata.rs:483) and verifies that a corrupt dimension string falls back to `EMBEDDING_DIM`. However, it only tests the "garbage string" case. Missing cases: (1) empty string as dimension value, (2) negative number string (e.g., "-768"), (3) zero ("0"), (4) extremely large value (e.g., "999999999"). "0" is particularly dangerous: `"0".parse::<u32>()` succeeds, producing `dim = 0`, which causes downstream issues identical to EH-38. This is the store-layer equivalent of the `dim: 0` model config bug.
- **Suggested fix:** Add tests for empty string, "0", negative, and overflow dimension values. The "0" case should verify whether `dim=0` is actually rejected (it currently is not -- it silently creates a store with `dim=0`).

#### TC-38: `BatchProvider` trait has zero tests and no mock implementation
- **Difficulty:** medium
- **Location:** `src/llm/provider.rs`
- **Description:** The `BatchProvider` trait (63 lines, 9 methods) has no test coverage. There is exactly one implementation (`LlmClient` in `batch.rs:269`) which requires a live API key to exercise. No mock implementation exists. This means: (1) `BatchPhase2` (the batch lifecycle) cannot be tested without API credentials, (2) the trait contract (e.g., `is_valid_batch_id` semantics, `wait_for_batch` behavior on unknown IDs) is specified only in doc comments, (3) adding a second provider has no contract test to verify against. TC-32 (batch.rs zero tests) and TC-38 are complementary: a mock `BatchProvider` enables TC-32's tests.
- **Suggested fix:** Create `#[cfg(test)] struct MockBatchProvider` in `llm/provider.rs` (or a shared test module) with configurable responses. Use it in `batch.rs` tests (TC-32). The mock needs: `submit_batch` returns preset batch ID, `check_batch_status` returns "ended", `fetch_batch_results` returns preset HashMap, `is_valid_batch_id` does prefix check.

#### TC-39: `Config` with `[embedding]` section has minimal edge-case tests
- **Difficulty:** easy
- **Location:** `src/config.rs:984-1013`
- **Description:** Three tests cover `[embedding]` in config: `test_embedding_config_preset` (model name only), `test_embedding_config_custom` (model + repo + dim), `test_no_embedding_section` (absent). Missing: (1) `[embedding]` with unknown fields (e.g., `foo = "bar"`) -- serde default behavior silently ignores unknown fields, (2) `[embedding]` with conflicting fields (preset name + custom dim), (3) `[embedding]` section present but empty, (4) the `tokenizer_path` vs `tokenizer` field name bug (AD-38) -- a test parsing `tokenizer = "tok.json"` would reveal it's silently ignored. The `EmbeddingConfig` struct uses serde defaults, so behavior with partial/malformed sections is untested.
- **Suggested fix:** Add: (1) `test_embedding_config_unknown_fields_ignored` -- parse TOML with extra fields, verify known fields still work, (2) `test_embedding_config_preset_with_dim_override` -- parse `model = "e5-base"` with `dim = 1024`, verify the config carries both, (3) `test_embedding_config_empty_section` -- just `[embedding]` with no fields, (4) `test_embedding_config_tokenizer_field_name` -- parse with `tokenizer_path = "custom.json"`, verify it's captured (and parse with `tokenizer = "custom.json"`, verify it's ignored -- catches AD-38).

#### TC-40: HNSW `build_batched_with_dim` with `dim=0` not tested
- **Difficulty:** easy
- **Location:** `src/hnsw/build.rs:127-234`
- **Description:** `build_batched_with_dim` accepts `dim: usize` and uses it for dimension validation (line 169: `if embedding.len() != dim`). With `dim=0`, any non-empty embedding causes `DimensionMismatch`, but an empty batch would succeed and create an index with `dim=0`. The `build_with_dim` path has the same issue but delegates to `prepare_index_data` which rejects empty embeddings. The batched path with no embeddings would produce an empty index with `dim=0`. No test covers `dim=0` for either build path.
- **Suggested fix:** Add `test_build_with_dim_zero_rejects` and `test_build_batched_with_dim_zero_rejects` -- verify that `dim=0` either returns error or produces an empty index regardless of input. This is the HNSW-layer guard for the `ModelConfig dim: 0` and `Store dim: 0` bugs.

## Algorithm Correctness

#### AC-20: `build_batched_with_dim` progress counter includes skipped zero-vectors
- **Difficulty:** easy
- **Location:** `src/hnsw/build.rs:190`
- **Description:** `total_inserted += batch.len()` counts all embeddings in the batch, including zero-vectors that were skipped on lines 176-179. The progress log on lines 202-207 reports `total_inserted` as "vectors so far" and computes `progress_pct` from it. If a batch of 100 embeddings has 5 zero-vectors, the log says "95 / ~100 vectors (95%)" would be correct, but the code actually reports "100 / ~100 vectors (100%)". The final `id_map.len()` on line 226 is correct (only inserted items), but the per-batch progress is inflated. This also means `progress_pct` can exceed 100% when `capacity` is an estimate and zero-vector skips are rare — the counter accumulates `batch.len()` which may overshoot the estimate.
- **Suggested fix:** Change line 190 to `total_inserted += data_for_insert.len();` — this counts only the embeddings that were actually inserted into the HNSW graph.

#### AC-21: `ModelConfig::resolve` custom model path accepts `has_repo && has_dim` but `repo` uses `unwrap_or_default`
- **Difficulty:** easy
- **Location:** `src/embedder/models.rs:112-115`
- **Description:** The guard condition on line 112 is `if has_repo && has_dim`, where `has_repo = embedding_cfg.repo.is_some()`. But line 115 uses `embedding_cfg.repo.clone().unwrap_or_default()`, which would yield an empty string if `repo` were `None`. The guard makes this dead code (repo is guaranteed `Some` here), so there is no runtime bug. However, `unwrap_or_default` masks the invariant — if the guard logic ever changes (e.g., relaxing to `has_repo || has_dim`), the empty-string default would silently produce an invalid config with `repo: ""`. A `.unwrap()` (safe because of the guard) or `.expect("guarded by has_repo")` would correctly fail on violated invariants.
- **Suggested fix:** Change to `.unwrap()` or `.expect("guarded by has_repo")`. Same for `dim` on line 124 — the guard ensures it's `Some`, so `unwrap_or(768)` is misleading.

#### AC-22: `bytes_to_embedding` log level inconsistent with `embedding_slice` — trace vs warn for same condition
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:935` vs `src/store/helpers.rs:917`
- **Description:** `embedding_slice` logs at `trace` level for dimension mismatch (line 917), while `bytes_to_embedding` logs at `warn` level for the same condition (line 935). The doc comment on `bytes_to_embedding` (line 931) says "Uses trace level logging consistent with embedding_slice()" but the implementation uses `warn`. This creates a discrepancy: during brute-force search, `embedding_slice` produces trace-level noise (expected — some embeddings may be from old dim), but `bytes_to_embedding` produces warn-level alerts for the same data condition. The warn on line 935 is a documentation/implementation mismatch but not an algorithm error — the actual impact is log noise on databases with mixed-dimension embeddings during operations that call `bytes_to_embedding` (e.g., HNSW build, contrastive neighbor computation).
- **Suggested fix:** Change line 935 from `tracing::warn!` to `tracing::trace!` to match the documented behavior and the sibling function. The dimension mismatch in both functions indicates the same non-error condition (embedding from a different model version).

#### AC-23: `cosine_similarity` returns `Some(0.0)` for zero-norm vectors — semantically wrong
- **Difficulty:** medium
- **Location:** `src/math.rs:11-28`
- **Description:** `cosine_similarity` uses `simsimd::SpatialSimilarity::dot` for L2-normalized vectors. For a zero-norm vector `[0, 0, ..., 0]`, the dot product with any vector is `0.0`, which is finite, so the function returns `Some(0.0)`. But cosine similarity is *undefined* for zero-norm vectors (division by zero in the full formula). The function's doc says "dot product = cosine similarity for L2-normalized vectors" — but a zero-norm vector is not L2-normalized (its norm is 0, not 1). Returning `Some(0.0)` implies "orthogonal" which is semantically wrong — it should be `None` (undefined). `full_cosine_similarity` correctly returns `None` for zero-norm vectors (denom == 0 check on line 56). The existing test `cosine_zero_norm_vector` (line 181) accepts both `None` and `Some(0.0)`, which masks this inconsistency. In practice, the HNSW build skips zero vectors (line 177 of build.rs), and the embedder L2-normalizes output, so zero-norm vectors shouldn't appear in the index. This is a correctness issue only if zero-norm vectors enter the search path outside normal indexing.
- **Suggested fix:** Add a zero-norm check to `cosine_similarity`: after the dimension/empty check, compute `let norm_sq: f32 = a.iter().map(|x| x*x).sum::<f32>(); if norm_sq == 0.0 { return None; }`. This adds O(n) cost but matches `full_cosine_similarity` semantics. Alternatively, document that `cosine_similarity` assumes L2-normalized inputs and returns `Some(0.0)` for zero-norm (which is technically a valid dot product, just not a valid cosine similarity).

#### AC-24: `search_by_candidate_ids` computes `use_rrf` independently of `search_filtered` — inconsistent conditions
- **Difficulty:** medium
- **Location:** `src/search/query.rs:332-335` vs `src/search/query.rs:79-81`
- **Description:** `search_filtered` determines RRF usage from `fsql.use_rrf` (which is `filter.enable_rrf && !filter.query_text.is_empty()`), and hybrid name matching from `fsql.use_hybrid` (which is `filter.name_boost > 0.0 && !query_text.is_empty() && is_name_like_query(query_text)`). `search_by_candidate_ids` recomputes these locally on lines 332-335 with the same logic. The conditions currently match, but they're duplicated — if `build_filter_sql` changes the conditions (e.g., adding a minimum query length), `search_by_candidate_ids` won't pick up the change. This is fragile duplication of a correctness-critical condition. Additionally, `search_by_candidate_ids` doesn't use `build_filter_sql` at all (it doesn't build SQL), so it reimplements the flag logic.
- **Suggested fix:** Extract the flag computation into a helper: `fn compute_search_flags(filter: &SearchFilter) -> (bool, bool)` returning `(use_rrf, use_hybrid)`. Both methods call this helper instead of duplicating the condition.

## Extensibility

#### EX-29: `ModelConfig::resolve` CLI/env paths reject non-preset names — no custom model via CLI or env
- **Difficulty:** medium
- **Location:** `src/embedder/models.rs:76-99`
- **Description:** The `resolve()` method has three priority levels: CLI flag, env var, config file. The CLI path (line 76-86) and env var path (line 88-99) only accept preset names via `from_preset()`. If the name is unknown, they immediately fall back to `e5_base()` and never check the config file for custom model fields. This means `cqs "query" --model my-custom` always falls back to default, even if `.cqs.toml` has a fully-specified `[embedding]` section with `model = "my-custom"`, `repo`, `dim`, etc. Only the config file path (line 104-137) supports custom models. The CLI/env should be able to *select* a custom model defined in the config, not just presets. This makes the custom model path effectively config-file-only, which is fine for persistent configuration but blocks one-off testing (e.g., comparing models in a shell loop).
- **Suggested fix:** When CLI or env provides an unknown preset name, fall through to the config file path instead of returning default. Change lines 85 and 99 from `return Self::e5_base()` to just continuing to the next priority level. The config file check already handles unknown names with custom fields. If the config also doesn't match, *then* fall back to default.

#### EX-30: `BatchProvider::is_valid_batch_id` validates Anthropic-specific `msgbatch_` prefix
- **Difficulty:** easy
- **Location:** `src/llm/provider.rs:59`, `src/llm/mod.rs:247-250`
- **Description:** The `BatchProvider` trait's `is_valid_batch_id(&self, id: &str) -> bool` method is meant to be provider-agnostic (it's on the trait), but the only implementation (`LlmClient`) delegates to `is_valid_batch_id()` in `mod.rs` which hardcodes the Anthropic format: `id.starts_with("msgbatch_")`. If a second provider were added (e.g., OpenAI with `batch_` prefix, or a local LLM with UUID IDs), the shared validation function would reject their IDs. The batch orchestration code (`submit_or_resume`, line 438) calls `client.is_valid_batch_id()` to validate persisted IDs, so a wrong validation silently rejects valid pending batches and triggers expensive resubmission.
- **Suggested fix:** The validation belongs entirely in the trait impl, not in a shared function. Move the Anthropic-specific check into `impl BatchProvider for LlmClient` and remove the shared `is_valid_batch_id()`. Each provider validates its own ID format. Alternatively, make `is_valid_batch_id` a provided method on the trait with a default implementation that accepts any non-empty ASCII string, and let `LlmClient` override it with the Anthropic-specific check.

#### EX-31: Three LLM entry points hardcode `ANTHROPIC_API_KEY` env var — blocks alternate providers
- **Difficulty:** medium
- **Location:** `src/llm/summary.rs:35`, `src/llm/hyde.rs:32`, `src/llm/doc_comments.rs:163`
- **Description:** All three LLM pass entry points (`llm_summary_pass`, `hyde_query_pass`, `doc_comment_pass`) independently read `std::env::var("ANTHROPIC_API_KEY")` and construct `LlmClient::new()` directly. The `BatchProvider` trait exists to abstract the provider, and the batch orchestration (`BatchPhase2`) correctly uses `&dyn BatchProvider`. But the provider *construction* is not abstracted — the three entry points bypass any factory/registry pattern and hardcode both the env var name and the concrete type. Adding an OpenAI or local provider would require modifying all 3 entry points to add `if/else` or `match` on a provider selector, plus a new env var name.
- **Suggested fix:** Extract provider construction into a factory function: `fn create_batch_provider(config: &Config, llm_config: &LlmConfig) -> Result<Box<dyn BatchProvider>, LlmError>`. The factory reads the appropriate env var based on the configured provider (e.g., `CQS_LLM_PROVIDER=anthropic` reads `ANTHROPIC_API_KEY`, `CQS_LLM_PROVIDER=openai` reads `OPENAI_API_KEY`). The three entry points call the factory instead of hardcoding `LlmClient::new`. This consolidates the provider selection in one place.

#### EX-32: `export-model` does not auto-detect `dim` from HuggingFace `config.json`
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs:51`
- **Description:** The `export-model` command generates a `model.toml` template with `# dim = ???  # Check {repo} config.json for hidden_size`. The user must manually look up the embedding dimension. But after ONNX export, the model's `config.json` is already downloaded to the output directory (it's part of the HuggingFace model files). The `hidden_size` field in `config.json` is the embedding dimension for all standard sentence-transformer models. Auto-detecting this would remove a manual step that requires model-specific knowledge — the user must know to look for `hidden_size` (not `embedding_dim`, `d_model`, or other field names used by different architectures).
- **Suggested fix:** After ONNX export succeeds, attempt to read `config.json` from the output directory: `if let Ok(config) = std::fs::read_to_string(output.join("config.json")) { if let Ok(json) = serde_json::from_str::<serde_json::Value>(&config) { if let Some(dim) = json["hidden_size"].as_u64() { /* use in template */ } } }`. Fall back to the `# dim = ???` comment if auto-detection fails.

#### EX-33: Adding a new CLI command requires coordinated edits across 4 files with no automated checklist
- **Difficulty:** medium
- **Location:** `src/cli/definitions.rs`, `src/cli/dispatch.rs`, `src/cli/commands/mod.rs`, new `src/cli/commands/<name>.rs`
- **Description:** Adding a CLI command requires: (1) add a variant to `Commands` enum in `definitions.rs`, (2) add a `match` arm in `dispatch.rs`, (3) create a handler file in `commands/`, (4) add `pub(crate) use` in `commands/mod.rs`. Steps 1 and 2 are compile-time enforced (missing match arm is a compiler error since there's no `_` wildcard on the `Commands` match). But steps 3 and 4 are manual — forgetting the re-export in `mod.rs` causes an import error in `dispatch.rs`, which is only caught when you try to compile. The real friction is step 2: the dispatch function is 230+ lines of pattern matching with manual argument destructuring. Each new command adds 5-15 lines to this function. Compare with the `define_languages!` and `define_chunk_types!` macros which generate all boilerplate from a single line. With 40+ commands currently, the dispatch match is the largest single function in the CLI layer.
- **Suggested fix:** Low priority since the compiler catches most errors via exhaustive match. Document the 4-step process in CONTRIBUTING.md as a "New CLI Command Checklist" to reduce discovery friction for future sessions.

#### EX-34: `LlmConfig` has no provider selector — assumes Anthropic Messages API format
- **Difficulty:** medium
- **Location:** `src/llm/mod.rs:165-169`
- **Description:** `LlmConfig` has three fields: `api_base`, `model`, `max_tokens`. It has no `provider` or `api_type` field to select between different LLM API formats (Anthropic Messages, OpenAI Chat Completions, local vLLM, etc.). The `api_base` is configurable (can point at a proxy), but the HTTP request format, headers (`x-api-key`, `anthropic-version`), and response parsing are all hardcoded in `LlmClient` methods (`batch.rs:44-49`). Even with `CQS_LLM_API_BASE` pointing at an OpenAI-compatible endpoint, the Anthropic-format request body (`type: "message_batch"` etc.) would be rejected. The `BatchProvider` trait was added to enable this decoupling, but the config layer hasn't followed — there's no way to select which provider to construct.
- **Suggested fix:** Add `provider: LlmProvider` to `LlmConfig` where `LlmProvider` is an enum (`Anthropic`, `OpenAI`, etc.) resolved from `CQS_LLM_PROVIDER` env var or config. The factory function from EX-31 uses this field to construct the right `BatchProvider` implementation. Keep Anthropic as the default.

## Robustness

#### RB-20: `prepare_index_data` uses unchecked `n * expected_dim` multiplication for Vec allocation
- **Difficulty:** easy
- **Location:** `src/hnsw/mod.rs:268`
- **Description:** `Vec::with_capacity(n * expected_dim)` performs an unchecked `usize` multiplication. With a large chunk count and high dimension from a custom model, this is safe in practice. However, if `expected_dim` is corrupt or absurdly large (e.g., from a malformed `ModelConfig` with `dim: usize::MAX / 2`), the multiplication silently wraps on release builds, producing a tiny Vec that panics on extend. The sibling code in `cagra.rs:448` correctly uses `chunk_count.saturating_mul(dim).saturating_mul(4)` to check allocation size. HNSW build doesn't.
- **Suggested fix:** Add `n.checked_mul(expected_dim).ok_or_else(|| HnswError::Build("embedding count * dimension would overflow".into()))?` before the allocation. Match the pattern already used in `cagra.rs`.

#### RB-21: `load_references` double-unwrap on rayon ThreadPoolBuilder failure
- **Difficulty:** easy
- **Location:** `src/reference.rs:69`
- **Description:** `rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().unwrap())` -- if the fallback pool build also fails (e.g., OS refuses to create threads due to resource limits), this panics. The function signature returns `Vec<ReferenceIndex>` (not Result), so the panic propagates to the CLI caller. Reference loading is called on every search command with configured references.
- **Suggested fix:** Return an empty `Vec` on pool failure with a warning, or switch the fallback to sequential loading instead of panicking on a second pool failure.

#### RB-22: `submit_batch_inner` submits empty batch to API without early return
- **Difficulty:** easy
- **Location:** `src/llm/batch.rs:19-78`
- **Description:** `submit_batch_inner` does not check if `items` is empty before building the request and POSTing to the Anthropic API. An empty `items` slice produces `{"requests": []}` which is submitted as a real API call. The API may accept it (creating a batch with zero items that immediately completes but wastes a round trip) or reject it with a 400 error. Neither outcome is useful. The callers (`llm_summary_pass`, `doc_comment_pass`, `hyde_query_pass`) are supposed to check for empty items before calling, but this is not enforced.
- **Suggested fix:** Add an early return at the top of `submit_batch_inner`: `if items.is_empty() { return Err(LlmError::InvalidInput("Cannot submit empty batch".into())); }`.

#### RB-23: `embedding_dim()` returns `ModelConfig.dim` before first inference, which may be 0 for custom models
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:517-518`
- **Description:** `embedding_dim()` falls back to `self.model_config.dim` when `detected_dim` hasn't been set (no inference yet). This is the same `dim` value that EH-38 identified can be 0 from config. If code calls `embedding_dim()` before any embedding is computed (e.g., for allocation sizing or dimension checks), it gets 0. This leads to: `Vec::with_capacity(0)`, `embedding_to_bytes` returning Ok for any embedding, and HNSW building with dim=0. The `detected_dim` OnceLock is set during `embed_batch`, so any path that reads `embedding_dim()` before `embed_batch` gets the potentially-invalid config value.
- **Suggested fix:** Validate `dim > 0` in `ModelConfig::resolve()` (fixing EH-38) so this fallback is always valid. Additionally, guard the fallback: if the config dim is 0, return a sensible error rather than silently propagating.

#### RB-24: `strip_prefixes` while loop has no iteration cap
- **Difficulty:** easy
- **Location:** `src/nl.rs:791-802`
- **Description:** The `while changed` loop in `strip_prefixes` removes prefix keywords from a line repeatedly. Each iteration strips one prefix. For normal code this is 1-3 iterations (e.g., `pub static mut` = 3 prefixes). But a pathological input line repeating a prefix keyword (e.g., a line consisting of 1000 repetitions of `pub `) triggers 1000 iterations, each calling `format!("{} ", prefix)` for every prefix in the list. This is O(iterations * prefixes) string allocations. Since `strip_prefixes` is called per-line per-struct/enum-chunk during indexing, a file with many such lines would cause significant slowdown. The loop cannot be infinite (each iteration removes at least one prefix occurrence), but it has no practical bound.
- **Suggested fix:** Add a max iteration guard: `let mut iters = 0; while changed && iters < 20 { iters += 1; ... }`. Twenty is generous -- no real declaration has 20 prefix keywords.

#### RB-25: `convert/mod.rs` `panic!` in non-test code for missing FORMAT_TABLE entry
- **Difficulty:** easy
- **Location:** `src/convert/mod.rs:212`
- **Description:** `FORMAT_TABLE.iter().find(|e| e.variant == format).unwrap_or_else(|| panic!("FORMAT_TABLE missing entry for {:?}", format))` panics in production code. The safety argument is that `detect_format` returns variants only from `FORMAT_TABLE`, but this coupling is implicit -- if a new `ConvertFormat` variant is added to the enum but not to `FORMAT_TABLE`, the panic fires on user input. This violates the project convention "No `unwrap()` except in tests."
- **Suggested fix:** Return an error: `.ok_or_else(|| anyhow::anyhow!("Unsupported format {:?} -- this is a bug, please report", format))?`. The function already returns `anyhow::Result`.

#### RB-26: `build_with_dim` manual slice indexing `data[start..end]` where `chunks_exact` is safer
- **Difficulty:** easy
- **Location:** `src/hnsw/build.rs:78-82`
- **Description:** The test-only `build_with_dim` computes `let start = i * dim; let end = start + dim;` and indexes `data[start..end]`. The `i * dim` multiplication can overflow on release builds if `dim` is very large, causing an out-of-bounds access or wrap-around. The `data` Vec was populated from validated embeddings, so in practice `start` and `end` are always in bounds, but the manual arithmetic is error-prone. Rust's `chunks_exact(dim)` iterator handles this safely and is cleaner.
- **Suggested fix:** Replace the manual indexing with: `let chunks: Vec<Vec<f32>> = data.chunks_exact(dim).map(|c| c.to_vec()).collect();`.

#### RB-27: `make_placeholders` helper has unchecked `n * 4` allocation
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:859`
- **Description:** `build_placeholders(n)` allocates `String::with_capacity(n * 4)` where `n` is a caller-supplied `usize`. The caller `make_placeholders` sends values >999 to `build_placeholders` directly. While no current caller passes `n > ~10000` (the largest batch operations), the function is `pub(crate)` with no input validation. An accidental call with a very large `n` (e.g., from a corrupt chunk count) would cause a large allocation and a slow loop. The `n * 4` multiplication can also overflow for `n > usize::MAX / 4`.
- **Suggested fix:** Add a sanity cap: `assert!(n <= 100_000, "make_placeholders called with unreasonable n={n}");` or use `n.checked_mul(4).unwrap_or(n)`. The cap prevents accidental misuse while being well above any practical batch size.

#### RB-28: `doc_writer/rewriter.rs:295` bare `.unwrap()` in non-test code
- **Difficulty:** easy
- **Location:** `src/doc_writer/rewriter.rs:295`
- **Description:** `matching_chunks.iter().min_by_key(...).unwrap()` in the `rewrite_file` function. The `else` branch is reached when `matching_chunks.len() > 1`, so `min_by_key` on a non-empty iterator always returns `Some`. The unwrap is technically safe, but it relies on the `else if` guard 7 lines above. This is the same pattern as RB-14 (previously fixed in train_data) -- bare `.unwrap()` in non-test code where an `.expect()` with a justification message would be more robust.
- **Suggested fix:** Change to `.expect("matching_chunks guaranteed non-empty by else-if guard")` to document the invariant.

## Platform Behavior

#### PB-29: `export_model` hardcodes `python3` — fails on Windows where binary is `python` or `py`
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs:11,22`
- **Description:** `cmd_export_model` calls `Command::new("python3")` twice (dependency check on line 11, ONNX export on line 22). On Windows, the Python interpreter is typically `python` or `py` (the Python Launcher), not `python3`. This is the exact same bug that PB-18 identified in `convert/pdf.rs` and `convert/chm.rs` — those were fixed by adding `find_python()` (tries `python3`, `python`, `py` in order with `--version` validation) and `find_7z()` respectively. The new `export_model.rs` (added in v1.7.0) doesn't use the existing `find_python()` helper. On Windows, `Command::new("python3").output()` returns `Err(NotFound)`, which propagates as a raw IO error with no actionable message — the user sees "The system cannot find the file specified" instead of "Python not found. Install python3."
- **Suggested fix:** Reuse the existing `find_python()` from `convert/pdf.rs`. Either move it to a shared module (e.g., `crate::util::find_python`) or copy the pattern. Replace both `Command::new("python3")` calls with `Command::new(&find_python()?)`.

#### PB-30: `export_model` output path not canonicalized — potential UNC prefix and mixed separators on Windows
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs:32,56`
- **Description:** The `output` parameter is a raw `PathBuf` from clap (line 612 of `definitions.rs`, `default_value = "."`). It is passed to `output.display().to_string()` on line 32 as a subprocess argument to Python's `optimum`, and to `output.join("model.toml")` on line 56 for file creation. On native Windows, `std::fs::canonicalize(".")` returns a UNC path like `\\?\C:\Users\foo\models`. Without `dunce::canonicalize`, the path could carry the `\\?\` prefix into the Python subprocess argument, which some Python tools don't handle correctly. Additionally, `output.display()` on Windows produces backslash separators, which is fine for Python but inconsistent with the rest of cqs (which normalizes to forward slashes everywhere). Every other command that takes a user-supplied path runs it through `dunce::canonicalize` (see `reference.rs:90`, `read.rs:37`, `lib.rs:401`).
- **Suggested fix:** Canonicalize at the entry point: `let output = dunce::canonicalize(output).with_context(|| format!("Output path '{}' not found", output.display()))?;` at the top of `cmd_export_model`. This is consistent with all other path-accepting commands.

#### PB-31: `find_ort_provider_dir` picks first subdirectory — non-deterministic when multiple ORT versions cached
- **Difficulty:** easy
- **Location:** `src/embedder/provider.rs:101-106`
- **Description:** `find_ort_provider_dir()` calls `std::fs::read_dir(&ort_cache)` and takes `.next()` — the first directory entry. `read_dir` returns entries in filesystem order, which is not guaranteed to be alphabetical or chronological on any OS. If the user has multiple ORT versions cached (e.g., after an ort crate upgrade), the function may return an older version's directory containing stale or incompatible provider libraries. The symlinked `.so` files from an older ORT version could cause CUDA provider initialization to fail silently (falling back to CPU) or crash with symbol version mismatches. This is Linux-only (`#[cfg(target_os = "linux")]`) but affects any Linux user who has upgraded the ort dependency.
- **Suggested fix:** Sort subdirectories by name descending (ORT version directories are named by version) and pick the latest: `.sorted_by(|a, b| b.path().cmp(&a.path())).next()`. Or filter to only directories whose name matches the current ort crate version (available from `ort::version()` or the compiled-in version string).

## Security

#### SEC-18: `export_model` passes user-supplied `repo` string to Python subprocess without validation
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs:22-33`
- **Description:** The `--repo` flag value is passed directly as a `Command::new("python3").args([..., repo, ...])` argument on line 27. Because `Command::new` uses `execvp` (not shell), shell metacharacters (`; && |`) are not interpreted -- this is NOT command injection. However, the `repo` string is also interpolated into a TOML template via `format!` on line 48 (`repo = "{repo}"`). A crafted repo string containing double quotes and newlines (e.g., `--repo 'evil"\n[embedding]\nonnx_path = "/etc/shadow"'`) would produce malformed TOML that, if blindly copied into `.cqs.toml`, could override other settings. The immediate `model.toml` output is a template file the user copies manually, so exploitation requires user action, but the file is generated without escaping the repo value. Additionally, `repo` is not validated as a plausible HuggingFace repo ID (should be `org/model` format), so typos produce a confusing optimum error rather than an early rejection.
- **Suggested fix:** (1) Validate repo format: `if !repo.contains('/') || repo.contains('"') || repo.contains('\n') { bail!("Invalid repo ID format. Expected: org/model-name"); }`. (2) Use `toml::to_string` for the template instead of `format!` to ensure proper TOML escaping. At minimum, escape double quotes in the repo string.

#### SEC-19: `export_model` writes `model.toml` with default umask -- world-readable on shared systems
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs:56`
- **Description:** `std::fs::write(output.join("model.toml"), toml)` creates the file with the default umask (typically 022 on Linux, resulting in 644 permissions). The `model.toml` file itself contains no secrets, but it sits next to the ONNX model files which are also written by the Python subprocess with default permissions. This is inconsistent with `config.rs` which explicitly sets 600 permissions on config files (lines 391-396). The `model.toml` is intended to be copied into `.cqs.toml` which may contain `llm_api_base` or other sensitive config. A user who copies the template verbatim and adds their API config to the same file would have that config world-readable unless they manually fix permissions.
- **Suggested fix:** This is low-severity since the file is a template, not a config. But for consistency with the config file permission hardening in `config.rs:391-396`, add `#[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)); }` after the write. Or document in the template comment that permissions should be restricted if secrets are added.

#### SEC-20: `EmbeddingConfig` custom model `onnx_path` and `tokenizer_path` accept path traversal
- **Difficulty:** medium
- **Location:** `src/embedder/models.rs:116-123`, `src/embedder/mod.rs:670-681`
- **Description:** A `.cqs.toml` config with `[embedding]` section can set `onnx_path = "../../etc/malicious.onnx"` or `tokenizer_path = "../../../tmp/evil.json"`. These values flow into `ModelConfig.onnx_path` and `ModelConfig.tokenizer_path`, then to `ensure_model()` (line 676-681) which calls `hf_hub::Api::model(repo).get(&config.onnx_path)`. The `hf_hub` API resolves paths relative to the HuggingFace cache directory, and the `get()` method specifically downloads from the repo -- so path traversal in `onnx_path` would request a non-existent file from HuggingFace (404 error), NOT read a local file. The `tokenizer_path` has the same path, going through `Tokenizer::from_file(tokenizer_path)` on line 317 -- but `tokenizer_path` comes from `model_paths()` which is the `hf_hub` resolved path (a cache directory path), NOT the raw config string. So the actual risk is limited: a malicious config causes a download attempt for a non-existent file, not local file access. However, if a user manually places an ONNX file at the resolved path (e.g., pre-populating the HF cache), the traversal component in the path could cause path confusion.
- **Suggested fix:** Validate that `onnx_path` and `tokenizer_path` don't contain `..` or absolute path components: `if path.contains("..") || Path::new(path).is_absolute() { warn and reject }`. This is defense-in-depth -- the HF hub API already constrains resolution, but validating at parse time catches issues earlier.

#### SEC-21: `api_key` stored in memory as plain `String` -- visible in core dumps
- **Difficulty:** hard
- **Location:** `src/llm/mod.rs:205`, `src/llm/batch.rs:48,130,157,212`
- **Description:** The `ANTHROPIC_API_KEY` is read from the environment into `LlmClient.api_key: String`. This is a heap-allocated, non-zeroing string that persists for the lifetime of the client (the entire LLM pass, which can be minutes for large batches). On crash, the key appears in core dumps. During normal operation, the string can be swapped to disk. After the client is dropped, the heap memory is freed but not zeroed -- the key persists in freed memory until overwritten. This is a standard concern for API keys in process memory. For cqs specifically: (1) the tool is a local CLI, not a server, (2) core dumps are disabled by default on most Linux distributions, (3) the key is already in the environment (readable via `/proc/self/environ`). The risk is primarily if cqs is run in a shared-memory environment or with core dumps enabled.
- **Suggested fix:** Low priority for a local CLI tool. If hardening is desired, use `secrecy::SecretString` (from the `secrecy` crate) which zeros memory on drop and prevents accidental logging. The `Display` impl for `SecretString` prints `[REDACTED]`, preventing accidental key exposure in tracing output (currently `self.api_key` could appear in debug traces if someone adds `?self` to a span).

#### SEC-22: `cargo audit` reports three unmaintained dependency warnings
- **Difficulty:** easy
- **Location:** `Cargo.lock` (transitive dependencies)
- **Description:** `cargo audit` reports: (1) `paste 1.0.15` (RUSTSEC-2024-0436) -- unmaintained, via `tokenizers`. Tracked in existing issue #63. (2) `bincode 1.3.3` (RUSTSEC-2025-0141) -- unmaintained, via `hnsw_rs`. NEW since v1.4.0. (3) `number_prefix 0.4.0` (RUSTSEC-2025-0119) -- unmaintained, via `indicatif -> hf-hub`. NEW since v1.4.0. None of these have known security vulnerabilities -- they are "unmaintained" advisories. No actual CVEs. `bincode` and `number_prefix` are transitive dependencies (through `hnsw_rs` and `hf-hub` respectively) and cannot be updated independently.
- **Suggested fix:** For `bincode`: check if `hnsw_rs` upstream has a newer version that uses `bincode2` or `postcard`. For `number_prefix`: check if `indicatif` or `hf-hub` have newer versions without it. For `paste`: tracked in #63, awaiting `tokenizers` upstream update. Create GitHub issues for the two new advisories if not already tracked.

#### SEC-23: `run_git_diff` validates `base` starts_with('-') but not null bytes
- **Difficulty:** easy
- **Location:** `src/cli/commands/mod.rs:220-224`
- **Description:** `run_git_diff` validates that the `base` ref doesn't start with `-` (argument injection), matching the SEC-14 fix for `git_diff_tree` and `git_show` in `train_data/git.rs:92,132`. However, `run_git_diff` does NOT check for null bytes (`\0`), while `git_diff_tree` and `git_show` both reject `sha.contains('\0')`. A null byte in `base` would cause `Command` to either truncate the argument (C string behavior) or produce an OS error, depending on the platform. On Linux, `execvp` truncates at the null, meaning `base = "HEAD\0--config=foo"` would pass only `"HEAD"` -- harmless but inconsistent. The existing validation pattern in `git.rs` rejects both `-` prefix and null bytes; `run_git_diff` only does the former.
- **Suggested fix:** Add `|| b.contains('\0')` to the validation check: `if b.starts_with('-') || b.contains('\0') { bail!(...) }`. This matches the pattern already used in `git_diff_tree` and `git_show`.

#### SEC-24: `LlmConfig` logs resolved `api_base` at info level -- potential URL leak in shared logs
- **Difficulty:** easy
- **Location:** `src/llm/summary.rs:29-33`, `src/llm/hyde.rs:27-30`, `src/llm/doc_comments.rs:157-161`
- **Description:** All three LLM pass entry points log the resolved `api_base` URL at `info` level: `tracing::info!(api_base = %llm_config.api_base, ...)`. If the user has configured a custom API proxy with credentials embedded in the URL (e.g., `https://user:token@proxy.internal/v1`), the full URL including credentials appears in logs. The `info` level is the default log level for cqs, so this is always visible. The `api_key` itself is correctly NOT logged (it's passed separately as a header), but proxy URLs with embedded auth tokens are common in enterprise environments.
- **Suggested fix:** Strip credentials from the URL before logging: `tracing::info!(api_base = %url::Url::parse(&llm_config.api_base).map(|mut u| { u.set_password(None); u.set_username("").ok(); u.to_string() }).unwrap_or_else(|_| llm_config.api_base.clone()), ...)`. Or simply log at `debug` level instead of `info`.

#### SEC-25: `model.toml` template TOML injection via `repo` string (specific vector)
- **Difficulty:** easy
- **Location:** `src/cli/commands/export_model.rs:42-54`
- **Description:** This is a concrete example of SEC-18. The template uses raw string interpolation: `repo = "{repo}"`. If `repo = r#"evil" \n onnx_path = "/tmp/backdoor.onnx"#`, the generated TOML becomes:
  ```toml
  repo = "evil"
  onnx_path = "/tmp/backdoor.onnx"
  onnx_path = "model.onnx"
  ```
  TOML spec says the last value wins for duplicate keys, so the template's `onnx_path` overrides the injected one -- this specific injection is actually neutralized by key ordering. However, injecting a new section header (e.g., `repo = "evil"\n\n[llm]\napi_base = "https://attacker.com/v1"`) WOULD work, adding unexpected config that persists when the template is copied into `.cqs.toml`. The attack requires: (1) attacker controls `--repo` flag value, (2) user copies the generated template into their config. Both are unlikely for a local CLI tool, but the fix is trivial.
- **Suggested fix:** Use `toml::to_string_pretty` to generate the template, or escape the repo string: `repo.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "")`. Alternatively, validate repo as alphanumeric + `/` + `-` + `.` + `_` only.

## Data Safety

#### DS-26: HNSW build and load hardcode `EMBEDDING_DIM` (768) -- broken for non-default models
- **Difficulty:** medium
- **Location:** `src/hnsw/build.rs:242` (`build_batched`), `src/hnsw/persist.rs:528` (`load`), `src/hnsw/persist.rs:635` (`try_load`), `src/cli/commands/index.rs:411`
- **Description:** The production HNSW build path (`build_hnsw_index_owned` in `index.rs:411`) calls `HnswIndex::build_batched()` which delegates to `build_batched_with_dim(batches, total, crate::EMBEDDING_DIM)` -- hardcoded 768. Similarly, `HnswIndex::load()` calls `load_with_dim(dir, basename, crate::EMBEDDING_DIM)` -- also hardcoded 768. The `_with_dim` variants exist and work correctly, but ALL callers use the convenience wrappers that hardcode 768. Meanwhile, `Store` reads the correct dimension from metadata into `store.dim`, and `embedding_batches()` (async_helpers.rs:329) correctly uses `self.store.dim` to deserialize embeddings. This means: with a BGE-large model (dim=1024), embeddings are correctly stored as 1024-dim in SQLite, correctly deserialized as 1024-dim by `embedding_batches`, but then passed to HNSW `build_batched` which expects 768-dim. The HNSW build rejects every embedding with `DimensionMismatch { expected: 768, actual: 1024 }`, producing an empty HNSW index. Search falls back to brute-force (functional but slow). CAGRA (`cagra.rs:461`) correctly uses `store.dim` -- only HNSW is broken. This is a data corruption class bug: the entire HNSW index is silently empty when using a non-default model.
- **Suggested fix:** Thread `store.dim` through all HNSW build and load paths: (1) `build_hnsw_index_owned` should call `build_batched_with_dim(batches, total, store.dim)`, (2) `try_load_with_ef` should accept a `dim: usize` parameter and call `load_with_dim(dir, basename, dim)`, (3) `load_hnsw_index` in `cli/mod.rs:138` should pass `store.dim`. Consider deprecating the `build_batched`/`load` convenience wrappers entirely since they encode an incorrect assumption. The `_with_dim` variants are the correct API.

#### DS-27: `Store::open` accepts `dim=0` from metadata without validation
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:404-408`
- **Description:** The dimension parsing in `open_with_config` is `row.and_then(|(s,)| s.parse::<u32>().ok()).map(|d| d as usize)`. `"0".parse::<u32>()` succeeds, producing `dim=0`. A store with `dim=0` causes: `embedding_to_bytes` produces 0-byte buffers (no error -- `bytemuck::cast_slice` on empty slice is valid), `embedding_slice` returns `None` for all embeddings (0 != any byte length), `bytes_to_embedding` returns `None` for all embeddings. The net effect: all embeddings are silently dropped during deserialization. Search returns no results. HNSW build produces an empty index. No error, no warning. The `"0"` case can occur from: (1) manual metadata corruption, (2) a custom model config with `dim: 0` (EH-38 -- which is accepted by `ModelConfig::resolve`), (3) a migration bug that writes `"0"` to dimensions. Compare with `check_schema_version` which rejects `version: 0` as "fresh database" (line 52 of metadata.rs), not corruption -- but `dim=0` is always corruption.
- **Suggested fix:** Add a minimum dimension check after parsing: `if dim == 0 { return Err(StoreError::Corruption("dimensions metadata is 0 -- invalid".into())); }`. A reasonable minimum is 2 (the smallest useful embedding space). This catches both the `"0"` parse case and the EH-38 propagation from `ModelConfig`.

#### DS-28: `resume()` returns unfiltered `results` instead of `valid_results` -- inflated caller counts
- **Difficulty:** easy
- **Location:** `src/llm/batch.rs:584`
- **Description:** `BatchPhase2::resume()` performs DS-20 validation (lines 528-550), filtering out stale content_hashes and storing only `valid_results` to the DB (line 576). But line 584 returns the original unfiltered `results` map. Callers use the return value for counting: `llm_summary_pass` reports `api_generated = api_results.len()` and `hyde_query_pass` does the same. After a `--force` rebuild, these counts are inflated -- e.g., reporting "api_generated=50" when only 30 were actually stored (20 stale). The consequence is diagnostic confusion, not data loss -- the DB has the correct data, only the reported count is wrong. However, the unfiltered results also flow to callers that may iterate them for further processing (e.g., computing statistics), which would include phantom entries that are not in the DB.
- **Suggested fix:** Change line 584 from `Ok(results)` to `Ok(valid_results)`. This makes the caller's count match what was actually persisted. One-line fix.

#### DS-29: Hash validation failure stores ALL results including stale -- stale data blocks future regeneration
- **Difficulty:** medium
- **Location:** `src/llm/batch.rs:536-538`
- **Description:** When `store.get_all_content_hashes()` fails (line 530-533), `valid_hashes` is an empty `HashSet`. Line 536 checks `if valid_hashes.is_empty()` and takes the "Couldn't fetch hashes -- store everything" branch, committing ALL batch results including stale entries to the DB. This means a transient store error during hash validation permanently commits stale summaries. On subsequent runs, those stale summaries are found by `collect_eligible_chunks` as "already cached" for their content_hash, preventing regeneration of the correct summary. The stale data persists indefinitely -- `--force` rebuilds change content but don't clear `llm_summaries` (by design, for cost savings). The only recovery is manual SQL: `DELETE FROM llm_summaries WHERE content_hash NOT IN (SELECT content_hash FROM chunks)`. This is the same finding as EH-39 but framed as a data safety issue: transient error --> permanent data corruption with no automatic recovery path.
- **Suggested fix:** Separate the "no hashes in DB" case (legitimate for pre-v13 indexes) from the "failed to fetch hashes" case. When `get_all_content_hashes()` returns `Err`, either propagate the error (fail the batch -- safest) or skip storage entirely and log at `error` level, letting the next run retry. Reserve the "store everything" path only for when the query succeeds but returns an empty set (truly no content_hashes in the DB).

#### DS-30: `check_model_version()` hardcodes default model -- rejects valid non-default indexes
- **Difficulty:** medium
- **Location:** `src/store/metadata.rs:93-94`, `src/store/mod.rs:422`
- **Description:** `Store::open()` at line 422 calls `check_model_version()` which delegates to `check_model_version_with(DEFAULT_MODEL_NAME)` -- hardcoded to `"intfloat/e5-base-v2"`. If a user configures BGE-large via `CQS_EMBEDDING_MODEL=bge-large` or config file, `Store::open()` rejects their index with `ModelMismatch("BAAI/bge-large-en-v1.5", "intfloat/e5-base-v2")`. The `check_model_version_with(expected)` variant exists (line 101) but is never called by `open()`. This means: after indexing with a non-default model (which correctly stores the model name in metadata), reopening the store for search fails. Every subsequent `cqs` command produces an error. The only workaround is `cqs index --force` with the default model, losing the custom model index. Combined with DS-26 (HNSW hardcodes dim), configurable models are broken at both the store layer (model name rejection) and the HNSW layer (dimension mismatch). Note: this is the same root cause as AD-43 but the data safety impact is different -- AD-43 is API design, this is "your index becomes unopenable".
- **Suggested fix:** `open_with_config` should accept the resolved model name and pass it to `check_model_version_with`. The simplest approach: `open()` calls `check_model_version_with(&ModelConfig::resolve(None, None).repo)` instead of `check_model_version()`. This respects the user's configured model. Alternatively, skip model validation entirely in `open()` -- dimension is already validated at embed-time, and model name is informational.

#### DS-31: Migration v15-to-v16 table rename is not idempotent -- fails on re-run after partial completion
- **Difficulty:** easy
- **Location:** `src/store/migrations.rs:205-236`
- **Description:** The v15-to-v16 migration creates `llm_summaries_v2`, copies data from `llm_summaries`, drops `llm_summaries`, then renames `llm_summaries_v2` to `llm_summaries`. All four steps run inside a single transaction (via `pool.begin()` in `migrate()`), so a crash mid-migration rolls back cleanly -- the transaction safety is correct. However, if the migration succeeds but the schema_version update fails (line 48-51 of `migrate()`, inside the SAME transaction, so this shouldn't happen), or if the migration is run twice (e.g., a bug in `check_schema_version` lets `version=15` through after successful migration), the CREATE TABLE fails because `llm_summaries_v2` already exists (no `IF NOT EXISTS`). Unlike v10-to-v11 which uses `IF NOT EXISTS`, v15-to-v16 does not. In practice, the single-transaction design means this scenario requires a SQLite bug (commit succeeds for data but not for metadata update) -- extremely unlikely but not impossible under disk-full conditions where the WAL checkpoint partially succeeds.
- **Suggested fix:** Add `IF NOT EXISTS` to the CREATE TABLE and make the copy INSERT idempotent: `INSERT OR IGNORE INTO llm_summaries_v2 ...`. This is defense-in-depth -- the transaction should protect against double-execution, but `IF NOT EXISTS` is a zero-cost guard that makes the migration safe even if called repeatedly.

#### DS-32: `set_hnsw_dirty(true)` and chunk upsert are not atomic -- crash window between them
- **Difficulty:** medium
- **Location:** `src/cli/commands/index.rs` (indexing pipeline), `src/cli/watch.rs` (watch mode)
- **Description:** The indexing pipeline calls `store.set_hnsw_dirty(true)` before writing chunks to SQLite, then saves the HNSW index, then calls `store.set_hnsw_dirty(false)`. The dirty flag and the chunk upsert are separate SQLite operations. If the process crashes after `set_hnsw_dirty(true)` but before any chunk writes, the next run sees `hnsw_dirty=true` and falls back to brute-force search, then rebuilds HNSW. This is safe -- the flag is conservative. However, if the process crashes after chunk writes succeed but before `set_hnsw_dirty(true)` (i.e., the flag write itself fails due to disk-full or WAL corruption), the HNSW index is stale (chunks were updated but HNSW was not rebuilt) and there is no dirty flag to indicate this. The result: incorrect search results with no indication of staleness. The `PRAGMA synchronous = NORMAL` setting (line 323 of mod.rs) means the dirty flag write may not be fsynced before chunk writes begin -- the WAL tail can be lost on power failure, potentially losing the dirty marker while keeping the chunk data (which was committed in an earlier WAL page). This is an inherent trade-off of NORMAL synchronous mode, documented in the WAL pragma comment (lines 318-322), and acceptable for a rebuildable index. The risk is limited to power-loss scenarios on spinning disks or WSL-NTFS where fsync behavior is unreliable.
- **Suggested fix:** Informational -- the current design is the correct trade-off for a rebuildable index. The dirty flag provides crash safety for the common case (process kill, OOM). Full fsync on every metadata write (PRAGMA synchronous=FULL) would halve indexing throughput. If stronger guarantees are needed, wrap `set_hnsw_dirty(true)` and chunk upserts in the same SQLite transaction so they are atomically committed. But this conflicts with the batch-streaming design (chunks are written in batches, dirty flag is set once at the start).

## Resource Management

#### RM-32: `fetch_batch_results` loads entire JSONL response body into memory with no size cap
- **Difficulty:** medium
- **Location:** `src/llm/batch.rs:226`
- **Description:** `fetch_batch_results` calls `response.text()?` which buffers the entire HTTP response body into a single `String`. With `MAX_BATCH_SIZE = 10,000` items and each JSONL line containing the full LLM response (up to 100 tokens per item plus JSON envelope), the response body can reach 10-20MB. The entire body coexists in memory with the parsed `HashMap<String, String>` of results. While 10-20MB is manageable for normal operation, `response.text()` has no size limit. Since `CQS_API_BASE` can redirect to arbitrary servers, a malicious endpoint could return an unbounded response body, causing OOM. The Anthropic API itself bounds responses by batch size, but the code doesn't enforce this on the client side.
- **Suggested fix:** Add a response size check before buffering: check `response.content_length()` against a cap (e.g., 100MB). Alternatively, stream the JSONL line-by-line using `BufReader::new(response)` to avoid holding the full body in memory. The line-by-line approach also reduces peak memory by ~50% (no simultaneous raw body + parsed results).

#### RM-33: `find_contrastive_neighbors` holds HashMap of embeddings and ndarray matrix simultaneously
- **Difficulty:** easy
- **Location:** `src/llm/summary.rs:181-243`
- **Description:** This is the same root cause as v1.5.0 RM-31 (still unfixed). `get_embeddings_by_hashes` returns a `HashMap<String, Embedding>` holding all N embeddings (~46MB at 15k chunks). The function then builds a `valid` Vec of references into the HashMap, then allocates an `Array2<f32>` matrix copying each embedding into ndarray rows (~46MB more). At this point, both the HashMap and the matrix coexist. The `embeddings` HashMap is not dropped until the function returns — well after the N*N similarity matrix (`sims`, ~900MB at 15k) is allocated at line 243. Peak memory is thus: HashMap(46MB) + matrix(46MB) + sims(900MB) = ~992MB, when it could be matrix(46MB) + sims(900MB) = ~946MB with an explicit `drop(embeddings)` after line 218.
- **Suggested fix:** Add `drop(embeddings);` after the `valid`-filtering loop ends at line 218 and before the matrix allocation at line 229. The `valid` Vec holds `&[f32]` slices that borrow from `embeddings`, so the embeddings HashMap cannot be dropped while `valid` exists. The actual fix requires restructuring: copy the float data into owned Vecs in the valid-filtering loop, then drop `embeddings` before building the matrix. Or build the matrix rows directly during the filtering loop, eliminating the `valid` intermediate entirely.

#### RM-34: `batch.lock` file created but never deleted — accumulates in `.cqs/` directory
- **Difficulty:** easy
- **Location:** `src/llm/batch.rs:358-391`
- **Description:** `acquire_batch_lock` creates a `batch.lock` file via `OpenOptions::new().create(true)`. The file lock is released when the `File` handle drops, but the zero-length file persists in `.cqs/` permanently. This is a single stale file (not a per-run accumulation), but `cqs gc` doesn't clean it up, and its presence can confuse users inspecting the `.cqs/` directory.
- **Suggested fix:** Delete the lock file after the guard drops, or add it to `cqs gc` cleanup. Since the file is zero-length and advisory locks don't depend on file content, deletion is safe. Alternatively, document it as expected (like WAL files).

## Performance

#### PERF-31: `strip_markdown_noise` chains 5 `String::replace()` calls -- 5 full-string scans + allocations
- **Difficulty:** easy
- **Location:** `src/nl.rs:649-654`
- **Description:** After the regex passes, `strip_markdown_noise` calls `.replace("***", "")`, `.replace("**", "")`, `.replace('*', "")`, `.replace("```", "")`, `.replace('`', "")` sequentially. Each `replace()` allocates a new String and scans the entire content. For a 1800-char markdown section, this is 5 allocations + 5 full scans. With ~5800 markdown sections in the index, this totals ~29,000 unnecessary allocations during indexing. A single-pass char-by-char approach (or a single regex like `` [*`]+ ``) would reduce this to 1 allocation.
- **Suggested fix:** Replace the 5 chained `.replace()` calls with a single `retain`-style pass that skips `*` and `` ` `` characters, e.g., `result.retain(|c| c != '*' && c != '`')`. This is a single in-place pass with zero allocations. The order-sensitive stripping of `***` before `**` before `*` is irrelevant when removing all `*` characters anyway -- the end result is identical.

#### PERF-32: `get_summaries_by_hashes` builds placeholders with `format!` per-item instead of using `make_placeholders`
- **Difficulty:** easy
- **Location:** `src/store/chunks/crud.rs:230-235`
- **Description:** `get_summaries_by_hashes` manually builds placeholder strings with `format!("?{}", i + 1)` per item in a `.map().collect::<Vec<_>>().join(",")` chain, allocating one String per placeholder plus the join buffer. Meanwhile, `make_placeholders()` exists with a static cache for sizes up to 999 and an optimized builder for larger sizes. This function is called during enrichment pre-fetch and LLM batch validation. The allocation waste is small per call (~500 Strings for a full batch) but the inconsistency means it misses the cache that was specifically built for this purpose.
- **Suggested fix:** Replace the manual placeholder construction with `make_placeholders(batch.len())`. Note: `make_placeholders` generates `?1,?2,...,?N` which is exactly the format needed. The `purpose` bind offset needs adjustment: use `format!("?{}", batch.len() + 1)` for the purpose parameter position.

#### PERF-33: `contrastive_neighbors` L2 normalization uses per-element indexed assignment instead of bulk copy
- **Difficulty:** easy
- **Location:** `src/llm/summary.rs:230-234`
- **Description:** The ndarray matrix is populated with a nested loop: `for (j, &v) in emb.iter().enumerate() { row[j] = v; }`. ndarray's `.assign()` with an `ArrayView` would use a single memcpy. This runs for every embedding (N rows * 768 element assignments). For N=10,000 embeddings, that's ~7.7M individual indexed assignments vs N bulk copies. The normalization pass on lines 236-239 uses `mapv` correctly, but the initial data population is unnecessarily slow. At 768 dimensions, bulk copy vs indexed assignment is a ~3-5x difference per row due to bounds checking and cache line utilization.
- **Suggested fix:** Replace the element-wise copy with `matrix.row_mut(i).assign(&ndarray::ArrayView1::from(*emb))`. This is a single memcpy per row with no per-element bounds checking.

#### PERF-34: `resume` clones entire `results` HashMap when `valid_hashes` fetch fails
- **Difficulty:** easy
- **Location:** `src/llm/batch.rs:537-538`
- **Description:** When `valid_hashes` is empty (hash fetch failed), `resume` clones the entire `results` HashMap: `(results.clone(), 0usize)`. LLM batch results can contain thousands of entries (one per chunk), each with a content_hash string and a multi-sentence summary string. The clone duplicates all key-value pairs unnecessarily. Since `resume` returns the original `results` at line 584, and the validated copy is only used for the `upsert_summaries_batch` call, restructuring to avoid the clone is straightforward.
- **Suggested fix:** Instead of creating a `valid_results` clone, use a reference. Change the flow so the storage path on lines 564-576 iterates `&results` directly when `valid_hashes` is empty, and iterates `&valid` (the filtered map) otherwise. A `Cow`-like approach: `let store_from: &HashMap<String, String> = if valid_hashes.is_empty() { &results } else { &valid };`.

#### PERF-35: `enrichment_pass` clones every chunk name to build `name_file_count`
- **Difficulty:** easy
- **Location:** `src/cli/enrichment.rs:52-58`
- **Description:** The enrichment pass pre-loads all chunk identities, then builds `name_file_count: HashMap<String, usize>` by cloning every chunk name on line 57: `name_file_count.entry(ci.name.clone()).or_insert(0) += 1`. With ~20,000 chunks, this allocates ~20,000 owned Strings for the HashMap keys. Since `identities` is dropped immediately after (line 59), the names could be moved rather than cloned.
- **Suggested fix:** Use a `&str`-keyed temporary map while `identities` is alive, then drop `identities`: `let name_file_count: HashMap<&str, usize>` built from `&identities[..]`. The later lookup `name_file_count.get(&cs.name)` works because `String` derefs to `&str`. This avoids all 20,000 clones. Alternatively, build the map with `identities.iter()` using `.entry(ci.name.as_str())` while `identities` is borrowed, then keep `identities` alive until the enrichment loop ends (it's already dropped at line 59, but the `name_file_count` is used throughout the loop).

#### PERF-36: `embed_batch` clones all input texts for tokenizer `encode_batch`
- **Difficulty:** easy
- **Location:** `src/embedder/mod.rs:549`
- **Description:** `self.tokenizer()?.encode_batch(texts.to_vec(), true)` clones the entire input texts slice into a new Vec of Strings. With batch sizes of 64 texts averaging ~200 chars each, this copies ~12KB of string data per batch. Over a 20,000-chunk index (~312 batches), this is ~4MB of unnecessary copies total. The same pattern appears at line 353 for the reranker path.
- **Suggested fix:** The `tokenizers` crate's `encode_batch` accepts `Vec<EncodeInput<'_>>`, and `EncodeInput::Single` can take a `Cow<str>`. Pass `texts.iter().map(|s| tokenizers::EncodeInput::Single(s.into())).collect::<Vec<_>>()` to avoid cloning the string data. This passes `&str` references wrapped in `Cow::Borrowed`, copying only 24 bytes per pointer instead of the full string content. If the API requires owned Strings, add a comment documenting the unavoidable clone.

#### PERF-37: `build_batched_with_dim` computes full L2 norm per embedding to detect zero vectors
- **Difficulty:** easy
- **Location:** `src/hnsw/build.rs:176`
- **Description:** For each embedding in every batch, `build_batched_with_dim` computes `let norm_sq: f32 = embedding.as_vec().iter().map(|x| x * x).sum()` -- a full 768-element dot product -- to detect zero vectors. E5-base-v2 outputs L2-normalized embeddings (norm ~= 1.0), so zero vectors are near-impossible (only from embedding failures, which are already caught upstream). This computes ~15M float multiply-and-add operations over 20,000 embeddings for a condition that essentially never triggers.
- **Suggested fix:** Replace with short-circuiting check: `embedding.as_vec().iter().all(|x| *x == 0.0)` or `embedding.as_vec().iter().any(|x| *x != 0.0)`. The `any` version checks a single element for normal embeddings (O(1) vs O(768)). This is a ~750x speedup for the common case while still catching actual zero vectors.

#### PERF-38: `resume` clones model name and purpose string per-result for `upsert_summaries_batch`
- **Difficulty:** easy
- **Location:** `src/llm/batch.rs:568-573`
- **Description:** In `resume`, the `to_store` Vec is built by cloning `model.clone()` and `self.purpose.to_string()` for every result entry (lines 570-571). With 10,000 batch results, this allocates 10,000 copies of the model name (~30 bytes) and 10,000 copies of the purpose string (~10 bytes), totaling ~400KB of redundant string allocation. These values are identical for all entries in the batch.
- **Suggested fix:** Change `upsert_summaries_batch` to take model and purpose as separate `&str` parameters: `upsert_summaries_batch(summaries: &[(String, String)], model: &str, purpose: &str)`. The `QueryBuilder::push_values` closure can bind model and purpose from the outer scope. This eliminates 2*N string clones per batch.

#### PERF-39: `prepare_index_data` validates dimensions in a separate pass before building
- **Difficulty:** easy
- **Location:** `src/hnsw/mod.rs:255-272`
- **Description:** `prepare_index_data` first iterates all embeddings to validate dimensions (lines 255-264), then iterates again to build `id_map` and flat data vector (lines 267-272). This is two full passes. The validation could be merged into the build loop for a single pass. However: this function is only used by the non-batched `build()` method, which is only used in tests. The production path uses `build_batched_with_dim` which already does single-pass validation. Low priority.
- **Suggested fix:** Merge the validation and build loops into a single pass. Since this only affects test performance, consider leaving a comment explaining the intentional simplicity-over-performance choice for test code, or merge anyway since the fix is trivial.
