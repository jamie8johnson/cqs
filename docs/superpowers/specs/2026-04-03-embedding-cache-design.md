# Embedding Cache Design

## Problem

Switching embedding models requires full reindexing (`cqs index --force`) because embeddings are stored in `index.db` with no model association. A 10K chunk project takes 3+ minutes to re-embed. During research evals, switching between BGE-large, v9-200k, and fine-tuned variants means waiting for recomputation of identical chunks.

## Solution

A global embedding cache at `~/.cache/cqs/embeddings.db` that stores embeddings keyed by `(content_hash, model_name)`. The index pipeline checks the cache before running the ONNX model. First index per model is slow; subsequent switches are database lookups.

## Architecture

### Cache database: `~/.cache/cqs/embeddings.db`

SQLite, separate from project index. Single table:

```sql
CREATE TABLE embedding_cache (
    content_hash TEXT NOT NULL,
    model_name TEXT NOT NULL,
    embedding BLOB NOT NULL,
    dim INTEGER NOT NULL,
    last_used INTEGER NOT NULL,  -- unix timestamp for LRU
    PRIMARY KEY (content_hash, model_name)
);

CREATE INDEX idx_cache_lru ON embedding_cache (last_used);
```

### Pipeline integration

In the embedding phase of `cqs index`:

1. For each chunk batch, look up `(content_hash, model_name)` in cache
2. Chunks with cache hits skip ONNX embedding — use cached blob directly
3. Chunks with cache misses go through ONNX as normal
4. After embedding, store new results in cache with current timestamp
5. Update `last_used` on cache hits

Model name comes from `ModelConfig::name()` — already exists and is unique per model configuration.

### Size management

- Default max size: 10GB (`CQS_CACHE_MAX_SIZE` env var to override)
- LRU eviction: when cache exceeds max size, delete oldest entries until under limit
- Eviction runs after batch inserts, not on every write
- Size tracked via `PRAGMA page_count * PRAGMA page_size`

### CLI commands

```bash
cqs cache stats          # Show: total size, entry count, models, oldest/newest
cqs cache clear          # Delete all cached embeddings
cqs cache clear --model bge-large  # Delete embeddings for one model
```

### What stays unchanged

- `index.db` schema — no migration
- `cqs index` behavior without cache — identical (cache is transparent acceleration)
- `--force` flag — still reindexes, but cache makes it fast for previously-seen models
- Embedding dimension handling — cached blobs include dim, validated on read

### Edge cases

- **Cache corruption:** `PRAGMA integrity_check` on open. If corrupt, delete and recreate.
- **Dimension mismatch:** If cached embedding dim doesn't match current model config, skip cache hit (model config changed).
- **Concurrent access:** WAL mode, advisory locking (same as index.db).
- **Disk full:** Cache writes are best-effort. Failure to write cache doesn't fail the index.
- **First run:** No cache exists. Created on first `cqs index`. Normal speed.

### Performance expectation

- Cache hit: ~0.1ms per chunk (SQLite blob read) vs ~5-10ms per chunk (ONNX inference on GPU)
- 10K chunk reindex with warm cache: ~2 seconds vs ~3 minutes
- Cache DB size: ~40 bytes overhead + embedding size per entry. 10K chunks × 1024-dim × 4 bytes = ~40MB per model. 10GB fits ~250 model×project combinations.

## Files to create/modify

- **Create:** `src/cache.rs` — `EmbeddingCache` struct, open/read/write/evict/stats/clear
- **Modify:** `src/cli/pipeline/embedding.rs` — check cache before embed, store after
- **Modify:** `src/cli/definitions.rs` — add `Cache` subcommand with `Stats`/`Clear`
- **Modify:** `src/cli/dispatch.rs` — route cache commands
- **Create:** `src/cli/commands/infra/cache.rs` — `cmd_cache_stats`, `cmd_cache_clear`
