# Adaptive Retrieval — Query-Aware Search Strategy Selection

## Goal

Classify incoming queries at runtime and route each to the best retrieval mechanism. Based on v2 eval data showing different categories benefit from different approaches.

## Critical Constraint: Summaries Are Index-Time, Not Search-Time

LLM summaries are prepended to NL descriptions **before embedding**. The embedding already contains the summary signal. Toggling summaries at search time requires **dual embeddings** (v2) — each chunk stored twice.

## Versions

### v1: Mechanism Routing (+2-4pp, no schema change)

Route between different retrieval *mechanisms* with the same index:
- NameOnly (FTS5) for identifier queries — skip embedding entirely
- Dense + type filter for structural queries
- Dense + SPLADE for structural/type queries (if SPLADE-Code available)
- Dense default for everything else

### v2: Dual Embeddings (+10-15pp, schema migration)

Store two embeddings per chunk. Route conceptual/behavioral to base embedding, structural/multi_step to summary-enriched embedding. Full oracle routing.

This spec covers **both v1 and v2**.

## Background: Per-Category Strategy Performance (v2 265q eval)

| Category | Dense only | + Summaries | + HyDE | Best |
|----------|-----------|-------------|--------|------|
| identifier_lookup | 100% | 96% (-4pp) | 100% | **Dense** |
| structural | 48% | 62% (+14pp) | 62% (+14pp) | **Summaries** |
| behavioral | 61% | 50% (-11pp) | 46% (-15pp) | **Dense** |
| conceptual | 37% | 22% (-15pp) | 15% (-22pp) | **Dense** |
| multi_step | 32% | 50% (+18pp) | 41% (+9pp) | **Summaries** |
| negation | 69% | 63% (-6pp) | 58% (-11pp) | **Dense** |
| type_filtered | 25% | 37% (+12pp) | 37% (+12pp) | **Summaries** |
| cross_language | 75% | 75% (0pp) | 75% (0pp) | **Either** |

Note: "Dense only" and "+ Summaries" are different index states, not search-time toggles. v1 cannot switch between them. v2 can.

## v1 Architecture: Mechanism Routing

```
cmd_query receives query text
    │
    ▼
┌──────────────────┐
│ classify_query()  │  ← BEFORE embedding (saves ~50ms for identifiers)
│  (< 1ms)          │
└──────┬───────────┘
       │
       ├─ IDENTIFIER (High) ───┐
       │   snake_case, ::, .    │
       │                        ▼
       │           ┌──────────────────────┐
       │           │ search_by_name()     │  ← FTS5, no embedding, ~1ms
       │           │ if 0 results →       │
       │           │   embed + dense      │  ← fallback
       │           └──────────────────────┘
       │
       ├─ STRUCTURAL (Medium) ─┐
       │   "functions that",    │
       │   type keywords        ▼
       │           ┌──────────────────────┐
       │           │ embed + dense        │
       │           │ + type filter boost  │  ← extract_type_hints()
       │           │ + SPLADE if avail    │
       │           └──────────────────────┘
       │
       └─ DEFAULT (any) ───────┐
           behavioral,          │
           conceptual,          ▼
           negation, etc.  ┌──────────────────────┐
                           │ embed + dense         │  ← current behavior exactly
                           └──────────────────────┘
```

## Implementation Plan

### Phase 1: QueryClassifier (~0.5 day)

**`src/search/router.rs`** (NEW)

```rust
/// Query categories for adaptive routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryCategory {
    IdentifierLookup,
    Structural,
    Behavioral,
    Conceptual,
    MultiStep,
    Negation,
    TypeFiltered,
    CrossLanguage,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence { High, Medium, Low }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStrategy {
    NameOnly,           // FTS5, skip embedding
    DenseDefault,       // current path
    DenseWithTypeHints, // add type filter from query text
    DenseWithSplade,    // dense + sparse fusion
}

/// Classification result. Infallible — always returns a valid classification.
pub struct Classification {
    pub category: QueryCategory,
    pub confidence: Confidence,
    pub strategy: SearchStrategy,
    pub type_hints: Option<Vec<ChunkType>>,  // for DenseWithTypeHints
}

/// Classify a query. Pure function, no I/O, cannot fail.
pub fn classify_query(query: &str) -> Classification { ... }
```

Classification rules:
```
IdentifierLookup (High):
  - All tokens match [a-zA-Z0-9_:.]+ (no spaces between non-identifier chars)
  - Contains :: or . (qualified names)
  - Single token with mixed case or underscores
  - NOT just common English words

Structural (Medium):
  - Contains: "struct", "enum", "trait", "impl", "interface", "class" as NL words
  - Pattern: "functions that", "methods that", "types that"
  - Pattern: "that return", "that take", "with signature"

Negation (High):
  - Contains: "not ", "without ", "except ", "never ", "avoid "
  - Override: negation signal trumps other categories

TypeFiltered (Medium):
  - Pattern: "all structs", "every enum", "test functions"
  - Extract type hints for boost (not filter)

CrossLanguage (High):
  - Mentions 2+ language names, or "equivalent", "translate", "port"

Behavioral (Medium):
  - Verbs: validates, processes, handles, manages, computes, parses
  - Pattern: "how does", "what does", "code that"

Conceptual (Medium):
  - Abstract nouns without code keywords: pattern, architecture, approach
  - Short (1-3 words) that aren't identifiers

MultiStep (Low):
  - Conjunctions: "and then", "before", "after", "where X and Y"

Unknown (Low):
  - No rules matched → default
```

**`src/search/mod.rs`** — Add `pub mod router;`

### Phase 2: Pipeline Integration (~1 day)

**`src/cli/commands/search/query.rs`** — Wire classifier BEFORE embedding

```rust
fn cmd_query_project(ctx: &QueryContext<'_>) -> Result<()> {
    let classification = if ctx.strategy == Strategy::Auto {
        let c = classify_query(&ctx.query);
        tracing::info!(
            category = %c.category,
            confidence = %c.confidence,
            strategy = %c.strategy,
            "Query classified"
        );
        c
    } else {
        // Explicit --strategy flag
        Classification::from_explicit(ctx.strategy)
    };

    match classification.strategy {
        SearchStrategy::NameOnly => {
            // Reuse existing cmd_query_name_only path
            let results = cmd_query_name_only_inner(ctx)?;
            if results.is_empty() {
                tracing::info!("NameOnly returned 0 results, falling back to dense");
                telemetry::log_fallback("name_only", "dense");
                // Fall through to dense
            } else {
                telemetry::log_routed(classification, results.len(), false);
                return display_results(ctx, results);
            }
        }
        SearchStrategy::DenseWithTypeHints => {
            // Boost (not filter) matching types
            if let Some(hints) = &classification.type_hints {
                tracing::debug!(types = ?hints, "Applying type boost");
            }
        }
        _ => {}
    }

    // Dense path (default or fallback)
    let embedding = ctx.embedder()?.embed(&ctx.query)?;
    let mut filter = ctx.build_filter();
    // Type hints as boost, not hard filter
    if let Some(hints) = &classification.type_hints {
        filter.type_boost_types = Some(hints.clone());
    }
    let results = ctx.store.search_filtered(&embedding, &filter, ctx.limit, ctx.threshold)?;
    telemetry::log_routed(classification, results.len(), false);
    display_results(ctx, results)
}
```

**`src/cli/batch/handlers/search.rs`** — Same wiring for batch mode

**`src/cli/definitions.rs`** — Add `--strategy auto|name|dense|type-hints|splade` flag
  - Default: `auto`
  - `name` = force NameOnly
  - `dense` = force dense (current behavior)
  - `type-hints` = force dense + type extraction
  - `splade` = existing `--splade` flag behavior

**`src/store/helpers/search_filter.rs`** — Add `type_boost_types: Option<Vec<ChunkType>>` field
  - Used in scoring to boost (not filter) matching chunk types
  - Boost factor: 1.2x for matching type (configurable)

### Phase 3: SpladeEncoder Pre-pooled Output (~0.5 day)

**`src/splade/mod.rs`** — Auto-detect output format

```rust
// After ONNX inference, check output name and shape
let output_key = if outputs.contains_key("sparse_vector") {
    "sparse_vector"  // pre-pooled (SPLADE-Code 0.6B export)
} else if outputs.contains_key("logits") {
    "logits"  // raw logits (our trained models)
} else {
    return Err(SpladeError::InferenceFailed(format!(
        "Unknown output: {:?}", outputs.keys().collect::<Vec<_>>()
    )));
};

match output_key {
    "sparse_vector" => {
        // Pre-pooled: [batch, vocab_size] — threshold directly
        tracing::debug!("Using pre-pooled SPLADE output");
        let (shape, data) = output.try_extract_tensor::<f32>()?;
        if shape.len() != 2 {
            return Err(SpladeError::InferenceFailed(
                format!("Pre-pooled output expected 2D, got {}D", shape.len())
            ));
        }
        // Threshold and collect non-zero entries
        ...
    }
    "logits" => {
        // Raw logits: existing path [batch, seq_len, vocab_size]
        ...
    }
}
```

### Phase 4: Telemetry (~0.5 day)

**`src/cli/telemetry.rs`** — Extended event

```json
{
    "ts": 1234,
    "cmd": "search",
    "query": "search_filtered",
    "category": "identifier_lookup",
    "confidence": "high",
    "strategy": "name_only",
    "fallback": false,
    "results": 3
}
```

Log both initial route AND fallback if triggered.

### Tests

#### Happy path (13 tests)
```
test_classify_identifier_snake_case         "search_filtered" → Identifier, High
test_classify_identifier_qualified          "HashMap::new" → Identifier, High
test_classify_identifier_camel              "SearchFilter" → Identifier, High
test_classify_behavioral                    "validates user input" → Behavioral, Medium
test_classify_negation                      "sort without allocating" → Negation, High
test_classify_structural                    "functions that return Result" → Structural, Medium
test_classify_type_filtered                 "all test functions" → TypeFiltered, Medium
test_classify_cross_language                "Python equivalent of map" → CrossLanguage, High
test_classify_conceptual                    "dependency injection" → Conceptual, Medium
test_classify_multi_step                    "find errors and retry" → MultiStep, Low
test_classify_unknown                       "asdf jkl" → Unknown, Low
test_extract_type_hints_struct              "find all structs" → [Struct]
test_extract_type_hints_none               "handle errors" → None
```

#### Adversarial (15 tests)
```
test_classify_empty                         "" → Unknown, Low (no panic)
test_classify_single_char                   "a" → Identifier, Medium (could be variable)
test_classify_very_long                     10K chars → completes <1ms, no panic
test_classify_unicode_identifier            "日本語_関数" → Identifier, Medium
test_classify_path_like                     "src/store/mod.rs" → Identifier, High (has . and /)
test_classify_only_stopwords                "the a an of" → Unknown, Low
test_classify_special_chars                 "fn<T: Hash>()" → Structural, Medium (contains fn)
test_classify_all_caps                      "WHERE IS THE ERROR" → Behavioral, Medium
test_classify_numbers                       "404" → Unknown, Low
test_classify_hex                           "0xFF" → Identifier, High
test_classify_mixed_signals                 "not struct" → Negation, High (negation trumps)
test_classify_sql_injection                 "'; DROP TABLE--" → Unknown, Low (no panic)
test_classify_null_bytes                    "foo\0bar" → handles gracefully
test_classify_type_hint_wrong_extraction    "error handling" → NOT [Enum] (error is ambiguous)
test_classify_identifier_common_word        "error" → Identifier, Medium (could be either)
```

#### Error path (5 tests)
```
test_name_only_fts_error_propagates         FTS5 error → propagated, NOT silent fallback
test_name_only_zero_results_falls_back      Empty FTS5 → falls back to dense
test_type_hints_no_match_in_index           Extracted type not in index → still returns results
test_splade_strategy_no_model               SPLADE selected but unavailable → graceful dense fallback
test_explicit_flag_bypasses_routing         --splade passed → router skipped entirely
```

#### Regression (3 tests)
```
test_dense_queries_identical_with_routing   10 queries, routing on vs off → same results
test_identifier_query_no_regression         Query that works via dense also works via NameOnly
test_all_flags_still_work                   --lang, --include-type, --ref with routing → no change
```

### Not Changed

- `src/store/mod.rs` — Store API untouched
- `src/hnsw/` — HNSW untouched
- `src/embedder/` — Embedding unchanged
- Schema — no migration for v1
- All existing search behavior — routing only activates with `--strategy auto` (default), explicit flags bypass

### Estimated Impact

| Change | R@1 impact | Latency impact | Risk |
|--------|-----------|---------------|------|
| NameOnly for identifiers | +1-2pp | -50ms for ~36% of queries | Low (fallback on miss) |
| Type boost for structural | +0-1pp | +0ms (scoring only) | Low (boost not filter) |
| SPLADE routing | TBD (needs eval) | +50-100ms when selected | Medium (model dependent) |
| **v1 total** | **+2-4pp** | **-20ms avg** | **Low** |
| v2 dual embeddings | +10-15pp additional | +2x index time | Medium (schema change) |

## Gaps and Risks

### 1. Heuristic classifier fragility
"handle_connection" looks like identifier but might mean "code that handles connections." Mitigated: NameOnly falls back to dense on zero results. Wrong classification for non-identifiers just uses dense default — no regression possible.

### 2. Type boost could still harm
Even as a boost (not filter), wrong type hints shift result order. Mitigated: only apply at Medium+ confidence. Boost factor is small (1.2x). Worst case: slightly different ordering, not missing results.

### 3. Per-category sample sizes are small
Bootstrap CIs ±9-14pp per category. The strategy table is noisy. We're building routing rules on uncertain data. Mitigated: v1 only makes safe choices (NameOnly for obvious identifiers, dense default for everything uncertain). More queries needed before v2.

### 4. No feedback loop
We classify but never learn if classification was correct. Mitigated: telemetry logs classification + fallback rate. Re-queries as implicit negative signal.

### 5. Interaction with --name-only flag
`--name-only` already exists in CLI. Router's NameOnly reuses the same code path. When `--strategy auto` detects an identifier, it's equivalent to the user passing `--name-only`. Document this.

## Phase 5: Dual Embeddings (v2, ~1.5 days)

### Schema Migration (v17 → v18)

**`src/store/migrations.rs`** — Add `embedding_base BLOB` column to `chunks` table
```sql
ALTER TABLE chunks ADD COLUMN embedding_base BLOB;
```
- `embedding` = current production embedding (with all enrichment including summaries)
- `embedding_base` = same chunk, NL description WITHOUT LLM summary prepended
- Both use the same model (BGE-large) and same dimensionality

**`src/store/mod.rs`** — Bump `CURRENT_SCHEMA_VERSION` to 18

### Index Pipeline Changes

**`src/cli/pipeline/embedding.rs`** — Generate both embeddings per chunk
- After NL generation with summaries: embed → store as `embedding`
- Strip summary prefix, re-embed → store as `embedding_base`
- Only for chunks that have LLM summaries; others get `embedding_base = embedding`

**`src/cli/commands/index/build.rs`** — Wire dual embedding into index pipeline

### HNSW Changes

**`src/hnsw/build.rs`** — Build second HNSW index for base embeddings
- `hnsw.graph` + `hnsw.data` (current, summary-enriched)
- `hnsw_base.graph` + `hnsw_base.data` (base, no summaries)
- Only built if `embedding_base` column is populated

**`src/hnsw/mod.rs`** — `HnswIndex` holds optional `base_index: Option<HnswInner>`

### Router Enhancement

**`src/search/query.rs`** — `search_adaptive` selects which HNSW index to query
- Conceptual, behavioral, negation → query `hnsw_base` (summaries hurt these)
- Structural, multi_step, type_filtered → query `hnsw` (summaries help these)
- Identifier → NameOnly (no HNSW at all)
- Unknown → query both, RRF merge

### v2 Tracing Requirements

- `tracing::info_span!("build_base_hnsw")` — separate span from enriched HNSW build
- `tracing::info!(with_summary = N, without_summary = M, "Dual embedding split")` — how many chunks have summaries
- `tracing::info!(index = "base"|"enriched", "HNSW search selected by router")` — which index was queried
- `tracing::debug!(base_results = N, enriched_results = M, "RRF merge for unknown query")` — merge stats
- `tracing::warn!` when base HNSW is missing/corrupt and falling back to enriched-only
- `tracing::info_span!("embed_base")` on the base embedding pass (separate from enriched)

### v2 Error Handling

- Migration fails midway (column added but no data backfilled) → graceful degradation, `embedding_base = NULL` is valid
- `embedding_base` is NULL for some chunks → skip those in base HNSW, log count with `tracing::info!`
- Base HNSW build fails → `tracing::warn!`, continue with enriched-only (no base routing)
- Disk full during dual HNSW build → clean `anyhow::Context` error, don't corrupt existing index
- Base embedding dimension mismatch after model change → detect in `Store::open`, set `hnsw_dirty`
- Model change (`CQS_EMBEDDING_MODEL`) → invalidate BOTH HNSW indexes (base + enriched)

### v2 Tests — Happy Path (8 tests)

```
test_dual_embedding_index_builds_both       Force reindex → both HNSW files exist
test_base_embedding_differs_from_enriched   Chunks with summaries have different base vs enriched embeddings
test_base_embedding_equals_when_no_summary  Chunks without summaries → base == enriched
test_conceptual_query_uses_base_index       Verify routing to base HNSW for conceptual queries
test_structural_query_uses_enriched_index   Verify routing to enriched HNSW for structural queries
test_unknown_query_merges_both              Both indexes queried, RRF merged, results from both
test_migration_v17_to_v18                   Upgrade adds column, existing embeddings intact
test_graceful_no_base_index                 Base HNSW missing → routing falls back to enriched only
```

### v2 Tests — Adversarial (8 tests)

```
test_migration_idempotent                    Run migration twice → no error, no duplicate column
test_migration_preserves_existing_data       All existing embeddings, calls, types intact after migration
test_base_embedding_null_graceful            Some chunks NULL embedding_base → base HNSW skips them, logs count
test_base_hnsw_missing_graceful              Delete base HNSW files between runs → routing detects, falls back, warns
test_base_hnsw_corrupt_graceful              Corrupt base HNSW checksum → detected on load, rebuild or enriched fallback
test_disk_full_during_dual_build             Base HNSW build error → enriched HNSW still valid, clean error message
test_model_change_invalidates_both           Change CQS_EMBEDDING_MODEL → both hnsw_dirty flags set
test_force_reindex_rebuilds_both             --force → both embedding columns regenerated + both HNSW rebuilt
```

### v2 Tests — Error Path (4 tests)

```
test_embed_base_fails_enriched_survives     Base embedding errors for one chunk → enriched embedding still stored, chunk still searchable
test_base_index_search_error_falls_back     Base HNSW search returns error → falls back to enriched, logs warning
test_rrf_merge_one_empty                    Base returns results, enriched returns empty (or vice versa) → uses non-empty set
test_rrf_merge_both_empty                   Both indexes return empty → returns empty (not error)
```

### Estimated Impact (v1 + v2 combined)

| Change | R@1 impact | Cost |
|--------|-----------|------|
| NameOnly for identifiers | +1-2pp | -50ms for 36% queries |
| Type boost for structural | +0-1pp | +0ms |
| Dual embedding routing | +5-10pp | 2x index storage, 1.5x index time |
| RRF merge for unknown | +1-2pp | 2x search for ~20% queries |
| **Total** | **+7-15pp** | 2x storage, 1.3x avg search |

### Total Test Count (v1 + v2): 56 tests

| Phase | Happy | Adversarial | Error | Regression | Total |
|-------|-------|-------------|-------|------------|-------|
| v1 classifier | 13 | 15 | — | — | 28 |
| v1 pipeline | — | — | 5 | 3 | 8 |
| v2 dual embed | 8 | 8 | 4 | — | 20 |
| **Total** | **21** | **23** | **9** | **3** | **56** |

## Phase 6: Explainable Search (depends on SPLADE-Code)

Once SPLADE-Code sparse vectors are stored per chunk, explain *why* a result matched:

**CLI:** `cqs "query" --explain`
```
fibonacci.py:fibonacci (score: 0.92)
  matched: fibonacci(2.4) recursive(2.1) fib(1.8)
  expanded: memoize(0.9) dynamic(0.7) cache(0.5)
```

**Graph UI:** Click search result → token activation heatmap showing which vocabulary terms drove the match, split into "matched" (present in input) vs "expanded" (learned semantic associations).

**Implementation:** Decode sparse vector token IDs back to words via the SPLADE tokenizer's vocabulary. Split into input-present vs expansion tokens. ~50 lines of code once sparse vectors exist.

**Unique value:** No production code search tool currently exposes token-level match attribution. Dense search gives opaque cosine scores. SPLADE's sparse vectors are inherently interpretable (each non-zero dimension maps to a vocabulary token), but no tool has surfaced this as a user-facing feature. The SPLADE-Code paper (arXiv 2603.22008, Figure 4) demonstrates the interpretability — we'd be the first to ship it as a feature.

## Open Questions

- Should `--strategy` flag be exposed in batch mode too? (Yes)
- Should type boost factor be configurable? (Later — hardcode 1.2x for v1)
- Should base embeddings be optional? (Yes — only generated when LLM summaries exist)
- Incremental reindex: if only some chunks have summaries, do we build both HNSW indexes? (Yes, base index includes all chunks, enriched index includes all chunks — the embeddings just differ for summary-enriched ones)
- Should we expose `--strategy` as a CLI flag for debugging? (Yes — `cqs "query" --strategy dense_with_summaries`)
- Should the classifier be trainable from user corrections? (v2 — start with heuristics)
- Should we implement the "fallback on zero results" pattern from the start? (Yes — cheap insurance)
