//! HyDE (Hypothetical Document Embeddings) query prediction pass.

use super::batch::resume_or_fetch_hyde_batch;
use super::{
    Client, LlmConfig, LlmError, HYDE_MAX_TOKENS, MAX_BATCH_SIZE, MAX_CONTENT_CHARS,
    MIN_CONTENT_CHARS,
};
use crate::Store;

/// Run the HyDE query prediction pass using the Batches API.
///
/// Scans all callable chunks, submits them as a batch to Claude for query prediction,
/// polls for completion, then stores results with purpose="hyde".
///
/// Returns the number of new HyDE predictions generated.
pub fn hyde_query_pass(
    store: &Store,
    quiet: bool,
    config: &crate::config::Config,
    max_hyde: usize,
) -> Result<usize, LlmError> {
    let _span = tracing::info_span!("hyde_query_pass").entered();

    let llm_config = LlmConfig::resolve(config);
    tracing::info!(
        model = %llm_config.model,
        api_base = %llm_config.api_base,
        "HyDE query pass starting"
    );

    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        LlmError::ApiKeyMissing(
            "HyDE query pass requires ANTHROPIC_API_KEY environment variable".to_string(),
        )
    })?;
    let client = Client::new(&api_key, llm_config)?;

    let effective_batch_size = if max_hyde > 0 {
        max_hyde.min(MAX_BATCH_SIZE)
    } else {
        MAX_BATCH_SIZE
    };

    let mut cached = 0usize;
    let mut skipped = 0usize;
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;

    // Phase 1: Collect callable chunks needing HyDE predictions
    // (custom_id=content_hash, content, signature, language) for batch API
    let mut batch_items: Vec<(String, String, String, String)> = Vec::new();
    let mut queued_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();

    let stats = store.stats()?;
    tracing::info!(chunks = stats.total_chunks, "Scanning for HyDE predictions");

    let mut batch_full = false;
    loop {
        let (chunks, next) = store.chunks_paged(cursor, PAGE_SIZE)?;
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
        let existing = store.get_summaries_by_hashes(&hashes, "hyde")?;

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

            // Queue for batch API (deduplicate by content_hash)
            if queued_hashes.insert(cs.content_hash.clone()) {
                batch_items.push((
                    cs.content_hash.clone(),
                    if cs.content.len() > MAX_CONTENT_CHARS {
                        cs.content[..cs.content.floor_char_boundary(MAX_CONTENT_CHARS)].to_string()
                    } else {
                        cs.content.clone()
                    },
                    cs.signature.clone(),
                    cs.language.to_string(),
                ));
                if batch_items.len() >= effective_batch_size {
                    batch_full = true;
                    break;
                }
            }
        }
        if batch_full {
            tracing::info!(
                max = effective_batch_size,
                "HyDE batch size limit reached, submitting partial batch"
            );
            break;
        }
    }

    tracing::info!(
        cached,
        skipped,
        api_needed = batch_items.len(),
        "HyDE scan complete"
    );

    // Phase 2: Submit batch to Claude API (or resume a pending one)
    let api_generated = if batch_items.is_empty() {
        // No new items needed, but check if a previous batch is still pending
        match store.get_pending_hyde_batch_id() {
            Ok(Some(pending)) => {
                tracing::info!(batch_id = %pending, "Resuming pending HyDE batch");
                let count = resume_or_fetch_hyde_batch(&client, store, &pending, quiet)?;
                tracing::info!(
                    count,
                    "Fetched pending HyDE batch results — new chunks will be processed on next run"
                );
                count
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read pending HyDE batch ID");
                0
            }
            _ => 0,
        }
    } else {
        // Check for a pending batch from a previous interrupted run
        let batch_id = match store.get_pending_hyde_batch_id() {
            Ok(Some(pending)) => {
                tracing::info!(batch_id = %pending, "Found pending HyDE batch, checking status");
                match client.check_batch_status(&pending) {
                    Ok(status)
                        if status == "in_progress"
                            || status == "finalizing"
                            || status == "created"
                            || status == "ended" =>
                    {
                        tracing::info!(batch_id = %pending, status = %status, "Pending HyDE batch still active, resuming");
                        pending
                    }
                    _ => {
                        tracing::warn!(old_batch = %pending, "Pending HyDE batch status unknown, submitting fresh");
                        tracing::info!(
                            count = batch_items.len(),
                            "Submitting HyDE batch to Claude API"
                        );
                        let id = client.submit_hyde_batch(&batch_items, HYDE_MAX_TOKENS)?;
                        if let Err(e) = store.set_pending_hyde_batch_id(Some(&id)) {
                            tracing::warn!(error = %e, "Failed to store pending HyDE batch ID");
                        }
                        tracing::info!(batch_id = %id, "HyDE batch submitted, waiting for results");
                        id
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read pending HyDE batch ID");
                tracing::info!(
                    count = batch_items.len(),
                    "Submitting HyDE batch to Claude API"
                );
                let id = client.submit_hyde_batch(&batch_items, HYDE_MAX_TOKENS)?;
                if let Err(e) = store.set_pending_hyde_batch_id(Some(&id)) {
                    tracing::warn!(error = %e, "Failed to store pending HyDE batch ID");
                }
                tracing::info!(batch_id = %id, "HyDE batch submitted, waiting for results");
                id
            }
            _ => {
                tracing::info!(
                    count = batch_items.len(),
                    "Submitting HyDE batch to Claude API"
                );
                let id = client.submit_hyde_batch(&batch_items, HYDE_MAX_TOKENS)?;
                if let Err(e) = store.set_pending_hyde_batch_id(Some(&id)) {
                    tracing::warn!(error = %e, "Failed to store pending HyDE batch ID");
                }
                tracing::info!(batch_id = %id, "HyDE batch submitted, waiting for results");
                id
            }
        };

        resume_or_fetch_hyde_batch(&client, store, &batch_id, quiet)?
    };

    tracing::info!(api_generated, cached, skipped, "HyDE query pass complete");

    Ok(api_generated)
}
