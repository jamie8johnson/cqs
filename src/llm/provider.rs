//! LLM batch provider abstraction.

use std::collections::HashMap;

use super::LlmError;

/// Trait for LLM batch API providers.
///
/// Abstracts the batch submission, polling, and result fetching lifecycle.
/// Currently implemented for Anthropic's Messages Batches API.
pub trait BatchProvider {
    /// Submit a batch of prompt requests. Returns the batch ID.
    ///
    /// `items` is a list of (custom_id, content, field3, language) -- field3 is chunk_type
    /// or signature depending on the prompt builder.
    /// `prompt_builder` constructs the user message from (content, field3, language).
    fn submit_batch(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
        purpose: &str,
        prompt_builder: fn(&str, &str, &str) -> String,
    ) -> Result<String, LlmError>;

    /// Submit a batch where prompts are already built (content field IS the prompt).
    ///
    /// Used by the contrastive summary path which pre-builds prompts with neighbor context.
    fn submit_batch_prebuilt(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError>;

    /// Submit a batch of doc-comment requests. Returns the batch ID.
    fn submit_doc_batch(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError>;

    /// Submit a batch of HyDE query prediction requests. Returns the batch ID.
    fn submit_hyde_batch(
        &self,
        items: &[(String, String, String, String)],
        max_tokens: u32,
    ) -> Result<String, LlmError>;

    /// Check the current status of a batch without polling.
    /// Returns status string (e.g., "ended", "in_progress").
    fn check_batch_status(&self, batch_id: &str) -> Result<String, LlmError>;

    /// Poll until a batch completes. Returns when status is "ended".
    fn wait_for_batch(&self, batch_id: &str, quiet: bool) -> Result<(), LlmError>;

    /// Fetch results from a completed batch. Returns map of custom_id -> response text.
    fn fetch_batch_results(&self, batch_id: &str) -> Result<HashMap<String, String>, LlmError>;

    /// Validate a batch ID format.
    fn is_valid_batch_id(&self, id: &str) -> bool;

    /// Get the model name for this provider.
    fn model_name(&self) -> &str;
}
