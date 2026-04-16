//! LLM summary pass orchestration — collects chunks, submits batches, stores results.

use std::collections::HashMap;

use ndarray::Array2;

use super::batch::BatchPhase2;
use super::{collect_eligible_chunks, LlmClient, LlmConfig, LlmError, MAX_BATCH_SIZE};
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
    lock_dir: Option<&std::path::Path>,
) -> Result<usize, LlmError> {
    let _span = tracing::info_span!("llm_summary_pass").entered();

    let llm_config = LlmConfig::resolve(config)?;
    tracing::debug!(
        api_base = %llm_config.api_base,
        "LLM API base"
    );
    tracing::info!(
        model = %llm_config.model,
        max_tokens = llm_config.max_tokens,
        "LLM config resolved"
    );

    let client = super::create_client(llm_config)?;

    // Phase 0: Precompute contrastive neighbors from embedding similarity
    let neighbor_map = match find_contrastive_neighbors(store, 3) {
        Ok(map) => map,
        Err(e) => {
            tracing::warn!(error = %e, "Contrastive neighbor computation failed, falling back to discriminating-only");
            HashMap::new()
        }
    };

    // Phase 1: Collect chunks needing summaries via shared filter
    let (eligible, cached, skipped) = collect_eligible_chunks(store, "summary", MAX_BATCH_SIZE)?;

    // EH-23: Warn when contrastive neighbors are empty but eligible chunks exist
    if neighbor_map.is_empty() && !eligible.is_empty() {
        tracing::warn!(
            eligible_count = eligible.len(),
            "Contrastive neighbor map is empty despite eligible callable chunks — summaries will lack contrastive context"
        );
    }

    // Build batch items with contrastive neighbor prompts
    let mut batch_items: Vec<super::provider::BatchSubmitItem> = Vec::with_capacity(eligible.len());
    for ec in &eligible {
        let neighbors = neighbor_map
            .get(&ec.content_hash)
            .cloned()
            .unwrap_or_default();
        let prompt = if neighbors.is_empty() {
            LlmClient::build_prompt(&ec.content, &ec.chunk_type, &ec.language)
        } else {
            LlmClient::build_contrastive_prompt(
                &ec.content,
                &ec.chunk_type,
                &ec.language,
                &neighbors,
            )
        };
        batch_items.push(super::provider::BatchSubmitItem {
            custom_id: ec.content_hash.clone(),
            content: prompt,
            context: ec.chunk_type.clone(),
            language: ec.language.clone(),
        });
    }
    if batch_items.len() >= MAX_BATCH_SIZE {
        tracing::info!(
            max = MAX_BATCH_SIZE,
            "Batch size limit reached, submitting partial batch"
        );
    }

    // Count how many batch items got contrastive neighbors
    let with_neighbors = if neighbor_map.is_empty() {
        0
    } else {
        batch_items
            .iter()
            .filter(|item| neighbor_map.contains_key(&item.custom_id))
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
        lock_dir,
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

    // Collect callable chunk identities (content_hash, name) via shared filter.
    // Pass purpose="" to skip the cache check — contrastive neighbors need all eligible chunks.
    let (eligible, _, _) = collect_eligible_chunks(store, "", 0)?;
    let chunk_ids: Vec<(String, String)> = eligible
        .into_iter()
        .map(|ec| (ec.content_hash, ec.name))
        .collect();

    // DS-21: Cap N×N matrix size to avoid OOM on very large codebases.
    // Memory is N×N×4 bytes (~3.4GB at 30k). Override with CQS_MAX_CONTRASTIVE_CHUNKS.
    let max_contrastive: usize = std::env::var("CQS_MAX_CONTRASTIVE_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);
    if chunk_ids.len() > max_contrastive {
        tracing::warn!(
            chunks = chunk_ids.len(),
            max = max_contrastive,
            "Too many callable chunks for contrastive neighbor matrix, skipping"
        );
        return Ok(HashMap::new());
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

    // DS-23: Warn when embedding fetch returns empty or significantly fewer than expected
    if embeddings.is_empty() && !chunk_ids.is_empty() {
        tracing::warn!(
            requested = chunk_ids.len(),
            "Embedding fetch returned empty — contrastive neighbor map will be empty"
        );
        return Ok(HashMap::new());
    } else if embeddings.len() < chunk_ids.len() / 2 {
        tracing::warn!(
            requested = chunk_ids.len(),
            returned = embeddings.len(),
            "Embedding fetch returned significantly fewer results than expected"
        );
    }

    // Filter to chunks with embeddings, build matrix
    let mut valid: Vec<(&str, &str, &[f32])> = Vec::new(); // (hash, name, embedding)
    let expected_dim = embeddings.values().next().map(|e| e.len());
    for (hash, name) in &chunk_ids {
        if let Some(emb) = embeddings.get(hash.as_str()) {
            // RB-15: Filter out embeddings with mismatched dimensions to prevent
            // ndarray panics when building the similarity matrix.
            if let Some(dim) = expected_dim {
                if emb.len() != dim {
                    tracing::warn!(
                        hash,
                        expected = dim,
                        actual = emb.len(),
                        "Skipping embedding with mismatched dimension"
                    );
                    continue;
                }
            }
            valid.push((hash, name, emb.as_slice()));
        }
    }

    let n = valid.len();
    if n < 2 {
        return Ok(HashMap::new());
    }

    let dim = valid[0].2.len();
    tracing::info!(chunks = n, dim, "Computing pairwise cosine similarity");

    // Copy valid entries into owned data so we can drop the HashMap (RM-33)
    let valid_owned: Vec<(String, String)> = valid
        .iter()
        .map(|(h, name, _)| (h.to_string(), name.to_string()))
        .collect();

    // Build L2-normalized ndarray matrix directly from borrowed data, then drop borrows
    let mut matrix = Array2::<f32>::zeros((n, dim));
    for (i, (_, _, emb)) in valid.iter().enumerate() {
        matrix.row_mut(i).assign(&ndarray::ArrayView1::from(*emb));
        // L2-normalize
        let norm = matrix.row(i).mapv(|x| x * x).sum().sqrt();
        if norm > 0.0 {
            matrix.row_mut(i).mapv_inplace(|x| x / norm);
        }
    }
    // RM-33: Drop borrowed data — embeddings HashMap (~46MB) freed before N*N matrix
    drop(valid);
    drop(embeddings);

    // Pairwise cosine = normalized @ normalized.T
    let sims = matrix.dot(&matrix.t());
    drop(matrix); // RM-39: Free N*dim*4 bytes (~49MB at 12k*1024)

    // PERF-43: Extract top-N neighbors per chunk using select_nth_unstable_by
    // for O(N) average per row instead of O(N log K) with BinaryHeap.
    // RM-5: Reuse a single candidates buffer to avoid N*(N-1) intermediate allocations.
    // PF-4: Build result map inline — no intermediate per_row_neighbors Vec, no candidates.clone().
    let mut result: HashMap<String, Vec<String>> = HashMap::with_capacity(n);
    let mut candidates: Vec<(usize, f32)> = Vec::with_capacity(n);
    for i in 0..n {
        let row = sims.row(i);
        candidates.clear();
        candidates.extend((0..n).filter(|&j| j != i).map(|j| (j, row[j])));

        if candidates.len() <= limit {
            candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        } else {
            candidates.select_nth_unstable_by(limit - 1, |a, b| b.1.total_cmp(&a.1));
            candidates.truncate(limit);
            candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        }

        if !candidates.is_empty() {
            let names: Vec<String> = candidates
                .iter()
                .map(|(idx, _)| valid_owned[*idx].1.clone())
                .collect();
            result.insert(valid_owned[i].0.clone(), names);
        }
    }
    drop(sims); // RM-39: Free N*N*4 bytes (~550MB at 12k)

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
    use crate::llm::MIN_CONTENT_CHARS;
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

    /// Condition 2 (Phase 5 follow-up): the eligibility filter now uses
    /// `is_code()` instead of `is_callable()`, so type-definition chunks
    /// (struct, enum, trait, interface, class, constant, impl, etc.) are
    /// summarizable. Only Section (markdown) and Module (file-level) and
    /// ConfigKey/Object/Namespace are still skipped.
    #[test]
    fn filter_accepts_code_chunk_types() {
        // Code chunk types — should pass the filter
        let code_types = [
            ChunkType::Function,
            ChunkType::Method,
            ChunkType::Constructor,
            ChunkType::Property,
            ChunkType::Macro,
            ChunkType::Extension,
            ChunkType::Test,
            ChunkType::Struct,
            ChunkType::Enum,
            ChunkType::Trait,
            ChunkType::Interface,
            ChunkType::Class,
            ChunkType::Constant,
            ChunkType::Impl,
            ChunkType::TypeAlias,
            ChunkType::Variable,
        ];
        for ct in code_types {
            assert!(
                ct.is_code(),
                "{ct:?} should be considered a code chunk and eligible for summary"
            );
        }

        // Non-code types — should be filtered out (markdown sections,
        // file-level modules, raw config keys are not worth summarizing)
        let non_code_types = [ChunkType::Section, ChunkType::Module, ChunkType::ConfigKey];
        for ct in non_code_types {
            assert!(!ct.is_code(), "{ct:?} should not be eligible for summary");
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
            window_zero.window_idx.is_none_or(|idx| idx <= 0),
            "window_idx=0 should NOT be filtered out"
        );

        let no_window = make_test_chunk_summary("fn_no_w", ChunkType::Function, 100, None, "h6");
        assert!(
            no_window.window_idx.is_none_or(|idx| idx <= 0),
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
        let store = crate::Store::open(&dir.path().join(crate::INDEX_DB_FILENAME)).unwrap();
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
        let store = crate::Store::open(&dir.path().join(crate::INDEX_DB_FILENAME)).unwrap();
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
