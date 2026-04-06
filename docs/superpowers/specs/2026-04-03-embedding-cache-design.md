# Embedding Cache Design

## Problem

Switching embedding models requires full reindexing (`cqs index --force`) because embeddings are stored in `index.db` with no model association. A 10K chunk project takes 3+ minutes to re-embed. During research evals, switching between BGE-large, v9-200k, and fine-tuned variants means waiting for recomputation of identical chunks.

## Solution

A global embedding cache at `~/.cache/cqs/embeddings.db` that stores embeddings keyed by `(content_hash, model_fingerprint)`. The index pipeline checks the cache before running the ONNX model. First index per model is slow; subsequent switches are database lookups.

The cache is **global** — shared across all projects, all users on the machine, all model variants. Two different projects with an identical function share the cached embedding. This is correct: same content + same model = same embedding.

## Architecture

### Cache database: `~/.cache/cqs/embeddings.db`

SQLite, WAL mode, separate from project index. Single table:

```sql
CREATE TABLE embedding_cache (
    content_hash TEXT NOT NULL,
    model_fingerprint TEXT NOT NULL,   -- hash of ONNX model file (not just name)
    embedding BLOB NOT NULL,
    dim INTEGER NOT NULL,
    created_at INTEGER NOT NULL,       -- unix timestamp
    PRIMARY KEY (content_hash, model_fingerprint)
);

CREATE INDEX idx_cache_created ON embedding_cache (created_at);
```

### Model fingerprint

`model_fingerprint` is a blake3 hash of the ONNX model file, computed once per cqs invocation. This catches:

- Fine-tuned models with the same name but different weights
- Model file upgrades (new HF revision, different ONNX export)
- Tokenizer changes (same model name, different vocab)

Computed at embedder initialization: `blake3::hash(fs::read(onnx_path)?)`. Cached in `Embedder` struct — one hash per session, not per chunk.

Fallback if model file is too large to hash (>2GB): `model_name + file_size + file_mtime` as the fingerprint.

### Pipeline integration

In the embedding phase of `cqs index`:

1. For each chunk batch, look up `(content_hash, model_fingerprint)` in cache
2. Chunks with cache hits skip ONNX embedding — use cached blob directly
3. Chunks with cache misses go through ONNX as normal
4. After embedding, batch-insert new results in cache (single transaction per pipeline batch)

**Write batching:** Cache writes follow the same batch boundaries as the embedding pipeline. One `INSERT OR IGNORE` transaction per batch, not per chunk. 10K chunks in ~10 batches of 1000.

**No timestamp updates on read.** Cache hits don't write `last_used`. Eviction uses `created_at` (least recently added), not LRU. This eliminates write amplification — 10K cache hits = 0 writes instead of 10K UPDATE statements. LRA is good enough for this use case: old embeddings from abandoned models expire naturally.

### Size management

- Default max size: 10GB (`CQS_CACHE_MAX_SIZE` env var to override)
- LRA eviction: when cache exceeds max size, delete oldest entries by `created_at` until under limit
- Eviction runs after batch inserts, not on every write
- Size tracked via `PRAGMA page_count * PRAGMA page_size`

### Integrity

**No integrity check on open.** `PRAGMA integrity_check` scans the entire database — seconds on a 10GB cache. SQLite's WAL mode handles interrupted-write corruption automatically. Bit rot is too rare to justify scan-on-open cost.

Integrity check available via `cqs cache verify` (explicit, not automatic).

### CLI commands

```bash
cqs cache stats                    # total size, entry count, models, oldest/newest
cqs cache clear                    # delete all cached embeddings
cqs cache clear --model bge-large  # delete embeddings for one model fingerprint
cqs cache prune --older-than 30d   # explicit pruning by age
cqs cache verify                   # re-encode a sample of entries with current model, compare
```

`verify` catches silent corruption: if a cache entry was stored by a different model with the same fingerprint (shouldn't happen with blake3, but belt-and-suspenders), the re-encoding will produce a different embedding. Report mismatches without auto-deleting.

### What stays unchanged

- `index.db` schema — no migration
- `cqs index` behavior without cache — identical (cache is transparent acceleration)
- `--force` flag — still reindexes, but cache makes it fast for previously-seen models
- Embedding dimension handling — cached blobs include dim, validated on read

### Edge cases

- **Dimension mismatch:** If cached embedding dim doesn't match current model config, skip cache hit. Model config may have changed dimension independently of the model file.
- **Concurrent access:** WAL mode, advisory locking (same as index.db).
- **Disk full:** Cache writes are best-effort. Failure to write cache doesn't fail the index.
- **First run:** No cache exists. Created on first `cqs index`. Normal speed.
- **Tokenizer mismatch:** Covered by model fingerprint — different tokenizer = different ONNX file = different fingerprint.

### Performance expectation

- Cache hit: ~0.1ms per chunk (SQLite blob read) vs ~5-10ms per chunk (ONNX inference on GPU)
- 10K chunk reindex with warm cache: ~2 seconds vs ~3 minutes
- Cache DB size per entry: ~4200 bytes (4096 embedding + ~100 SQLite overhead + index entries). 10K chunks × 1024-dim = ~42MB per model. 10GB fits ~200 model×project combinations.

## Files to create/modify

- **Create:** `src/cache.rs` — `EmbeddingCache` struct, open/read_batch/write_batch/evict/stats/clear/verify
- **Modify:** `src/cli/pipeline/embedding.rs` — check cache before embed, store after
- **Modify:** `src/embedder/mod.rs` — compute and store model fingerprint
- **Modify:** `src/cli/definitions.rs` — add `Cache` subcommand with `Stats`/`Clear`/`Prune`/`Verify`
- **Modify:** `src/cli/dispatch.rs` — route cache commands
- **Create:** `src/cli/commands/infra/cache.rs` — `cmd_cache_stats`, `cmd_cache_clear`, `cmd_cache_prune`, `cmd_cache_verify`
