# Audit Triage — v1.0.7

## P1: Easy + High Impact (fix now)

| # | Finding | Status |
|---|---------|--------|
| EH-9 | Silent truncation on embedding count mismatch — add assert | ✅ fixed |
| EH-11 | Drain-before-write loses items on store error — drain after success | ✅ fixed |
| CQ-12 | `generate_nl_with_call_context` has zero tests — core SQ-4 function | ✅ fixed (4 tests) |
| CQ-7 | `replace_file_chunks` dead code — 100 lines, zero production callers | ✅ removed |
| CQ-8 | Doc comment merged accident — `extract_file_context` has wrong doc | ✅ fixed |

## P2: Easy + Medium Impact (fix in batch)

| # | Finding | Status |
|---|---------|--------|
| AD-13 | `generate_nl_with_call_context` 5 positional params — fold into `CallContext` | |
| AD-14 | `callee_document_frequencies` misnamed — returns counts not frequencies | ✅ renamed to `callee_caller_counts` |
| CQ-10 | Same as AD-14 — IDF metric name/impl mismatch | ✅ fixed with AD-14 |
| EH-8 | Progress bar dangling on error — use guard pattern | ✅ fixed (closure pattern) |
| EH-10 | `update_embeddings_batch` no-ops silently for missing IDs | ✅ fixed (rows_affected check) |
| EH-12 | `unwrap()` on progress template — use `expect()` | ✅ fixed |
| OB-8 | `enrichment_pass` missing skip metrics and timing | |
| CQ-9 | `NlTemplate::Standard` doc stale — says "current" but Compact is used | ✅ fixed |

## P3: Easy + Low Impact (fix if time)

| # | Finding | Status |
|---|---------|--------|
| AD-15 | `update_embeddings_batch` String role undocumented — add doc comment | ✅ fixed |
| AD-16 | `chunks_paged` cursor convention inconsistent with `embedding_batches` | |
| AD-17 | `enrichment_pass` `quiet: bool` leaks CLI concern — add doc comment | |
| OB-10 | `chunks_paged` no result metrics in span | |
| OB-11 | `flush_enrichment_batch` missing tracing span | |
| OB-12 | `callee_document_frequencies` no result count in span | |
| DOC-1 | CONTRIBUTING.md missing vue.rs, aspx.rs | ✅ fixed |
| DOC-2 | IDF threshold not in rustdoc | |
| DOC-3 | README schema version auto-migration unclear | |
| DOC-4 | store/chunks.rs module doc incomplete | |

## Batch 2 — P1

| # | Finding | Status |
|---|---------|--------|
| RB-B1 | Call context lookup by name not ID — `new`/`parse`/`build` get merged callers across files | ✅ fixed (skip ambiguous names) |

## Batch 2 — P2

| # | Finding | Status |
|---|---------|--------|
| AC-B1 | IDF comment says >10% but code uses >=0.10 — boundary mismatch | ✅ fixed |
| AC-B2 | `page_size=500` is a `let` not a `const` — inconsistent with `ENRICH_EMBED_BATCH` | ✅ fixed → `ENRICHMENT_PAGE_SIZE` |
| TC-B1 | `update_embeddings_batch` and `chunks_paged` — zero unit tests | |
| TC-B2 | `callee_caller_counts` — no unit tests | |

## Batch 2 — P3

| # | Finding | Status |
|---|---------|--------|
| TC-B3 | No integration test for `enrichment_pass` | |
| RB-B2 | Enrichment pass pre-loads full call graph into memory | |
| EX-B1 | Four enrichment tuning params hardcoded, no config surface | |

## Batch 3 — P2

| # | Finding | Status |
|---|---------|--------|
| RM-B2 | Enrichment creates fresh Embedder — doubles model init time (~500ms) | deferred — refactoring pipeline embedder ownership is non-trivial |
| PERF-B3 | `generate_nl_description` called twice per enriched chunk | deferred — storing base NL requires schema change |

## Batch 3 — P3

| # | Finding | Status |
|---|---------|--------|
| DS-B1 | Partial enrichment on failure — already-enriched not tracked for skip | |
| DS-B2 | `chunks_paged` cursor may return fewer rows than page_size after pruning | |
| DS-B3 | `name_file_count` HashMap clones names unnecessarily | |
| PERF-B2 | Full enrichment on every `cqs index` even for incremental changes | |

## Batch 3 — P4

| # | Finding | Status |
|---|---------|--------|
| PERF-B1 | `chunks_paged` loads full content for skipped chunks — fetch ID-only first | |
| RM-B1 | Pre-loaded callers/callees maps ~60-80MB for 50K chunks | |

## Red Team — P1

| # | Finding | Status |
|---|---------|--------|
| RT-DATA-1 | HNSW ID desync on zero-vector skip — `base_idx + i` vs `id_map.len()` | ✅ fixed |

## Red Team — P2

| # | Finding | Status |
|---|---------|--------|
| RT-INJ-1 | `CQS_PDF_SCRIPT` env var arbitrary script execution via malicious .envrc | |
| RT-DATA-5 | Batch/chat OnceLock caches never invalidate — stale after external index | |
| RT-DATA-6 | SQLite commit and HNSW save not atomic — crash leaves desync | |

## Red Team — P3

| # | Finding | Status |
|---|---------|--------|
| RT-DATA-2 | Enrichment pass no idempotency marker — partial state on interrupt | |
| RT-DATA-3 | Watch mode HNSW orphan accumulation — no deletion API | |
| RT-FS-1 | `read_context_lines` reads files from DB paths without boundary check | |
| RT-FS-2 | `resolve_parent_context` same gap as RT-FS-1 | |
| RT-INJ-2 | Batch `read_line` OOM before 1MB check (post-hoc) | |
| RT-RES-1 | Chat mode no input length limit | |
| RT-DATA-4 | Notes file lock vs atomic rename race | |

## Red Team — P4

| # | Finding | Status |
|---|---------|--------|
| RT-RES-2 | `node_letter()` fragile u8 cast | |

## P4: Medium/Hard or Low Impact (create issues)

| # | Finding | Status |
|---|---------|--------|
| OB-9 | `update_embeddings_batch` per-row UPDATE — batch with QueryBuilder | |
| CQ-11 | `batch_count_query` format! SQL injection pattern — add enum restriction | |
