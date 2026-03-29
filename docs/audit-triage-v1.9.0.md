# Audit Triage — v1.7.0

Audit date: 2026-03-27
Total findings: 95 (after dedup: ~80 unique)

## Critical Theme: Multi-Model Support is Non-Functional

The v1.7.0 headline feature (configurable embedding models) is broken end-to-end:
1. `--model` CLI flag parsed but ignored (AD-37)
2. Store rejects non-default model indexes on open (AD-43/DS-30)
3. HNSW build/load hardcodes 768-dim (DS-26)
4. Three independent default model name definitions (AD-41)

These form one fix cluster — all must be resolved together.

## P1: Fix Immediately

| ID | Category | Finding | Difficulty | Status |
|----|----------|---------|------------|--------|
| AD-37 | API Design | `--model` flag ignored by all commands except doctor | medium | |
| AD-43/DS-30 | API/Data | `check_model_version()` hardcodes default — rejects valid non-default indexes | medium | |
| DS-26 | Data Safety | HNSW build/load hardcodes EMBEDDING_DIM (768) — empty index for non-default models | medium | |
| AD-41 | API Design | Three independent default model name definitions must sync | easy | |
| AD-38 | API Design | `export_model` template uses `tokenizer` instead of `tokenizer_path` | easy | |
| EH-38 | Error Handling | `ModelConfig::resolve` accepts `dim: 0` for custom models | easy | |
| DS-27 | Data Safety | `Store::open` accepts `dim=0` from metadata without validation | easy | |
| EH-36 | Error Handling | `Store::open` silently defaults corrupt dimension to 768 | easy | |
| EH-34/DS-28 | Error/Data | `resume()` returns unfiltered results — inflated counts | easy | |
| EH-39/DS-29 | Error/Data | Hash validation failure stores ALL results including stale — blocks regeneration | medium | |
| DOC-29 | Documentation | ROADMAP says v1.6.0, should be v1.7.0; completed items not checked off | easy | |
| DOC-36 | Documentation | ROADMAP test count "1993" stale | easy | |

## P2: Fix in Batch

| ID | Category | Finding | Difficulty | Status |
|----|----------|---------|------------|--------|
| EH-35 | Error Handling | `submit_fresh` swallows `set_pending` failure — batch ID lost on crash | medium | |
| EX-29 | Extensibility | `ModelConfig::resolve` CLI/env reject non-preset names — custom model via CLI impossible | medium | |
| TC-31 | Test Coverage | Zero integration tests for `--model` pipeline end-to-end | hard | |
| TC-32 | Test Coverage | `batch.rs` 608 lines with zero tests | medium | |
| TC-38 | Test Coverage | `BatchProvider` trait has no mock — blocks batch testing | medium | |
| CQ-33 | Code Quality | `nl.rs` 2055-line monolith — split into fts/fields/markdown/nl | medium | |
| CQ-29 | Code Quality | `upsert_type_edges` 120-line SQL logic duplicated between single/batch | medium | |
| CQ-32 | Code Quality | `should_skip_line` hardcodes 12 keywords, inconsistent with data-driven FieldStyle | medium | |
| AD-39 | API Design | `BatchProvider` uses opaque 4-tuple instead of named struct | medium | |
| AD-42 | API Design | `Store::dim` is `pub` mutable — should be getter | easy | |
| AC-23 | Algo Correct | `cosine_similarity` returns `Some(0.0)` for zero-norm — should be `None` | medium | |
| AC-24 | Algo Correct | `search_by_candidate_ids` duplicates RRF flag computation from `search_filtered` | medium | |
| SEC-18/25 | Security | `export_model` TOML injection via repo string interpolation | easy | |
| PERF-31 | Performance | `strip_markdown_noise` 5x String::replace → single `retain()` pass | easy | |
| PERF-33 | Performance | Contrastive neighbors per-element indexed copy → `assign()` bulk memcpy | easy | |
| RM-32 | Resources | `fetch_batch_results` loads entire JSONL body with no size cap | medium | |
| RM-33 | Resources | Contrastive neighbors double-buffer (~46MB HashMap + ~46MB matrix) | easy | |
| EX-31 | Extensibility | Three LLM entry points hardcode `ANTHROPIC_API_KEY` | medium | |

## P3: Fix if Time

| ID | Category | Finding | Difficulty | Status |
|----|----------|---------|------------|--------|
| DOC-30 | Documentation | CONTRIBUTING.md llm/ listing missing `provider.rs` | easy | |
| DOC-31 | Documentation | README "LoRA fine-tuning triplets" — stale | easy | |
| DOC-32 | Documentation | CLI help text `TrainData` says "LoRA fine-tuning" — stale | easy | |
| DOC-33 | Documentation | `embedder/mod.rs:27` comment mentions LoRA — stale | easy | |
| DOC-34 | Documentation | Three doc comments hardcode "768-dim E5-base-v2" — stale | easy | |
| DOC-35 | Documentation | README config example omits `[embedding]` section | easy | |
| DOC-37 | Documentation | README command list missing `export-model` and `doctor` | easy | |
| CQ-28 | Code Quality | `Embedder::new` / `new_cpu` near-identical constructors | easy | |
| CQ-30 | Code Quality | `normalize_for_fts` duplicated 8-line token-streaming block | easy | |
| CQ-31 | Code Quality | `strip_prefixes` allocates `format!()` per loop iteration | easy | |
| AD-40 | API Design | `embedding_to_bytes` Result vs `embedding_slice`/`bytes_to_embedding` Option | easy | |
| AC-20 | Algo Correct | HNSW progress counter includes skipped zero-vectors | easy | |
| AC-21 | Algo Correct | `ModelConfig::resolve` `unwrap_or_default` behind guaranteed `Some` guard | easy | |
| AC-22 | Algo Correct | `bytes_to_embedding` logs at warn, doc says trace | easy | |
| EH-32 | Error Handling | `export_model` conflates missing Python with missing packages | easy | |
| EH-33 | Error Handling | `CQS_LLM_MAX_TOKENS` parse failure silently falls back | easy | |
| EH-37/OB-27 | Error/Obs | `stored_model_name()` swallows DB errors via `.ok()` | easy | |
| OB-23 | Observability | `detect_provider`/`create_session` zero tracing — GPU selection silent | easy | |
| OB-24 | Observability | `LlmConfig::resolve` no tracing span | easy | |
| OB-25 | Observability | `generate_nl_with_call_context_and_summary` zero tracing | easy | |
| OB-26 | Observability | `export_model` Python check logs no details on failure | easy | |
| RB-20 | Robustness | `prepare_index_data` unchecked `n * expected_dim` multiplication | easy | |
| RB-22 | Robustness | `submit_batch_inner` submits empty batch without early return | easy | |
| RB-23 | Robustness | `embedding_dim()` returns `ModelConfig.dim` before inference (may be 0) | easy | |
| RB-24 | Robustness | `strip_prefixes` while loop has no iteration cap | easy | |
| RB-25 | Robustness | `convert/mod.rs` `panic!` for missing FORMAT_TABLE entry | easy | |
| RB-26 | Robustness | `build_with_dim` manual slice indexing — use `chunks_exact` | easy | |
| RB-27 | Robustness | `make_placeholders` unchecked `n * 4` allocation | easy | |
| RB-28 | Robustness | `doc_writer/rewriter.rs` bare `.unwrap()` in non-test code | easy | |
| PB-29 | Platform | `export_model` hardcodes `python3` — fails on Windows | easy | |
| PB-30 | Platform | `export_model` output path not canonicalized with dunce | easy | |
| PB-31 | Platform | `find_ort_provider_dir` picks first subdir — non-deterministic | easy | |
| SEC-19 | Security | `export_model` writes `model.toml` with default umask | easy | |
| SEC-23 | Security | `run_git_diff` missing null byte check (inconsistent with git.rs) | easy | |
| SEC-24 | Security | `LlmConfig` logs `api_base` at info — proxy URLs may have embedded auth | easy | |
| PERF-32 | Performance | `get_summaries_by_hashes` manual placeholders bypasses cached `make_placeholders` | easy | |
| PERF-34 | Performance | `resume` clones entire results HashMap on hash fetch failure | easy | |
| PERF-35 | Performance | Enrichment clones 20K chunk names → use `&str`-keyed map | easy | |
| PERF-36 | Performance | `embed_batch` clones all input texts for tokenizer | easy | |
| PERF-37 | Performance | HNSW full L2 norm per embedding for zero-vector check → short-circuit | easy | |
| PERF-38 | Performance | `resume` clones model+purpose per-result for upsert | easy | |
| PERF-39 | Performance | `prepare_index_data` double-pass (test-only code) | easy | |
| RM-34 | Resources | `batch.lock` file created but never deleted | easy | |
| TC-33 | Test Coverage | `Embedding::try_new` zero tests (NaN/Inf/empty guard) | easy | |
| TC-34 | Test Coverage | `export_model` zero tests (template has confirmed bug) | easy | |
| TC-35 | Test Coverage | `ModelConfig::resolve` with `dim: 0` not tested | easy | |
| TC-36 | Test Coverage | `Config::validate` NaN/Inf not tested — NaN passes clamp unchanged | easy | |
| TC-37 | Test Coverage | `Store::open` dimension parse edge values ("0", "", negative) not tested | easy | |
| TC-39 | Test Coverage | `[embedding]` config minimal tests — missing unknown fields, empty, name confusion | easy | |
| TC-40 | Test Coverage | HNSW `build_batched_with_dim(dim=0)` not tested | easy | |

## P4: Create Issues / Defer

| ID | Category | Finding | Difficulty | Status |
|----|----------|---------|------------|--------|
| EX-30 | Extensibility | `BatchProvider::is_valid_batch_id` hardcodes Anthropic `msgbatch_` prefix | easy | |
| EX-32 | Extensibility | `export-model` doesn't auto-detect dim from config.json | easy | |
| EX-33 | Extensibility | Adding CLI command requires 4-file edits (compiler catches most) | medium | |
| EX-34 | Extensibility | `LlmConfig` missing provider selector — assumes Anthropic | medium | |
| SEC-20 | Security | `EmbeddingConfig` custom paths accept `..` (mitigated by HF hub API) | medium | |
| SEC-21 | Security | API key stored as plain `String` in memory | hard | |
| SEC-22 | Security | `cargo audit`: bincode + number_prefix unmaintained (transitive, no CVEs) | easy | |
| DS-31 | Data Safety | Migration v15-v16 not idempotent (txn protects in practice) | easy | |
| DS-32 | Data Safety | `set_hnsw_dirty` and chunk upsert not atomic (inherent WAL NORMAL trade-off) | medium | |
| RB-21 | Robustness | `load_references` double-unwrap on rayon pool failure | easy | |
