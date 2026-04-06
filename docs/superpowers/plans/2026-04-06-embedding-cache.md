# Embedding Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Global embedding cache at `~/.cache/cqs/embeddings.db` that eliminates redundant ONNX inference when switching models or re-indexing unchanged content.

**Architecture:** Separate SQLite DB keyed by `(content_hash, model_fingerprint)`. Transparent cache layer in the embedding pipeline. Model fingerprint = blake3 of ONNX file. LRA eviction (no write-on-read). Batch writes following pipeline boundaries.

**Tech Stack:** Rust, SQLite (sqlx), blake3

**Spec:** `docs/superpowers/specs/2026-04-03-embedding-cache-design.md`

---

## File Structure

**New files:**
- `src/cache.rs` — `EmbeddingCache` struct: open, read_batch, write_batch, evict, stats, clear, prune, verify
- `src/cli/commands/infra/cache.rs` — CLI handlers: cmd_cache_stats, cmd_cache_clear, cmd_cache_prune, cmd_cache_verify

**Modified files:**
- `src/lib.rs` — add `pub mod cache`
- `src/embedder/mod.rs` — compute model fingerprint (blake3 of ONNX file), expose via `Embedder::model_fingerprint()`
- `src/cli/pipeline/embedding.rs` — cache check before embed, cache store after embed
- `src/cli/definitions.rs` — `Cache` subcommand enum (Stats, Clear, Prune, Verify)
- `src/cli/dispatch.rs` — route cache commands
- `src/cli/commands/infra/mod.rs` — add `mod cache`

---

### Task 1: EmbeddingCache struct and SQLite DB

**Files:**
- Create: `src/cache.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write tests first**

```rust
#[cfg(test)]
mod tests {
    // test_open_creates_db — open on nonexistent path creates the DB
    // test_roundtrip — write embeddings, read back, verify identical
    // test_miss — read nonexistent content_hash returns empty
    // test_batch_write — write 100 entries in one call, verify count
    // test_different_fingerprints — same content_hash, different model_fingerprint → separate entries
    // test_dim_mismatch — cached embedding with wrong dim is skipped
    // test_eviction — insert entries, trigger eviction at max_size, oldest entries removed
    // test_clear — clear all, verify empty
    // test_clear_by_model — clear one fingerprint, others survive
    // test_prune_by_age — entries older than N days removed
    // test_stats — verify counts, size, model list
    // test_concurrent_open — two EmbeddingCache instances on same DB don't corrupt (WAL)
    // test_write_failure_doesnt_panic — disk full simulation (best-effort writes)
}
```

- [ ] **Step 2: Implement EmbeddingCache**

Core struct:
```rust
pub struct EmbeddingCache {
    pool: sqlx::SqlitePool,
    rt: tokio::runtime::Runtime,
    max_size_bytes: u64,
}
```

Methods:
- `open(path) -> Result<Self>` — create DB + table if not exists, WAL mode
- `read_batch(content_hashes, model_fingerprint) -> HashMap<String, Vec<f32>>` — batch SELECT, return hits
- `write_batch(entries: &[(content_hash, embedding)], model_fingerprint, dim) -> Result<usize>` — single transaction INSERT OR IGNORE
- `evict() -> Result<usize>` — if DB size > max_size_bytes, delete oldest by created_at until under limit
- `stats() -> Result<CacheStats>` — count, size, unique fingerprints, oldest/newest
- `clear(model_fingerprint: Option<&str>) -> Result<usize>` — delete all or by fingerprint
- `prune_older_than(days: u32) -> Result<usize>` — delete by created_at
- `verify_sample(embedder, n: usize) -> Result<VerifyReport>` — re-encode n random entries, compare

Each public method gets a `tracing::info_span!` at entry. Errors get `tracing::warn!`.

- [ ] **Step 3: Add to lib.rs**

```rust
pub mod cache;
```

- [ ] **Step 4: Run tests**

```bash
cargo test --features gpu-index -- cache::tests
```

- [ ] **Step 5: Commit**

```bash
git add src/cache.rs src/lib.rs
git commit -m "feat: add EmbeddingCache struct with SQLite persistence"
```

---

### Task 2: Model fingerprint

**Files:**
- Modify: `src/embedder/mod.rs`

- [ ] **Step 1: Write test**

Test that `model_fingerprint()` returns a consistent blake3 hash for the same model, and changes when the model file changes.

- [ ] **Step 2: Compute fingerprint at embedder init**

In `Embedder::new()`, after loading the ONNX session, compute blake3 of the model file:

```rust
let fingerprint = {
    let model_bytes = std::fs::read(&onnx_path)?;
    blake3::hash(&model_bytes).to_hex().to_string()
};
```

Store as `self.model_fingerprint: String`. Expose via `pub fn model_fingerprint(&self) -> &str`.

For large files (>2GB), fall back to `format!("{}_{}_{}", model_name, file_size, file_mtime)`.

- [ ] **Step 3: Run test**

```bash
cargo test --features gpu-index -- embedder::tests::test_model_fingerprint
```

- [ ] **Step 4: Commit**

```bash
git add src/embedder/mod.rs
git commit -m "feat: compute model fingerprint (blake3 of ONNX file)"
```

---

### Task 3: Pipeline integration

**Files:**
- Modify: `src/cli/pipeline/embedding.rs`

This is the payoff — cache check before ONNX, cache store after.

- [ ] **Step 1: Open cache at pipeline start**

In the embedding pipeline setup (where `Embedder::new()` is called), also open the cache:

```rust
let cache = EmbeddingCache::open(cache_path).ok(); // best-effort
let fingerprint = embedder.model_fingerprint().to_string();
```

Cache path: `~/.cache/cqs/embeddings.db` (create parent dirs if needed).

- [ ] **Step 2: Cache check before embedding**

For each batch of chunks entering the embedding stage:

```rust
// Separate hits and misses
let content_hashes: Vec<&str> = batch.iter().map(|c| c.content_hash.as_str()).collect();
let cached = cache.read_batch(&content_hashes, &fingerprint)?;

let mut to_embed: Vec<&Chunk> = Vec::new();
let mut results: Vec<(String, Embedding)> = Vec::new();

for chunk in &batch {
    if let Some(embedding) = cached.get(&chunk.content_hash) {
        results.push((chunk.id.clone(), embedding.clone()));
    } else {
        to_embed.push(chunk);
    }
}

// Only embed cache misses
if !to_embed.is_empty() {
    let new_embeddings = embedder.embed_documents(&texts_for_to_embed)?;
    // ... store in results AND write to cache
}
```

- [ ] **Step 3: Cache store after embedding**

After ONNX inference for cache misses, batch-write new embeddings to cache:

```rust
let new_entries: Vec<(String, Vec<f32>)> = new_embeddings
    .iter()
    .map(|(hash, emb)| (hash.clone(), emb.to_vec()))
    .collect();
if let Some(ref cache) = cache {
    if let Err(e) = cache.write_batch(&new_entries, &fingerprint, dim) {
        tracing::warn!(error = %e, "Cache write failed (best-effort)");
    }
}
```

- [ ] **Step 4: Evict after indexing completes**

At the end of `cmd_index`, after all embedding is done:

```rust
if let Some(ref cache) = cache {
    if let Err(e) = cache.evict() {
        tracing::warn!(error = %e, "Cache eviction failed");
    }
}
```

- [ ] **Step 5: Add tracing for cache hit rate**

Log at the end of the embedding stage:
```rust
tracing::info!(
    total = batch.len(),
    cache_hits = cached.len(),
    cache_misses = to_embed.len(),
    "Embedding cache stats for batch"
);
```

- [ ] **Step 6: Tests**

- `test_pipeline_with_cache_miss` — first index with empty cache, all misses
- `test_pipeline_with_cache_hit` — re-index same content, all hits, no ONNX calls
- `test_pipeline_cache_disabled` — cache open fails (bad path), pipeline still works
- `test_pipeline_mixed_hits` — some chunks cached, some new, correct results for both

- [ ] **Step 7: Commit**

```bash
git add src/cli/pipeline/embedding.rs
git commit -m "feat: wire embedding cache into index pipeline"
```

---

### Task 4: CLI commands

**Files:**
- Create: `src/cli/commands/infra/cache.rs`
- Modify: `src/cli/definitions.rs`
- Modify: `src/cli/dispatch.rs`
- Modify: `src/cli/commands/infra/mod.rs`

- [ ] **Step 1: Add Cache subcommand enum**

```rust
#[derive(Subcommand)]
pub enum CacheCmd {
    /// Show cache statistics
    Stats,
    /// Clear cached embeddings
    Clear {
        #[arg(long)]
        model: Option<String>,
    },
    /// Remove entries older than N days
    Prune {
        #[arg(long, value_name = "DAYS")]
        older_than: u32,
    },
    /// Verify cached embeddings against current model
    Verify {
        #[arg(long, default_value = "100")]
        sample: usize,
    },
}
```

- [ ] **Step 2: Implement handlers**

Each handler opens the cache, calls the appropriate method, prints results:

- `cmd_cache_stats` — prints table: total entries, size, unique models, oldest/newest
- `cmd_cache_clear` — deletes all or by model, prints count removed
- `cmd_cache_prune` — deletes by age, prints count removed
- `cmd_cache_verify` — re-encodes sample, reports mismatches

Each with `tracing::info_span!` and proper error handling via `anyhow`.

- [ ] **Step 3: Wire dispatch**

In `dispatch.rs`, route `Commands::Cache(cmd)` to the handlers.

- [ ] **Step 4: Tests**

- Integration test: `cqs cache stats` on empty cache shows zeros
- Integration test: `cqs cache clear` after indexing shows entries removed

- [ ] **Step 5: Commit**

```bash
git add src/cli/commands/infra/cache.rs src/cli/definitions.rs src/cli/dispatch.rs src/cli/commands/infra/mod.rs
git commit -m "feat: add cqs cache stats/clear/prune/verify commands"
```

---

### Task 5: Documentation and cleanup

**Files:**
- Modify: `README.md` — add cache section
- Modify: `CONTRIBUTING.md` — add `src/cache.rs` to architecture
- Modify: `CHANGELOG.md` — entry

- [ ] **Step 1: Update docs**

README: add Cache section with usage examples:
```
cqs cache stats
cqs cache clear
cqs cache prune --older-than 30
cqs cache verify --sample 100
```

CONTRIBUTING: add `cache.rs` to architecture overview.

- [ ] **Step 2: Run full test suite**

```bash
cargo test --features gpu-index 2>&1 | grep "^test result:"
```

- [ ] **Step 3: Commit**

```bash
git add README.md CONTRIBUTING.md
git commit -m "docs: add embedding cache to README and architecture"
```

---

## Notes for implementer

- **Cache is best-effort.** All cache operations that fail should warn and continue. The pipeline must work identically with or without a functioning cache.
- **Tracing convention.** Every public method gets `tracing::info_span!` at entry. Cache hits/misses logged at info level. Errors at warn.
- **Batch boundaries.** Cache reads and writes follow the same batch boundaries as the embedding pipeline (typically 1000 chunks per batch). One transaction per batch, not per chunk.
- **No write-on-read.** `read_batch` does NOT update timestamps. Eviction uses `created_at` (LRA).
- **Model fingerprint.** Computed once per `Embedder::new()`, stored on the struct, passed to all cache operations. blake3 of the ONNX file contents. Fallback for >2GB files: `name_size_mtime`.
- **Global cache.** `~/.cache/cqs/embeddings.db` is shared across all projects. This is intentional — identical code in different repos should share embeddings.
- **Dimension validation.** On cache read, verify the cached embedding's dim matches the current model's dim. Skip mismatches (possible if a model was fine-tuned to a different dim with the same name).
- **Use `--features gpu-index`** for all cargo commands.
- **Run `cargo fmt`** before every commit.
