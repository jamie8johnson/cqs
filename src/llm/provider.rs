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
    /// Build a provider from resolved config, optionally registering a
    /// per-item streaming-persist callback. The Anthropic provider
    /// ignores the callback (fetch-at-end semantics); the Local provider
    /// stores it for use during the worker-pool fanout.
    fn build(
        &self,
        cfg: LlmConfig,
        on_item: Option<OnItemCallback>,
    ) -> Result<Box<dyn BatchProvider>, LlmError>;
}

/// Callback type for the per-item streaming persist hook.
///
/// `(custom_id, text)` — see provider-specific construction APIs (e.g.
/// [`crate::llm::local::LocalProvider::with_on_item_complete`]) for the
/// concurrency contract.
///
/// **Concurrency contract:** the callback may be invoked from multiple
/// worker threads concurrently. Implementations must be `Fn + Send + Sync`
/// and must serialize any shared mutable state internally (typically via
/// `Mutex<Connection>`). Panics in the callback are caught and logged;
/// they do not abort the batch. SQLite `INSERT OR IGNORE` on the
/// `content_hash` primary key gracefully handles redundant writes from
/// both streaming and `fetch_batch_results` paths.
pub type OnItemCallback = Box<dyn Fn(&str, &str) + Send + Sync>;

/// Typed per-[`BatchKind`] prompt context carried by each [`BatchSubmitItem`].
///
/// Each variant carries exactly the payload the matching prompt builder
/// consumes, so a construction-site typo cannot send the wrong field. The
/// `Prebuilt` path ignores per-item context, so it carries no payload — the
/// "unused" state is a unit variant, enforced by the type rather than by
/// convention.
///
/// | Variant      | Prompt builder        | Payload                    |
/// |--------------|-----------------------|----------------------------|
/// | `Prebuilt`   | content is verbatim   | none                       |
/// | `DocComment` | `build_doc_prompt`    | chunk-type label (`ct`)    |
/// | `Hyde`       | `build_hyde_prompt`   | function signature (`sig`) |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptContext {
    /// Pre-built prompts — [`BatchSubmitItem::content`] IS the user message,
    /// so no per-item context is consumed.
    Prebuilt,
    /// Doc-comment generation — carries the chunk-type label that
    /// `build_doc_prompt` substitutes into "Write a concise doc comment for
    /// this {chunk_type}".
    DocComment { chunk_type: String },
    /// HyDE query prediction — carries the function signature that
    /// `build_hyde_prompt` renders on the "Signature:" line.
    Hyde { signature: String },
}

impl PromptContext {
    /// The string the selected prompt builder consumes. `Prebuilt` yields the
    /// empty string because its builder ignores per-item context.
    pub(super) fn payload(&self) -> &str {
        match self {
            Self::Prebuilt => "",
            Self::DocComment { chunk_type } => chunk_type,
            Self::Hyde { signature } => signature,
        }
    }
}

/// A single item in a batch submission.
/// Named fields replace the opaque `(String, String, String, String)` tuple
/// to prevent positional errors at call sites.
pub struct BatchSubmitItem {
    /// Unique identifier for correlating results (typically content_hash)
    pub custom_id: String,
    /// Source code content or pre-built prompt
    pub content: String,
    /// Typed per-kind prompt context. Each variant carries only the payload
    /// its prompt builder reads; the `Prebuilt` path carries none. Replaces
    /// the former overloaded `String` whose meaning depended on the
    /// [`BatchKind`] chosen at submit time.
    pub context: PromptContext,
    /// Programming language name
    pub language: String,
}

/// Which prompt builder a batch submission uses.
///
/// The variant selects which `build_*_prompt` closure `submit_batch_inner` /
/// `submit_via_chat_completions` runs. Adding a fourth purpose (e.g.
/// `Classification`, `ContrastiveRepair`, `CodeReview`) is a single new
/// variant + the impl's `match` arm — no new trait method × impl on every
/// `BatchProvider` (4 sites: `LlmClient`, `LocalProvider`,
/// `MockBatchProvider`, `DefaultValidationProvider`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchKind {
    /// Pre-built prompts — `BatchSubmitItem::content` IS the user message.
    /// Used by the contrastive summary path which pre-builds prompts with
    /// neighbor context. Pairs with [`PromptContext::Prebuilt`] (no payload);
    /// the `language` field is ignored.
    Prebuilt,
    /// Doc-comment generation — uses `LlmClient::build_doc_prompt`, which
    /// reads the chunk-type label from [`PromptContext::DocComment`].
    DocComment,
    /// HyDE query prediction — uses `LlmClient::build_hyde_prompt`, which
    /// reads the function signature from [`PromptContext::Hyde`].
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
///
/// `Send + Sync` matches the sibling traits `VectorIndex`, `IndexBackend`,
/// `Reranker`. Both shipping impls (`LlmClient` in `llm/batch.rs`,
/// `LocalProvider` in `llm/local.rs`) satisfy it; making it explicit keeps a
/// future async-batch refactor from surfacing as a confusing object-safety
/// error.
pub trait BatchProvider: Send + Sync {
    /// Submit a batch under the given `kind`. The kind selects which prompt
    /// builder constructs the user message from each `BatchSubmitItem`'s
    /// `(content, context, language)` triple (or, for [`BatchKind::Prebuilt`],
    /// uses `content` directly).
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

    /// Validate that the model name configured for this provider matches the
    /// provider's expected naming convention. Default accepts any non-empty
    /// model so impls can opt in incrementally.
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

    // The optional streaming per-item callback is passed at construction
    // (via [`crate::llm::create_client`]'s `on_item` arg, or
    // [`LocalProvider::with_on_item_complete`] for direct construction in
    // tests). Keeping it off the trait keeps every method `&self` and lets
    // callers hold `&dyn BatchProvider` without wrapping in a `Mutex` just to
    // register the callback.
    //
    // The Anthropic provider ignores the callback (fetch-at-end semantics).
    // The Local provider stores it under interior mutability.
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
