# SPLADE Sparse-Dense Hybrid Search Design

## Problem

Cosine-only search (91.2% fixture R@1) is excellent for semantic queries but weak for exact-name and mixed-intent queries. Our RRF attempt (FTS5 + cosine fusion) degraded quality by 17pp because FTS5 is raw token matching with no learned importance weights. We need a better sparse signal.

## Solution

Add SPLADE (Sparse Lexical and Expansion Model) as a learned sparse retrieval path parallel to the existing dense cosine path. Linear interpolation fuses both scores with a tunable weight α. Off-the-shelf model first, fine-tune if signal.

## Architecture

```
Query → [Dense: embed → HNSW cosine] → scored candidates
      → [Sparse: SPLADE encode → inverted index dot product] → scored candidates
      → Linear interpolation (α * dense + (1-α) * sparse) → final ranking
      → SQL scoring (demotion, path filter) → results
```

**Index time:** Each chunk gets both a dense embedding (existing) and a sparse SPLADE vector (new). Dense goes to HNSW. Sparse goes to SQLite `sparse_vectors` table, loaded into an in-memory inverted index at startup.

**Query time:** Query gets both dense and sparse encodings. Dense searches HNSW (with traversal-time filtering for chunk_type/language). Sparse searches the inverted index (with the same filter applied during scoring). Fusion produces the final candidate list that feeds into the existing SQL scoring pipeline.

**Toggling:** `SearchFilter.enable_splade: bool` controls whether the sparse path runs. FTS5 RRF (`enable_rrf`) remains independently toggleable. Default: both off (cosine-only). Eval configs test all combinations.

## Components

### 1. SPLADE Encoder (`src/splade/`)

ONNX model loaded alongside the embedding model. Shares ORT runtime.

**Model:** `naver/splade-cocondenser-ensembledistil` (110M params). Download via hf-hub, same pattern as embedding model download. ONNX export via `optimum` or use pre-exported ONNX if available.

**Input:** NL description text (same text used for dense embedding).

**Output:** Sparse vector `Vec<(u32, f32)>` — vocabulary token ID → learned importance weight. Typically 100-300 non-zero entries out of ~30K vocabulary.

**Encoding:** Pass text through model, apply ReLU + log(1+x) activation on logits, threshold to keep only non-zero weights. Standard SPLADE inference.

**Query vs document encoding:** SPLADE uses the same model for both but may apply different thresholds. Start with symmetric (same threshold for query and document).

**Module structure:**
```
src/splade/
  mod.rs      — SpladeEncoder struct, encode_document(), encode_query()
  model.rs    — Model config, download, ONNX session management
```

### 2. Sparse Index (`src/splade/index.rs`)

In-memory inverted index loaded from SQLite on store open.

**Structure:**
```rust
pub struct SpladeIndex {
    // token_id → [(chunk_id_index, weight)]
    // chunk_id_index maps to id_map (same as HNSW)
    postings: HashMap<u32, Vec<(usize, f32)>>,
    // Sequential chunk ID map (parallel to HNSW id_map)
    id_map: Vec<String>,
}
```

**Query:** For each non-zero token in the query sparse vector, look up the posting list. Accumulate dot product scores per chunk. Return top-k by score.

**Filtering:** Same `chunk_type_language_map` predicate used by HNSW traversal filtering. Skip chunks that don't match during score accumulation.

**Memory:** ~10-50MB for 10K chunks (300 tokens/chunk × 10K chunks × 8 bytes per entry). Acceptable.

### 3. Storage (Schema v17)

New SQLite table:
```sql
CREATE TABLE sparse_vectors (
    chunk_id TEXT NOT NULL,
    token_id INTEGER NOT NULL,
    weight REAL NOT NULL,
    PRIMARY KEY (chunk_id, token_id)
);
CREATE INDEX idx_sparse_token ON sparse_vectors(token_id);
```

**Bundled with RT-DATA-2** — add enrichment idempotency marker in the same migration:
```sql
ALTER TABLE chunks ADD COLUMN enrichment_version INTEGER NOT NULL DEFAULT 0;
```

`enrichment_version` increments each time the enrichment pass processes a chunk. Enrichment pass skips chunks where `enrichment_version >= current_version`. Prevents double-application of call graph context.

### 4. Fusion (`src/search/query.rs`)

In `search_filtered_with_index`, when `enable_splade` is true:

```rust
// Run both paths
let dense_results = idx.search_with_filter(query, candidate_count, &predicate);
let sparse_results = splade_index.search_with_filter(sparse_query, candidate_count, &predicate);

// Merge: union of candidates, interpolate scores
let alpha = scoring_config.splade_alpha; // default 0.7 (cosine-heavy)
let fused = merge_scored_results(&dense_results, &sparse_results, alpha);
```

**Score normalization:** Cosine scores are already [0, 1]. SPLADE dot product scores are unbounded. Normalize SPLADE scores to [0, 1] via min-max within the result set before interpolation.

**Missing scores:** A candidate found by dense but not sparse gets sparse_score = 0 (and vice versa). This naturally handles the case where SPLADE has no opinion on a chunk.

### 5. Config

New fields on `ScoringConfig` (runtime-configurable via `.cqs.toml`):

```toml
[scoring]
splade_alpha = 0.7        # weight for dense score (1-alpha for sparse)
splade_threshold = 0.01   # minimum weight to keep in sparse vector
```

New field on `SearchFilter`:
```rust
pub enable_splade: bool,  // default: false
```

## Pipeline Integration

### Index time

The indexing pipeline adds a SPLADE encoding step:

```
parse → embed (dense) → encode (SPLADE) → write (both to SQLite)
                                         → enrichment pass
                                         → build HNSW
                                         → load sparse index
```

SPLADE encoding runs on the NL description text — same input as dense embedding. Sequential with dense embedding (simpler), could be parallelized later.

**Batch encoding:** Like the dense embedder, SPLADE encoder processes chunks in batches for GPU efficiency.

### Query time

`search_filtered_with_index` checks `filter.enable_splade`:

1. **False** (default): existing cosine-only path, unchanged
2. **True**: run both paths, fuse, feed into SQL scoring

The sparse index is loaded alongside HNSW in `CommandContext` / batch `BatchContext`.

### HNSW traversal filtering compatibility

Dense path uses HNSW traversal-time filtering (chunk_type, language). Sparse path applies the same predicate during score accumulation — skip postings for non-matching chunks. Both paths respect the same filter.

## Model Management

**Download:** On first use of `--splade` or `enable_splade`, download the ONNX model via hf-hub. Same pattern as `cqs init` for the embedding model. Cache in `~/.cache/huggingface/`.

**Config:** `CQS_SPLADE_MODEL` env var or `[splade] model = "..."` in `.cqs.toml`. Default: `naver/splade-cocondenser-ensembledistil`.

**ONNX export:** If the model isn't available as pre-exported ONNX, use `cqs export-model` with a SPLADE-compatible export path. Or ship a pre-exported ONNX.

**Lazy loading:** SPLADE encoder loads only when `enable_splade` is true. No overhead for users who don't use it. `OnceLock` in `CommandContext`, same pattern as the reranker.

## Eval Configs

| Config | Dense | FTS5 | SPLADE | α | What it tests |
|--------|-------|------|--------|---|---------------|
| A | cosine | — | — | — | Current best (91.2%) |
| B | cosine | RRF | — | — | Current RRF (74.0%) |
| G | cosine | — | SPLADE | 0.7 | SPLADE cosine-heavy |
| H | cosine | — | SPLADE | 0.5 | Equal weight |
| I | cosine | — | SPLADE | 0.3 | SPLADE-heavy |
| J | cosine | RRF | SPLADE | 0.5 | All three signals |

**Success criteria:**
- Any of G/H/I beats Config A (91.2%) on 296q fixture eval → SPLADE adds value
- If none beat A → off-the-shelf SPLADE doesn't help. Fine-tune on 200K pairs, re-eval.
- If fine-tuned SPLADE also doesn't beat A → architecture doesn't help for code search. Stop.

## What This Does NOT Change

- `resolve_target` / name-based lookup — uses FTS5 `search_by_name`, not the search pipeline
- Scoring pipeline (demotion, path filter, note boost) — runs after fusion
- CAGRA — SPLADE is CPU-only, independent
- `search()` (no filter, no SPLADE) — unchanged
- Existing embedder — SPLADE is a separate model, not a replacement

## Dependencies

- `ort` (existing) — ONNX runtime for SPLADE inference
- `hf-hub` (existing) — model download
- `tokenizers` (existing) — tokenization for SPLADE input
- No new crate dependencies

## Effort Estimate

1. SPLADE encoder module + ONNX inference (~1 day)
2. Sparse index (in-memory inverted index + SQLite persistence) (~0.5 day)
3. Schema v17 migration (sparse_vectors + enrichment_version) (~0.5 day)
4. Pipeline integration (index time + query time) (~1 day)
5. Fusion scoring + SearchFilter wiring (~0.5 day)
6. Eval configs + sweep (~0.5 day)
7. Testing + verification (~0.5 day)

Total: ~4-5 days. First eval results after day 3 (encoder + index + basic fusion).
