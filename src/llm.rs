//! Claude API client for LLM-generated function summaries (SQ-6).
//!
//! Uses `reqwest::blocking` to avoid nested tokio runtime issues
//! (the Store already uses `rt.block_on()`).
//!
//! The summary pass uses the Batches API for throughput (no RPM limit, 50% discount).
//! Individual summarize_chunk() is available for single-chunk fallback.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::Store;

const API_BASE: &str = "https://api.anthropic.com/v1";
const API_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-haiku-4-5";
const MAX_TOKENS: u32 = 100;
const MAX_CONTENT_CHARS: usize = 8000;
const MIN_CONTENT_CHARS: usize = 50;
/// Poll interval for batch completion
const BATCH_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Claude API client for generating summaries.
pub struct Client {
    http: reqwest::blocking::Client,
    api_key: String,
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
    pub fn new(api_key: &str) -> Self {
        Self {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("Failed to create HTTP client"),
            api_key: api_key.to_string(),
        }
    }

    /// Build the prompt for a code chunk.
    fn build_prompt(content: &str, chunk_type: &str, language: &str) -> String {
        let truncated = if content.len() > MAX_CONTENT_CHARS {
            &content[..MAX_CONTENT_CHARS]
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
    fn submit_batch(&self, items: &[(String, String, String, String)]) -> Result<String> {
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
            .send()
            .context("Failed to submit batch")?;

        let status = response.status();
        if status == 401 {
            bail!("Invalid ANTHROPIC_API_KEY (401 Unauthorized)");
        }
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<ApiError>(&body) {
                bail!("Batch submission failed: {}", err.error.message);
            }
            bail!("Batch submission failed: HTTP {status}: {body}");
        }

        let batch: BatchResponse = response.json().context("Failed to parse batch response")?;
        tracing::info!(batch_id = %batch.id, count = items.len(), "Batch submitted");
        Ok(batch.id)
    }

    /// Check the current status of a batch without polling.
    fn check_batch_status(&self, batch_id: &str) -> Result<String> {
        let url = format!("{}/messages/batches/{}", API_BASE, batch_id);
        let response = self
            .http
            .get(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .send()
            .context("Failed to check batch status")?;

        if !response.status().is_success() {
            let body = response.text().unwrap_or_default();
            bail!("Batch status check failed: {body}");
        }

        let batch: BatchResponse = response.json().context("Failed to parse batch status")?;
        Ok(batch.processing_status)
    }

    /// Poll until a batch completes. Returns when status is "ended".
    fn wait_for_batch(&self, batch_id: &str, quiet: bool) -> Result<()> {
        let url = format!("{}/messages/batches/{}", API_BASE, batch_id);
        loop {
            let response = self
                .http
                .get(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .send()
                .context("Failed to poll batch status")?;

            if !response.status().is_success() {
                let body = response.text().unwrap_or_default();
                bail!("Batch status check failed: {body}");
            }

            let batch: BatchResponse = response.json().context("Failed to parse batch status")?;

            match batch.processing_status.as_str() {
                "ended" => {
                    tracing::info!(batch_id, "Batch complete");
                    return Ok(());
                }
                "canceling" | "canceled" | "expired" => {
                    bail!(
                        "Batch {} ended with status: {}",
                        batch_id,
                        batch.processing_status
                    );
                }
                _ => {
                    // "in_progress" or "created"
                    if !quiet {
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
    fn fetch_batch_results(&self, batch_id: &str) -> Result<HashMap<String, String>> {
        let url = format!("{}/messages/batches/{}/results", API_BASE, batch_id);
        let response = self
            .http
            .get(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .send()
            .context("Failed to fetch batch results")?;

        if !response.status().is_success() {
            let body = response.text().unwrap_or_default();
            bail!("Batch results fetch failed: {body}");
        }

        // Results are JSONL (one JSON object per line)
        let body = response
            .text()
            .context("Failed to read batch results body")?;
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
) -> Result<usize> {
    client
        .wait_for_batch(batch_id, quiet)
        .context("Batch processing failed")?;

    if !quiet {
        eprintln!();
    }

    let results = client
        .fetch_batch_results(batch_id)
        .context("Failed to fetch batch results")?;

    // Store API-generated summaries
    let api_summaries: Vec<(String, String, String)> = results
        .into_iter()
        .map(|(hash, summary)| (hash, summary, MODEL.to_string()))
        .collect();
    let count = api_summaries.len();
    if !api_summaries.is_empty() {
        store
            .upsert_summaries_batch(&api_summaries)
            .context("Failed to store API summaries")?;
    }

    // Clear pending batch marker
    store.set_pending_batch_id(None).ok();

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
pub fn llm_summary_pass(store: &Store, quiet: bool) -> Result<usize> {
    let _span = tracing::info_span!("llm_summary_pass").entered();

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .context("--llm-summaries requires ANTHROPIC_API_KEY environment variable")?;
    let client = Client::new(&api_key);

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

    let stats = store.stats().context("Failed to get index stats")?;
    if !quiet {
        eprintln!(
            "Scanning {} chunks for LLM summaries...",
            stats.total_chunks
        );
    }

    loop {
        let (chunks, next) = store
            .chunks_paged(cursor, PAGE_SIZE)
            .context("Failed to page chunks")?;
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
        let existing = store
            .get_summaries_by_hashes(&hashes)
            .context("Failed to fetch existing summaries")?;

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
            }
        }
    }

    // Store doc-comment summaries immediately
    if !to_store.is_empty() {
        store
            .upsert_summaries_batch(&to_store)
            .context("Failed to store doc-comment summaries")?;
    }

    if !quiet {
        eprintln!(
            "  {} cached, {} from doc comments, {} skipped, {} need API calls",
            cached,
            doc_extracted,
            skipped,
            batch_items.len()
        );
    }

    // Phase 2: Submit batch to Claude API (or resume a pending one)
    let api_generated = if batch_items.is_empty() {
        // No new items needed, but check if a previous batch is still pending
        if let Ok(Some(pending)) = store.get_pending_batch_id() {
            if !quiet {
                eprintln!("Resuming pending batch {}", pending);
            }
            resume_or_fetch_batch(&client, store, &pending, quiet)?
        } else {
            0
        }
    } else {
        // Check for a pending batch from a previous interrupted run
        let batch_id = if let Ok(Some(pending)) = store.get_pending_batch_id() {
            // Verify it's still valid (not expired/canceled)
            if !quiet {
                eprint!("Found pending batch {}, checking status...", pending);
            }
            match client.check_batch_status(&pending) {
                Ok(status) if status == "in_progress" || status == "finalizing" => {
                    if !quiet {
                        eprint!(" still processing, resuming\nWaiting for results");
                    }
                    pending
                }
                Ok(status) if status == "ended" => {
                    if !quiet {
                        eprintln!(" completed, fetching results");
                    }
                    pending
                }
                _ => {
                    // Stale/failed batch — submit fresh
                    if !quiet {
                        eprintln!(" stale, submitting new batch");
                        eprint!("Submitting batch of {} to Claude API", batch_items.len());
                    }
                    let id = client
                        .submit_batch(&batch_items)
                        .context("Failed to submit summary batch")?;
                    store.set_pending_batch_id(Some(&id)).ok();
                    if !quiet {
                        eprint!(" (batch {})\nWaiting for results", id);
                    }
                    id
                }
            }
        } else {
            if !quiet {
                eprint!("Submitting batch of {} to Claude API", batch_items.len());
            }
            let id = client
                .submit_batch(&batch_items)
                .context("Failed to submit summary batch")?;
            store.set_pending_batch_id(Some(&id)).ok();
            if !quiet {
                eprint!(" (batch {})\nWaiting for results", id);
            }
            id
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
    if !quiet {
        eprintln!(
            "LLM summaries: {} from API, {} from doc comments, {} cached, {} skipped",
            api_generated, doc_extracted, cached, skipped
        );
    }

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
}
