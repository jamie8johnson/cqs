//! HyDE (Hypothetical Document Embeddings) query prediction pass.

use super::batch::BatchPhase2;
use super::{collect_eligible_chunks, LlmConfig, LlmError, MAX_BATCH_SIZE, MAX_CONTENT_CHARS};
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
    lock_dir: Option<&std::path::Path>,
) -> Result<usize, LlmError> {
    let _span = tracing::info_span!("hyde_query_pass").entered();

    let llm_config = LlmConfig::resolve(config);
    tracing::debug!(
        api_base = %llm_config.api_base,
        "LLM API base"
    );
    tracing::info!(
        model = %llm_config.model,
        "HyDE query pass starting"
    );

    let hyde_max_tokens = llm_config.hyde_max_tokens;
    let client = super::create_client(llm_config)?;

    let effective_batch_size = if max_hyde > 0 {
        max_hyde.min(MAX_BATCH_SIZE)
    } else {
        MAX_BATCH_SIZE
    };

    // Phase 1: Collect callable chunks needing HyDE predictions via shared filter
    let (eligible, cached, skipped) = collect_eligible_chunks(store, "hyde", effective_batch_size)?;

    // Build batch items: (content_hash, truncated_content, signature, language)
    let batch_items: Vec<super::provider::BatchSubmitItem> = eligible
        .into_iter()
        .map(|ec| {
            let content = if ec.content.len() > MAX_CONTENT_CHARS {
                ec.content[..ec.content.floor_char_boundary(MAX_CONTENT_CHARS)].to_string()
            } else {
                ec.content
            };
            super::provider::BatchSubmitItem {
                custom_id: ec.content_hash,
                content,
                context: ec.signature,
                language: ec.language,
            }
        })
        .collect();
    if batch_items.len() >= effective_batch_size {
        tracing::info!(
            max = effective_batch_size,
            "HyDE batch size limit reached, submitting partial batch"
        );
    }

    tracing::info!(
        cached,
        skipped,
        api_needed = batch_items.len(),
        "HyDE scan complete"
    );

    // Phase 2: Submit batch to Claude API (or resume a pending one)
    let phase2 = BatchPhase2 {
        purpose: "hyde",
        max_tokens: hyde_max_tokens,
        quiet,
        lock_dir,
    };
    let api_results = phase2.submit_or_resume(
        &client,
        store,
        &batch_items,
        &|s| s.get_pending_hyde_batch_id(),
        &|s, id| s.set_pending_hyde_batch_id(id),
        &|c, items, max_tok| c.submit_hyde_batch(items, max_tok),
    )?;
    let api_generated = api_results.len();

    tracing::info!(api_generated, cached, skipped, "HyDE query pass complete");

    Ok(api_generated)
}
