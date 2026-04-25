//! Batch submission, polling, and result fetching for the Claude Batches API.

use std::collections::HashMap;

use super::provider::BatchProvider;
use super::{
    ApiError, BatchItem, BatchRequest, BatchResponse, BatchResult, ChatMessage, LlmClient,
    LlmError, MessagesRequest, API_VERSION, BATCH_POLL_INTERVAL,
};
use crate::Store;

/// P2 #33 (LLM half): build the user-visible message for `LlmError::Api` so
/// that the daemon error envelope (which propagates the full anyhow chain)
/// never carries the raw HTTP response body, the parsed Anthropic
/// `error.message` (which sometimes echoes prompt fragments), or any other
/// remote-controlled string. Operators with journal access can still see the
/// full body via `tracing::debug!` at the call site.
///
/// Returned shape: `"Anthropic API error during {context} (status: {status})"`.
/// `context` is a short fixed-string label like "batch submission", picked by
/// the caller to keep the user-visible message useful for triage.
fn redacted_api_message(context: &str, status: u16) -> String {
    format!("Anthropic API error during {context} (status: {status})")
}

impl LlmClient {
    /// Core batch submission: builds requests using the given prompt builder, posts to the API.
    /// `items` is a list of (custom_id, content, field3, language) — field3 is chunk_type or signature
    /// depending on the prompt builder.
    /// `prompt_builder` constructs the user message from (content, field3, language).
    /// `purpose` is used in error/log messages (e.g. "Batch", "Doc batch", "Hyde batch").
    fn submit_batch_inner(
        &self,
        items: &[super::provider::BatchSubmitItem],
        max_tokens: u32,
        purpose: &str,
        prompt_builder: fn(&str, &str, &str) -> String,
    ) -> Result<String, LlmError> {
        if items.is_empty() {
            return Err(LlmError::BatchFailed("Cannot submit empty batch".into()));
        }
        let _span =
            tracing::info_span!("submit_batch_inner", purpose, count = items.len()).entered();
        let model = self.llm_config.model.clone();
        let requests: Vec<BatchItem> = items
            .iter()
            .map(|item| BatchItem {
                custom_id: item.custom_id.clone(),
                params: MessagesRequest {
                    model: model.clone(),
                    max_tokens,
                    messages: vec![ChatMessage {
                        role: "user".to_string(),
                        content: prompt_builder(&item.content, &item.context, &item.language),
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
            let body = response.text().unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Failed to read HTTP error response body");
                String::new()
            });
            // P2 #33 (LLM half): keep the full body for operator-visible debug
            // logs, but the user-visible LlmError::Api message must NOT carry
            // remote-controlled text (raw HTTP body or parsed `error.message`,
            // both of which can echo prompt fragments).
            let parsed = serde_json::from_str::<ApiError>(&body).ok();
            tracing::debug!(
                purpose,
                status = %status,
                parsed_anthropic_message = parsed.as_ref().map(|p| p.error.message.as_str()).unwrap_or(""),
                body_len = body.len(),
                body = %body,
                "{purpose} submission failed at LLM API",
            );
            let context = format!("{purpose} submission");
            return Err(LlmError::Api {
                status: status.as_u16(),
                message: redacted_api_message(&context, status.as_u16()),
            });
        }

        let batch: BatchResponse = response.json()?;
        if !super::is_valid_anthropic_batch_id(&batch.id) {
            return Err(LlmError::InvalidBatchId(batch.id));
        }
        tracing::info!(batch_id = %batch.id, count = items.len(), "{purpose} submitted");
        Ok(batch.id)
    }

    /// Submit a batch where prompts are already built (content field IS the prompt).
    /// Used by the contrastive summary path which pre-builds prompts with neighbor context.
    pub(super) fn submit_batch_prebuilt(
        &self,
        items: &[super::provider::BatchSubmitItem],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        // Identity: content is already the full prompt, ignore field3/language
        self.submit_batch_inner(items, max_tokens, "Batch", |content, _, _| {
            content.to_string()
        })
    }

    /// Submit a batch of doc-comment requests to the Batches API.
    /// Like `submit_batch` but uses `build_doc_prompt` instead of `build_prompt`.
    /// `items` is a list of (custom_id, content, chunk_type, language).
    /// Returns the batch ID for polling.
    pub(super) fn submit_doc_batch(
        &self,
        items: &[super::provider::BatchSubmitItem],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        self.submit_batch_inner(items, max_tokens, "Doc batch", Self::build_doc_prompt)
    }

    /// Submit a batch of HyDE query prediction requests to the Batches API.
    /// Like `submit_doc_batch` but uses `build_hyde_prompt` instead of `build_doc_prompt`.
    /// `items` is a list of (custom_id, content, signature, language).
    /// Returns the batch ID for polling.
    pub(super) fn submit_hyde_batch(
        &self,
        items: &[super::provider::BatchSubmitItem],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        self.submit_batch_inner(items, max_tokens, "Hyde batch", Self::build_hyde_prompt)
    }

    /// Check the current status of a batch without polling.
    pub(super) fn check_batch_status(&self, batch_id: &str) -> Result<String, LlmError> {
        let _span = tracing::debug_span!("check_batch_status", batch_id).entered();
        if !super::is_valid_anthropic_batch_id(batch_id) {
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
            let body = response.text().unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Failed to read HTTP error response body");
                String::new()
            });
            // P2 #33 (LLM half): redact body from user-visible error.
            tracing::debug!(
                batch_id,
                status,
                body_len = body.len(),
                body = %body,
                "Batch status check failed at LLM API",
            );
            return Err(LlmError::Api {
                status,
                message: redacted_api_message("batch status check", status),
            });
        }

        let batch: BatchResponse = response.json()?;
        Ok(batch.processing_status)
    }

    /// Poll until a batch completes. Returns when status is "ended".
    pub(super) fn wait_for_batch(&self, batch_id: &str, quiet: bool) -> Result<(), LlmError> {
        if !super::is_valid_anthropic_batch_id(batch_id) {
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
                let body = response.text().unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "Failed to read HTTP error response body");
                    String::new()
                });
                // P2 #33 (LLM half): redact body from user-visible error.
                tracing::debug!(
                    batch_id,
                    status,
                    body_len = body.len(),
                    body = %body,
                    "Batch status poll failed at LLM API",
                );
                return Err(LlmError::Api {
                    status,
                    message: redacted_api_message("batch status poll", status),
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
    /// Returns a map from custom_id to summary text.
    pub(super) fn fetch_batch_results(
        &self,
        batch_id: &str,
    ) -> Result<HashMap<String, String>, LlmError> {
        if !super::is_valid_anthropic_batch_id(batch_id) {
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
            let body = response.text().unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Failed to read HTTP error response body");
                String::new()
            });
            // P2 #33 (LLM half): redact body from user-visible error.
            tracing::debug!(
                batch_id,
                status,
                body_len = body.len(),
                body = %body,
                "Batch results fetch failed at LLM API",
            );
            return Err(LlmError::Api {
                status,
                message: redacted_api_message("batch results fetch", status),
            });
        }

        // RM-32: Check response size before buffering to prevent OOM.
        // Check Content-Length header first (fast path), then enforce limit
        // while reading the body (handles chunked transfer encoding where
        // content_length() returns None).
        const MAX_RESPONSE_BYTES: u64 = 100 * 1024 * 1024; // 100MB
        if let Some(len) = response.content_length() {
            if len > MAX_RESPONSE_BYTES {
                return Err(LlmError::Api {
                    status: 200,
                    message: format!(
                        "Batch response too large: {} bytes (max {})",
                        len, MAX_RESPONSE_BYTES
                    ),
                });
            }
        }

        // Read body with size limit via Read::take() — works for both
        // Content-Length and chunked transfer encoding responses.
        use std::io::Read;
        let mut body_bytes = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut body_bytes)
            .map_err(|e| {
                // P2 #33 (LLM half): keep the io::Error in debug logs only;
                // its Display can include filesystem paths in some chains.
                tracing::debug!(batch_id, error = %e, "Failed to read batch response body");
                LlmError::Api {
                    status: 200,
                    message: "Failed to read batch response body (I/O error)".to_string(),
                }
            })?;
        if body_bytes.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(LlmError::Api {
                status: 200,
                message: format!(
                    "Batch response exceeded {} byte limit while streaming",
                    MAX_RESPONSE_BYTES
                ),
            });
        }
        let body = String::from_utf8(body_bytes).map_err(|e| {
            // P2 #33 (LLM half): the FromUtf8Error Display includes the byte
            // offset and a slice of bytes — that's remote-controlled. Keep
            // detail in debug, redact the user-visible error.
            tracing::debug!(batch_id, error = %e, "Batch response body not valid UTF-8");
            LlmError::Api {
                status: 200,
                message: "Batch response not valid UTF-8".to_string(),
            }
        })?;
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
    fn submit_batch_prebuilt(
        &self,
        items: &[super::provider::BatchSubmitItem],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        LlmClient::submit_batch_prebuilt(self, items, max_tokens)
    }

    fn submit_doc_batch(
        &self,
        items: &[super::provider::BatchSubmitItem],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        LlmClient::submit_doc_batch(self, items, max_tokens)
    }

    fn submit_hyde_batch(
        &self,
        items: &[super::provider::BatchSubmitItem],
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
        super::is_valid_anthropic_batch_id(id)
    }

    fn model_name(&self) -> &str {
        &self.llm_config.model
    }
}

/// Configuration for the Phase 2 batch orchestration pattern.
/// Type alias for pending metadata get/set closures (clippy::type_complexity).
type PendingFn = dyn Fn(&Store, Option<&str>) -> Result<(), crate::store::StoreError>;
/// Type alias for batch submit closures (clippy::type_complexity).
type SubmitFn = dyn Fn(
    &dyn BatchProvider,
    &[super::provider::BatchSubmitItem],
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
    /// `batch_items`: items to submit (empty = only check for pending).
    /// `get_pending`: reads the pending batch ID from the store.
    /// `set_pending`: writes/clears the pending batch ID in the store.
    /// `submit`: submits a new batch via the client.
    /// Returns the raw results map. Results are stored in the DB with `self.purpose`.
    pub fn submit_or_resume(
        &self,
        client: &dyn BatchProvider,
        store: &Store,
        batch_items: &[super::provider::BatchSubmitItem],
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

        let result = self.resume(client, store, &batch_id, set_pending);
        // RM-34: Clean up lock file after batch operation completes
        drop(_batch_lock);
        self.cleanup_batch_lock();
        result
    }

    /// Remove the batch lock file if it exists (RM-34).
    /// Called after the lock handle is dropped to avoid leaving stale lock files on disk.
    fn cleanup_batch_lock(&self) {
        if let Some(dir) = self.lock_dir {
            let lock_path = dir.join("batch.lock");
            let _ = std::fs::remove_file(&lock_path);
        }
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
        let hash_result = store.get_all_content_hashes();
        let valid_hashes: std::collections::HashSet<String> = match &hash_result {
            Ok(hashes) => hashes.iter().cloned().collect(),
            Err(e) => {
                tracing::warn!(error = %e, "Could not validate content hashes, storing all results");
                std::collections::HashSet::new()
            }
        };

        let (valid_results, stale_count) = if valid_hashes.is_empty() && hash_result.is_err() {
            // Hash fetch failed — skip storage entirely to avoid committing stale data (DS-29).
            // Next run will retry the batch.
            tracing::error!(purpose = self.purpose, "Cannot validate batch results — skipping storage to prevent stale data. Will retry on next run.");
            if let Err(e) = clear_pending(store, None) {
                tracing::warn!(error = %e, purpose = self.purpose, "Failed to clear pending batch ID");
            }
            return Ok(results);
        } else if valid_hashes.is_empty() {
            // No hashes in DB (fresh/pre-v13 index) — store everything (PERF-34: move, not clone)
            (results, 0usize)
        } else {
            let mut valid = HashMap::new();
            let mut stale = 0usize;
            for (hash, text) in results {
                if valid_hashes.contains(&hash) {
                    valid.insert(hash, text);
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
        // PERF-38: model/purpose are cloned per item because upsert_summaries_batch takes
        // &[(String, String, String, String)]. Refactoring to separate params would change
        // the Store API signature and all callers — not worth it for batch sizes < 10k.
        let model = client.model_name().to_string();
        let purpose = self.purpose.to_string();
        let to_store: Vec<(String, String, String, String)> = valid_results
            .iter()
            .map(|(hash, text)| (hash.clone(), text.clone(), model.clone(), purpose.clone()))
            .collect();
        if !to_store.is_empty() {
            store.upsert_summaries_batch(&to_store)?;
        }

        // Clear pending batch marker
        if let Err(e) = clear_pending(store, None) {
            tracing::warn!(error = %e, purpose = self.purpose, "Failed to clear pending batch ID");
        }

        Ok(valid_results)
    }

    /// Submit a fresh batch and store its pending ID.
    fn submit_fresh(
        &self,
        client: &dyn BatchProvider,
        store: &Store,
        batch_items: &[super::provider::BatchSubmitItem],
        set_pending: &PendingFn,
        submit: &SubmitFn,
    ) -> Result<String, LlmError> {
        let _span = tracing::info_span!("submit_fresh", purpose = self.purpose).entered();
        tracing::info!(
            count = batch_items.len(),
            purpose = self.purpose,
            "Submitting batch to LLM provider"
        );
        let id = submit(client, batch_items, self.max_tokens)?;
        set_pending(store, Some(&id)).map_err(|e| {
            tracing::error!(
                error = %e,
                batch_id = %id,
                purpose = self.purpose,
                "Failed to store pending batch ID — batch {} submitted but ID lost. \
                 Manual recovery: cqs llm-resume --batch-id {}",
                id, id
            );
            LlmError::Store(e)
        })?;
        tracing::info!(batch_id = %id, purpose = self.purpose, "Batch submitted, waiting for results");
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::provider::{BatchSubmitItem, MockBatchProvider};
    use crate::test_helpers::setup_store;
    use std::collections::HashMap;

    // ===== P2 #33 (LLM half): redacted_api_message =====

    /// The user-visible API error message must NOT echo any remote-controlled
    /// text (raw HTTP body, parsed Anthropic `error.message`, etc.). It must
    /// carry only the operational context label and HTTP status code.
    #[test]
    fn redacted_api_message_excludes_remote_text() {
        let msg = redacted_api_message("batch submission", 502);
        assert_eq!(
            msg,
            "Anthropic API error during batch submission (status: 502)"
        );
        // No control characters, no JSON braces, no quotes from the body.
        assert!(
            !msg.contains('{'),
            "redacted message should not look like JSON"
        );
        assert!(
            !msg.contains('"'),
            "redacted message should not contain quoted body fragments"
        );
    }

    #[test]
    fn redacted_api_message_carries_status_for_triage() {
        for status in [400u16, 401, 403, 429, 500, 502, 503, 504] {
            let msg = redacted_api_message("batch status check", status);
            assert!(
                msg.contains(&status.to_string()),
                "status {} missing from message: {}",
                status,
                msg
            );
            assert!(msg.contains("batch status check"));
        }
    }

    /// Direct: confirm `LlmError::Api { message }` Display does not include any
    /// of the body content if we construct it via the redactor — even if the
    /// caller's body had injection markers.
    #[test]
    fn llm_error_api_with_redacted_message_drops_body() {
        // Simulated raw body that an attacker-controlled prompt could trigger:
        // contains the prompt fragment + an "ignore prior instructions" payload.
        let pretend_body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your prompt: IGNORE_PRIOR_INSTRUCTIONS xyz secret-from-content"}}"#;

        // The constructed error must NOT echo the body at all.
        let err = LlmError::Api {
            status: 400,
            message: redacted_api_message("batch submission", 400),
        };
        let rendered = format!("{err}");
        assert!(
            !rendered.contains("IGNORE_PRIOR_INSTRUCTIONS"),
            "rendered error must not echo body: {}",
            rendered
        );
        assert!(
            !rendered.contains("secret-from-content"),
            "rendered error must not echo body: {}",
            rendered
        );
        assert!(
            rendered.contains("400"),
            "status code expected: {}",
            rendered
        );
        assert!(
            rendered.contains("batch submission"),
            "context label expected: {}",
            rendered
        );

        // And confirm the body itself is not a substring (safety).
        assert!(
            !rendered.contains(pretend_body),
            "rendered error must not contain raw body: {}",
            rendered
        );
    }

    /// Insert a minimal chunk with a specific content_hash for batch validation tests.
    fn insert_chunk_with_hash(store: &Store, content_hash: &str) {
        let embedding = crate::embedder::Embedding::new(vec![0.0f32; crate::EMBEDDING_DIM]);
        let embedding_bytes =
            crate::store::helpers::embedding_to_bytes(&embedding, crate::EMBEDDING_DIM).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        store.rt.block_on(async {
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name,
                     signature, content, content_hash, doc, line_start, line_end, embedding,
                     source_mtime, created_at, updated_at)
                     VALUES (?1, ?2, 'file', 'rust', 'function', ?3,
                     '', 'fn test() {}', ?4, NULL, 1, 10, ?5, 0, ?6, ?6)",
            )
            .bind(content_hash) // id = content_hash for simplicity
            .bind("test.rs")
            .bind(format!("test_{}", content_hash))
            .bind(content_hash)
            .bind(&embedding_bytes)
            .bind(&now)
            .execute(&store.pool)
            .await
            .unwrap();
        });
    }

    /// DS-28 regression: resume should only return results whose content_hashes
    /// exist in the current index, filtering out stale hashes from prior builds.
    #[test]
    fn test_resume_returns_valid_results_only() {
        let (store, _dir) = setup_store();

        // Insert two chunks with known hashes
        insert_chunk_with_hash(&store, "hash_aaa");
        insert_chunk_with_hash(&store, "hash_bbb");

        // Mock provider returns results for valid hashes + one stale hash
        let mut results = HashMap::new();
        results.insert("hash_aaa".to_string(), "summary for aaa".to_string());
        results.insert("hash_bbb".to_string(), "summary for bbb".to_string());
        results.insert("hash_stale".to_string(), "summary for stale".to_string());

        let mock = MockBatchProvider::new("msgbatch_test123", results);

        let phase2 = BatchPhase2 {
            purpose: "summary",
            max_tokens: 1024,
            quiet: true,
            lock_dir: None,
        };

        // Submit with items so we go through the fresh-submit + resume path.
        let items = vec![BatchSubmitItem {
            custom_id: "hash_aaa".to_string(),
            content: "fn aaa() {}".to_string(),
            context: "function".to_string(),
            language: "rust".to_string(),
        }];

        let result = phase2.submit_or_resume(
            &mock,
            &store,
            &items,
            &|s| s.get_pending_batch_id(),
            &|s, id| s.set_pending_batch_id(id),
            &|c, items, max_tok| c.submit_batch_prebuilt(items, max_tok),
        );

        let map = result.unwrap();
        // Valid hashes should be present
        assert!(
            map.contains_key("hash_aaa"),
            "hash_aaa should be in results"
        );
        assert!(
            map.contains_key("hash_bbb"),
            "hash_bbb should be in results"
        );
        // Stale hash should be filtered out
        assert!(
            !map.contains_key("hash_stale"),
            "stale hash should be filtered out"
        );
        assert_eq!(map.len(), 2);
    }

    /// RB-22: empty batch_items with no pending batch should return Ok(empty).
    #[test]
    fn test_empty_batch_items() {
        let (store, _dir) = setup_store();
        let mock = MockBatchProvider::new("msgbatch_unused", HashMap::new());

        let phase2 = BatchPhase2 {
            purpose: "summary",
            max_tokens: 1024,
            quiet: true,
            lock_dir: None,
        };

        let result = phase2.submit_or_resume(
            &mock,
            &store,
            &[], // empty batch items
            &|s| s.get_pending_batch_id(),
            &|s, id| s.set_pending_batch_id(id),
            &|c, items, max_tok| c.submit_batch_prebuilt(items, max_tok),
        );

        let map = result.unwrap();
        assert!(
            map.is_empty(),
            "empty batch_items should return empty results"
        );
    }

    #[test]
    fn test_is_valid_batch_id() {
        // Valid IDs
        assert!(super::super::is_valid_anthropic_batch_id("msgbatch_abc123"));
        assert!(super::super::is_valid_anthropic_batch_id(
            "msgbatch_0123456789"
        ));
        assert!(super::super::is_valid_anthropic_batch_id(
            "msgbatch_ABCdef_underscore"
        ));

        // Invalid: empty
        assert!(!super::super::is_valid_anthropic_batch_id(""));
        // Invalid: wrong prefix
        assert!(!super::super::is_valid_anthropic_batch_id("batch_123"));
        // Invalid: no msgbatch_ prefix
        assert!(!super::super::is_valid_anthropic_batch_id("not_a_batch"));
        // Invalid: contains spaces
        assert!(
            !super::super::is_valid_anthropic_batch_id("msgbatch_has spaces"),
            "spaces should be rejected"
        );
        // Invalid: contains path separator
        assert!(
            !super::super::is_valid_anthropic_batch_id("msgbatch_has/slash"),
            "slash should be rejected"
        );
        // Invalid: over-length (100+ chars)
        assert!(
            !super::super::is_valid_anthropic_batch_id(&format!("msgbatch_{}", "a".repeat(100))),
            "over-length should be rejected"
        );
    }

    /// TC-46: Batch with results for nonexistent chunks returns empty map
    /// (all results filtered as stale). Requires at least one real chunk
    /// in the DB so the valid_hashes set is non-empty (empty DB = store-all).
    #[test]
    fn test_all_results_filtered_returns_empty() {
        let (store, _dir) = setup_store();
        // Insert one real chunk so valid_hashes is non-empty (otherwise the
        // code assumes fresh DB and stores everything without filtering).
        insert_chunk_with_hash(&store, "hash_real");

        let mut results = HashMap::new();
        results.insert("nonexistent_hash_1".to_string(), "summary 1".to_string());
        results.insert("nonexistent_hash_2".to_string(), "summary 2".to_string());

        let mock = MockBatchProvider::new("msgbatch_filter", results);
        let phase2 = BatchPhase2 {
            purpose: "summary",
            max_tokens: 1024,
            quiet: true,
            lock_dir: None,
        };

        let items = vec![BatchSubmitItem {
            custom_id: "nonexistent_hash_1".to_string(),
            content: "fn ghost() {}".to_string(),
            context: "function".to_string(),
            language: "rust".to_string(),
        }];

        let result = phase2.submit_or_resume(
            &mock,
            &store,
            &items,
            &|s| s.get_pending_batch_id(),
            &|s, id| s.set_pending_batch_id(id),
            &|c, items, max_tok| c.submit_batch_prebuilt(items, max_tok),
        );

        let map = result.unwrap();
        assert!(map.is_empty(), "All stale results should be filtered out");
    }

    /// TC-46: submit_or_resume with a stored pending batch ID resumes correctly.
    #[test]
    fn test_resume_with_pending_batch() {
        let (store, _dir) = setup_store();

        // Insert a chunk so the result passes validation
        insert_chunk_with_hash(&store, "hash_resume");

        // Set a pending batch ID (simulating a prior interrupted run)
        store
            .set_pending_batch_id(Some("msgbatch_pending_resume"))
            .unwrap();

        let mut results = HashMap::new();
        results.insert("hash_resume".to_string(), "resumed summary".to_string());

        let mock = MockBatchProvider::new("msgbatch_pending_resume", results);
        let phase2 = BatchPhase2 {
            purpose: "summary",
            max_tokens: 1024,
            quiet: true,
            lock_dir: None,
        };

        // Empty items -- but there's a pending batch to resume
        let result = phase2.submit_or_resume(
            &mock,
            &store,
            &[],
            &|s| s.get_pending_batch_id(),
            &|s, id| s.set_pending_batch_id(id),
            &|c, items, max_tok| c.submit_batch_prebuilt(items, max_tok),
        );

        let map = result.unwrap();
        assert!(
            map.contains_key("hash_resume"),
            "Resumed batch should return valid results"
        );
    }

    #[test]
    fn test_clear_pending() {
        let (store, _dir) = setup_store();

        // Set a pending batch ID
        store
            .set_pending_batch_id(Some("msgbatch_pending123"))
            .unwrap();
        assert_eq!(
            store.get_pending_batch_id().unwrap(),
            Some("msgbatch_pending123".to_string())
        );

        // Clear it via set_pending_batch_id(None) — same pattern used by BatchPhase2
        store.set_pending_batch_id(None).unwrap();
        assert_eq!(store.get_pending_batch_id().unwrap(), None);
    }
}
