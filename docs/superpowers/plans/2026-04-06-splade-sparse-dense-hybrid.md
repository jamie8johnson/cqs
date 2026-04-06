# SPLADE Sparse-Dense Hybrid Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add SPLADE learned sparse retrieval as a configurable search signal alongside existing dense cosine search. Linear interpolation fusion with tunable α. Off-the-shelf model, eval-driven validation.

**Architecture:** SPLADE encoder (ONNX) produces sparse vectors at index time. In-memory inverted index serves queries. Fusion in `search_filtered_with_index` interpolates dense + sparse scores. Schema v17 adds `sparse_vectors` table + RT-DATA-2 `enrichment_version`.

**Tech Stack:** Rust, ORT (ONNX Runtime), SQLite, hf-hub

**Spec:** `docs/superpowers/specs/2026-04-06-splade-sparse-dense-hybrid-design.md`

---

## File Structure

**New files:**
- `src/splade/mod.rs` — SpladeEncoder, encode_document(), encode_query(), model download
- `src/splade/index.rs` — SpladeIndex (in-memory inverted index), search, persist/load
- `src/splade/tests.rs` — unit tests for encoder and index

**Modified files:**
- `src/lib.rs` — add `pub mod splade`
- `src/store/migrations.rs` — schema v17 (sparse_vectors + enrichment_version)
- `src/store/helpers/mod.rs` — bump CURRENT_SCHEMA_VERSION to 17
- `src/store/mod.rs` — sparse vector CRUD methods
- `src/search/query.rs` — fusion logic in search_filtered_with_index
- `src/store/helpers/search_filter.rs` — add `enable_splade: bool`
- `src/search/scoring/config.rs` — add `splade_alpha: f32`, `splade_threshold: f32`
- `src/cli/pipeline/mod.rs` — SPLADE encoding in index pipeline
- `src/cli/pipeline/embedding.rs` — parallel SPLADE encoding stage
- `src/cli/store.rs` — load SpladeIndex in CommandContext
- `src/cli/batch/mod.rs` — load SpladeIndex in BatchContext
- `src/cli/definitions.rs` — `--splade` flag
- `src/cli/commands/search/query.rs` — wire enable_splade from CLI flag
- `src/cli/batch/handlers/search.rs` — wire enable_splade in batch
- `src/config.rs` — `[splade]` config section
- `tests/pipeline_eval.rs` — new eval configs G/H/I/J
- `Cargo.toml` — `splade` feature flag (optional, default off initially)

---

### Task 1: Schema v17 migration

**Files:**
- Modify: `src/store/migrations.rs`
- Modify: `src/store/helpers/mod.rs`

- [ ] **Step 1: Write failing test for schema v17**

Add a test that opens a v16 database and verifies migration to v17 creates `sparse_vectors` table and `enrichment_version` column.

- [ ] **Step 2: Implement migration**

In `src/store/migrations.rs`, add `migrate_v16_to_v17`:

```rust
async fn migrate_v16_to_v17(conn: &mut SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v16_to_v17").entered();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sparse_vectors (
            chunk_id TEXT NOT NULL,
            token_id INTEGER NOT NULL,
            weight REAL NOT NULL,
            PRIMARY KEY (chunk_id, token_id)
        )"
    ).execute(&mut *conn).await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_sparse_token ON sparse_vectors(token_id)"
    ).execute(&mut *conn).await?;

    // RT-DATA-2: enrichment idempotency marker
    sqlx::query(
        "ALTER TABLE chunks ADD COLUMN enrichment_version INTEGER NOT NULL DEFAULT 0"
    ).execute(&mut *conn).await?;

    tracing::info!("Migrated to v17: sparse_vectors table + enrichment_version column");
    Ok(())
}
```

Bump `CURRENT_SCHEMA_VERSION` to 17 in `src/store/helpers/mod.rs`.

- [ ] **Step 3: Run migration test**

```bash
cargo test --features gpu-index -- migrate_v16 2>&1 | grep "test result"
```

- [ ] **Step 4: Commit**

```bash
git add src/store/migrations.rs src/store/helpers/mod.rs
git commit -m "feat: schema v17 — sparse_vectors table + enrichment_version (RT-DATA-2)"
```

---

### Task 2: SPLADE encoder module

**Files:**
- Create: `src/splade/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Create module skeleton with tracing**

Create `src/splade/mod.rs`:

```rust
//! SPLADE sparse encoder for learned sparse retrieval.
//!
//! Produces sparse vectors (token_id → weight) from text input.
//! Used alongside the dense embedder for hybrid search.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use ort::session::Session;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SpladeError {
    #[error("Model not found: {0}")]
    ModelNotFound(String),
    #[error("ONNX inference failed: {0}")]
    InferenceFailed(String),
    #[error("Tokenization failed: {0}")]
    TokenizationFailed(String),
}

/// A sparse vector: token vocabulary ID → learned importance weight.
pub type SparseVector = Vec<(u32, f32)>;

/// SPLADE encoder using ONNX Runtime.
pub struct SpladeEncoder {
    session: Mutex<Session>,
    tokenizer: tokenizers::Tokenizer,
    threshold: f32,
}
```

- [ ] **Step 2: Implement model loading with error handling**

```rust
impl SpladeEncoder {
    pub fn new(model_dir: &std::path::Path, threshold: f32) -> Result<Self, SpladeError> {
        let _span = tracing::info_span!("splade_encoder_new", dir = %model_dir.display()).entered();

        let onnx_path = model_dir.join("model.onnx");
        if !onnx_path.exists() {
            return Err(SpladeError::ModelNotFound(
                format!("No model.onnx at {}", model_dir.display())
            ));
        }

        let tokenizer_path = model_dir.join("tokenizer.json");
        if !tokenizer_path.exists() {
            return Err(SpladeError::ModelNotFound(
                format!("No tokenizer.json at {}", model_dir.display())
            ));
        }

        let session = crate::embedder::provider::create_session(&onnx_path)
            .map_err(|e| SpladeError::InferenceFailed(e.to_string()))?;

        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| SpladeError::TokenizationFailed(e.to_string()))?;

        tracing::info!(
            threshold,
            "SPLADE encoder loaded"
        );

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            threshold,
        })
    }
}
```

- [ ] **Step 3: Implement encode method**

```rust
impl SpladeEncoder {
    /// Encode text into a sparse vector.
    /// Applies ReLU + log(1+x) activation, thresholds to keep significant weights.
    pub fn encode(&self, text: &str) -> Result<SparseVector, SpladeError> {
        let _span = tracing::debug_span!("splade_encode", text_len = text.len()).entered();

        let encoding = self.tokenizer
            .encode(text, true)
            .map_err(|e| SpladeError::TokenizationFailed(e.to_string()))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding.get_attention_mask().iter().map(|&m| m as i64).collect();
        let seq_len = input_ids.len();

        let session = self.session.lock().unwrap();
        // Run ONNX inference — output shape [1, seq_len, vocab_size]
        // Apply max pooling over sequence length, then ReLU + log(1+x)
        // ... (implementation details depend on model output format)

        // Threshold: keep only entries above self.threshold
        let sparse: SparseVector = activations.iter()
            .enumerate()
            .filter(|(_, &w)| w > self.threshold)
            .map(|(id, &w)| (id as u32, w))
            .collect();

        tracing::debug!(non_zero = sparse.len(), "SPLADE encoding complete");
        Ok(sparse)
    }

    /// Batch encode multiple texts.
    pub fn encode_batch(&self, texts: &[&str]) -> Result<Vec<SparseVector>, SpladeError> {
        let _span = tracing::debug_span!("splade_encode_batch", count = texts.len()).entered();
        texts.iter().map(|t| self.encode(t)).collect()
    }
}
```

- [ ] **Step 4: Add to lib.rs**

```rust
pub mod splade;
```

Gate behind feature flag in Cargo.toml if desired, or always compiled (the model is lazy-loaded).

- [ ] **Step 5: Write unit tests**

Tests in `src/splade/mod.rs`:
- `test_sparse_vector_not_empty` — encode a known string, verify non-empty output
- `test_sparse_vector_threshold` — verify all weights exceed threshold
- `test_encode_empty_string` — graceful handling
- `test_encode_batch_consistency` — single encode matches batch encode for same input

Skip if model not downloaded (use `#[ignore]` like embedding tests).

- [ ] **Step 6: Commit**

```bash
git add src/splade/ src/lib.rs
git commit -m "feat: add SPLADE encoder module (ONNX, lazy-loaded)"
```

---

### Task 3: Sparse inverted index

**Files:**
- Create: `src/splade/index.rs`
- Modify: `src/store/mod.rs` — CRUD for sparse_vectors

- [ ] **Step 1: Implement SpladeIndex struct**

Create `src/splade/index.rs`:

```rust
//! In-memory inverted index for SPLADE sparse vectors.

use std::collections::HashMap;
use crate::index::IndexResult;
use super::SparseVector;

/// In-memory inverted index loaded from SQLite.
pub struct SpladeIndex {
    /// token_id → [(chunk_index, weight)]
    postings: HashMap<u32, Vec<(usize, f32)>>,
    /// Sequential chunk ID map (parallel to HNSW id_map)
    id_map: Vec<String>,
}
```

- [ ] **Step 2: Implement search with filtering and tracing**

```rust
impl SpladeIndex {
    /// Search the inverted index with a sparse query vector.
    /// Returns top-k results by dot product score.
    pub fn search(&self, query: &SparseVector, k: usize) -> Vec<IndexResult> {
        self.search_with_filter(query, k, &|_: &str| true)
    }

    /// Search with a chunk_id predicate filter.
    pub fn search_with_filter(
        &self,
        query: &SparseVector,
        k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<IndexResult> {
        let _span = tracing::debug_span!(
            "splade_index_search", k, query_terms = query.len()
        ).entered();

        // Accumulate dot product scores per chunk
        let mut scores: HashMap<usize, f32> = HashMap::new();
        for &(token_id, query_weight) in query {
            if let Some(postings) = self.postings.get(&token_id) {
                for &(chunk_idx, doc_weight) in postings {
                    // Apply filter
                    if let Some(chunk_id) = self.id_map.get(chunk_idx) {
                        if !filter(chunk_id) {
                            continue;
                        }
                    }
                    *scores.entry(chunk_idx).or_insert(0.0) += query_weight * doc_weight;
                }
            }
        }

        // Sort by score descending, take top-k
        let mut results: Vec<_> = scores.into_iter()
            .filter_map(|(idx, score)| {
                self.id_map.get(idx).map(|id| IndexResult {
                    id: id.clone(),
                    score,
                })
            })
            .collect();
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);

        tracing::debug!(results = results.len(), "SPLADE search complete");
        results
    }

    pub fn len(&self) -> usize { self.id_map.len() }
    pub fn is_empty(&self) -> bool { self.id_map.is_empty() }
}
```

- [ ] **Step 3: Implement build and persistence**

```rust
impl SpladeIndex {
    /// Build from a list of (chunk_id, sparse_vector) pairs.
    pub fn build(chunks: Vec<(String, SparseVector)>) -> Self {
        let _span = tracing::info_span!("splade_index_build", chunks = chunks.len()).entered();
        let mut postings: HashMap<u32, Vec<(usize, f32)>> = HashMap::new();
        let mut id_map = Vec::with_capacity(chunks.len());

        for (idx, (chunk_id, sparse)) in chunks.into_iter().enumerate() {
            for &(token_id, weight) in &sparse {
                postings.entry(token_id).or_default().push((idx, weight));
            }
            id_map.push(chunk_id);
        }

        tracing::info!(
            tokens = postings.len(),
            chunks = id_map.len(),
            "SPLADE index built"
        );
        Self { postings, id_map }
    }
}
```

- [ ] **Step 4: Add Store methods for sparse_vectors CRUD**

In `src/store/mod.rs` or a new `src/store/sparse.rs`:

```rust
/// Upsert sparse vectors for a batch of chunks.
pub fn upsert_sparse_vectors(&self, vectors: &[(String, Vec<(u32, f32)>)]) -> Result<(), StoreError>

/// Load all sparse vectors for building the in-memory index.
pub fn load_all_sparse_vectors(&self) -> Result<Vec<(String, Vec<(u32, f32)>)>, StoreError>

/// Delete sparse vectors for chunks that no longer exist.
pub fn prune_orphan_sparse_vectors(&self) -> Result<usize, StoreError>
```

Each method should have a `tracing::info_span!` or `debug_span!` at entry.

- [ ] **Step 5: Write tests**

- `test_build_empty` — empty input produces empty index
- `test_build_and_search` — insert known vectors, search, verify top result
- `test_search_filter` — filter excludes matching chunks
- `test_search_no_match` — query terms not in index returns empty
- `test_dot_product_correct` — manual dot product verification
- `test_store_roundtrip` — upsert then load, verify identical vectors

- [ ] **Step 6: Commit**

```bash
git add src/splade/index.rs src/store/
git commit -m "feat: add SPLADE inverted index + Store persistence"
```

---

### Task 4: Fusion scoring

**Files:**
- Modify: `src/search/scoring/config.rs` — add splade_alpha
- Modify: `src/store/helpers/search_filter.rs` — add enable_splade
- Modify: `src/search/query.rs` — fusion logic
- Modify: `src/config.rs` — [splade] config section

- [ ] **Step 1: Add config fields**

In `ScoringConfig`:
```rust
pub splade_alpha: f32,      // default 0.7 (cosine-heavy)
```

In `SearchFilter`:
```rust
pub enable_splade: bool,    // default false
```

In `Config` (.cqs.toml):
```toml
[splade]
alpha = 0.7
threshold = 0.01
model = "naver/splade-cocondenser-ensembledistil"
```

- [ ] **Step 2: Implement fusion in search_filtered_with_index**

In `src/search/query.rs`, extend the `has_type_or_lang_filter` block to also handle SPLADE:

```rust
if filter.enable_splade {
    if let Some(splade_idx) = splade_index {
        let sparse_query = /* encode query with SPLADE */;
        let sparse_results = splade_idx.search_with_filter(&sparse_query, candidate_count, &predicate);

        // Normalize SPLADE scores to [0, 1]
        let max_sparse = sparse_results.iter().map(|r| r.score).fold(0.0f32, f32::max);
        let sparse_normalized: HashMap<String, f32> = sparse_results.iter()
            .map(|r| (r.id.clone(), if max_sparse > 0.0 { r.score / max_sparse } else { 0.0 }))
            .collect();

        // Fuse with dense results
        let alpha = scoring_config.splade_alpha;
        // ... merge and interpolate
    }
}
```

The function signature for `search_filtered_with_index` gains an optional `splade_index` parameter (or reads it from the Store/Context).

- [ ] **Step 3: Write fusion tests**

- `test_fusion_splade_disabled` — enable_splade=false returns same as cosine-only
- `test_fusion_alpha_1` — α=1.0 returns pure cosine ranking
- `test_fusion_alpha_0` — α=0.0 returns pure sparse ranking
- `test_fusion_score_interpolation` — manual verification of fused scores
- `test_fusion_missing_scores` — candidate in dense but not sparse gets sparse=0

- [ ] **Step 4: Commit**

```bash
git add src/search/ src/store/helpers/ src/config.rs
git commit -m "feat: sparse-dense fusion scoring with configurable alpha"
```

---

### Task 5: Pipeline integration (index time)

**Files:**
- Modify: `src/cli/pipeline/mod.rs` — add SPLADE encoding stage
- Modify: `src/cli/commands/index/build.rs` — build SpladeIndex after indexing
- Modify: `src/cli/store.rs` — load SpladeIndex in CommandContext
- Modify: `src/cli/batch/mod.rs` — load SpladeIndex in BatchContext

- [ ] **Step 1: Add SPLADE encoding to pipeline**

After the dense embedding stage, add SPLADE encoding:

```rust
// In the pipeline, after embed_documents:
if let Some(splade) = &splade_encoder {
    let sparse = splade.encode(&nl_text)
        .map_err(|e| {
            tracing::warn!(error = %e, chunk = %chunk.name, "SPLADE encoding failed, skipping");
            e
        })
        .ok();
    // Store sparse vector alongside dense embedding
}
```

SPLADE failures should warn and skip (don't abort indexing for a supplementary signal).

- [ ] **Step 2: Build SpladeIndex after chunk insertion**

In `cmd_index`, after HNSW build:

```rust
if splade_enabled {
    let _span = tracing::info_span!("build_splade_index").entered();
    let vectors = store.load_all_sparse_vectors()?;
    let splade_index = SpladeIndex::build(vectors);
    tracing::info!(chunks = splade_index.len(), "SPLADE index built");
}
```

- [ ] **Step 3: Load SpladeIndex in CommandContext and BatchContext**

Same `OnceLock` lazy loading pattern as HNSW and reranker. Load from SQLite on first access.

- [ ] **Step 4: Wire CLI flag**

In `src/cli/definitions.rs`:
```rust
#[arg(long, help = "Enable SPLADE sparse-dense hybrid search")]
pub splade: bool,
```

Wire through to `SearchFilter.enable_splade` in `src/cli/commands/search/query.rs`.

In batch mode, add `--splade` flag to batch search command parsing.

- [ ] **Step 5: Commit**

```bash
git add src/cli/ src/splade/
git commit -m "feat: wire SPLADE into index pipeline and search commands"
```

---

### Task 6: Eval configs

**Files:**
- Modify: `tests/pipeline_eval.rs`

- [ ] **Step 1: Add SPLADE eval configs**

Add configs G through J to `test_fixture_eval_296q`. Each config builds the SpladeIndex from fixtures and runs the eval with different alpha values.

```rust
// Config G: Cosine + SPLADE (α=0.7)
let filter = SearchFilter {
    languages: Some(vec![case.language]),
    enable_splade: true,
    ..Default::default()
};
```

The eval needs to:
1. Build a SpladeIndex from the fixture chunks (SPLADE-encode each fixture's NL text)
2. Pass the SpladeIndex to `search_filtered_with_index`
3. Report results alongside existing configs

- [ ] **Step 2: Add alpha sweep**

Test α values: 0.3, 0.5, 0.7. Report each as a separate row in the eval output.

- [ ] **Step 3: Commit**

```bash
git add tests/pipeline_eval.rs
git commit -m "test: add SPLADE eval configs G/H/I/J to fixture eval"
```

---

### Task 7: Model download and config

**Files:**
- Modify: `Cargo.toml` — optional `splade` feature
- Modify: `src/cli/commands/infra/init.rs` — download SPLADE model on init
- Modify: `src/cli/commands/infra/doctor.rs` — report SPLADE model status

- [ ] **Step 1: Add feature flag**

In `Cargo.toml`:
```toml
splade = []  # No extra deps — uses existing ort + tokenizers
```

Gate SPLADE code behind `#[cfg(feature = "splade")]` so it's opt-in during development. Add to `default` features once proven.

- [ ] **Step 2: Model download in init**

Extend `cqs init` to optionally download the SPLADE model:
```bash
cqs init --splade  # downloads SPLADE model alongside embedding model
```

Use hf-hub to download `naver/splade-cocondenser-ensembledistil`. Export to ONNX if needed (check if pre-exported ONNX exists on the hub).

- [ ] **Step 3: Doctor check**

Add SPLADE model status to `cqs doctor` output:
```
SPLADE: loaded (naver/splade-cocondenser-ensembledistil)
```
or
```
SPLADE: not available (run cqs init --splade to download)
```

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/cli/commands/infra/
git commit -m "feat: SPLADE model download via init, doctor check"
```

---

### Task 8: Documentation and cleanup

**Files:**
- Modify: `CONTRIBUTING.md` — mention SPLADE in architecture
- Modify: `README.md` — add `--splade` flag to search options
- Modify: `CHANGELOG.md` — entry for SPLADE

- [ ] **Step 1: Update docs**

Add SPLADE to README search options:
```
cqs "query" --splade          # Sparse-dense hybrid search (requires SPLADE model)
```

Update CONTRIBUTING.md architecture to include `src/splade/`.

- [ ] **Step 2: Run full test suite**

```bash
cargo test --features gpu-index 2>&1 | grep "^test result:" | awk '{p+=$4; f+=$6} END {printf "%d pass, %d fail\n", p, f}'
```

- [ ] **Step 3: Run eval and record baseline**

```bash
cargo test --release --features gpu-index --test pipeline_eval test_fixture_eval_296q -- --ignored --nocapture
```

Record Config G/H/I results in RESULTS.md. Compare to Config A (91.2%).

- [ ] **Step 4: Commit**

```bash
git add CONTRIBUTING.md README.md CHANGELOG.md
git commit -m "docs: add SPLADE to README, CONTRIBUTING, CHANGELOG"
```

---

## Notes for implementer

- **Feature gate everything.** Use `#[cfg(feature = "splade")]` so the feature doesn't affect non-SPLADE users until proven. No compile-time cost for opt-out.
- **ONNX model format.** The naver SPLADE model may not have a pre-exported ONNX on HuggingFace. Check first. If not, use `cqs export-model` or `optimum` to export. The opset-11 weight injection trick from `train_lora.py` may be needed.
- **Tokenizer compatibility.** SPLADE uses BERT tokenizer (WordPiece). The token IDs in the sparse vector are BERT vocabulary indices. Verify the tokenizer from the model hub matches.
- **Score normalization.** Min-max normalization within result set. If SPLADE returns only one result, its normalized score is 1.0. Handle the edge case of empty result sets.
- **Tracing convention.** Every public function gets `tracing::info_span!` or `debug_span!`. Errors get `tracing::warn!`. Follow the pattern in `src/hnsw/search.rs` and `src/embedder/mod.rs`.
- **Error handling.** SPLADE is supplementary. All SPLADE failures should warn and fall back to cosine-only, never abort the search or indexing pipeline. Use `Result` internally but catch at the integration points.
- **The `enrichment_version` column** (RT-DATA-2) is in the schema migration but not used by SPLADE. Wire it into the enrichment pass as a separate commit — the schema change is bundled for efficiency but the feature is independent.
- **Use `--features gpu-index`** for all cargo commands. This is the project default.
- **Run `cargo fmt`** before every commit.
