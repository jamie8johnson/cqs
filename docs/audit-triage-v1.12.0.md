# Audit Triage — v1.9.0+

88 findings across 14 categories (3 batches). Triaged 2026-03-29.

## P1: Easy + High Impact — Fix Immediately

| # | Category | Finding | Status |
|---|----------|---------|--------|
| RB-34 | Robustness | `build_with_dim(dim=0)` → `chunks_exact(0)` panic | |
| RB-29 | Robustness | Empty chunk name → zero-vector embedding → silent search degradation | |
| DS-33 | Data Safety | `delete_phantom_chunks` exceeds SQLite 999-param limit on 1000+ chunk files | |
| RM-37 | Resource Mgmt | 100MB response cap bypassed via chunked transfer encoding (red team fix ineffective) | |
| DOC-38 | Documentation | README says "E5-base-v2 default" — contradicts actual BGE-large default | |
| DOC-42 | Documentation | SECURITY.md default model download size wrong, labels swapped | |
| DOC-44 | Documentation | Migration log message says "768-dim" — misleads users on current default | |
| DOC-47 | Documentation | README config example says "defaults to e5-base" | |
| AD-44 | API Design | CLI `--help` says "e5-base (default)" — wrong since v1.9.0 | |
| AD-47 | API Design | `EMBEDDING_DIM` doc comment says "E5-base-v2 (768)" — value is 1024 | |
| PB-32 | Platform | `CQS_ONNX_DIR` path not canonicalized with dunce | |
| SEC-25 | Security | `CQS_ONNX_DIR` missing dunce::canonicalize (same as PB-32) | |

## P2: Medium Effort + High Impact — Fix in Batch

| # | Category | Finding | Status |
|---|----------|---------|--------|
| AC-25 | Algorithm | BFS node cap allows unbounded overshoot — hub function adds all callers past 10K | |
| AC-28 | Algorithm | `full_cosine_similarity` f32 accumulation at 1024-dim — precision loss vs SIMD path | |
| PERF-40 | Performance | N individual UPDATEs per enrichment batch — 10K round-trips per pass | |
| PERF-43 | Performance | O(N²) heap extraction in contrastive neighbors after BLAS matmul | |
| PERF-44 | Performance | Notes loaded + NoteBoostIndex rebuilt twice per search | |
| PERF-46 | Performance | `cached_notes_summaries` deep-clones all notes on every warm-cache hit | |
| EH-40 | Error Handling | `resume()` double `get_all_content_hashes()` call — TOCTOU | |
| EH-43 | Error Handling | `submit_fresh` swallows `set_pending` failure — batch ID lost (carryover) | |
| DS-35 | Data Safety | HNSW incremental inserts create orphan vectors across watch restarts | |
| RM-39 | Resource Mgmt | Contrastive neighbors peak ~961MB vs documented ~550MB — missing `drop(matrix)` | |
| RB-31 | Robustness | `Language::grammar()` panics on grammar-less languages; 9 callers no fallback | |
| TC-41 | Test Coverage | watch.rs 789 lines, zero tests | |
| TC-43 | Test Coverage | SEC-20 path traversal rejection has zero tests (security-relevant) | |
| CQ-34 | Code Quality | `process_file_changes` 12 parameters — needs WatchState struct | |
| SEC-28 | Security | Custom `[embedding] repo` bypasses SEC-18 validation | |

## P3: Easy + Low Impact — Fix If Time

| # | Category | Finding | Status |
|---|----------|---------|--------|
| DOC-39 | Documentation | embedder/mod.rs three stale "E5-base-v2" doc comments | |
| DOC-40 | Documentation | store/helpers.rs ModelInfo docs say "E5-base-v2, 768-dim" | |
| DOC-41 | Documentation | test_helpers.rs and scoring/candidate.rs say "768-dim" | |
| DOC-43 | Documentation | nl/mod.rs comment attributes 512-token limit to E5-base-v2 | |
| DOC-45 | Documentation | CONTRIBUTING.md missing cqs-verify skill | |
| DOC-46 | Documentation | CQS_ONNX_DIR not documented in README or SECURITY.md | |
| AD-45 | API Design | EmbeddingConfig serde default comment says "e5-base" | |
| AD-46 | API Design | store::MODEL_NAME/EXPECTED_DIMENSIONS comments say "E5-base-v2" | |
| AD-48 | API Design | Three layers of default model name indirection (redundant aliasing) | |
| AD-50 | API Design | `VectorIndex` trait missing `dim()` method | |
| AD-53 | API Design | `ModelInfo.dimensions` u32 vs ModelConfig/Store usize — forces casts | |
| AD-54 | API Design | `Embedding::new` vs `try_new` ambiguity — no guidance on which to use | |
| OB-28 | Observability | `detect_provider`/`create_session` still missing spans (OB-23 carryover) | |
| OB-29 | Observability | `parse_unified_diff` no span, silent on malformed input | |
| OB-30 | Observability | `find_changed_functions` no span (train_data path) | |
| OB-31 | Observability | `load_audit_state`/`save_audit_state` no spans | |
| OB-32 | Observability | `update_embeddings_batch` silent on zero-row batch | |
| EH-41 | Error Handling | `notes_need_reindex` error silently swallowed, no log | |
| EH-42 | Error Handling | `chunk_count()` error silently swallowed, no log | |
| EH-44 | Error Handling | `response.text().unwrap_or_default()` in 4 HTTP error paths | |
| CQ-35 | Code Quality | `check_model_version` dead code with `#[allow(dead_code)]` | |
| RB-30 | Robustness | `checkpoint_sha.as_ref().unwrap()` fragile invariant in train_data | |
| RB-32 | Robustness | `CagraIndex::search` returns empty Vec on error — indistinguishable from no results | |
| AC-29 | Algorithm | `BoundedScoreHeap::new(0)` silently discards all pushes | |
| AC-30 | Algorithm | `token_pack` vs `index_pack` inconsistent at budget=0 | |
| DS-36 | Data Safety | HNSW save backup `let _ = rename()` silently discards failure | |
| PERF-41 | Performance | `get_enrichment_hashes_batch` manual placeholders instead of helper | |
| PERF-42 | Performance | `delete_phantom_chunks` comment says "batch at 500" but doesn't batch | |
| RM-35 | Resource Mgmt | `clear_session` drops ONNX session but not LRU query cache | |
| RM-36 | Resource Mgmt | Query cache doc says "768 floats" — now 1024 | |
| EX-36 | Extensibility | `doc_format` stringly-typed tag — no compile-time safety | |
| EX-37 | Extensibility | `cmd_init` download size heuristic hardcoded, not on ModelConfig | |
| EX-38 | Extensibility | `CAGRA_THRESHOLD` (5000) no env var or config override | |
| PB-35 | Platform | `ModelConfig.onnx_path` forward-slash path used in PathBuf::join — no contract | |
| SEC-26 | Security | `api_base` URL with credentials logged at info/warn level (SEC-24 carryover) | |
| SEC-27 | Security | `train_data::git_show` missing colon check on path param | |
| TC-42 | Test Coverage | `delete_phantom_chunks` zero direct tests — 3 untested code paths | |
| TC-44 | Test Coverage | `SearchFilter::validate` zero tests — NaN boost, path length untested | |
| TC-49 | Test Coverage | `validate_finite_f32` zero direct tests | |

## P4: Hard or Low Impact — Create Issues

| # | Category | Finding | Status |
|---|----------|---------|--------|
| CQ-36 | Code Quality | `doc_comment_pass` duplicates chunk scanning pattern (CQ-23 carryover) | |
| CQ-37 | Code Quality | `nl/mod.rs` still 1056 lines — generation functions could split | |
| CQ-38 | Code Quality | `parser/markdown.rs` 2030 lines — largest file, natural split points | |
| CQ-39 | Code Quality | Nine `clippy::too_many_arguments` suppressions | |
| AD-49 | API Design | `--json` vs `--format` inconsistency across commands | |
| AD-51 | API Design | `Embedder::new` vs `new_cpu` near-identical constructors (CQ-28 carryover) | |
| AD-52 | API Design | `ModelInfo` lives in store::helpers but belongs in embedder::models | |
| AC-26 | Algorithm | `test_reachability` can't detect test-to-test chains across equivalence classes | |
| AC-27 | Algorithm | Waterfall budget surplus cascading (AC-16 carryover, cap added but incomplete) | |
| AC-31 | Algorithm | `reverse_bfs(max_depth=0)` undocumented edge case | |
| EX-35 | Extensibility | `extract_method_name_from_line` hardcodes 13 visibility modifiers outside LanguageDef | |
| EX-39 | Extensibility | `HYDE_MAX_TOKENS` bypasses LlmConfig — not configurable | |
| TC-45 | Test Coverage | `ensure_model`/`CQS_ONNX_DIR` zero tests — offline deployment untested | |
| TC-46 | Test Coverage | llm/batch.rs 820 lines, 4 tests, all happy-path (TC-27/TC-32 carryover) | |
| TC-47 | Test Coverage | `build_batched_with_dim(dim=0)` untested (TC-40 carryover) | |
| TC-48 | Test Coverage | `clamp_config_f32` NaN passthrough → zero search results untested | |
| DS-34 | Data Safety | Watch reindex_notes TOCTOU — reads notes.toml then writes SQLite, gap for concurrent edit | |
| DS-37 | Data Safety | Watch reindex_files two-transaction gap — phantoms visible between commits | |
| RB-33 | Robustness | `rewrite_file` no file lock — concurrent --improve-docs can overwrite | |
| PERF-45 | Performance | `EMBED_BATCH_SIZE` halved from 64→32 reactively without diagnosis | |
| RM-38 | Resource Mgmt | `git_log(max_commits=0)` loads entire history into Vec | |
| RM-40 | Resource Mgmt | HNSW fully loaded into RAM, not mmapped (~800MB at 100k/1024-dim) | |
| PB-33 | Platform | `export_model` uses `display()` instead of `to_string_lossy()` for subprocess | |
| PB-34 | Platform | macOS prune_missing case-fold diverges from APFS for non-ASCII (PB-24 carryover) | |
| SEC-29 | Security | `webhelp_to_markdown` walks content/ without root symlink check | |
