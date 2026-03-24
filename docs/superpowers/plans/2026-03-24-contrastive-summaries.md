# Contrastive Discriminating Summaries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enhance LLM summary prompts with nearest-neighbor context so summaries say "unlike X, this function does Y" instead of generic descriptions. Exp 8 showed contrastive +18.2pp vs +16.3pp discriminating — a 1.9pp gain from neighbor context alone.

**Architecture:** Within the existing `llm_summary_pass`, precompute a full pairwise cosine similarity matrix from chunk embeddings (already in SQLite at this pipeline stage), extract top-3 neighbors per chunk, pass neighbor names into `build_prompt` for contrastive prompting. No schema changes — summaries stored with existing `purpose = "summary"`.

**Tech Stack:** Rust, ndarray, sqlx, Claude Batches API

---

## Constraints

- **HNSW is NOT available during summary pass** — it's built after summaries (step 6 in pipeline). Embeddings ARE available (written in step 1).
- **Brute-force cosine is fast** — benchmarked at 1.3s for 10k chunks (768-dim matrix multiply + top-3 extraction). Negligible vs 5-15 min API wait.
- **ndarray** is already a dependency — used in HNSW and embedder.
- **10,000 item batch cap** — neighbor precomputation is one-time, not per-item.
- **`submit_doc_batch` and `submit_hyde_batch` are NOT affected** — they use separate prompt builders (`build_doc_prompt`, `build_hyde_prompt`) and keep their existing 4-tuple signatures.

## Neighbor Strategy

**Brute-force cosine on embeddings.** FTS name search was considered but rejected:
- FTS phrase-prefix (`name:"merge sort"*`) only matches prefix variants of the same name — it will NOT find `heap_sort` from `merge_sort`.
- Token-split OR (`name:sort`) is noisy and needs custom query logic.
- Embedding cosine directly measures semantic similarity — exactly what we want.

**Process:**
1. Load all callable chunk embeddings from SQLite (one batch query)
2. Build ndarray matrix (N × 768), L2-normalize rows
3. Matrix multiply: `sims = embeddings @ embeddings.T` → N × N similarity matrix
4. For each chunk, extract top-3 neighbors (excluding self, same language)
5. Store as `HashMap<String, Vec<String>>` keyed by content_hash → neighbor names
6. Look up neighbors per batch item during collection

**Memory:** N × N × 4 bytes. At 10k chunks = 381 MB (brief, dropped after extraction). At 7k chunks (our index) = 187 MB.

---

### Task 1: Add `find_contrastive_neighbors` function

**Files:**
- Modify: `src/llm/summary.rs` — add neighbor computation function

This is a standalone function (not a Store method) because it operates on in-memory embeddings, not the database.

- [ ] **Step 1: Write tests**

```rust
#[cfg(test)]
mod neighbor_tests {
    #[test]
    fn test_find_neighbors_basic() {
        // 4 chunks: A (similar to B), C (similar to D), all Rust
        // Verify A's neighbors include B, C's neighbors include D
    }

    #[test]
    fn test_find_neighbors_excludes_self() {
        // Verify chunk is never its own neighbor
    }

    #[test]
    fn test_find_neighbors_respects_limit() {
        // 10 similar chunks, limit=3, verify only 3 returned
    }

    #[test]
    fn test_find_neighbors_empty_input() {
        // Zero chunks → empty HashMap, no panic
    }

    #[test]
    fn test_find_neighbors_single_chunk() {
        // One chunk → no neighbors possible → empty vec
    }
}
```

- [ ] **Step 2: Implement `find_contrastive_neighbors`**

```rust
use ndarray::{Array2, Axis};

/// Precompute top-N nearest neighbors for all chunks by cosine similarity.
///
/// Loads all callable chunk embeddings, builds a pairwise similarity matrix,
/// and returns a map from content_hash to neighbor names.
///
/// This runs during `llm_summary_pass` Phase 1, when embeddings are in SQLite
/// but HNSW is not yet built. Benchmarked at ~1.3s for 10k chunks.
fn find_contrastive_neighbors(
    store: &Store,
    limit: usize,
) -> Result<HashMap<String, Vec<String>>, LlmError> {
    let _span = tracing::info_span!("find_contrastive_neighbors", limit).entered();

    // Load all callable chunk identities + embeddings
    // Use chunks_paged to iterate, filter callable, collect (content_hash, name, embedding)
    let mut chunks: Vec<(String, String, Vec<f32>)> = Vec::new(); // (content_hash, name, embedding)

    let mut cursor = 0i64;
    loop {
        let (page, next) = store.chunks_paged(cursor, 500)?;
        if page.is_empty() { break; }
        cursor = next;
        for cs in &page {
            if !cs.chunk_type.is_callable() { continue; }
            if cs.content.len() < MIN_CONTENT_CHARS { continue; }
            if cs.window_idx.is_some_and(|idx| idx > 0) { continue; }
            // Fetch embedding for this chunk
            // (batch this for efficiency)
            chunks.push((cs.content_hash.clone(), cs.name.clone(), Vec::new())); // placeholder
        }
    }

    if chunks.len() < 2 {
        tracing::info!(count = chunks.len(), "Too few callable chunks for contrastive neighbors");
        return Ok(HashMap::new());
    }

    // Batch-fetch embeddings by content_hash
    let hashes: Vec<&str> = chunks.iter().map(|(h, _, _)| h.as_str()).collect();
    let embeddings = store.get_embeddings_by_hashes(&hashes)?;

    // Build matrix — only chunks with embeddings
    let mut valid_chunks: Vec<(String, String, &[f32])> = Vec::new();
    for (hash, name, _) in &chunks {
        if let Some(emb) = embeddings.get(hash.as_str()) {
            valid_chunks.push((hash.clone(), name.clone(), emb));
        }
    }

    let n = valid_chunks.len();
    if n < 2 {
        return Ok(HashMap::new());
    }

    let dim = valid_chunks[0].2.len();
    tracing::info!(chunks = n, dim, "Computing pairwise cosine similarity");

    // Build ndarray matrix, L2-normalize
    let mut matrix = Array2::<f32>::zeros((n, dim));
    for (i, (_, _, emb)) in valid_chunks.iter().enumerate() {
        for (j, &v) in emb.iter().enumerate() {
            matrix[[i, j]] = v;
        }
        // L2-normalize row
        let norm = matrix.row(i).mapv(|x| x * x).sum().sqrt();
        if norm > 0.0 {
            matrix.row_mut(i).mapv_inplace(|x| x / norm);
        }
    }

    // Pairwise cosine = matrix @ matrix.T
    let sims = matrix.dot(&matrix.t());

    // Extract top-N neighbors per chunk (excluding self)
    let mut result: HashMap<String, Vec<String>> = HashMap::new();
    for i in 0..n {
        let mut scored: Vec<(usize, f32)> = (0..n)
            .filter(|&j| j != i)
            .map(|j| (j, sims[[i, j]]))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let neighbors: Vec<String> = scored.iter()
            .take(limit)
            .map(|(j, _)| valid_chunks[*j].1.clone())
            .collect();
        if !neighbors.is_empty() {
            result.insert(valid_chunks[i].0.clone(), neighbors);
        }
    }

    let with_neighbors = result.len();
    tracing::info!(total = n, with_neighbors, "Contrastive neighbors computed");

    Ok(result)
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib -- llm::summary::neighbor_tests
```

- [ ] **Step 4: Commit**

```bash
git commit -m "feat: brute-force cosine neighbor computation for contrastive summaries"
```

---

### Task 2: Introduce `SummaryBatchItem` struct

**Files:**
- Modify: `src/llm/summary.rs` — define struct, use in batch collection
- Modify: `src/llm/batch.rs` — update `submit_batch` signature

Replace the 4-tuple with a named struct. This is cleaner than a 5-tuple and prevents positional confusion.

- [ ] **Step 1: Define struct in `summary.rs`**

```rust
/// A chunk queued for LLM summary generation via the Batches API.
pub(super) struct SummaryBatchItem {
    pub content_hash: String,
    pub content: String,
    pub chunk_type: String,
    pub language: String,
    pub neighbors: Vec<String>,
}
```

- [ ] **Step 2: Update `submit_batch` in `batch.rs`**

```rust
// Before:
pub(super) fn submit_batch(&self, items: &[(String, String, String, String)], max_tokens: u32)
// After:
pub(super) fn submit_batch(&self, items: &[SummaryBatchItem], max_tokens: u32)
```

Map fields by name: `item.content_hash`, `item.content`, etc. Pass `&item.neighbors` to `build_prompt`.

**Note:** `submit_doc_batch` and `submit_hyde_batch` keep their existing 4-tuple signatures — they use different prompt builders and don't need neighbors.

- [ ] **Step 3: Update batch collection in `summary.rs`**

Replace `batch_items.push((hash, content, type, lang))` with `batch_items.push(SummaryBatchItem { ... })`.

- [ ] **Step 4: Update all 3 `submit_batch` call sites in `summary.rs`**

These are at lines ~184, ~196, ~205 in `summary.rs`. All pass `&batch_items` — no change needed since the type changed.

- [ ] **Step 5: Build and verify**

```bash
cargo build
```

- [ ] **Step 6: Commit**

```bash
git commit -m "refactor: replace batch item 4-tuple with SummaryBatchItem struct"
```

---

### Task 3: Update `build_prompt` for contrastive mode

**Files:**
- Modify: `src/llm/prompts.rs`

- [ ] **Step 1: Write tests**

```rust
#[test]
fn test_build_prompt_no_neighbors_unchanged() {
    let prompt = Client::build_prompt("fn foo() {}", "function", "rust", &[]);
    assert!(prompt.contains("unique and distinguishable"));
    assert!(!prompt.contains("similar to but different from"));
}

#[test]
fn test_build_prompt_single_neighbor() {
    let prompt = Client::build_prompt("fn merge_sort() {}", "function", "rust", &["heap_sort".into()]);
    assert!(prompt.contains("heap_sort"));
    assert!(prompt.contains("distinguishes"));
}

#[test]
fn test_build_prompt_three_neighbors() {
    let prompt = Client::build_prompt("fn merge_sort() {}", "function", "rust",
        &["heap_sort".into(), "insertion_sort".into(), "quicksort".into()]);
    assert!(prompt.contains("heap_sort"));
    assert!(prompt.contains("insertion_sort"));
    assert!(prompt.contains("quicksort"));
}

#[test]
fn test_build_prompt_long_neighbor_names_within_budget() {
    let long_name = "a".repeat(200);
    let neighbors = vec![long_name.clone(), long_name.clone(), long_name.clone()];
    let content = "x".repeat(7000);
    let prompt = Client::build_prompt(&content, "function", "rust", &neighbors);
    // Prompt should not exceed reasonable size (content is truncated to MAX_CONTENT_CHARS)
    assert!(prompt.len() < 9000); // MAX_CONTENT_CHARS (8000) + prompt overhead + neighbor names
}
```

- [ ] **Step 2: Add neighbors parameter to `build_prompt`**

```rust
fn build_prompt(content: &str, chunk_type: &str, language: &str, neighbors: &[String]) -> String {
    let truncated = if content.len() > MAX_CONTENT_CHARS {
        &content[..content.floor_char_boundary(MAX_CONTENT_CHARS)]
    } else {
        content
    };
    if neighbors.is_empty() {
        // Existing discriminating prompt (unchanged)
        format!(
            "Describe what makes this {} unique and distinguishable from similar {}s. \
             Focus on the specific algorithm, approach, or behavioral characteristics \
             that distinguish it. One sentence only. Be specific, not generic.\n\n```{}\n{}\n```",
            chunk_type, chunk_type, language, truncated
        )
    } else {
        // Truncate neighbor list to avoid blowing prompt budget
        let neighbor_list: String = neighbors.iter()
            .take(5)
            .map(|n| if n.len() > 60 { &n[..60] } else { n.as_str() })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "This {} is similar to but different from: {}. \
             Describe what specifically distinguishes this {} from those. \
             Focus on the algorithm, data structure, or behavioral difference. \
             One sentence only. Be concrete.\n\n```{}\n{}\n```",
            chunk_type, neighbor_list, chunk_type, language, truncated
        )
    }
}
```

- [ ] **Step 3: Update existing `build_prompt` tests (3 in prompts.rs)**

Add `&[]` as 4th argument to the 3 existing test calls.

- [ ] **Step 4: Update `submit_batch` call site in `batch.rs`**

Pass `&item.neighbors` to `build_prompt`.

- [ ] **Step 5: Run tests**

```bash
cargo test --lib -- llm::prompts
```

- [ ] **Step 6: Commit**

```bash
git commit -m "feat: contrastive prompt with embedding-based neighbor context"
```

---

### Task 4: Wire neighbor precomputation into summary pass

**Files:**
- Modify: `src/llm/summary.rs`

- [ ] **Step 1: Call `find_contrastive_neighbors` at start of Phase 1**

Before the batch collection loop, compute all neighbors:

```rust
// Precompute contrastive neighbors from embedding similarity
let neighbor_map = match find_contrastive_neighbors(store, 3) {
    Ok(map) => map,
    Err(e) => {
        tracing::warn!(error = %e, "Contrastive neighbor computation failed, falling back to discriminating-only");
        HashMap::new()
    }
};
```

**Design decision:** Neighbor computation failure is non-fatal. All chunks fall back to the existing discriminating prompt (empty neighbors).

- [ ] **Step 2: Look up neighbors per batch item**

In the batch collection loop, after building content/chunk_type/language:

```rust
let neighbors = neighbor_map
    .get(&cs.content_hash)
    .cloned()
    .unwrap_or_default();

batch_items.push(SummaryBatchItem {
    content_hash: cs.content_hash.clone(),
    content,
    chunk_type: cs.chunk_type.to_string(),
    language: cs.language.to_string(),
    neighbors,
});
```

- [ ] **Step 3: Add batch-level tracing**

After the collection loop:

```rust
let with_neighbors = batch_items.iter().filter(|item| !item.neighbors.is_empty()).count();
tracing::info!(
    total = batch_items.len(),
    with_neighbors,
    without = batch_items.len() - with_neighbors,
    "Summary batch items collected"
);
```

- [ ] **Step 4: Build and test**

```bash
cargo build && cargo test
```

- [ ] **Step 5: Commit**

```bash
git commit -m "feat: wire contrastive neighbors into LLM summary pass"
```

---

### Task 5: Tests and documentation

**Files:**
- Modify: `src/llm/summary.rs` (tests)

- [ ] **Step 1: Test neighbor failure fallback**

```rust
#[test]
fn test_neighbor_failure_produces_empty_neighbors() {
    // Verify that when neighbor_map is empty (simulating failure),
    // batch items still get created with empty neighbors vec
    let item = SummaryBatchItem {
        content_hash: "abc".into(),
        content: "fn foo() {}".into(),
        chunk_type: "function".into(),
        language: "rust".into(),
        neighbors: Vec::new(),
    };
    // Prompt should be the non-contrastive variant
    let prompt = Client::build_prompt(&item.content, &item.chunk_type, &item.language, &item.neighbors);
    assert!(prompt.contains("unique and distinguishable"));
}
```

- [ ] **Step 2: Manual smoke test**

```bash
# Delete existing summaries to force regeneration
sqlite3 .cqs/index.db "DELETE FROM llm_summaries WHERE purpose = 'summary';"
cqs index --llm-summaries 2>&1 | grep -E "neighbors|contrastive|Batch items"
```

Expected output:
```
Contrastive neighbors computed: total=2500, with_neighbors=2100
Summary batch items collected: total=2500, with_neighbors=2100, without=400
```

- [ ] **Step 3: Add doc comment to `find_contrastive_neighbors`**

Note the known limitations:
- Neighbor names can become stale if chunks are added/removed (summaries cached by content_hash, not re-checked when neighbors change)
- Functions with very common names (e.g., `new`, `init`) may get neighbors that are semantically unrelated — the embedding similarity handles this better than name matching would
- Memory usage: N×N similarity matrix is brief (~200-400 MB for typical indexes)

- [ ] **Step 4: Commit**

```bash
git commit -m "test: contrastive summary tests and documentation"
```

---

## Estimated effort

| Task | Time | Risk |
|------|------|------|
| 1. Neighbor computation | 30 min | Medium — ndarray matrix ops, embedding loading |
| 2. Batch struct refactor | 15 min | Low — mechanical |
| 3. Prompt update | 15 min | Low — string formatting |
| 4. Wire into summary pass | 15 min | Low — map lookup |
| 5. Tests + docs | 15 min | Low |
| **Total** | **~1.5 hours** | |

## Known limitations

- **Neighbor staleness:** Contrastive summaries are cached by `content_hash`. If a neighbor is renamed or deleted, the summary still references the old name. Summaries only regenerate when the function's own content changes, not when its neighbors change. This is acceptable — summaries are approximations, and stale neighbor references don't break retrieval.
- **Common names:** Functions named `new`, `init`, `test_*` may get semantically unrelated neighbors that happen to have similar embeddings. The contrastive prompt still improves over the non-contrastive version because the LLM sees the actual code, not just neighbor names.
- **Memory:** The N×N similarity matrix uses N²×4 bytes. At 10k chunks = 381 MB, at 50k = 9.3 GB. For very large indexes, should switch to batched top-k (process chunks in groups, not full matrix). Current cqs indexes are <10k chunks.

## What this does NOT include

- **Training data augmentation** — this only affects index-time summaries. The `augment_with_summaries.py` script for training data is separate.
- **Eval** — should re-run hard eval (both raw and full-pipeline) after deploying.

## Dependencies

- `ndarray` — already a dependency (used by HNSW, embedder).
- `store.get_embeddings_by_hashes()` — exists in `src/store/chunks/embeddings.rs`.
- `store.chunks_paged()` — exists in `src/store/chunks/query.rs`.
- No schema changes. No new dependencies.
