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

use crate::store::ChunkSummary;
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

/// Resolved LLM configuration (env vars > config file > constants).
pub struct LlmConfig {
    pub api_base: String,
    pub model: String,
    pub max_tokens: u32,
}

impl LlmConfig {
    /// Resolve config with priority: env vars > config file > hardcoded constants.
    pub fn resolve(config: &crate::config::Config) -> Self {
        Self {
            api_base: std::env::var("CQS_API_BASE")
                .ok()
                .or_else(|| config.llm_api_base.clone())
                .unwrap_or_else(|| API_BASE.to_string()),
            model: std::env::var("CQS_LLM_MODEL")
                .ok()
                .or_else(|| config.llm_model.clone())
                .unwrap_or_else(|| MODEL.to_string()),
            max_tokens: std::env::var("CQS_LLM_MAX_TOKENS")
                .ok()
                .and_then(|s| s.parse().ok())
                .or(config.llm_max_tokens)
                .unwrap_or(MAX_TOKENS),
        }
    }
}

/// Claude API client for generating summaries.
pub struct Client {
    http: reqwest::blocking::Client,
    api_key: String,
    llm_config: LlmConfig,
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
    pub fn new(api_key: &str, llm_config: LlmConfig) -> Result<Self, LlmError> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            api_key: api_key.to_string(),
            llm_config,
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

    /// Build the prompt for generating a doc comment for a code chunk.
    ///
    /// Unlike `build_prompt` (one-sentence summary), this generates a full documentation
    /// comment with language-specific conventions (Rust `# Arguments`/`# Returns`, Python
    /// Google-style docstrings, Go function-name-first, etc.).
    fn build_doc_prompt(content: &str, chunk_type: &str, language: &str) -> String {
        let truncated = if content.len() > MAX_CONTENT_CHARS {
            &content[..content.floor_char_boundary(MAX_CONTENT_CHARS)]
        } else {
            content
        };

        let appendix = match language {
            "rust" => "\n\nUse `# Arguments`, `# Returns`, `# Errors`, `# Panics` sections as appropriate.",
            "python" => "\n\nFormat as a Google-style docstring (Args/Returns/Raises sections).",
            "go" => "\n\nStart with the function name per Go conventions.",
            _ => "",
        };

        format!(
            "Write a concise doc comment for this {}. \
             Describe what it does, its parameters, and return value. \
             Output only the doc text, no code fences or comment markers.{}\n\n\
             ```{}\n{}\n```",
            chunk_type, appendix, language, truncated
        )
    }

    /// Submit a batch of summary requests to the Batches API.
    ///
    /// `items` is a list of (custom_id, content, chunk_type, language).
    /// `max_tokens` controls the per-request token limit.
    /// Returns the batch ID for polling.
    fn submit_batch(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        let model = self.llm_config.model.clone();
        let requests: Vec<BatchItem> = items
            .iter()
            .map(|(id, content, chunk_type, language)| BatchItem {
                custom_id: id.clone(),
                params: MessagesRequest {
                    model: model.clone(),
                    max_tokens,
                    messages: vec![ChatMessage {
                        role: "user".to_string(),
                        content: Self::build_prompt(content, chunk_type, language),
                    }],
                },
            })
            .collect();

        let url = format!("{}/messages/batches", self.llm_config.api_base);
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
        let url = format!("{}/messages/batches/{}", self.llm_config.api_base, batch_id);
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
        let url = format!("{}/messages/batches/{}", self.llm_config.api_base, batch_id);
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
        let url = format!(
            "{}/messages/batches/{}/results",
            self.llm_config.api_base, batch_id
        );
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
                                if !trimmed.is_empty() {
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

    /// Submit a batch of doc-comment requests to the Batches API.
    ///
    /// Like `submit_batch` but uses `build_doc_prompt` instead of `build_prompt`.
    /// `items` is a list of (custom_id, content, chunk_type, language).
    /// Returns the batch ID for polling.
    fn submit_doc_batch(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        let model = self.llm_config.model.clone();
        let requests: Vec<BatchItem> = items
            .iter()
            .map(|(id, content, chunk_type, language)| BatchItem {
                custom_id: id.clone(),
                params: MessagesRequest {
                    model: model.clone(),
                    max_tokens,
                    messages: vec![ChatMessage {
                        role: "user".to_string(),
                        content: Self::build_doc_prompt(content, chunk_type, language),
                    }],
                },
            })
            .collect();

        let url = format!("{}/messages/batches", self.llm_config.api_base);
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
                .map(|err| format!("Doc batch submission failed: {}", err.error.message))
                .unwrap_or_else(|_| format!("Doc batch submission failed: HTTP {status}: {body}"));
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let batch: BatchResponse = response.json()?;
        tracing::info!(batch_id = %batch.id, count = items.len(), "Doc batch submitted");
        Ok(batch.id)
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
    let model = client.llm_config.model.clone();
    let api_summaries: Vec<(String, String, String, String)> = results
        .into_iter()
        .map(|(hash, summary)| (hash, summary, model.clone(), "summary".to_string()))
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

/// Signal words that indicate an intentional doc comment, even if short.
///
/// These words (case-insensitive) mark comments that carry meaningful safety,
/// maintenance, or deprecation signals. A short doc containing any of these
/// should be preserved rather than replaced by LLM-generated text.
const SIGNAL_WORDS: &[&str] = &[
    "SAFETY",
    "UNSAFE",
    "INVARIANT",
    "TODO",
    "FIXME",
    "HACK",
    "NOTE",
    "XXX",
    "BUG",
    "DEPRECATED",
    "SECURITY",
    "WARN",
];

/// Determine whether a chunk needs an LLM-generated doc comment.
///
/// Returns `true` when the chunk is a callable (function/method/property/macro),
/// is the first window (or not windowed), and has either no doc comment or a
/// "thin" doc (fewer than 30 characters with no signal words).
pub fn needs_doc_comment(chunk: &ChunkSummary) -> bool {
    // Only callable types get doc comments
    if !chunk.chunk_type.is_callable() {
        return false;
    }

    // Only first window (or non-windowed)
    if chunk.window_idx.is_some_and(|idx| idx > 0) {
        return false;
    }

    match &chunk.doc {
        None => true,
        Some(doc) => {
            let trimmed = doc.trim();
            if trimmed.is_empty() {
                return true;
            }
            // Adequate doc — no replacement needed
            if trimmed.len() >= 30 {
                return false;
            }
            // Thin doc — check for signal words before replacing
            let upper = trimmed.to_uppercase();
            !SIGNAL_WORDS.iter().any(|w| upper.contains(w))
        }
    }
}

/// Wait for a doc batch to complete, fetch results, store them, and clear the pending marker.
fn resume_or_fetch_doc_batch(
    client: &Client,
    store: &Store,
    batch_id: &str,
    quiet: bool,
) -> Result<HashMap<String, String>, LlmError> {
    client.wait_for_batch(batch_id, quiet)?;

    if !quiet {
        eprintln!();
    }

    let results = client.fetch_batch_results(batch_id)?;

    // Cache doc-comment results
    let model = client.llm_config.model.clone();
    let to_store: Vec<(String, String, String, String)> = results
        .iter()
        .map(|(hash, doc)| {
            (
                hash.clone(),
                doc.clone(),
                model.clone(),
                "doc-comment".to_string(),
            )
        })
        .collect();
    if !to_store.is_empty() {
        store.upsert_summaries_batch(&to_store)?;
    }

    // Clear pending doc batch marker
    if let Err(e) = store.set_pending_doc_batch_id(None) {
        tracing::warn!(error = %e, "Failed to clear pending doc batch ID");
    }

    Ok(results)
}

/// Run the LLM doc-comment generation pass using the Batches API.
///
/// Scans all indexed chunks, selects those needing doc comments (via `needs_doc_comment`),
/// checks the cache, submits uncached candidates as a batch to Claude with `build_doc_prompt`,
/// and returns the results. Cached results are returned without an API call.
///
/// `max_docs` limits how many functions to process (0 = unlimited).
pub fn doc_comment_pass(
    store: &Store,
    config: &crate::config::Config,
    max_docs: usize,
) -> Result<Vec<crate::doc_writer::DocCommentResult>, LlmError> {
    let _span = tracing::info_span!("doc_comment_pass").entered();

    let llm_config = LlmConfig::resolve(config);
    tracing::info!(
        model = %llm_config.model,
        api_base = %llm_config.api_base,
        "Doc comment pass starting"
    );

    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        LlmError::ApiKeyMissing(
            "--improve-docs requires ANTHROPIC_API_KEY environment variable".to_string(),
        )
    })?;
    let client = Client::new(&api_key, llm_config)?;

    // Phase 1: Collect candidates
    let mut candidates: Vec<ChunkSummary> = Vec::new();
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;

    let stats = store.stats()?;
    tracing::info!(
        chunks = stats.total_chunks,
        "Scanning for doc comment candidates"
    );

    loop {
        let (chunks, next) = store.chunks_paged(cursor, PAGE_SIZE)?;
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        for cs in chunks {
            if needs_doc_comment(&cs) {
                candidates.push(cs);
            }
        }
    }

    tracing::info!(
        candidates = candidates.len(),
        "Doc comment candidates found"
    );

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Check cache — filter out already-generated docs
    let hashes: Vec<&str> = candidates.iter().map(|c| c.content_hash.as_str()).collect();
    let cached = store.get_summaries_by_hashes(&hashes, "doc-comment")?;

    // Split into cached hits and uncached misses
    let mut cached_results: Vec<(&ChunkSummary, String)> = Vec::new();
    let mut uncached: Vec<&ChunkSummary> = Vec::new();

    for c in &candidates {
        if let Some(doc) = cached.get(&c.content_hash) {
            cached_results.push((c, doc.clone()));
        } else {
            uncached.push(c);
        }
    }

    tracing::info!(
        cached = cached_results.len(),
        uncached = uncached.len(),
        "Cache check complete"
    );

    // Sort: no doc first, then thin doc, by content length descending (meatier functions first)
    uncached.sort_by(|a, b| {
        let a_no_doc = a.doc.as_ref().is_none_or(|d| d.trim().is_empty());
        let b_no_doc = b.doc.as_ref().is_none_or(|d| d.trim().is_empty());
        // no-doc before thin-doc
        b_no_doc
            .cmp(&a_no_doc)
            .then_with(|| b.content.len().cmp(&a.content.len()))
    });

    // Apply max_docs cap (across cached + uncached)
    let total_available = cached_results.len() + uncached.len();
    let effective_cap = if max_docs == 0 {
        total_available
    } else {
        max_docs
    };

    // Cached results count toward the cap first
    let cached_to_use = cached_results.len().min(effective_cap);
    let uncached_cap = effective_cap.saturating_sub(cached_to_use);
    uncached.truncate(uncached_cap);

    // Phase 2: Submit batch for uncached candidates (or resume pending)
    let api_results: HashMap<String, String> = if uncached.is_empty() {
        // Check for pending batch from previous interrupted run
        match store.get_pending_doc_batch_id() {
            Ok(Some(pending)) => {
                tracing::info!(batch_id = %pending, "Resuming pending doc batch");
                resume_or_fetch_doc_batch(&client, store, &pending, false)?
            }
            _ => HashMap::new(),
        }
    } else {
        // Build batch items
        let mut batch_items: Vec<(String, String, String, String)> = Vec::new();
        let mut queued_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();

        for cs in &uncached {
            if queued_hashes.insert(cs.content_hash.clone()) {
                let content = if cs.content.len() > MAX_CONTENT_CHARS {
                    cs.content[..cs.content.floor_char_boundary(MAX_CONTENT_CHARS)].to_string()
                } else {
                    cs.content.clone()
                };
                batch_items.push((
                    cs.content_hash.clone(),
                    content,
                    cs.chunk_type.to_string(),
                    cs.language.to_string(),
                ));
                if batch_items.len() >= MAX_BATCH_SIZE {
                    break;
                }
            }
        }

        // Check for pending batch
        let batch_id = match store.get_pending_doc_batch_id() {
            Ok(Some(pending)) => {
                tracing::info!(batch_id = %pending, "Found pending doc batch, checking status");
                match client.check_batch_status(&pending) {
                    Ok(status)
                        if status == "in_progress"
                            || status == "finalizing"
                            || status == "created"
                            || status == "ended" =>
                    {
                        tracing::info!(batch_id = %pending, status = %status, "Resuming pending doc batch");
                        pending
                    }
                    _ => {
                        tracing::info!(
                            count = batch_items.len(),
                            "Submitting doc batch to Claude API"
                        );
                        let id = client.submit_doc_batch(&batch_items, 800)?;
                        if let Err(e) = store.set_pending_doc_batch_id(Some(&id)) {
                            tracing::warn!(error = %e, "Failed to store pending doc batch ID");
                        }
                        id
                    }
                }
            }
            _ => {
                tracing::info!(
                    count = batch_items.len(),
                    "Submitting doc batch to Claude API"
                );
                let id = client.submit_doc_batch(&batch_items, 800)?;
                if let Err(e) = store.set_pending_doc_batch_id(Some(&id)) {
                    tracing::warn!(error = %e, "Failed to store pending doc batch ID");
                }
                id
            }
        };

        resume_or_fetch_doc_batch(&client, store, &batch_id, false)?
    };

    // Phase 3: Build results from cached + API responses
    // Deduplicate by content_hash: multiple chunks can share the same hash
    // (windowed chunks, same function body). One doc comment per unique function.
    let mut results: Vec<crate::doc_writer::DocCommentResult> = Vec::new();
    let mut seen_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (cs, doc) in cached_results.iter().take(cached_to_use) {
        if !seen_hashes.insert(cs.content_hash.clone()) {
            continue;
        }
        results.push(crate::doc_writer::DocCommentResult {
            file: cs.file.clone(),
            function_name: cs.name.clone(),
            content_hash: cs.content_hash.clone(),
            generated_doc: doc.clone(),
            language: cs.language,
            line_start: cs.line_start as usize,
            had_existing_doc: cs.doc.as_ref().is_some_and(|d| !d.trim().is_empty()),
        });
    }

    for cs in &uncached {
        if seen_hashes.contains(&cs.content_hash) {
            continue;
        }
        if let Some(doc) = api_results.get(&cs.content_hash) {
            seen_hashes.insert(cs.content_hash.clone());
            results.push(crate::doc_writer::DocCommentResult {
                file: cs.file.clone(),
                function_name: cs.name.clone(),
                content_hash: cs.content_hash.clone(),
                generated_doc: doc.clone(),
                language: cs.language,
                line_start: cs.line_start as usize,
                had_existing_doc: cs.doc.as_ref().is_some_and(|d| !d.trim().is_empty()),
            });
        }
    }

    tracing::info!(
        total = results.len(),
        cached = cached_to_use,
        api_generated = api_results.len(),
        "Doc comment pass complete"
    );

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ChunkType, Language};

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

    #[test]
    fn llm_config_defaults_from_empty_config() {
        let config = crate::config::Config::default();
        let llm = LlmConfig::resolve(&config);
        assert_eq!(llm.api_base, API_BASE);
        assert_eq!(llm.model, MODEL);
        assert_eq!(llm.max_tokens, MAX_TOKENS);
    }

    #[test]
    fn llm_config_from_config_file_fields() {
        let config = crate::config::Config {
            llm_model: Some("claude-sonnet-4-20250514".to_string()),
            llm_api_base: Some("https://custom.api/v1".to_string()),
            llm_max_tokens: Some(200),
            ..Default::default()
        };
        let llm = LlmConfig::resolve(&config);
        assert_eq!(llm.model, "claude-sonnet-4-20250514");
        assert_eq!(llm.api_base, "https://custom.api/v1");
        assert_eq!(llm.max_tokens, 200);
    }

    #[test]
    fn llm_config_env_overrides_config_file() {
        let config = crate::config::Config {
            llm_model: Some("from-config".to_string()),
            llm_api_base: Some("https://from-config/v1".to_string()),
            llm_max_tokens: Some(200),
            ..Default::default()
        };

        // Set env vars (scoped to this test via unsafe + cleanup)
        std::env::set_var("CQS_LLM_MODEL", "from-env");
        std::env::set_var("CQS_API_BASE", "https://from-env/v1");
        std::env::set_var("CQS_LLM_MAX_TOKENS", "500");

        let llm = LlmConfig::resolve(&config);

        // Clean up env vars
        std::env::remove_var("CQS_LLM_MODEL");
        std::env::remove_var("CQS_API_BASE");
        std::env::remove_var("CQS_LLM_MAX_TOKENS");

        assert_eq!(llm.model, "from-env");
        assert_eq!(llm.api_base, "https://from-env/v1");
        assert_eq!(llm.max_tokens, 500);
    }

    #[test]
    fn llm_config_invalid_max_tokens_env_falls_through() {
        let config = crate::config::Config {
            llm_max_tokens: Some(300),
            ..Default::default()
        };

        std::env::set_var("CQS_LLM_MAX_TOKENS", "not_a_number");
        let llm = LlmConfig::resolve(&config);
        std::env::remove_var("CQS_LLM_MAX_TOKENS");

        // Invalid env var should fall through to config value
        assert_eq!(llm.max_tokens, 300);
    }

    // ===== needs_doc_comment tests =====

    /// Helper: build a minimal ChunkSummary with the given fields.
    fn make_chunk(
        chunk_type: ChunkType,
        doc: Option<&str>,
        window_idx: Option<i32>,
    ) -> ChunkSummary {
        ChunkSummary {
            id: "test::func".to_string(),
            file: std::path::PathBuf::from("src/test.rs"),
            language: Language::Rust,
            chunk_type,
            name: "test_func".to_string(),
            signature: "fn test_func()".to_string(),
            content: "fn test_func() { todo!() }".to_string(),
            doc: doc.map(|s| s.to_string()),
            line_start: 1,
            line_end: 3,
            content_hash: "abc123".to_string(),
            window_idx,
            parent_id: None,
            parent_type_name: None,
        }
    }

    #[test]
    fn test_needs_doc_comment_no_doc() {
        let chunk = make_chunk(ChunkType::Function, None, None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_thin() {
        // Doc under 30 chars with no signal words
        let chunk = make_chunk(ChunkType::Function, Some("A short doc"), None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_signal_words() {
        // Thin doc but contains SAFETY signal word — preserve it
        let chunk = make_chunk(ChunkType::Function, Some("/// SAFETY: requires lock"), None);
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_adequate() {
        // Doc >= 30 chars — no replacement needed
        let chunk = make_chunk(
            ChunkType::Function,
            Some("Parse a configuration file from disk and validate all fields."),
            None,
        );
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_not_callable() {
        // Struct/Enum are not callable — should return false
        let chunk = make_chunk(ChunkType::Struct, None, None);
        assert!(!needs_doc_comment(&chunk));

        let chunk = make_chunk(ChunkType::Enum, None, None);
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_window_idx_nonzero() {
        // Non-first window — skip
        let chunk = make_chunk(ChunkType::Function, None, Some(1));
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_window_idx_zero() {
        // First window — should be considered
        let chunk = make_chunk(ChunkType::Function, None, Some(0));
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_empty_doc() {
        // Empty string doc — same as no doc
        let chunk = make_chunk(ChunkType::Function, Some(""), None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_whitespace_doc() {
        // Whitespace-only doc — same as no doc
        let chunk = make_chunk(ChunkType::Function, Some("   \n  "), None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_method() {
        // Method is callable — should be considered
        let chunk = make_chunk(ChunkType::Method, None, None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_signal_word_case_insensitive() {
        // Signal words are case-insensitive
        let chunk = make_chunk(ChunkType::Function, Some("todo: fix this"), None);
        assert!(!needs_doc_comment(&chunk));

        let chunk = make_chunk(ChunkType::Function, Some("Deprecated"), None);
        assert!(!needs_doc_comment(&chunk));

        let chunk = make_chunk(ChunkType::Function, Some("FIXME later"), None);
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_all_signal_words() {
        for word in SIGNAL_WORDS {
            let doc = format!("Has {}", word);
            let chunk = make_chunk(ChunkType::Function, Some(&doc), None);
            assert!(
                !needs_doc_comment(&chunk),
                "Signal word '{}' should prevent replacement",
                word
            );
        }
    }

    // ===== build_doc_prompt tests =====

    #[test]
    fn test_build_doc_prompt_rust() {
        let prompt =
            Client::build_doc_prompt("fn foo() -> Result<(), Error> {}", "function", "rust");
        assert!(prompt.contains("doc comment"));
        assert!(prompt.contains("```rust"));
        assert!(prompt.contains("# Arguments"));
        assert!(prompt.contains("# Returns"));
        assert!(prompt.contains("# Errors"));
        assert!(prompt.contains("# Panics"));
    }

    #[test]
    fn test_build_doc_prompt_python() {
        let prompt = Client::build_doc_prompt("def foo(x: int) -> str:", "function", "python");
        assert!(prompt.contains("doc comment"));
        assert!(prompt.contains("```python"));
        assert!(prompt.contains("Google-style docstring"));
        assert!(prompt.contains("Args/Returns/Raises"));
    }

    #[test]
    fn test_build_doc_prompt_go() {
        let prompt = Client::build_doc_prompt("func Foo() error {}", "function", "go");
        assert!(prompt.contains("doc comment"));
        assert!(prompt.contains("```go"));
        assert!(prompt.contains("function name per Go conventions"));
    }

    #[test]
    fn test_build_doc_prompt_default() {
        let prompt = Client::build_doc_prompt("function foo() {}", "function", "javascript");
        assert!(prompt.contains("doc comment"));
        assert!(prompt.contains("```javascript"));
        // No language-specific appendix
        assert!(!prompt.contains("# Arguments"));
        assert!(!prompt.contains("Google-style"));
        assert!(!prompt.contains("Go conventions"));
    }

    #[test]
    fn test_build_doc_prompt_truncation() {
        let long = "x".repeat(10000);
        let prompt = Client::build_doc_prompt(&long, "function", "rust");
        assert!(prompt.len() < 10000 + 300);
    }
}
