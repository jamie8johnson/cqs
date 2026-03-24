//! LLM summary pass orchestration — collects chunks, submits batches, stores results.

use super::batch::resume_or_fetch_batch;
use super::{Client, LlmConfig, LlmError, MAX_BATCH_SIZE, MAX_CONTENT_CHARS, MIN_CONTENT_CHARS};
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

    let mut doc_extracted = 0usize;
    let mut cached = 0usize;
    let mut skipped = 0usize;
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;

    // Phase 1: Collect chunks needing summaries
    // Store doc-comment summaries immediately, collect API-needing chunks
    let mut to_store: Vec<(String, String, String, String)> = Vec::new();
    // (custom_id=content_hash, content, chunk_type, language) for batch API
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

            // Doc comment shortcut
            if let Some(ref doc) = cs.doc {
                if doc.len() > 10 {
                    let first_sentence = extract_first_sentence(doc);
                    if !first_sentence.is_empty() {
                        to_store.push((
                            cs.content_hash.clone(),
                            first_sentence,
                            "doc-comment".to_string(),
                            "summary".to_string(),
                        ));
                        doc_extracted += 1;
                        continue;
                    }
                }
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

    // Store doc-comment summaries immediately
    if !to_store.is_empty() {
        store.upsert_summaries_batch(&to_store)?;
    }

    tracing::info!(
        cached,
        doc_extracted,
        skipped,
        api_needed = batch_items.len(),
        "Summary scan complete"
    );

    // Phase 2: Submit batch to Claude API (or resume a pending one)
    let api_generated = if batch_items.is_empty() {
        // No new items needed, but check if a previous batch is still pending
        match store.get_pending_batch_id() {
            Ok(Some(pending)) => {
                tracing::info!(batch_id = %pending, "Resuming pending batch");
                let count = resume_or_fetch_batch(&client, store, &pending, quiet)?;
                tracing::info!(
                    count,
                    "Fetched pending batch results — new chunks will be processed on next run"
                );
                count
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read pending batch ID");
                0
            }
            _ => 0,
        }
    } else {
        // Check for a pending batch from a previous interrupted run
        let batch_id = match store.get_pending_batch_id() {
            Ok(Some(pending)) => {
                // Verify it's still valid (not expired/canceled)
                tracing::info!(batch_id = %pending, "Found pending batch, checking status");
                match client.check_batch_status(&pending) {
                    Ok(status) if status == "in_progress" || status == "finalizing" => {
                        tracing::info!(batch_id = %pending, status = %status, "Pending batch still processing, resuming");
                        pending
                    }
                    Ok(status) if status == "created" => {
                        // Batch queued but not started yet — wait for it
                        tracing::info!(batch_id = %pending, "Pending batch still queued, waiting");
                        pending
                    }
                    Ok(status) if status == "ended" => {
                        tracing::info!(batch_id = %pending, "Pending batch completed, fetching results");
                        pending
                    }
                    _ => {
                        tracing::warn!(old_batch = %pending, "Pending batch status unknown, submitting fresh — old batch results may be lost");
                        tracing::info!(count = batch_items.len(), "Submitting batch to Claude API");
                        let id = client.submit_batch(&batch_items, client.llm_config.max_tokens)?;
                        if let Err(e) = store.set_pending_batch_id(Some(&id)) {
                            tracing::warn!(error = %e, "Failed to store pending batch ID");
                        }
                        tracing::info!(batch_id = %id, "Batch submitted, waiting for results");
                        id
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read pending batch ID");
                tracing::info!(count = batch_items.len(), "Submitting batch to Claude API");
                let id = client.submit_batch(&batch_items, client.llm_config.max_tokens)?;
                if let Err(e) = store.set_pending_batch_id(Some(&id)) {
                    tracing::warn!(error = %e, "Failed to store pending batch ID");
                }
                tracing::info!(batch_id = %id, "Batch submitted, waiting for results");
                id
            }
            _ => {
                tracing::info!(count = batch_items.len(), "Submitting batch to Claude API");
                let id = client.submit_batch(&batch_items, client.llm_config.max_tokens)?;
                if let Err(e) = store.set_pending_batch_id(Some(&id)) {
                    tracing::warn!(error = %e, "Failed to store pending batch ID");
                }
                tracing::info!(batch_id = %id, "Batch submitted, waiting for results");
                id
            }
        };

        resume_or_fetch_batch(&client, store, &batch_id, quiet)?
    };

    tracing::info!(
        api_generated,
        doc_extracted,
        cached,
        skipped,
        "LLM summary pass complete"
    );

    Ok(api_generated + doc_extracted)
}

/// Extract the first sentence from a doc comment.
fn extract_first_sentence(doc: &str) -> String {
    let trimmed = doc.trim();
    if let Some(pos) = trimmed.find(['.', '!', '?']) {
        let sentence = trimmed[..=pos].trim();
        if sentence.len() > 10 {
            return sentence.to_string();
        }
    }
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    if first_line.len() > 10 {
        first_line.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_first_sentence_period() {
        assert_eq!(
            extract_first_sentence("Parse a config file. Returns validated settings."),
            "Parse a config file."
        );
    }

    #[test]
    fn test_extract_first_sentence_no_period() {
        assert_eq!(
            extract_first_sentence("Parse a config file and return settings"),
            "Parse a config file and return settings"
        );
    }

    #[test]
    fn test_extract_first_sentence_short() {
        assert_eq!(extract_first_sentence("Hi."), "");
    }

    #[test]
    fn test_extract_first_sentence_multiline() {
        assert_eq!(
            extract_first_sentence("Parse a config file.\n\nThis handles TOML and JSON."),
            "Parse a config file."
        );
    }

    #[test]
    fn extract_first_sentence_url_with_period() {
        // URL period — cuts at first period in domain (known behavior, not a bug)
        let r = extract_first_sentence("See https://example.com. Usage guide.");
        assert_eq!(r, "See https://example.");
    }

    #[test]
    fn extract_first_sentence_short_falls_to_line() {
        // "Short." is 6 chars <=10, falls to first line
        let r = extract_first_sentence("Short. More text here.");
        assert_eq!(r, "Short. More text here.");
    }

    #[test]
    fn extract_first_sentence_exclamation() {
        let r = extract_first_sentence("This is great! More.");
        assert_eq!(r, "This is great!");
    }

    #[test]
    fn extract_first_sentence_question() {
        let r = extract_first_sentence("Is this working? Yes.");
        assert_eq!(r, "Is this working?");
    }

    #[test]
    fn extract_first_sentence_whitespace_only() {
        assert_eq!(extract_first_sentence("   \n  \t  "), "");
    }

    #[test]
    fn extract_first_sentence_empty_input() {
        assert_eq!(extract_first_sentence(""), "");
    }

    #[test]
    fn extract_first_sentence_boundary_11_chars() {
        assert_eq!(extract_first_sentence("1234567890."), "1234567890.");
    }

    #[test]
    fn extract_first_sentence_short_multiline() {
        // Both sentence and first line too short
        assert_eq!(extract_first_sentence("OK.\nMore"), "");
    }
}
