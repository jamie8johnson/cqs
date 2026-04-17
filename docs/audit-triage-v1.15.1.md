# Audit Triage — v1.13.0

132 findings across 16 categories. 2026-03-31 (updated 2026-04-01).

**Status: 80 fixed, 4 wontfix/verified, 48 unfixed (5 P2, 15 P3, 28 P4).** Verified 2026-04-01.

## P1: Easy + High Impact — Fix Immediately

| # | Finding | Status |
|---|---------|--------|
| RB-1 | `cmd_query` panics on multi-byte UTF-8 at byte 200 | ✅ PR #737 |
| RB-2 | `rerank_with_passages` assert_eq! panics on mismatched lengths | ✅ PR #737 |
| AC-2 | `score_candidate` negative scores invert note boost/demotion | ✅ PR #737 |
| AC-5 | `rrf_fuse` double-counts duplicate IDs in semantic list | ✅ PR #737 |
| AC-4 | `bfs_shortest_path` in trace — no node cap, unbounded memory | ✅ PR #737 |
| SHL-1 | MAX_TOKENS_PER_WINDOW=480 hardcoded — handicaps large-context models | ✅ PR #737 |
| SHL-7 | MAX_CONTRASTIVE_CHUNKS=15000 kills contrastive summaries for large codebases | ✅ PR #737 |
| DS-39 | `bytes_to_embedding` silently returns None on dim mismatch — empty search | ✅ PR #737 |
| SEC-1 | `cmd_index` creates .cqs dir without 0o700 permissions | ✅ PR #737 |
| SEC-2 | Telemetry file world-readable with user queries | ✅ PR #737 |
| DOC-1 | Language count says 51 in 7 locations — should be 52 | ✅ PR #737 |
| DOC-3 | lib.rs says E5-base is default — reversed since v1.9.0 | ✅ PR #737 |
| DOC-4 | PRIVACY.md says E5-base is default — reversed | ✅ PR #737 |
| EH-2 | `try_acquire_index_lock` treats I/O errors as "lock held" | ✅ PR #737 |
| EH-3 | CI `find_dead_code` failure shows "0 dead code" — false pass | ✅ PR #737 |

## P2: Medium Effort + High Impact — Fix in Batch

| # | Finding | Status |
|---|---------|--------|
| SHL-3 | NL description .take(1800) char budget assumes 512-token model | ✅ PR #737 |
| SHL-6 | HNSW params (M=24, ef=200) hardcoded for 10k-100k — no config override | ✅ CQS_HNSW_M/EF_CONSTRUCTION/EF_SEARCH env vars |
| SHL-12 | EMBED_BATCH_SIZE=64 fixed regardless of GPU VRAM | ✅ CQS_EMBED_BATCH_SIZE env var |
| AC-1 | Cross-project RRF scores compared as absolute — invalid | |
| DS-38 | DEFERRED transactions + no process lock = SQLITE_BUSY on concurrent index | |
| DS-43 | Batch mode doesn't reload HNSW dim after config change | |
| SEC-3 | ensure_model doesn't verify joined paths stay inside CQS_ONNX_DIR | ✅ PR #738 |
| PERF-2 | upsert_fts_conditional: 2 SQL per chunk instead of batching (22K round trips) | ✅ PR #738 |
| PERF-6 | finalize_results clones ChunkRow for every search result | ✅ PR #738 |
| RM-5 | Contrastive neighbors allocates N*(N-1) pairs — 1.6GB intermediate | ✅ PR #738 |
| CQ-2 | dispatch_test_map duplicates 80-line reverse-BFS with divergent guard | |
| RB-5 | embed_batched panics on unexpected ONNX output shape | |
| EX-3 | Windowing pinned to 512 tokens — thread ModelConfig::max_seq_length | ✅ max_tokens_per_window(model_max_seq) |
| PB-7 | rewrite_file locks source file — Windows mandatory lock hazard | |

## P3: Easy + Low Impact — Fix If Time

| # | Finding | Status |
|---|---------|--------|
| DOC-2 | mod.rs feature flag doc comments missing lang-st | ✅ PR #737 |
| DOC-5 | SECURITY.md missing v9-200k preset | ✅ PR #737 |
| DOC-6 | PRIVACY.md missing v9-200k preset | ✅ PR #737 |
| DOC-7 | README eval section cites 55-query eval | ✅ PR #737 |
| DOC-8 | Migration doc comment says "768-dim E5-base-v2" | ✅ PR #737 |
| DOC-9 | CHANGELOG missing IEC 61131-3 entry | ✅ PR #737 |
| DOC-10 | Cargo.toml description: "51 languages", stale eval metrics | ✅ PR #737 |
| EH-1 | serde_json::to_value().ok() drops errors in 7 locations | ✅ PR #744 (all 3 remaining fixed) |
| EH-4 | build_brief_data degrades silently on failure | ✅ verified correct (degrades with warn) |
| EH-5 | set_permissions failures silently ignored in 6 locations | ✅ PR #744 (10 sites) |
| EH-6 | index_notes parse error → Ok((0, false)) | ✅ PR #744 (eprintln added) |
| CQ-1 | dispatch_trace re-implements BFS instead of calling bfs_shortest_path | |
| CQ-3 | Impact test-suggestion JSON duplicated between cmd and batch | ✅ format_test_suggestions shared (impact/format.rs) |
| CQ-4 | cmd_test_map computes unused _test_names HashSet | ✅ PR #737 |
| CQ-5 | cmd_trace uses N queries vs batch handler's batched query | |
| CQ-6 | filter_by_pattern dead code with #[allow(dead_code)] | ✅ PR #737 |
| OB-1 | plan() missing tracing span | ✅ PR #737 |
| OB-2 | create_client/LlmClient::new missing spans | ✅ PR #737 |
| OB-3 | delete_phantom_chunks missing span | ✅ PR #737 |
| OB-4 | search_filtered outer wrapper missing span | ✅ PR #737 |
| OB-5 | Batch get_ref missing timing | ✅ PR #737 |
| AC-3 | score_name_match missing "query contains name" tier | ✅ PR #737 |
| AC-6 | token_pack first-item override can exceed budget | ✅ PR #737 |
| RB-3 | Language::grammar() dead code that panics | ✅ PR #737 |
| RB-4 | Cli::model_config() panics on temporal coupling | |
| RB-6 | enrichment_hash unstable under LLM whitespace variation | |
| PB-1 | HNSW save lock truncates, load lock doesn't | ✅ PR #737 |
| PB-2 | collect_events non-canonicalized path comparison | |
| PB-3 | find_7z hardcodes Windows path | ✅ PR #744 (env var lookup) |
| PB-4 | Duplicate find_python in export_model and pdf | ✅ PR #744 (shared convert::find_python) |
| PB-6 | ort_runtime_search_dir reads /proc/self/cmdline — no macOS fallback | |
| PERF-1 | find_test_chunks cache clones Vec<ChunkSummary> per call | |
| PERF-3 | Watch mode per-file upsert_type_edges instead of batched | ✅ PR #737 |
| PERF-4 | Watch all_calls linear scan per file | ✅ PR #737 |
| PERF-5 | upsert_chunks_and_calls individual DELETE per caller | ✅ PR #737 |
| SEC-5 | expand_query_for_fts no debug_assert on pre-sanitized input | ✅ debug_assert added |
| SEC-6 | search_by_name FTS query via format! with quotes | |
| TC-1 | Reranker: NaN passthrough, only 6 tests | ✅ PR #744 (NaN/Inf tests) |
| TC-2 | structured_text.rs: untested method/action/type queries | ✅ 8 tests exist |
| TC-3 | rerank_with_passages assert panics (same as RB-2) | ✅ same fix as RB-2 (PR #737) |
| TC-4 | score_candidate: no NaN embedding test | ✅ PR #744 |
| DS-40 | Migration v15→v16 not idempotent on partial rollback | ✅ PR #737 |
| DS-41 | set_hnsw_dirty failure skips reindex permanently | |
| DS-42 | prune_all TOCTOU with concurrent file creation | |
| RM-1 | train_data dedup HashMap unbounded | |
| RM-3 | webhelp merged string no size limit | |
| RM-4 | Watch mtime pruning only at >10K entries | |
| RM-6 | Watch embedder retry clones ModelConfig per attempt | ✅ verified (clone only on retry, not per-attempt) |
| SHL-2 | Reranker max_length=512 hardcoded | ✅ CQS_RERANKER_MAX_LENGTH env var |
| SHL-8 | gather BFS 200-node cap regardless of codebase size | ✅ CQS_GATHER_MAX_NODES env var |
| SHL-9 | impact BFS 10K cap no relation to graph density | ✅ CQS_IMPACT_MAX_NODES env var |
| SHL-10 | CAGRA 2GB RAM cap ignores actual available RAM | |
| SHL-11 | Rayon 4 threads hardcoded | ✅ CQS_RAYON_THREADS env var |
| SHL-15 | Query cache 32 entries small for batch mode | |
| EX-4 | DEFAULT_NAME_BOOST etc scattered across 3 modules | |
| EX-5 | Watch mode constants not configurable | ✅ PR #744 (CQS_WATCH_* env vars) |
| EX-6 | Markdown section size limits hardcoded | ✅ PR #744 (CQS_MD_* env vars) |

## P4: Hard or Low Impact — Issues for Hard, Fix Trivials

| # | Finding | Status |
|---|---------|--------|
| CQ-7 | cmd/batch 2128-line duplication — systemic, needs shared serialization layer | |
| CQ-8 | Four clippy::too_many_arguments suppressions remain | worse (5 now) |
| CQ-9 | lib.rs re-exports ~70 items | |
| EX-1 | New CLI command requires 5+ file changes | |
| EX-2 | HNSW build params not configurable (M, ef_construction) | ✅ same as SHL-6 |
| AD-1 | cmd_blame parameter ordering inconsistent | |
| AD-2 | name vs target inconsistent across 10+ commands | |
| AD-3 | root vs project_root naming split | |
| AD-4 | gather() takes pre-computed embedding vs peer functions taking embedder | |
| AD-5 | 26 commands use bare json: bool instead of OutputArgs | |
| AD-6 | ScoutOptions/PlacementOptions different default patterns | |
| AD-7 | embedding_slice returns Option where Result would be better | |
| AD-8 | analyze_impact takes 5 positional params vs options struct | |
| AD-9 | Embedding::new accepts any data without validation | |
| AD-10 | search_single_project returns Option hiding failures | |
| PB-5 | macOS case-fold diverges from APFS Unicode normalization | |
| SEC-4 | convert_directory output path not validated | |
| RM-2 | Bm25Index::build clones entire corpus | |
| TC-5 | suggest_placement_with_options zero direct tests | |
| SHL-4 | diff.rs comment "768 dims" | ✅ PR #737 |
| SHL-5 | embedder comment "[batch, seq_len, 768]" | ✅ PR #737 |
| SHL-13 | RRF K=60 not tuned for code search | ✅ PR #744 (documented + CQS_RRF_K env var) |
| SHL-14 | DEFAULT_THRESHOLD=0.3 not documented as model-specific | ✅ PR #744 (documented) |

## Untriaged Batch (OB-6 + RX-1 through RX-22) — Triaged 2026-04-01

### Already Fixed (11)

| # | Finding | Status |
|---|---------|--------|
| OB-6 | Carryover OB-28–32 resolved | ✅ verified |
| RX-5 | No per-query diagnostics | ✅ PR #740 |
| RX-6 | No enrichment ablation | ✅ PR #740 |
| RX-11 | Enrichment layers not togglable | ✅ same as RX-6 |
| RX-15 | Reranker max_length hardcoded | ✅ CQS_RERANKER_MAX_LENGTH |
| RX-22 | No structured eval output | ✅ PR #740 |
| RX-3 | Reranker model not in config file | ✅ PR #743 |
| RX-7 | fixture_path requires match arm per language | ✅ PR #743 |
| RX-13 | NL char budget env var read not cached | ✅ PR #743 |
| RX-14 | Windowing overlap fixed at 64 tokens | ✅ PR #743 |
| RX-21 | Triplet lacks metadata for filtering | ✅ PR #743 |

### P3 (2)

| # | Finding | Status |
|---|---------|--------|
| RX-19 | Enrichment assembly order not configurable | wontfix (validated order) |
| RX-20 | No hook for custom eval metrics | wontfix (5 metrics sufficient) |

### P4: Research Infra (10)

| # | Finding | Status |
|---|---------|--------|
| RX-1 | No A/B test infra for model comparison | |
| RX-2 | ScoringConfig not runtime-overridable | |
| RX-4 | Eval cases hardcoded in Rust (partially: JSON real eval exists) | |
| RX-8 | Training data lacks enriched variants | |
| RX-9 | BM25 hard neg selection not pluggable | |
| RX-10 | No call-graph contrastive pair generation | |
| RX-12 | Enrichment hash couples all layers | |
| RX-16 | Reference system lacks A/B comparison | |
| RX-17 | Different-dim models can't share HNSW | |
| RX-18 | No trait abstraction for scoring | |
