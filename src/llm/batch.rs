//! Batch submission, polling, and result fetching for the Claude Batches API.

use std::collections::HashMap;

use super::provider::BatchProvider;
use super::{
    ApiError, BatchItem, BatchRequest, BatchResponse, BatchResult, ChatMessage, LlmClient,
    LlmError, MessagesRequest, API_VERSION, BATCH_POLL_INTERVAL,
};
use crate::Store;

impl LlmClient {
    /// Core batch submission: builds requests using the given prompt builder, posts to the API.
    ///
    /// `items` is a list of (custom_id, content, field3, language) — field3 is chunk_type or signature
    /// depending on the prompt builder.
    /// `prompt_builder` constructs the user message from (content, field3, language).
    /// `purpose` is used in error/log messages (e.g. "Batch", "Doc batch", "Hyde batch").
    fn submit_batch_inner(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
        purpose: &str,
        prompt_builder: fn(&str, &str, &str) -> String,
    ) -> Result<String, LlmError> {
        let _span =
            tracing::info_span!("submit_batch_inner", purpose, count = items.len()).entered();
        let model = self.llm_config.model.clone();
        let requests: Vec<BatchItem> = items
            .iter()
            .map(|(id, content, field3, language)| BatchItem {
                custom_id: id.clone(),
                params: MessagesRequest {
                    model: model.clone(),
                    max_tokens,
                    messages: vec![ChatMessage {
                        role: "user".to_string(),
                        content: prompt_builder(content, field3, language),
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
                .map(|err| format!("{purpose} submission failed: {}", err.error.message))
                .unwrap_or_else(|_| format!("{purpose} submission failed: HTTP {status}: {body}"));
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let batch: BatchResponse = response.json()?;
        if !super::is_valid_batch_id(&batch.id) {
            return Err(LlmError::InvalidBatchId(batch.id));
        }
        tracing::info!(batch_id = %batch.id, count = items.len(), "{purpose} submitted");
        Ok(batch.id)
    }

    /// Submit a batch where prompts are already built (content field IS the prompt).
    ///
    /// Used by the contrastive summary path which pre-builds prompts with neighbor context.
    pub(super) fn submit_batch_prebuilt(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        // Identity: content is already the full prompt, ignore field3/language
        self.submit_batch_inner(items, max_tokens, "Batch", |content, _, _| {
            content.to_string()
        })
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
        self.submit_batch_inner(items, max_tokens, "Doc batch", Self::build_doc_prompt)
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
        self.submit_batch_inner(items, max_tokens, "Hyde batch", Self::build_hyde_prompt)
    }

    /// Check the current status of a batch without polling.
    pub(super) fn check_batch_status(&self, batch_id: &str) -> Result<String, LlmError> {
        let _span = tracing::debug_span!("check_batch_status", batch_id).entered();
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
}

impl BatchProvider for LlmClient {
    fn submit_batch(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
        purpose: &str,
        prompt_builder: fn(&str, &str, &str) -> String,
    ) -> Result<String, LlmError> {
        self.submit_batch_inner(items, max_tokens, purpose, prompt_builder)
    }

    fn submit_batch_prebuilt(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        LlmClient::submit_batch_prebuilt(self, items, max_tokens)
    }

    fn submit_doc_batch(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        LlmClient::submit_doc_batch(self, items, max_tokens)
    }

    fn submit_hyde_batch(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        LlmClient::submit_hyde_batch(self, items, max_tokens)
    }

    fn check_batch_status(&self, batch_id: &str) -> Result<String, LlmError> {
        LlmClient::check_batch_status(self, batch_id)
    }

    fn wait_for_batch(&self, batch_id: &str, quiet: bool) -> Result<(), LlmError> {
        LlmClient::wait_for_batch(self, batch_id, quiet)
    }

    fn fetch_batch_results(&self, batch_id: &str) -> Result<HashMap<String, String>, LlmError> {
        LlmClient::fetch_batch_results(self, batch_id)
    }

    fn is_valid_batch_id(&self, id: &str) -> bool {
        super::is_valid_batch_id(id)
    }

    fn model_name(&self) -> &str {
        &self.llm_config.model
    }
}

/// Configuration for the Phase 2 batch orchestration pattern.
///
/// Type alias for pending metadata get/set closures (clippy::type_complexity).
type PendingFn = dyn Fn(&Store, Option<&str>) -> Result<(), crate::store::StoreError>;
/// Type alias for batch submit closures (clippy::type_complexity).
type SubmitFn = dyn Fn(
    &dyn BatchProvider,
    &[(String, String, String, String)],
    u32,
) -> Result<String, LlmError>;

/// Captures the per-purpose differences (pending metadata key, submit function, purpose string)
/// so the orchestration logic (`submit_or_resume`) can be shared across summary, doc, and HyDE passes.
pub(super) struct BatchPhase2<'a> {
    /// Purpose label for log messages and storage (e.g. "summary", "hyde", "doc-comment").
    pub purpose: &'static str,
    /// Max tokens for the batch API request.
    pub max_tokens: u32,
    /// Whether to suppress progress output.
    pub quiet: bool,
    /// DS-25: Directory for `batch.lock` file to prevent concurrent batch submission.
    /// When set, a file lock is acquired before the check-then-set on pending batch ID.
    /// Typically the `.cqs` directory. `None` disables locking (e.g. in tests).
    pub lock_dir: Option<&'a std::path::Path>,
}

impl BatchPhase2<'_> {
    /// Acquire the batch lock file if `lock_dir` is set, preventing concurrent
    /// batch submission races (DS-25). Returns the held file (lock released on drop).
    fn acquire_batch_lock(&self) -> Result<Option<std::fs::File>, LlmError> {
        let Some(dir) = self.lock_dir else {
            return Ok(None);
        };
        let lock_path = dir.join("batch.lock");
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| {
                LlmError::Io(std::io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to open batch lock at {}: {}",
                        lock_path.display(),
                        e
                    ),
                ))
            })?;
        lock_file.lock().map_err(|e| {
            LlmError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to acquire batch lock at {}: {}",
                    lock_path.display(),
                    e
                ),
            ))
        })?;
        tracing::debug!(
            lock_path = %lock_path.display(),
            purpose = self.purpose,
            "Acquired batch lock"
        );
        Ok(Some(lock_file))
    }

    /// Run the Phase 2 orchestration: check for pending batch, submit or resume, fetch results.
    ///
    /// `batch_items`: items to submit (empty = only check for pending).
    /// `get_pending`: reads the pending batch ID from the store.
    /// `set_pending`: writes/clears the pending batch ID in the store.
    /// `submit`: submits a new batch via the client.
    ///
    /// Returns the raw results map. Results are stored in the DB with `self.purpose`.
    pub fn submit_or_resume(
        &self,
        client: &dyn BatchProvider,
        store: &Store,
        batch_items: &[(String, String, String, String)],
        get_pending: &dyn Fn(&Store) -> Result<Option<String>, crate::store::StoreError>,
        set_pending: &PendingFn,
        submit: &SubmitFn,
    ) -> Result<HashMap<String, String>, LlmError> {
        let _span = tracing::info_span!(
            "submit_or_resume",
            purpose = self.purpose,
            count = batch_items.len()
        )
        .entered();

        // DS-25: Acquire file lock before the check-then-set on pending batch ID.
        // This prevents two concurrent `cqs index --llm-summaries` from both seeing
        // "no pending batch" and both submitting fresh batches.
        // Lock is held through get_pending -> submit -> set_pending, released on drop.
        let _batch_lock = self.acquire_batch_lock()?;

        if batch_items.is_empty() {
            // No new items needed, but check if a previous batch is still pending
            return match get_pending(store) {
                Ok(Some(pending)) => {
                    tracing::info!(batch_id = %pending, purpose = self.purpose, "Resuming pending batch");
                    let results = self.resume(client, store, &pending, set_pending)?;
                    tracing::info!(
                        count = results.len(),
                        purpose = self.purpose,
                        "Fetched pending batch results — new chunks will be processed on next run"
                    );
                    Ok(results)
                }
                Err(e) => {
                    // EH-24: don't swallow store errors — they could mean lost batch results
                    tracing::error!(
                        error = %e,
                        purpose = self.purpose,
                        "Failed to read pending batch ID — if a batch was in progress, results may be lost. \
                         Check store health and re-run with --llm-summaries to recover."
                    );
                    Err(LlmError::Store(e))
                }
                _ => Ok(HashMap::new()),
            };
        }

        // Check for a pending batch from a previous interrupted run
        let batch_id = match get_pending(store) {
            Ok(Some(pending)) => {
                tracing::info!(batch_id = %pending, purpose = self.purpose, "Found pending batch, checking status");
                match client.check_batch_status(&pending) {
                    Ok(status)
                        if status == "in_progress"
                            || status == "finalizing"
                            || status == "created"
                            || status == "ended" =>
                    {
                        tracing::info!(
                            batch_id = %pending,
                            status = %status,
                            purpose = self.purpose,
                            "Pending batch still active, resuming"
                        );
                        pending
                    }
                    Ok(status) => {
                        // EH-25: log the actual status so we can diagnose
                        tracing::warn!(
                            old_batch = %pending,
                            status = %status,
                            purpose = self.purpose,
                            "Pending batch has unexpected status '{}', abandoning and submitting fresh. \
                             If the batch was valid, its results are lost.",
                            status
                        );
                        // Clear the stale pending marker before submitting fresh
                        if let Err(e) = set_pending(store, None) {
                            tracing::warn!(error = %e, "Failed to clear stale pending batch ID");
                        }
                        self.submit_fresh(client, store, batch_items, set_pending, submit)?
                    }
                    Err(e) => {
                        tracing::warn!(
                            old_batch = %pending,
                            error = %e,
                            purpose = self.purpose,
                            "Failed to check pending batch status, submitting fresh"
                        );
                        if let Err(e) = set_pending(store, None) {
                            tracing::warn!(error = %e, "Failed to clear stale pending batch ID");
                        }
                        self.submit_fresh(client, store, batch_items, set_pending, submit)?
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, purpose = self.purpose, "Failed to read pending batch ID");
                self.submit_fresh(client, store, batch_items, set_pending, submit)?
            }
            _ => self.submit_fresh(client, store, batch_items, set_pending, submit)?,
        };

        self.resume(client, store, &batch_id, set_pending)
    }

    /// Wait for batch, fetch results, store with purpose, clear pending marker.
    fn resume(
        &self,
        client: &dyn BatchProvider,
        store: &Store,
        batch_id: &str,
        clear_pending: &PendingFn,
    ) -> Result<HashMap<String, String>, LlmError> {
        let _span = tracing::info_span!("batch_resume", batch_id, purpose = self.purpose).entered();
        client.wait_for_batch(batch_id, self.quiet)?;

        if !self.quiet {
            eprintln!();
        }

        let results = client.fetch_batch_results(batch_id)?;

        // DS-20: Validate results against current index — skip stale content_hashes
        // (e.g., after --force rebuild, the batch results reference chunks that no longer exist)
        let valid_hashes: std::collections::HashSet<String> = match store.get_all_content_hashes() {
            Ok(hashes) => hashes.into_iter().collect(),
            Err(e) => {
                tracing::warn!(error = %e, "Could not validate content hashes, storing all results");
                std::collections::HashSet::new()
            }
        };

        let (valid_results, stale_count) = if valid_hashes.is_empty() {
            // Couldn't fetch hashes — store everything (backwards-compatible)
            (results.clone(), 0usize)
        } else {
            let mut valid = HashMap::new();
            let mut stale = 0usize;
            for (hash, text) in &results {
                if valid_hashes.contains(hash) {
                    valid.insert(hash.clone(), text.clone());
                } else {
                    stale += 1;
                }
            }
            (valid, stale)
        };

        if stale_count > 0 {
            tracing::warn!(
                stale = stale_count,
                valid = valid_results.len(),
                purpose = self.purpose,
                "Skipped {} stale batch results (content_hash not in current index — likely from a previous build)",
                stale_count
            );
        }

        // Store results with the given purpose
        let model = client.model_name().to_string();
        let to_store: Vec<(String, String, String, String)> = valid_results
            .iter()
            .map(|(hash, text)| {
                (
                    hash.clone(),
                    text.clone(),
                    model.clone(),
                    self.purpose.to_string(),
                )
            })
            .collect();
        if !to_store.is_empty() {
            store.upsert_summaries_batch(&to_store)?;
        }

        // Clear pending batch marker
        if let Err(e) = clear_pending(store, None) {
            tracing::warn!(error = %e, purpose = self.purpose, "Failed to clear pending batch ID");
        }

        Ok(results)
    }

    /// Submit a fresh batch and store its pending ID.
    fn submit_fresh(
        &self,
        client: &dyn BatchProvider,
        store: &Store,
        batch_items: &[(String, String, String, String)],
        set_pending: &PendingFn,
        submit: &SubmitFn,
    ) -> Result<String, LlmError> {
        tracing::info!(
            count = batch_items.len(),
            purpose = self.purpose,
            "Submitting batch to Claude API"
        );
        let id = submit(client, batch_items, self.max_tokens)?;
        if let Err(e) = set_pending(store, Some(&id)) {
            tracing::warn!(error = %e, purpose = self.purpose, "Failed to store pending batch ID");
        }
        tracing::info!(batch_id = %id, purpose = self.purpose, "Batch submitted, waiting for results");
        Ok(id)
    }
}
