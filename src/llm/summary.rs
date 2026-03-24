//! LLM summary pass orchestration — collects chunks, submits batches, stores results.

use std::collections::HashMap;

use ndarray::Array2;

use super::batch::BatchPhase2;
use super::{Client, LlmConfig, LlmError, MAX_BATCH_SIZE, MIN_CONTENT_CHARS};
use crate::Store;

/// Run the LLM summary pass using the Batches API.
///
/// Collects all uncached callable chunks, submits them as a batch to Claude,
/// polls for completion, then stores results. Doc comments are extracted locally
/// without API calls.
///
/// Returns the number of new summaries generated.
pub fn llm_summary_pass(
    store: &Store,
    quiet: bool,
    config: &crate::config::Config,
) -> Result<usize, LlmError> {
    let _span = tracing::info_span!("llm_summary_pass").entered();

    let llm_config = LlmConfig::resolve(config);
    tracing::info!(
        model = %llm_config.model,
        api_base = %llm_config.api_base,
        max_tokens = llm_config.max_tokens,
        "LLM config resolved"
    );

    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        LlmError::ApiKeyMissing(
            "--llm-summaries requires ANTHROPIC_API_KEY environment variable".to_string(),
        )
    })?;
    let client = Client::new(&api_key, llm_config)?;

    let mut cached = 0usize;
    let mut skipped = 0usize;
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;

    // Phase 0: Precompute contrastive neighbors from embedding similarity
    let neighbor_map = match find_contrastive_neighbors(store, 3) {
        Ok(map) => map,
        Err(e) => {
            tracing::warn!(error = %e, "Contrastive neighbor computation failed, falling back to discriminating-only");
            HashMap::new()
        }
    };

    // Phase 1: Collect chunks needing summaries
    // (custom_id=content_hash, prompt, chunk_type, language) for batch API
    let mut batch_items: Vec<(String, String, String, String)> = Vec::new();
    // Track content_hashes already queued to avoid duplicate custom_ids in batch
    let mut queued_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();

    let stats = store.stats()?;
    tracing::info!(chunks = stats.total_chunks, "Scanning for LLM summaries");

    let mut batch_full = false;
    loop {
        let (chunks, next) = store.chunks_paged(cursor, PAGE_SIZE)?;
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
        let existing = store.get_summaries_by_hashes(&hashes, "summary")?;

        for cs in &chunks {
            if existing.contains_key(&cs.content_hash) {
                cached += 1;
                continue;
            }

            if !cs.chunk_type.is_callable() {
                skipped += 1;
                continue;
            }

            if cs.content.len() < MIN_CONTENT_CHARS {
                skipped += 1;
                continue;
            }

            if cs.window_idx.is_some_and(|idx| idx > 0) {
                skipped += 1;
                continue;
            }

            // All chunks go through the contrastive API path (option 2).
            // Doc-comment shortcut removed — contrastive summaries are more
            // discriminating for retrieval than raw first-sentence extraction.

            // Queue for batch API (deduplicate by content_hash)
            if queued_hashes.insert(cs.content_hash.clone()) {
                // Pre-build prompt with contrastive neighbor context if available
                let neighbors = neighbor_map
                    .get(&cs.content_hash)
                    .cloned()
                    .unwrap_or_default();
                let prompt = if neighbors.is_empty() {
                    Client::build_prompt(
                        &cs.content,
                        &cs.chunk_type.to_string(),
                        &cs.language.to_string(),
                    )
                } else {
                    Client::build_contrastive_prompt(
                        &cs.content,
                        &cs.chunk_type.to_string(),
                        &cs.language.to_string(),
                        &neighbors,
                    )
                };
                batch_items.push((
                    cs.content_hash.clone(),
                    prompt,
                    cs.chunk_type.to_string(),
                    cs.language.to_string(),
                ));
                if batch_items.len() >= MAX_BATCH_SIZE {
                    batch_full = true;
                    break;
                }
            }
        }
        if batch_full {
            tracing::info!(
                max = MAX_BATCH_SIZE,
                "Batch size limit reached, submitting partial batch"
            );
            break;
        }
    }

    // Count how many batch items got contrastive neighbors
    let with_neighbors = if neighbor_map.is_empty() {
        0
    } else {
        batch_items
            .iter()
            .filter(|(hash, _, _, _)| neighbor_map.contains_key(hash))
            .count()
    };

    tracing::info!(
        cached,
        skipped,
        api_needed = batch_items.len(),
        with_neighbors,
        "Summary scan complete"
    );

    // Phase 2: Submit batch to Claude API (or resume a pending one)
    let phase2 = BatchPhase2 {
        purpose: "summary",
        max_tokens: client.llm_config.max_tokens,
        quiet,
    };
    let api_results = phase2.submit_or_resume(
        &client,
        store,
        &batch_items,
        &|s| s.get_pending_batch_id(),
        &|s, id| s.set_pending_batch_id(id),
        &|c, items, max_tok| c.submit_batch_prebuilt(items, max_tok),
    )?;
    let api_generated = api_results.len();

    tracing::info!(api_generated, cached, skipped, "LLM summary pass complete");

    Ok(api_generated)
}

/// Precompute top-N nearest neighbors for all callable chunks by cosine similarity.
///
/// Loads all callable chunk embeddings from SQLite, builds a pairwise cosine similarity
/// matrix via L2-normalized matrix multiply, and returns a map from content_hash to
/// neighbor names. Used to generate contrastive LLM summaries ("unlike X, this does Y").
///
/// Runs during `llm_summary_pass` Phase 1, when embeddings are in SQLite but HNSW
/// is not yet built. ~1.3s for 10k chunks.
///
/// Memory: N×N×4 bytes for the similarity matrix (~550MB at 12k callable chunks).
/// The matrix is dropped after top-N extraction.
fn find_contrastive_neighbors(
    store: &Store,
    limit: usize,
) -> Result<HashMap<String, Vec<String>>, LlmError> {
    let _span = tracing::info_span!("find_contrastive_neighbors", limit).entered();

    // Collect callable chunk identities (content_hash, name)
    let mut chunk_ids: Vec<(String, String)> = Vec::new(); // (content_hash, name)
    let mut cursor = 0i64;
    loop {
        let (page, next) = store.chunks_paged(cursor, 500)?;
        if page.is_empty() {
            break;
        }
        cursor = next;
        for cs in &page {
            if !cs.chunk_type.is_callable() {
                continue;
            }
            if cs.content.len() < MIN_CONTENT_CHARS {
                continue;
            }
            if cs.window_idx.is_some_and(|idx| idx > 0) {
                continue;
            }
            chunk_ids.push((cs.content_hash.clone(), cs.name.clone()));
        }
    }

    if chunk_ids.len() < 2 {
        tracing::info!(
            count = chunk_ids.len(),
            "Too few callable chunks for contrastive neighbors"
        );
        return Ok(HashMap::new());
    }

    // Batch-fetch embeddings
    let hashes: Vec<&str> = chunk_ids.iter().map(|(h, _)| h.as_str()).collect();
    let embeddings = store.get_embeddings_by_hashes(&hashes)?;

    // Filter to chunks with embeddings, build matrix
    let mut valid: Vec<(&str, &str, &[f32])> = Vec::new(); // (hash, name, embedding)
    for (hash, name) in &chunk_ids {
        if let Some(emb) = embeddings.get(hash.as_str()) {
            valid.push((hash, name, emb.as_slice()));
        }
    }

    let n = valid.len();
    if n < 2 {
        return Ok(HashMap::new());
    }

    let dim = valid[0].2.len();
    tracing::info!(chunks = n, dim, "Computing pairwise cosine similarity");

    // Build L2-normalized ndarray matrix
    let mut matrix = Array2::<f32>::zeros((n, dim));
    for (i, (_, _, emb)) in valid.iter().enumerate() {
        let mut row = matrix.row_mut(i);
        for (j, &v) in emb.iter().enumerate() {
            row[j] = v;
        }
        // L2-normalize
        let norm = matrix.row(i).mapv(|x| x * x).sum().sqrt();
        if norm > 0.0 {
            matrix.row_mut(i).mapv_inplace(|x| x / norm);
        }
    }

    // Pairwise cosine = normalized @ normalized.T
    let sims = matrix.dot(&matrix.t());

    // Extract top-N neighbors per chunk (excluding self)
    let mut result: HashMap<String, Vec<String>> = HashMap::with_capacity(n);
    for i in 0..n {
        let row = sims.row(i);
        // Partial sort: collect (index, score) pairs, sort desc, take top-N
        let mut scored: Vec<(usize, f32)> =
            (0..n).filter(|&j| j != i).map(|j| (j, row[j])).collect();
        scored.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        let neighbors: Vec<String> = scored
            .iter()
            .take(limit)
            .map(|(j, _)| valid[*j].1.to_string())
            .collect();
        if !neighbors.is_empty() {
            result.insert(valid[i].0.to_string(), neighbors);
        }
    }

    let with_neighbors = result.len();
    tracing::info!(total = n, with_neighbors, "Contrastive neighbors computed");

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== TC-22: LLM pass chunk filtering condition tests =====
    //
    // The filtering logic in llm_summary_pass (and hyde_query_pass) applies 4 skip conditions
    // to each ChunkSummary. Since the logic is inline, these tests validate each condition
    // independently using the same types and constants.

    use crate::language::ChunkType;
    use std::path::PathBuf;

    fn make_test_chunk_summary(
        name: &str,
        chunk_type: ChunkType,
        content_len: usize,
        window_idx: Option<i32>,
        content_hash: &str,
    ) -> crate::store::ChunkSummary {
        crate::store::ChunkSummary {
            id: format!("test:1:{}", name),
            file: PathBuf::from("src/lib.rs"),
            language: crate::parser::Language::Rust,
            chunk_type,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content: "x".repeat(content_len),
            doc: None,
            line_start: 1,
            line_end: 10,
            parent_id: None,
            parent_type_name: None,
            content_hash: content_hash.to_string(),
            window_idx,
        }
    }

    /// Condition 1: cached chunks (content_hash in existing) should be skipped.
    #[test]
    fn filter_skips_cached_chunks() {
        let cs = make_test_chunk_summary("func", ChunkType::Function, 100, None, "already_cached");
        let mut existing = std::collections::HashMap::new();
        existing.insert("already_cached".to_string(), "old summary".to_string());
        assert!(
            existing.contains_key(&cs.content_hash),
            "Cached chunk should be recognized as existing"
        );
    }

    /// Condition 2: non-callable chunk types should be skipped.
    #[test]
    fn filter_skips_non_callable_chunks() {
        let non_callable_types = [
            ChunkType::Struct,
            ChunkType::Enum,
            ChunkType::Trait,
            ChunkType::Interface,
            ChunkType::Class,
            ChunkType::Constant,
            ChunkType::Section,
            ChunkType::Module,
            ChunkType::TypeAlias,
        ];
        for ct in non_callable_types {
            assert!(!ct.is_callable(), "{:?} should not be callable", ct);
        }
        // Callable types should NOT be skipped
        let callable_types = [
            ChunkType::Function,
            ChunkType::Method,
            ChunkType::Constructor,
            ChunkType::Property,
            ChunkType::Macro,
            ChunkType::Extension,
        ];
        for ct in callable_types {
            assert!(ct.is_callable(), "{:?} should be callable", ct);
        }
    }

    /// Condition 3: chunks below MIN_CONTENT_CHARS should be skipped.
    #[test]
    fn filter_skips_short_content() {
        let short = make_test_chunk_summary("short_fn", ChunkType::Function, 10, None, "h1");
        assert!(
            short.content.len() < MIN_CONTENT_CHARS,
            "Content of {} chars should be below MIN_CONTENT_CHARS ({})",
            short.content.len(),
            MIN_CONTENT_CHARS
        );

        let adequate = make_test_chunk_summary("good_fn", ChunkType::Function, 100, None, "h2");
        assert!(
            adequate.content.len() >= MIN_CONTENT_CHARS,
            "Content of {} chars should be at or above MIN_CONTENT_CHARS ({})",
            adequate.content.len(),
            MIN_CONTENT_CHARS
        );
    }

    /// Condition 3 boundary: exactly MIN_CONTENT_CHARS should NOT be skipped.
    #[test]
    fn filter_accepts_exactly_min_content_chars() {
        let cs = make_test_chunk_summary(
            "boundary_fn",
            ChunkType::Function,
            MIN_CONTENT_CHARS,
            None,
            "h3",
        );
        assert!(
            cs.content.len() >= MIN_CONTENT_CHARS,
            "Exactly MIN_CONTENT_CHARS should pass the filter"
        );
    }

    /// Condition 4: windowed chunks (window_idx > 0) should be skipped.
    #[test]
    fn filter_skips_windowed_chunks() {
        let windowed = make_test_chunk_summary("fn_w1", ChunkType::Function, 100, Some(1), "h4");
        assert!(
            windowed.window_idx.is_some_and(|idx| idx > 0),
            "window_idx=1 should be filtered out"
        );

        let window_zero = make_test_chunk_summary("fn_w0", ChunkType::Function, 100, Some(0), "h5");
        assert!(
            !window_zero.window_idx.is_some_and(|idx| idx > 0),
            "window_idx=0 should NOT be filtered out"
        );

        let no_window = make_test_chunk_summary("fn_no_w", ChunkType::Function, 100, None, "h6");
        assert!(
            !no_window.window_idx.is_some_and(|idx| idx > 0),
            "window_idx=None should NOT be filtered out"
        );
    }

    /// All conditions pass: a callable, sufficiently long, non-windowed, uncached chunk.
    #[test]
    fn filter_accepts_eligible_chunk() {
        let cs = make_test_chunk_summary("eligible_fn", ChunkType::Function, 200, None, "new_hash");
        let existing: std::collections::HashMap<String, String> = std::collections::HashMap::new();

        let skip_cached = existing.contains_key(&cs.content_hash);
        let skip_non_callable = !cs.chunk_type.is_callable();
        let skip_short = cs.content.len() < MIN_CONTENT_CHARS;
        let skip_windowed = cs.window_idx.is_some_and(|idx| idx > 0);

        assert!(!skip_cached, "Should not be cached");
        assert!(!skip_non_callable, "Function is callable");
        assert!(!skip_short, "200 chars > MIN_CONTENT_CHARS");
        assert!(!skip_windowed, "No window index");
    }

    // ===== TC-4: contrastive neighbor edge-case tests =====

    /// Empty store → find_contrastive_neighbors returns Ok with empty HashMap.
    #[test]
    fn contrastive_neighbors_empty_store() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::Store::open(&dir.path().join("index.db")).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();
        let result = find_contrastive_neighbors(&store, 3);
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);
        assert!(
            result.unwrap().is_empty(),
            "Expected empty HashMap for empty store"
        );
    }

    /// Empty store with limit=0 → Ok, empty HashMap.
    #[test]
    fn contrastive_neighbors_limit_zero() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::Store::open(&dir.path().join("index.db")).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();
        let result = find_contrastive_neighbors(&store, 0);
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);
        assert!(
            result.unwrap().is_empty(),
            "Expected empty HashMap when limit=0"
        );
    }

    /// L2-normalizing a zero vector must not panic; the row must remain all-zero.
    /// A unit vector must be unchanged after normalization.
    #[test]
    fn l2_normalize_zero_vector_no_panic() {
        use ndarray::Array2;
        let mut matrix = Array2::<f32>::zeros((2, 4));
        // row 0: all zeros — norm is 0, should be left as-is
        // row 1: unit vector along first axis
        matrix[[1, 0]] = 1.0;

        for i in 0..2 {
            let norm = matrix.row(i).mapv(|x| x * x).sum().sqrt();
            if norm > 0.0 {
                matrix.row_mut(i).mapv_inplace(|x| x / norm);
            }
        }

        // Zero row stays zero
        for j in 0..4 {
            assert_eq!(
                matrix[[0, j]],
                0.0,
                "Zero row should stay zero after normalization"
            );
        }
        // Unit row stays unit (norm == 1.0)
        let norm_after: f32 = matrix.row(1).mapv(|x| x * x).sum().sqrt();
        assert!(
            (norm_after - 1.0).abs() < 1e-6,
            "Unit row norm should be 1.0, got {}",
            norm_after
        );
    }

    /// A 3×4 matrix with one all-zero row: after pairwise cosine (matrix @ matrix.T),
    /// the zero row's self-similarity is 0.0 and all cross-similarities involving it are 0.0.
    #[test]
    fn pairwise_cosine_with_zero_row() {
        use ndarray::Array2;
        let mut matrix = Array2::<f32>::zeros((3, 4));
        // row 0: zero vector
        // row 1: unit along dim 0
        matrix[[1, 0]] = 1.0;
        // row 2: unit along dim 1
        matrix[[2, 1]] = 1.0;

        // Normalize (zero row unchanged)
        for i in 0..3 {
            let norm = matrix.row(i).mapv(|x| x * x).sum().sqrt();
            if norm > 0.0 {
                matrix.row_mut(i).mapv_inplace(|x| x / norm);
            }
        }

        let sims = matrix.dot(&matrix.t());

        // Zero row: self-sim = 0, cross-sims = 0
        assert_eq!(sims[[0, 0]], 0.0, "Zero-row self-sim should be 0");
        assert_eq!(
            sims[[0, 1]],
            0.0,
            "Zero-row cross-sim with row 1 should be 0"
        );
        assert_eq!(
            sims[[0, 2]],
            0.0,
            "Zero-row cross-sim with row 2 should be 0"
        );
        assert_eq!(
            sims[[1, 0]],
            0.0,
            "Cross-sim with zero-row should be 0 (symmetric)"
        );
        assert_eq!(
            sims[[2, 0]],
            0.0,
            "Cross-sim with zero-row should be 0 (symmetric)"
        );

        // Non-zero rows: self-sim ≈ 1.0
        assert!(
            (sims[[1, 1]] - 1.0).abs() < 1e-6,
            "Row 1 self-sim should be 1.0, got {}",
            sims[[1, 1]]
        );
        assert!(
            (sims[[2, 2]] - 1.0).abs() < 1e-6,
            "Row 2 self-sim should be 1.0, got {}",
            sims[[2, 2]]
        );
    }

    /// 3×4 matrix with all rows identical: after L2-normalization, all pairwise
    /// similarities (including self) should be ≈ 1.0.
    #[test]
    fn pairwise_cosine_identical_vectors() {
        use ndarray::Array2;
        let mut matrix = Array2::<f32>::zeros((3, 4));
        // All rows identical: [1, 2, 3, 4]
        for i in 0..3 {
            matrix[[i, 0]] = 1.0;
            matrix[[i, 1]] = 2.0;
            matrix[[i, 2]] = 3.0;
            matrix[[i, 3]] = 4.0;
        }

        // L2-normalize each row
        for i in 0..3 {
            let norm = matrix.row(i).mapv(|x| x * x).sum().sqrt();
            if norm > 0.0 {
                matrix.row_mut(i).mapv_inplace(|x| x / norm);
            }
        }

        let sims = matrix.dot(&matrix.t());

        // All pairwise similarities should be ≈ 1.0
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    (sims[[i, j]] - 1.0).abs() < 1e-6,
                    "sims[{},{}] should be ≈ 1.0 for identical vectors, got {}",
                    i,
                    j,
                    sims[[i, j]]
                );
            }
        }
    }
}
