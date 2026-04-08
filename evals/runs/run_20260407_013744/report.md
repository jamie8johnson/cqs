# Eval Run: run_20260407_013744

Date: 2026-04-07 01:38 UTC
Queries: 48

## Summary

| Config | N | R@1 | R@5 | MRR | R@5(acc) |
|--------|---|-----|-----|-----|----------|
| bge-large_no-sparse_minilm-v1 | 32 | 3.1% [0.0, 9.4] | 6.2% [0.0, 15.6] | 0.0531 [0.0083, 0.1260] | 6.2% [0.0, 15.6] |
| bge-large_no-sparse_no-rerank | 32 | 9.4% [0.0, 21.9] | 37.5% [21.9, 53.1] | 0.1883 [0.0972, 0.2981] | 37.5% [21.9, 53.1] |

## Per-Category R@1

| Config | Category | N | R@1 | MRR |
|--------|----------|---|-----|-----|
| bge-large_no-sparse_minilm-v1 | behavioral | 4 | 25.0% [0.0, 75.0] | 0.2500 [0.0000, 0.7500] |
| bge-large_no-sparse_minilm-v1 | conceptual | 4 | 0.0% [0.0, 0.0] | 0.0167 [0.0000, 0.0500] |
| bge-large_no-sparse_minilm-v1 | cross_lang | 4 | 0.0% [0.0, 0.0] | 0.0000 [0.0000, 0.0000] |
| bge-large_no-sparse_minilm-v1 | identifier | 4 | 0.0% [0.0, 0.0] | 0.0000 [0.0000, 0.0000] |
| bge-large_no-sparse_minilm-v1 | multi_step | 4 | 0.0% [0.0, 0.0] | 0.0417 [0.0000, 0.1250] |
| bge-large_no-sparse_minilm-v1 | negation | 4 | 0.0% [0.0, 0.0] | 0.0417 [0.0000, 0.1250] |
| bge-large_no-sparse_minilm-v1 | structural | 4 | 0.0% [0.0, 0.0] | 0.0000 [0.0000, 0.0000] |
| bge-large_no-sparse_minilm-v1 | type_filtered | 4 | 0.0% [0.0, 0.0] | 0.0750 [0.0000, 0.1875] |
| bge-large_no-sparse_no-rerank | behavioral | 4 | 0.0% [0.0, 0.0] | 0.2708 [0.0833, 0.4375] |
| bge-large_no-sparse_no-rerank | conceptual | 4 | 0.0% [0.0, 0.0] | 0.0500 [0.0000, 0.1500] |
| bge-large_no-sparse_no-rerank | cross_lang | 4 | 0.0% [0.0, 0.0] | 0.0000 [0.0000, 0.0000] |
| bge-large_no-sparse_no-rerank | identifier | 4 | 50.0% [0.0, 100.0] | 0.5227 [0.0455, 1.0000] |
| bge-large_no-sparse_no-rerank | multi_step | 4 | 25.0% [0.0, 75.0] | 0.3000 [0.0000, 0.7500] |
| bge-large_no-sparse_no-rerank | negation | 4 | 0.0% [0.0, 0.0] | 0.1333 [0.0000, 0.2667] |
| bge-large_no-sparse_no-rerank | structural | 4 | 0.0% [0.0, 0.0] | 0.1686 [0.0455, 0.2917] |
| bge-large_no-sparse_no-rerank | type_filtered | 4 | 0.0% [0.0, 0.0] | 0.0607 [0.0000, 0.1214] |

## Pairwise Comparisons

**bge-large_no-sparse_no-rerank vs bge-large_no-sparse_minilm-v1**: delta R@1 = 6.2pp [-6.2, 18.8], p = 0.451


## Failure Inventory (best config, top-1 misses)

- **id-001** [identifier] "search_filtered function": expected `search_filtered`, got miss (score gap: 0.0032)
- **id-002** [identifier] "EmbeddingCache struct": expected `EmbeddingCache`, got miss (score gap: 0.0012)
- **id-003** [identifier] "prepare_for_embedding": expected `prepare_for_embedding`, got miss (score gap: 0.0000)
- **id-004** [identifier] "HnswIndex": expected `HnswIndex`, got miss (score gap: 0.0041)
- **beh-002** [behavioral] "find callers of a given function in the call graph": expected `callers_of`, got miss (score gap: 0.0393)
- **beh-003** [behavioral] "parse source code file into chunks": expected `parse_file`, got miss (score gap: 0.0042)
- **beh-004** [behavioral] "save HNSW index to disk with checksums": expected `save`, got miss (score gap: 0.0029)
- **con-002** [conceptual] "search quality measurement and ranking metrics": expected `ScoringConfig`, got miss (score gap: 0.0002)
- **con-003** [conceptual] "GPU acceleration for nearest neighbor search": expected `CagraIndex`, got miss (score gap: 0.0033)
- **con-004** [conceptual] "reciprocal rank fusion for hybrid search": expected `rrf_merge`, got miss (score gap: 0.0001)
- **tf-001** [type_filtered] "tests for the parser module": expected `test_parse_rust_basic`, got miss (score gap: 0.0015)
- **tf-004** [type_filtered] "integration tests for search functionality": expected `test_search_filtered_empty_store`, got miss (score gap: 0.0000)
- **st-001** [structural] "implementations of VectorIndex trait": expected `HnswIndex`, got miss (score gap: 0.0000)
- **st-002** [structural] "functions that take &Store as a parameter": expected `search_filtered`, got miss (score gap: 0.0020)
- **st-003** [structural] "trait definition for search index": expected `VectorIndex`, got miss (score gap: 0.0003)
- **st-004** [structural] "Display impl for chunk types": expected `ChunkType`, got miss (score gap: 0.0066)
- **neg-001** [negation] "parse function that is NOT for markdown": expected `parse_file`, got miss (score gap: 0.0253)
- **neg-002** [negation] "search that does NOT use keyword matching": expected `search_filtered`, got miss (score gap: 0.0003)
- **neg-003** [negation] "index build that is NOT GPU accelerated": expected `build_batched_with_dim`, got miss (score gap: 0.0021)
- **ms-002** [multi_step] "what happens when a user runs a semantic search query": expected `cmd_query`, got miss (score gap: 0.0003)
