//! LLM batch provider abstraction.

use std::collections::HashMap;

use super::{LlmConfig, LlmError};

/// Registry entry for a named LLM provider.
///
/// Each provider supplies its canonical env-var name and a factory that
/// turns a resolved [`LlmConfig`] into a [`BatchProvider`] trait object.
/// Resolution and dispatch walk a single static slice (`PROVIDERS` in
/// `super`), so adding a third provider is one impl + one slice row,
/// not three coordinated edits.
pub(crate) trait ProviderRegistry: Sync {
    fn name(&self) -> &'static str;
    fn build(&self, cfg: LlmConfig) -> Result<Box<dyn BatchProvider>, LlmError>;
}

/// Callback type for the per-item streaming persist hook.
///
/// `(custom_id, text)` — see [`BatchProvider::set_on_item_complete`] for the
/// full concurrency contract.
pub type OnItemCallback = Box<dyn Fn(&str, &str) + Send + Sync>;

/// A single item in a batch submission.
/// Named fields replace the opaque `(String, String, String, String)` tuple
/// to prevent positional errors at call sites.
pub struct BatchSubmitItem {
    /// Unique identifier for correlating results (typically content_hash)
    pub custom_id: String,
    /// Source code content or pre-built prompt
    pub content: String,
    /// Stringly-typed context bag whose meaning depends on which prompt
    /// builder (`build_summary_prompt` / `build_doc_prompt` /
    /// `build_hyde_prompt` / pre-built path) consumes the item. P3-47:
    /// upgrading to a `PromptContext` enum touches every reader and
    /// every construction site (see `llm/batch.rs:874,993`,
    /// `llm/doc_comments.rs:266`, `llm/summary.rs:86`,
    /// `llm/hyde.rs:57`, `llm/local.rs:739`) so the enum migration is
    /// scoped to a separate change. Until then, the field's payload is:
    ///
    /// | Submission path                          | Field carries           |
    /// |------------------------------------------|-------------------------|
    /// | `BatchKind::Prebuilt` (contrastive)      | unused (`String::new`)  |
    /// | `BatchKind::DocComment` (doc comments)   | function signature      |
    /// | `BatchKind::Hyde` (HyDE queries)         | unused (`String::new`)  |
    /// | summary batches (`llm_summary_pass`)     | chunk type label        |
    ///
    /// The receiver decides; a typo at the call site silently sends
    /// the wrong field. This is the same tuple-with-named-fields
    /// problem the struct was meant to solve, just one level deeper —
    /// hence the deferred refactor.
    pub context: String,
    /// Programming language name
    pub language: String,
}

/// Which prompt builder a batch submission uses.
///
/// #1347 / EX-V1.33-1: replaces the three `submit_*_batch` trait methods
/// (`Prebuilt` / `DocComment` / `Hyde`) that differed only in which
/// `build_*_prompt` closure was passed to `submit_batch_inner` /
/// `submit_via_chat_completions`. Adding a fourth purpose (e.g.
/// `Classification`, `ContrastiveRepair`, `CodeReview`) is now a single new
/// variant + the impl's `match` arm — instead of: a new trait method × a
/// new impl on every `BatchProvider` (4 sites: `LlmClient`, `LocalProvider`,
/// `MockBatchProvider`, `DefaultValidationProvider`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchKind {
    /// Pre-built prompts — `BatchSubmitItem::content` IS the user message.
    /// Used by the contrastive summary path which pre-builds prompts with
    /// neighbor context. `context` / `language` fields ignored.
    Prebuilt,
    /// Doc-comment generation — uses `LlmClient::build_doc_prompt`.
    /// `BatchSubmitItem::context` carries the function signature.
    DocComment,
    /// HyDE query prediction — uses `LlmClient::build_hyde_prompt`.
    /// `BatchSubmitItem::context` carries the function signature.
    Hyde,
}

impl BatchKind {
    /// Short label for logging / error messages
    /// (`"Batch"` / `"Doc batch"` / `"Hyde batch"` historically — kept
    /// stable so log greps don't break).
    pub fn purpose_label(&self) -> &'static str {
        match self {
            Self::Prebuilt => "Batch",
            Self::DocComment => "Doc batch",
            Self::Hyde => "Hyde batch",
        }
    }
}

/// Trait for LLM batch API providers.
/// Abstracts the batch submission, polling, and result fetching lifecycle.
/// Currently implemented for Anthropic's Messages Batches API.
pub trait BatchProvider {
    /// Submit a batch under the given `kind`. The kind selects which prompt
    /// builder constructs the user message from each `BatchSubmitItem`'s
    /// `(content, context, language)` triple (or, for [`BatchKind::Prebuilt`],
    /// uses `content` directly).
    ///
    /// #1347 / EX-V1.33-1: collapses the previous trio of `submit_batch_prebuilt`
    /// / `submit_doc_batch` / `submit_hyde_batch` methods into one.
    fn submit_batch(
        &self,
        kind: BatchKind,
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

    /// EXT-V1.36-1 / P3: validate that the model name configured for this
    /// provider matches the provider's expected naming convention. Default
    /// accepts any non-empty model so impls can opt in incrementally.
    ///
    /// Anthropic: should override and reject names that don't start with
    /// `claude-`. Local OpenAI-compat: any non-empty name is fine because
    /// vLLM/LMDeploy etc. expose arbitrary identifiers. A future OpenAI
    /// provider should validate `gpt-` / `o1-` / `o3-` prefixes.
    ///
    /// Called from `submit_batch` *before* the API roundtrip so a
    /// wrong-provider/model combo (e.g. `--provider anthropic --model
    /// gpt-4o`) fails fast with the offending name in the error instead of
    /// surfacing as an opaque API error.
    fn validate_model(&self, model: &str) -> Result<(), LlmError> {
        if model.is_empty() {
            return Err(LlmError::Configuration {
                message: "model name must not be empty".into(),
            });
        }
        Ok(())
    }

    /// Optional streaming callback invoked once per completed item.
    ///
    /// Callers (e.g. `llm_summary_pass`) can set this to persist results
    /// to SQLite as they arrive, enabling crash-safe partial completion
    /// without changing the store-all-at-end contract of `fetch_batch_results`.
    ///
    /// **Concurrency contract:** the callback may be invoked from multiple
    /// worker threads concurrently. Implementations must be `Fn + Send + Sync`
    /// and must serialize any shared mutable state internally (typically via
    /// `Mutex<Connection>`). Panics in the callback are caught and logged;
    /// they do not abort the batch. SQLite `INSERT OR IGNORE` on the
    /// `content_hash` primary key gracefully handles redundant writes from
    /// both streaming and `fetch_batch_results` paths.
    ///
    /// Default: no-op. The Anthropic path uses fetch-at-end semantics and
    /// ignores the callback.
    fn set_on_item_complete(&mut self, _cb: OnItemCallback) {}
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
        _kind: BatchKind,
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
            _kind: BatchKind,
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
