## Code Quality

#### CQ-V1.36-1: Enrichment hot path drops `model_max_seq_len`, falls back to 512 default
- **Difficulty:** medium
- **Location:** src/nl/mod.rs:43-59 (and src/cli/enrichment.rs:223)
- **Description:** Configurable-models disaster pattern, *new* instance not in v1.33 triage. `enrichment_pass` (production reindex hot path called from `cli/commands/index/build.rs:634`) takes `model_config: &ModelConfig` as a parameter (line 26) but threads only `summary`/`hyde` into `cqs::generate_nl_with_call_context_and_summary` (line 223). That function then calls the legacy 1-arg `generate_nl_description(chunk)` (src/nl/mod.rs:59) which routes through `generate_nl_with_template` → `CQS_MAX_SEQ_LENGTH` env (default 512). Result: for nomic-coderank (2048-tok) or any model with seq > 512, every enriched chunk's section-content preview is capped at ~1800 chars instead of the model-correct ~8000. The initial embedding pipeline (`cli/pipeline/embedding.rs:146`, `cli/watch/reindex.rs:520`) already calls `generate_nl_description_with_seq_len(c, model_max_seq_len)`, so enrichment silently undoes part of that work. P1-28 (#1330) was claimed as "fixed" in v1.33 triage — the fix only updated the initial embedding sites; the enrichment pass was missed.
- **Suggested fix:** Add `model_max_seq_len: usize` parameter to `generate_nl_with_call_context_and_summary`, plumb through to `generate_nl_with_template_and_seq_len(chunk, NlTemplate::Compact, model_max_seq_len)` instead of the legacy 1-arg call. Pass `model_config.max_seq_length()` from `enrichment_pass`. Verify with `grep -rn "generate_nl_description\b" src/` returning only test sites and the convenience-wrapper definition itself.

#### CQ-V1.36-2: `resolve_splade_model_dir()` zero-arg wrapper drops `[splade]` config at all 6 production sites
- **Difficulty:** easy
- **Location:** src/splade/mod.rs:213-215
- **Description:** Configurable-models disaster pattern: `pub fn resolve_splade_model_dir() -> Option<PathBuf>` calls `resolve_splade_model_dir_with_config(None)` — passing `None` is documented as "match the legacy no-config behavior". Six production callers use the zero-arg variant: `cli/store.rs:321`, `cli/batch/mod.rs:941` and `:1804`, `cli/commands/index/build.rs:689`, `cli/watch/events.rs:286`, `cli/watch/reindex.rs:69`. Every one ignores the user's `.cqs.toml` `[splade]` section (`preset`, `model_path`, `tokenizer_path`). Users editing config see no effect on watch/index/batch paths; only env vars work. Doc comment at `:208` warns "this single helper is the *only* place SPLADE paths are resolved" — but the helper itself silently strips config.
- **Suggested fix:** Either delete the zero-arg wrapper and force every call site to thread the loaded `cfg.splade.as_ref()`, or change the wrapper to accept a `Config`/`&AuxModelSection` and convert all call sites. The latter is mechanical; the former exposes the wiring gap at the type level. Add an integration test that sets `[splade] preset = "..."` in `.cqs.toml` and asserts watch/reindex picks it up.

#### CQ-V1.36-3: `generate_nl_with_template` legacy env-only entry point still public, masks bug from CQ-V1.36-1
- **Difficulty:** easy
- **Location:** src/nl/mod.rs:197-203
- **Description:** `generate_nl_with_template(chunk, template)` reads `CQS_MAX_SEQ_LENGTH` (default 512) and forwards to `_with_template_and_seq_len`. It is `pub`, re-exported from `lib.rs:232`, and is the path through which `generate_nl_description` (1-arg) and the enrichment hot path silently truncate previews. Doc comment admits: "legacy 1-arg API kept for compatibility". With CQ-V1.36-1 fixed, this wrapper has zero non-test callers. Leaving it `pub` invites the same disaster (any new caller picks the env-default rather than the model-correct value).
- **Suggested fix:** After CQ-V1.36-1 is fixed, downgrade `generate_nl_with_template` to `pub(crate)` (or delete entirely if the test at `:815` is updated to call `_with_template_and_seq_len`). Same for `generate_nl_description`. Both wrappers are tombstones for an API that should require the seq-len parameter.

#### CQ-V1.36-4: Stale dead-code reports in `cqs dead` for src/llm/* — index out of sync with v1.33 trait unification
- **Difficulty:** easy
- **Location:** Tooling artifact; affects audit/triage workflow.
- **Description:** `cqs dead --json` flags `submit_doc_batch`/`submit_hyde_batch` at `src/llm/batch.rs:387,395`, `src/llm/local.rs:681,689`, `src/llm/provider.rs:153,161,207,215` as dead. Those names no longer exist (PR #1347 / EX-V1.33-1 unified them into `submit_batch`); only doc-comments and historical eval JSON mention them. The HNSW index has stale chunk_id mappings — `cqs read` at those line numbers shows the new code. This isn't a code defect but it actively wastes auditor time chasing ghosts. `assert_eq!(human_bytes(...))` and `format_uptime(...)` are similarly mis-flagged because the `_with_seq_len` macro/test reflection isn't traced.
- **Suggested fix:** Run `cqs index` (or `systemctl --user restart cqs-watch`) to refresh post-#1347. Longer term: dead-code analyzer should require the chunk's content to be reachable from a live caller-graph node, not just match a chunk-id from the snapshot when the file at that line no longer contains the function. Equivalently, attach the indexed-at content_hash to dead entries and skip when the on-disk hash differs.

#### CQ-V1.36-5: 5 public NL-gen entry points where 2 would suffice (wrapper proliferation)
- **Difficulty:** easy
- **Location:** src/nl/mod.rs:43,167,175,197,209
- **Description:** Public surface: `generate_nl_with_call_context_and_summary` (7 args), `generate_nl_description` (1 arg), `generate_nl_description_with_seq_len` (2 args), `generate_nl_with_template` (2 args, env-default), `generate_nl_with_template_and_seq_len` (3 args). Three of these are 1-line forwarders to a more-parameterized variant. Only `_with_call_context_and_summary` and `_with_template_and_seq_len` carry semantic content. The 1-arg `generate_nl_description` is the trap from CQ-V1.36-1. This is mostly dead surface area — `lib.rs:232` re-exports both `generate_nl_with_call_context_and_summary` AND `generate_nl_with_template`, and the 1-arg `generate_nl_description` is exported via the unqualified glob from older releases.
- **Suggested fix:** After CQ-V1.36-1, retain only `generate_nl_with_template_and_seq_len` (free-form) and `generate_nl_with_call_context_and_summary` (enrichment). Make the latter take `model_max_seq_len`. Mark the three legacy entry points `#[deprecated]` for one release, then delete (per project policy: no external users, no deprecation cycle needed). The `tests/eval_test.rs:57` use of `generate_nl_description` should switch to the seq-len variant so eval mirrors production.

#### CQ-V1.36-6: `enrichment_pass` accepts `model_config` but ignores it
- **Difficulty:** easy
- **Location:** src/cli/enrichment.rs:23-28, :223-231
- **Description:** Function signature takes `model_config: &cqs::embedder::ModelConfig` (line 26) but the parameter is read only by surrounding logic in `enrichment.rs` not visible in the relevant block — when calling `generate_nl_with_call_context_and_summary` at line 223, only `summary` and `hyde` are threaded, not the model. The compiler can't catch this because `model_config` is used elsewhere in the function (e.g., for batch sizing). This is the classic "parameter exists but isn't propagated to the place it should constrain" bug — same shape as the configurable-models disaster.
- **Suggested fix:** Once CQ-V1.36-1 widens the NL signature, pass `model_config.max_seq_length()` (or whichever accessor is correct — verify against `embedder/models.rs`) through. Add a regression test: enrich a chunk with both 512-seq and 2048-seq models, assert the produced NL string differs in length.

#### CQ-V1.36-7: `VectorIndex::search_with_filter` default impl uses unchecked `k * 3`
- **Difficulty:** easy
- **Location:** src/index.rs:103
- **Description:** Trait default impl for filter-aware search: `self.search(query, k * 3).into_iter().filter(...)`. P1-42 (claimed fixed in #1326) addressed `limit*3`/`limit*2` overflow in the brute-force scoring path under `src/search/`, but this trait default — used by any backend that doesn't override (e.g., the brute-force `Vec<Embedding>` backend) — still has the same shape. With `k = usize::MAX/2` (legitimate-looking large `k` from a misconfigured `--limit` env), `k * 3` overflows in release without panic. A test harness or mis-routed daemon request could trip this.
- **Suggested fix:** Replace with `k.saturating_mul(3)`. Same one-token fix as P1-42; likely just missed because triage scoped to `src/search/`.

DONE
