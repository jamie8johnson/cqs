//! Claude API client for LLM-generated function summaries (SQ-6).
//!
//! Uses `reqwest::blocking` to avoid nested tokio runtime issues
//! (the Store already uses `rt.block_on()`).
//!
//! The summary pass uses the Batches API for throughput (no RPM limit, 50% discount).
//! Individual summarize_chunk() is available for single-chunk fallback.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Store;

/// Typed error for LLM operations (EH-14).
///
/// CLI callers convert to `anyhow::Error` at the boundary via the blanket `From`.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("API key missing: {0}")]
    ApiKeyMissing(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("Batch failed: {0}")]
    BatchFailed(String),
    #[error("Invalid batch ID: {0}")]
    InvalidBatchId(String),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Store error: {0}")]
    Store(#[from] crate::store::StoreError),
}

const API_BASE: &str = "https://api.anthropic.com/v1";
const API_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-haiku-4-5";
const MAX_TOKENS: u32 = 100;
const MAX_CONTENT_CHARS: usize = 8000;
const MIN_CONTENT_CHARS: usize = 50;
const MAX_BATCH_SIZE: usize = 10_000;
/// Poll interval for batch completion
const BATCH_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Claude API client for generating summaries.
pub struct Client {
    http: reqwest::blocking::Client,
    api_key: String,
}

fn is_valid_batch_id(id: &str) -> bool {
    id.starts_with("msgbatch_")
        && id.len() < 100
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

// --- Messages API types ---

#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ChatMessage>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
}

// --- Batches API types ---

#[derive(Serialize)]
struct BatchRequest {
    requests: Vec<BatchItem>,
}

#[derive(Serialize)]
struct BatchItem {
    custom_id: String,
    params: MessagesRequest,
}

#[derive(Deserialize)]
struct BatchResponse {
    id: String,
    processing_status: String,
}

#[derive(Deserialize)]
struct BatchResult {
    custom_id: String,
    result: BatchResultInner,
}

#[derive(Deserialize)]
struct BatchResultInner {
    #[serde(rename = "type")]
    result_type: String,
    message: Option<MessagesResponse>,
}

#[derive(Deserialize)]
struct ApiError {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    message: String,
}

impl Client {
    pub fn new(api_key: &str) -> Result<Self, LlmError> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            api_key: api_key.to_string(),
        })
    }

    /// Build the prompt for a code chunk.
    fn build_prompt(content: &str, chunk_type: &str, language: &str) -> String {
        let truncated = if content.len() > MAX_CONTENT_CHARS {
            &content[..content.floor_char_boundary(MAX_CONTENT_CHARS)]
        } else {
            content
        };
        format!(
            "Summarize this {} in one sentence. Focus on what it does, not how.\n\n```{}\n{}\n```",
            chunk_type, language, truncated
        )
    }

    /// Submit a batch of summary requests to the Batches API.
    ///
    /// `items` is a list of (custom_id, content, chunk_type, language).
    /// Returns the batch ID for polling.
    fn submit_batch(&self, items: &[(String, String, String, String)]) -> Result<String, LlmError> {
        let requests: Vec<BatchItem> = items
            .iter()
            .map(|(id, content, chunk_type, language)| BatchItem {
                custom_id: id.clone(),
                params: MessagesRequest {
                    model: MODEL.to_string(),
                    max_tokens: MAX_TOKENS,
                    messages: vec![ChatMessage {
                        role: "user".to_string(),
                        content: Self::build_prompt(content, chunk_type, language),
                    }],
                },
            })
            .collect();

        let url = format!("{}/messages/batches", API_BASE);
        let response = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&BatchRequest { requests })
            .send()?;

        let status = response.status();
        if status == 401 {
            return Err(LlmError::Api {
                status: 401,
                message: "Invalid ANTHROPIC_API_KEY (401 Unauthorized)".to_string(),
            });
        }
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            let message = serde_json::from_str::<ApiError>(&body)
                .map(|err| format!("Batch submission failed: {}", err.error.message))
                .unwrap_or_else(|_| format!("Batch submission failed: HTTP {status}: {body}"));
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let batch: BatchResponse = response.json()?;
        tracing::info!(batch_id = %batch.id, count = items.len(), "Batch submitted");
        Ok(batch.id)
    }

    /// Check the current status of a batch without polling.
    fn check_batch_status(&self, batch_id: &str) -> Result<String, LlmError> {
        if !is_valid_batch_id(batch_id) {
            return Err(LlmError::InvalidBatchId(batch_id.to_string()));
        }
        let url = format!("{}/messages/batches/{}", API_BASE, batch_id);
        let response = self
            .http
            .get(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .send()?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            return Err(LlmError::Api {
                status,
                message: format!("Batch status check failed: {body}"),
            });
        }

        let batch: BatchResponse = response.json()?;
        Ok(batch.processing_status)
    }

    /// Poll until a batch completes. Returns when status is "ended".
    fn wait_for_batch(&self, batch_id: &str, quiet: bool) -> Result<(), LlmError> {
        if !is_valid_batch_id(batch_id) {
            return Err(LlmError::InvalidBatchId(batch_id.to_string()));
        }
        let url = format!("{}/messages/batches/{}", API_BASE, batch_id);
        loop {
            let response = self
                .http
                .get(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .send()?;

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body = response.text().unwrap_or_default();
                return Err(LlmError::Api {
                    status,
                    message: format!("Batch status check failed: {body}"),
                });
            }

            let batch: BatchResponse = response.json()?;

            match batch.processing_status.as_str() {
                "ended" => {
                    tracing::info!(batch_id, "Batch complete");
                    return Ok(());
                }
                "canceling" | "canceled" | "expired" => {
                    return Err(LlmError::BatchFailed(format!(
                        "Batch {} ended with status: {}",
                        batch_id, batch.processing_status
                    )));
                }
                _ => {
                    if !quiet {
                        // Progress dot — tracing has no equivalent for inline progress
                        eprint!(".");
                    }
                    tracing::debug!(batch_id, status = %batch.processing_status, "Batch still processing");
                    std::thread::sleep(BATCH_POLL_INTERVAL);
                }
            }
        }
    }

    /// Fetch results from a completed batch.
    ///
    /// Returns a map from custom_id to summary text.
    fn fetch_batch_results(&self, batch_id: &str) -> Result<HashMap<String, String>, LlmError> {
        if !is_valid_batch_id(batch_id) {
            return Err(LlmError::InvalidBatchId(batch_id.to_string()));
        }
        let url = format!("{}/messages/batches/{}/results", API_BASE, batch_id);
        let response = self
            .http
            .get(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .send()?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            return Err(LlmError::Api {
                status,
                message: format!("Batch results fetch failed: {body}"),
            });
        }

        // Results are JSONL (one JSON object per line)
        let body = response.text()?;
        let mut results = HashMap::new();

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<BatchResult>(line) {
                Ok(result) => {
                    if result.result.result_type == "succeeded" {
                        if let Some(msg) = result.result.message {
                            let text = msg
                                .content
                                .into_iter()
                                .find(|b| b.block_type == "text")
                                .and_then(|b| b.text);
                            if let Some(s) = text {
                                let trimmed = s.trim().to_string();
                                if !trimmed.is_empty() && trimmed.len() < 500 {
                                    results.insert(result.custom_id, trimmed);
                                }
                            }
                        }
                    } else {
                        tracing::warn!(
                            custom_id = %result.custom_id,
                            result_type = %result.result.result_type,
                            "Batch item not succeeded"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to parse batch result line");
                }
            }
        }

        tracing::info!(batch_id, succeeded = results.len(), "Batch results fetched");
        Ok(results)
    }
}

/// Wait for a batch to complete, fetch results, store them, and clear the pending marker.
fn resume_or_fetch_batch(
    client: &Client,
    store: &Store,
    batch_id: &str,
    quiet: bool,
) -> Result<usize, LlmError> {
    client.wait_for_batch(batch_id, quiet)?;

    if !quiet {
        // Newline after progress dots
        eprintln!();
    }

    let results = client.fetch_batch_results(batch_id)?;

    // Store API-generated summaries
    let api_summaries: Vec<(String, String, String)> = results
        .into_iter()
        .map(|(hash, summary)| (hash, summary, MODEL.to_string()))
        .collect();
    let count = api_summaries.len();
    if !api_summaries.is_empty() {
        store.upsert_summaries_batch(&api_summaries)?;
    }

    // Clear pending batch marker
    if let Err(e) = store.set_pending_batch_id(None) {
        tracing::warn!(error = %e, "Failed to clear pending batch ID");
    }

    Ok(count)
}

/// A summary entry ready for storage.
pub struct SummaryEntry {
    pub content_hash: String,
    pub summary: String,
    pub model: String,
}

/// Run the LLM summary pass using the Batches API.
///
/// Collects all uncached callable chunks, submits them as a batch to Claude,
/// polls for completion, then stores results. Doc comments are extracted locally
/// without API calls.
///
/// Returns the number of new summaries generated.
pub fn llm_summary_pass(store: &Store, quiet: bool) -> Result<usize, LlmError> {
    let _span = tracing::info_span!("llm_summary_pass").entered();

    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        LlmError::ApiKeyMissing(
            "--llm-summaries requires ANTHROPIC_API_KEY environment variable".to_string(),
        )
    })?;
    let client = Client::new(&api_key)?;

    let mut doc_extracted = 0usize;
    let mut cached = 0usize;
    let mut skipped = 0usize;
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;

    // Phase 1: Collect chunks needing summaries
    // Store doc-comment summaries immediately, collect API-needing chunks
    let mut to_store: Vec<(String, String, String)> = Vec::new();
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
        let existing = store.get_summaries_by_hashes(&hashes)?;

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
                    cs.content.clone(),
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
                        let id = client.submit_batch(&batch_items)?;
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
                let id = client.submit_batch(&batch_items)?;
                if let Err(e) = store.set_pending_batch_id(Some(&id)) {
                    tracing::warn!(error = %e, "Failed to store pending batch ID");
                }
                tracing::info!(batch_id = %id, "Batch submitted, waiting for results");
                id
            }
            _ => {
                tracing::info!(count = batch_items.len(), "Submitting batch to Claude API");
                let id = client.submit_batch(&batch_items)?;
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
    fn test_build_prompt() {
        let prompt = Client::build_prompt("fn foo() {}", "function", "rust");
        assert!(prompt.contains("function"));
        assert!(prompt.contains("```rust"));
        assert!(prompt.contains("fn foo()"));
    }

    #[test]
    fn test_build_prompt_truncation() {
        let long = "x".repeat(10000);
        let prompt = Client::build_prompt(&long, "function", "rust");
        // Prompt should contain truncated content
        assert!(prompt.len() < 10000 + 200); // prompt overhead + truncated
    }

    #[test]
    fn build_prompt_multibyte_no_panic() {
        let content: String = std::iter::repeat('あ').take(2667).collect();
        let prompt = Client::build_prompt(&content, "function", "rust");
        assert!(prompt.len() <= 8100);
    }

    #[test]
    fn is_valid_batch_id_accepts_real_ids() {
        assert!(is_valid_batch_id("msgbatch_abc123"));
        assert!(is_valid_batch_id("msgbatch_0123456789abcdef_ABCDEF"));
    }

    #[test]
    fn is_valid_batch_id_rejects_crafted() {
        assert!(!is_valid_batch_id("../../v1/complete"));
        assert!(!is_valid_batch_id("msgbatch_abc?redirect=evil.com"));
        assert!(!is_valid_batch_id(""));
        assert!(!is_valid_batch_id("not_a_batch"));
        assert!(!is_valid_batch_id(
            &("msgbatch_".to_string() + &"a".repeat(200))
        ));
    }
}
