//! HyDE (Hypothetical Document Embeddings) query prediction pass.

use super::batch::BatchPhase2;
use super::{collect_eligible_chunks, max_content_chars, LlmConfig, LlmError};
use crate::Store;

/// Run the HyDE query prediction pass using the Batches API.
/// Scans all callable chunks, submits them as a batch to Claude for query prediction,
/// polls for completion, then stores results with purpose="hyde".
/// Returns the number of new HyDE predictions generated.
pub fn hyde_query_pass(
    store: &Store,
    quiet: bool,
    config: &crate::config::Config,
    max_hyde: usize,
    lock_dir: Option<&std::path::Path>,
) -> Result<usize, LlmError> {
    let _span = tracing::info_span!("hyde_query_pass").entered();

    let llm_config = LlmConfig::resolve(config)?;
    tracing::debug!(
        api_base = %llm_config.api_base,
        "LLM API base"
    );
    tracing::info!(
        model = %llm_config.model,
        "HyDE query pass starting"
    );

    let hyde_max_tokens = llm_config.hyde_max_tokens;
    let model_name = llm_config.model.clone();
    let mut client = super::create_client(llm_config)?;

    // LocalProvider: stream per-item persist so Ctrl-C mid-batch doesn't
    // lose completed work. The Anthropic path's default no-op ignores this.
    client.set_on_item_complete(store.stream_summary_writer(model_name, "hyde".to_string()));

    let max_batch_size = crate::limits::llm_max_batch_size();
    let effective_batch_size = if max_hyde > 0 {
        max_hyde.min(max_batch_size)
    } else {
        max_batch_size
    };

    // Phase 1: Collect callable chunks needing HyDE predictions via shared filter
    let (eligible, cached, skipped) = collect_eligible_chunks(store, "hyde", effective_batch_size)?;

    // Build batch items: (content_hash, truncated_content, signature, language)
    let batch_items: Vec<super::provider::BatchSubmitItem> = eligible
        .into_iter()
        .map(|ec| {
            let content = if ec.content.len() > max_content_chars() {
                ec.content[..ec.content.floor_char_boundary(max_content_chars())].to_string()
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
    let result = phase2.submit_or_resume(
        client.as_ref(),
        store,
        &batch_items,
        &|s| s.get_pending_hyde_batch_id(),
        &|s, id| s.set_pending_hyde_batch_id(id),
        &|c, items, max_tok| c.submit_batch(crate::llm::provider::BatchKind::Hyde, items, max_tok),
    );

    // #1126 / P2.60: drain the per-Store summary queue regardless of
    // success/failure. Streamed HyDE rows are buffered in-memory; the
    // final flush narrows the re-fetch window and is idempotent.
    if let Err(e) = store.flush_pending_summaries() {
        tracing::warn!(error = %e, "final flush of summary queue failed; rows retained for next run");
    }

    let api_results = result?;
    let api_generated = api_results.len();

    tracing::info!(api_generated, cached, skipped, "HyDE query pass complete");

    Ok(api_generated)
}

// P2.87: TC-HAP — empty-store happy-path pin for `hyde_query_pass`.
//
// `hyde_query_pass` shipped with zero tests. The full happy path needs a
// running LLM endpoint (Anthropic Batches or local provider) with an
// httpmock-backed fixture, which the existing `local_provider_integration`
// suite exercises for `llm_summary_pass`. The minimal pin we add here is
// the "no eligible chunks" path: with an empty store, `collect_eligible_chunks`
// returns no items, `submit_or_resume` short-circuits before any HTTP, and
// the function returns `Ok(0)`. A regression that, e.g., started making
// an API call before checking the eligibility list would surface here as
// a connection-refused error.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::{ModelInfo, Store};

    // #1312 / #1305: HYDE_ENV_LOCK was a file-local Mutex; replaced by the
    // module-wide `crate::llm::LLM_ENV_LOCK` so this test serializes
    // against `doc_comments::tests` and any other future caller that
    // mutates the shared `CQS_LLM_*` env vars.

    /// Build an empty store with the canonical `ModelInfo::default()` so
    /// `init` succeeds and the dim/model metadata is in place. Returns
    /// `(tempdir, Store)` — the tempdir must outlive the store.
    fn empty_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("index.db");
        let store = Store::open(&path).expect("open store");
        store.init(&ModelInfo::default()).expect("init store");
        (dir, store)
    }

    /// P2.87: empty store → zero candidates → `submit_or_resume` short-
    /// circuits to `Ok(HashMap::new())` → caller returns `Ok(0)`.
    /// Local-provider config so no real API key / network is touched.
    #[test]
    fn hyde_query_pass_returns_zero_for_empty_store() {
        let _g = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Save / restore env so we don't poison sibling tests.
        let prev_provider = std::env::var("CQS_LLM_PROVIDER").ok();
        let prev_api_base = std::env::var("CQS_LLM_API_BASE").ok();
        let prev_model = std::env::var("CQS_LLM_MODEL").ok();
        let prev_allow = std::env::var("CQS_LLM_ALLOW_INSECURE").ok();

        std::env::set_var("CQS_LLM_PROVIDER", "local");
        // Plausible URL — never contacted because batch_items is empty
        // and there's no pending batch in the store.
        std::env::set_var("CQS_LLM_API_BASE", "http://127.0.0.1:1/v1");
        std::env::set_var("CQS_LLM_MODEL", "test-model");
        std::env::set_var("CQS_LLM_ALLOW_INSECURE", "1");

        let (_tmp, store) = empty_store();
        let config = Config::default();
        let result = hyde_query_pass(&store, true, &config, 0, None);

        // Restore env.
        match prev_provider {
            Some(v) => std::env::set_var("CQS_LLM_PROVIDER", v),
            None => std::env::remove_var("CQS_LLM_PROVIDER"),
        }
        match prev_api_base {
            Some(v) => std::env::set_var("CQS_LLM_API_BASE", v),
            None => std::env::remove_var("CQS_LLM_API_BASE"),
        }
        match prev_model {
            Some(v) => std::env::set_var("CQS_LLM_MODEL", v),
            None => std::env::remove_var("CQS_LLM_MODEL"),
        }
        match prev_allow {
            Some(v) => std::env::set_var("CQS_LLM_ALLOW_INSECURE", v),
            None => std::env::remove_var("CQS_LLM_ALLOW_INSECURE"),
        }

        let n = result.expect("hyde_query_pass on empty store must not error");
        assert_eq!(
            n, 0,
            "empty store must yield zero new HyDE predictions (no API call made)"
        );
    }
}
