# Audit Triage — v1.5.0 (2026-03-25)

82 findings across 14 categories. 6th full audit.

## P1 — Fix immediately (easy + high impact)

| # | Finding | Category | Status |
|---|---------|----------|--------|
| RB-15 | `find_contrastive_neighbors` panics on mismatched embedding dims | Robustness | |
| EH-24 | `submit_or_resume` swallows store error, loses completed batch results | Error Handling | |
| EH-25 | Unknown batch status abandoned without logging, causes duplicate resubmit | Error Handling | |
| SEC-14 | `git_diff_tree`/`git_show` pass unvalidated SHA to subprocess | Security | |
| SEC-15 | `CQS_API_BASE` accepts http:// — API key sent in cleartext | Security | |
| SEC-17 | `git_show` path param not validated — argument injection | Security | |
| AC-18 | `sanitize_fts_query` produces whitespace-only → FTS5 MATCH error | Algorithm Correctness | |
| RB-17 | Same root cause as AC-18 (whitespace-only FTS query) | Robustness | |
| DOC-22 | SECURITY.md still lists LoRA as default model | Documentation | |
| DOC-23 | PRIVACY.md still lists LoRA as default model | Documentation | |
| DOC-24 | Model download size ~547MB stale (base E5 is ~438MB) | Documentation | |
| DOC-25 | README table header says "LoRA" — should drop it | Documentation | |
| DOC-26 | ROADMAP.md says v1.4.2, should be v1.5.0 | Documentation | |
| RM-30 | Watch mtime pruning condition tautological (dead branch) | Resource Management | |
| OB-19 | `compute_hints_batch` missing tracing span | Observability | |
| OB-20 | `test_reachability` missing tracing span | Observability | |
| OB-21 | `embed_query` missing tracing span | Observability | |
| OB-22 | `flush_enrichment_batch` missing tracing span | Observability | |

## P2 — Fix in batch (medium effort + high impact)

| # | Finding | Category | Status |
|---|---------|----------|--------|
| DS-20 | Batch resume stores stale results after --force rebuild | Data Safety | |
| DS-25 | Concurrent --llm-summaries races on pending batch ID | Data Safety | |
| DS-21 | Contrastive N×N matrix no size cap — OOM on large repos | Data Safety | |
| RM-28 | Same root cause as DS-21 (N×N OOM) | Resource Management | |
| RM-29 | `load_references` unbounded rayon par_iter | Resource Management | |
| PERF-25 | Contrastive per-row full sort O(n² log n) → partial sort | Performance | |
| PERF-26 | Deferred type edges per-file txns → batch | Performance | |
| PERF-29 | Enrichment hash fetch per-page → batch | Performance | |
| PERF-30 | Call graph string duplication (~40MB at 500K edges) | Performance | |
| RB-16 | `search_across_projects` double-unwrap on rayon pool fail | Robustness | |
| CQ-23 | LLM chunk scanning loop duplicated 3-4 places | Code Quality | |
| SEC-16 | Function names injected into LLM prompt (indirect injection) | Security | |
| EX-23 | `doc_format_for` not in LanguageDef — last holdout | Extensibility | |
| AC-16 | Waterfall budget tracking inconsistent — can exceed budget | Algorithm Correctness | |
| AD-34 | `score_candidate` 9 positional args with clippy suppression | API Design | |
| DS-24 | HNSW save rollback doesn't restore originals | Data Safety | |
| PB-25 | Mtime second-precision on WSL — misses sub-second writes | Platform Behavior | |
| TC-27 | BatchPhase2 error paths untested | Test Coverage | |
| CQ-27 | cli/mod.rs 2043 lines — needs split | Code Quality | |

## P3 — Fix if time (easy + low impact)

| # | Finding | Category | Status |
|---|---------|----------|--------|
| DOC-27 | README TL;DR "89.1% Recall@1" ambiguous | Documentation | |
| DOC-28 | Cargo.toml NDCG@10 0.965 doesn't match evals | Documentation | |
| EH-23 | Contrastive failure silently degrades summaries | Error Handling | |
| EH-26 | `create_context` ignores missing mtime | Error Handling | |
| EH-27 | `content_hash` unwrap_or_default masks schema issues | Error Handling | |
| EH-28 | Metadata unwrap_or_default hides missing version | Error Handling | |
| EH-31 | Gather bridge errors reduce quality silently | Error Handling | |
| AD-30 | cosine_similarity vs full_cosine_similarity return type mismatch | API Design | |
| AD-31 | Blame --depth overloads -n with different semantics | API Design | |
| AD-32 | CQS_API_BASE breaks CQS_LLM_* prefix convention | API Design | |
| AD-33 | Residual to_json() methods on Serialize types | API Design | |
| AD-35 | TrainDataConfig/Stats missing Debug/Serialize | API Design | |
| AD-36 | llm::Client name collision | API Design | |
| CQ-22 | index_pack duplicates token_pack | Code Quality | |
| CQ-24 | open_project_store / readonly near-identical | Code Quality | |
| CQ-26 | waterfall_pack 150 lines repetitive | Code Quality | |
| PERF-27 | HashSet filter uses linear any() instead of contains() | Performance | |
| PERF-28 | rrf_fuse HashMap allocation per query | Performance | |
| RM-31 | Contrastive double-buffers embeddings | Resource Management | |
| AC-17 | test_reachability equivalence class self-loop | Algorithm Correctness | |
| AC-19 | parent_boost cross-file name collision | Algorithm Correctness | |
| DS-22 | Enrichment hash f32 IDF boundary non-determinism | Data Safety | |
| DS-23 | Contrastive empty map on embedding fetch failure | Data Safety | |
| EX-24 | build_doc_prompt hardcoded language appendix | Extensibility | |
| PB-22 | is_test_chunk forward-slash-only path split | Platform Behavior | |
| PB-23 | check_origins_stale mixed path separators | Platform Behavior | |
| PB-26 | Watch canonicalize on deleted files | Platform Behavior | |
| PB-27 | nl.rs path split forward-slash only | Platform Behavior | |
| PB-28 | markdown.rs link slug forward-slash only | Platform Behavior | |
| RB-18 | compute_modify_threshold empty results | Robustness | |
| RB-19 | apply_doc_edits silent skip on content change | Robustness | |
| TC-24 | full_cosine_similarity zero direct tests | Test Coverage | |
| TC-25 | enrichment_pass zero direct tests | Test Coverage | |
| TC-26 | generate_nl_with_call_context_and_summary zero tests | Test Coverage | |
| TC-28 | Bm25Index select_negatives missing failure test | Test Coverage | |
| TC-29 | full_cosine_similarity NaN path untested | Test Coverage | |
| TC-30 | IDF callee filtering threshold untested | Test Coverage | |

## P4 — Create issues (hard or low impact)

| # | Finding | Category | Status |
|---|---------|----------|--------|
| EX-25 | nl.rs field extraction hardcoded for 6 languages | Extensibility | |
| EX-26 | LLM module tightly coupled to Anthropic API | Extensibility | |
| EX-27 | EMBEDDING_DIM compile-time constant | Extensibility | |
| EX-28 | 30+ scattered BATCH_SIZE constants | Extensibility | |
| CQ-25 | LLM-generated doc comments ~2000 lines boilerplate | Code Quality | |
| PB-24 | prune_missing case-insensitive filesystem mismatch | Platform Behavior | |
| EH-29 | read_context_lines errors silently dropped | Error Handling | |
| EH-30 | Bm25 empty docs as hard negatives | Error Handling | |
