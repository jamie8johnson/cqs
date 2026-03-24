//! Batch submission, polling, and result fetching for the Claude Batches API.

use std::collections::HashMap;

use super::{
    ApiError, BatchItem, BatchRequest, BatchResponse, BatchResult, ChatMessage, Client, LlmError,
    MessagesRequest, API_VERSION, BATCH_POLL_INTERVAL,
};
use crate::Store;

impl Client {
    /// Submit a batch of summary requests to the Batches API.
    ///
    /// `items` is a list of (custom_id, content, chunk_type, language).
    /// `max_tokens` controls the per-request token limit.
    /// Returns the batch ID for polling.
    pub(super) fn submit_batch(
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
    pub(super) fn check_batch_status(&self, batch_id: &str) -> Result<String, LlmError> {
        if !super::is_valid_batch_id(batch_id) {
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
    pub(super) fn wait_for_batch(&self, batch_id: &str, quiet: bool) -> Result<(), LlmError> {
        if !super::is_valid_batch_id(batch_id) {
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
    pub(super) fn fetch_batch_results(
        &self,
        batch_id: &str,
    ) -> Result<HashMap<String, String>, LlmError> {
        if !super::is_valid_batch_id(batch_id) {
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
    pub(super) fn submit_doc_batch(
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

    /// Submit a batch of HyDE query prediction requests to the Batches API.
    ///
    /// Like `submit_doc_batch` but uses `build_hyde_prompt` instead of `build_doc_prompt`.
    /// `items` is a list of (custom_id, content, signature, language).
    /// Returns the batch ID for polling.
    pub(super) fn submit_hyde_batch(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        let model = self.llm_config.model.clone();
        let requests: Vec<BatchItem> = items
            .iter()
            .map(|(id, content, signature, language)| BatchItem {
                custom_id: id.clone(),
                params: MessagesRequest {
                    model: model.clone(),
                    max_tokens,
                    messages: vec![ChatMessage {
                        role: "user".to_string(),
                        content: Self::build_hyde_prompt(content, signature, language),
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
                .map(|err| format!("Hyde batch submission failed: {}", err.error.message))
                .unwrap_or_else(|_| format!("Hyde batch submission failed: HTTP {status}: {body}"));
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let batch: BatchResponse = response.json()?;
        tracing::info!(batch_id = %batch.id, count = items.len(), "Hyde batch submitted");
        Ok(batch.id)
    }
}

/// Wait for a batch to complete, fetch results, store them, and clear the pending marker.
pub(super) fn resume_or_fetch_batch(
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

/// Wait for a HyDE batch to complete, fetch results, store them, and clear the pending marker.
pub(super) fn resume_or_fetch_hyde_batch(
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

    // Store API-generated HyDE predictions
    let model = client.llm_config.model.clone();
    let api_summaries: Vec<(String, String, String, String)> = results
        .into_iter()
        .map(|(hash, summary)| (hash, summary, model.clone(), "hyde".to_string()))
        .collect();
    let count = api_summaries.len();
    if !api_summaries.is_empty() {
        store.upsert_summaries_batch(&api_summaries)?;
    }

    // Clear pending batch marker
    if let Err(e) = store.set_pending_hyde_batch_id(None) {
        tracing::warn!(error = %e, "Failed to clear pending hyde batch ID");
    }

    Ok(count)
}

/// Wait for a doc batch to complete, fetch results, store them, and clear the pending marker.
pub(super) fn resume_or_fetch_doc_batch(
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
