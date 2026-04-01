//! LLM batch provider abstraction.

use std::collections::HashMap;

use super::LlmError;

/// A single item in a batch submission.
/// Named fields replace the opaque `(String, String, String, String)` tuple
/// to prevent positional errors at call sites.
pub struct BatchSubmitItem {
    /// Unique identifier for correlating results (typically content_hash)
    pub custom_id: String,
    /// Source code content or pre-built prompt
    pub content: String,
    /// Context field: chunk_type (summaries), signature (doc comments), or unused (prebuilt/HyDE)
    pub context: String,
    /// Programming language name
    pub language: String,
}

/// Trait for LLM batch API providers.
/// Abstracts the batch submission, polling, and result fetching lifecycle.
/// Currently implemented for Anthropic's Messages Batches API.
pub trait BatchProvider {
    /// Submit a batch of prompt requests. Returns the batch ID.
    /// `prompt_builder` constructs the user message from (content, context, language).
    fn submit_batch(
        &self,
        items: &[BatchSubmitItem],
        max_tokens: u32,
        purpose: &str,
        prompt_builder: fn(&str, &str, &str) -> String,
    ) -> Result<String, LlmError>;

    /// Submit a batch where prompts are already built (content field IS the prompt).
    /// Used by the contrastive summary path which pre-builds prompts with neighbor context.
    fn submit_batch_prebuilt(
        &self,
        items: &[BatchSubmitItem],
        max_tokens: u32,
    ) -> Result<String, LlmError>;

    /// Submit a batch of doc-comment requests. Returns the batch ID.
    fn submit_doc_batch(
        &self,
        items: &[BatchSubmitItem],
        max_tokens: u32,
    ) -> Result<String, LlmError>;

    /// Submit a batch of HyDE query prediction requests. Returns the batch ID.
    fn submit_hyde_batch(
        &self,
        items: &[BatchSubmitItem],
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
    /// Default accepts any non-empty ID. Provider implementations should
    /// override with provider-specific validation (e.g., Anthropic checks
    /// for `msgbatch_` prefix).
    fn is_valid_batch_id(&self, id: &str) -> bool {
        !id.is_empty()
    }

    /// Get the model name for this provider.
    fn model_name(&self) -> &str;
}

/// Mock batch provider for testing batch orchestration without API calls.
#[cfg(test)]
pub(crate) struct MockBatchProvider {
    pub batch_id: String,
    pub results: HashMap<String, String>,
    pub model: String,
}

#[cfg(test)]
impl MockBatchProvider {
    pub fn new(batch_id: &str, results: HashMap<String, String>) -> Self {
        Self {
            batch_id: batch_id.to_string(),
            results,
            model: "mock-model".to_string(),
        }
    }
}

#[cfg(test)]
impl BatchProvider for MockBatchProvider {
    fn submit_batch(
        &self,
        _items: &[BatchSubmitItem],
        _max_tokens: u32,
        _purpose: &str,
        _prompt_builder: fn(&str, &str, &str) -> String,
    ) -> Result<String, LlmError> {
        Ok(self.batch_id.clone())
    }

    fn submit_batch_prebuilt(
        &self,
        _items: &[BatchSubmitItem],
        _max_tokens: u32,
    ) -> Result<String, LlmError> {
        Ok(self.batch_id.clone())
    }

    fn submit_doc_batch(
        &self,
        _items: &[BatchSubmitItem],
        _max_tokens: u32,
    ) -> Result<String, LlmError> {
        Ok(self.batch_id.clone())
    }

    fn submit_hyde_batch(
        &self,
        _items: &[BatchSubmitItem],
        _max_tokens: u32,
    ) -> Result<String, LlmError> {
        Ok(self.batch_id.clone())
    }

    fn check_batch_status(&self, _batch_id: &str) -> Result<String, LlmError> {
        Ok("ended".to_string())
    }

    fn wait_for_batch(&self, _batch_id: &str, _quiet: bool) -> Result<(), LlmError> {
        Ok(())
    }

    fn fetch_batch_results(&self, _batch_id: &str) -> Result<HashMap<String, String>, LlmError> {
        Ok(self.results.clone())
    }

    fn is_valid_batch_id(&self, id: &str) -> bool {
        // Mock mimics Anthropic validation
        id.starts_with("msgbatch_")
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal provider that uses default is_valid_batch_id (accepts any non-empty ID).
    struct DefaultValidationProvider;

    impl BatchProvider for DefaultValidationProvider {
        fn submit_batch(
            &self,
            _items: &[BatchSubmitItem],
            _max_tokens: u32,
            _purpose: &str,
            _prompt_builder: fn(&str, &str, &str) -> String,
        ) -> Result<String, LlmError> {
            Ok(String::new())
        }

        fn submit_batch_prebuilt(
            &self,
            _items: &[BatchSubmitItem],
            _max_tokens: u32,
        ) -> Result<String, LlmError> {
            Ok(String::new())
        }

        fn submit_doc_batch(
            &self,
            _items: &[BatchSubmitItem],
            _max_tokens: u32,
        ) -> Result<String, LlmError> {
            Ok(String::new())
        }

        fn submit_hyde_batch(
            &self,
            _items: &[BatchSubmitItem],
            _max_tokens: u32,
        ) -> Result<String, LlmError> {
            Ok(String::new())
        }

        fn check_batch_status(&self, _batch_id: &str) -> Result<String, LlmError> {
            Ok("ended".to_string())
        }

        fn wait_for_batch(&self, _batch_id: &str, _quiet: bool) -> Result<(), LlmError> {
            Ok(())
        }

        fn fetch_batch_results(
            &self,
            _batch_id: &str,
        ) -> Result<HashMap<String, String>, LlmError> {
            Ok(HashMap::new())
        }

        fn model_name(&self) -> &str {
            "default-test"
        }
    }

    #[test]
    fn default_is_valid_batch_id_accepts_any_nonempty() {
        let provider = DefaultValidationProvider;
        assert!(provider.is_valid_batch_id("any_format_123"));
        assert!(provider.is_valid_batch_id("custom-provider-batch-xyz"));
        assert!(provider.is_valid_batch_id("msgbatch_abc"));
    }

    #[test]
    fn default_is_valid_batch_id_rejects_empty() {
        let provider = DefaultValidationProvider;
        assert!(!provider.is_valid_batch_id(""));
    }

    #[test]
    fn mock_provider_uses_anthropic_validation() {
        let mock = MockBatchProvider::new("msgbatch_test", HashMap::new());
        assert!(mock.is_valid_batch_id("msgbatch_abc123"));
        assert!(!mock.is_valid_batch_id("other_format"));
    }
}
