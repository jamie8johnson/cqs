//! Claude API client for LLM-generated function summaries (SQ-6).
//!
//! Uses `reqwest::blocking` to avoid nested tokio runtime issues
//! (the Store already uses `rt.block_on()`).
//!
//! The summary pass uses the Batches API for throughput (no RPM limit, 50% discount).
//! Individual summarize_chunk() is available for single-chunk fallback.
//!
//! Split into submodules by concern:
//! - `prompts` - prompt construction (summary, doc, HyDE)
//! - `batch` - batch submission, polling, result fetching
//! - `summary` - llm_summary_pass orchestration
//! - `doc_comments` - doc comment generation pass + needs_doc_comment
//! - `hyde` - HyDE query prediction pass

mod batch;
mod doc_comments;
mod hyde;
mod prompts;
mod summary;

use std::time::Duration;

use serde::{Deserialize, Serialize};

// Re-export public API
pub use doc_comments::needs_doc_comment;
pub use hyde::hyde_query_pass;
pub use summary::llm_summary_pass;

// doc_comment_pass returns Vec<crate::doc_writer::DocCommentResult>
pub use doc_comments::doc_comment_pass;

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
/// Max tokens for HyDE query predictions (3-5 short queries).
const HYDE_MAX_TOKENS: u32 = 150;
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

impl Client {
    /// Creates a new LLM client instance with the specified API key and configuration.
    ///
    /// Initializes an HTTP client with a 60-second timeout and disables automatic redirect following. The API key is stored for use in subsequent requests.
    ///
    /// # Arguments
    ///
    /// * `api_key` - The API key for authenticating requests to the LLM service
    /// * `llm_config` - Configuration settings for the LLM client behavior
    ///
    /// # Returns
    ///
    /// A new `Self` instance ready to make LLM requests, or an `LlmError` if the HTTP client initialization fails.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be constructed.
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
}

/// Validates whether a string is a properly formatted batch ID.
///
/// # Arguments
///
/// * `id` - The string to validate as a batch ID
///
/// # Returns
///
/// Returns `true` if the ID starts with "msgbatch_", is less than 100 characters long, and contains only ASCII alphanumeric characters or underscores. Returns `false` otherwise.
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

/// A summary entry ready for storage.
pub struct SummaryEntry {
    pub content_hash: String,
    pub summary: String,
    pub model: String,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ===== TC-21: JSONL parsing tests =====

    /// Helper: parse JSONL body into a HashMap<custom_id, text>, replicating
    /// the inline logic from `Client::fetch_batch_results`.
    fn parse_batch_results_jsonl(body: &str) -> std::collections::HashMap<String, String> {
        let mut results = std::collections::HashMap::new();
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(result) = serde_json::from_str::<BatchResult>(line) {
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
                }
            }
        }
        results
    }

    #[test]
    fn parse_jsonl_succeeded_result() {
        let jsonl = r#"{"custom_id":"hash_abc","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"Parses configuration files."}]}}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results.get("hash_abc").unwrap(),
            "Parses configuration files."
        );
    }

    #[test]
    fn parse_jsonl_filters_non_succeeded() {
        let jsonl = r#"{"custom_id":"hash_fail","result":{"type":"errored","message":null}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert!(
            results.is_empty(),
            "Non-succeeded results should be filtered out"
        );
    }

    #[test]
    fn parse_jsonl_multiple_lines() {
        let jsonl = concat!(
            r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"First summary."}]}}}"#,
            "\n",
            r#"{"custom_id":"h2","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"Second summary."}]}}}"#,
            "\n",
            r#"{"custom_id":"h3","result":{"type":"errored","message":null}}"#,
        );
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 2);
        assert_eq!(results.get("h1").unwrap(), "First summary.");
        assert_eq!(results.get("h2").unwrap(), "Second summary.");
        assert!(!results.contains_key("h3"));
    }

    #[test]
    fn parse_jsonl_skips_empty_lines() {
        let jsonl = concat!(
            "\n",
            r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"Summary."}]}}}"#,
            "\n",
            "\n",
            "   \n",
        );
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        assert_eq!(results.get("h1").unwrap(), "Summary.");
    }

    #[test]
    fn parse_jsonl_skips_invalid_json() {
        let jsonl = concat!(
            "not valid json\n",
            r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"Valid."}]}}}"#,
        );
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.len(), 1);
        assert_eq!(results.get("h1").unwrap(), "Valid.");
    }

    #[test]
    fn parse_jsonl_trims_whitespace_text() {
        let jsonl = r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"  Trimmed summary.  "}]}}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.get("h1").unwrap(), "Trimmed summary.");
    }

    #[test]
    fn parse_jsonl_skips_empty_text() {
        let jsonl = r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"   "}]}}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert!(results.is_empty(), "Whitespace-only text should be skipped");
    }

    #[test]
    fn parse_jsonl_finds_text_block_among_others() {
        // Content has a non-text block followed by a text block
        let jsonl = r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"tool_use","text":null},{"type":"text","text":"Found it."}]}}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(results.get("h1").unwrap(), "Found it.");
    }

    #[test]
    fn parse_jsonl_no_message_on_succeeded() {
        // Succeeded but message is null — should produce no result
        let jsonl = r#"{"custom_id":"h1","result":{"type":"succeeded","message":null}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert!(results.is_empty());
    }

    #[test]
    fn parse_jsonl_truncated_json() {
        // First line valid, second line truncated mid-JSON → only first result returned
        let jsonl = concat!(
            r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"Valid line."}]}}}"#,
            "\n",
            r#"{"custom_id":"h2","result":{"type":"succeeded","message":{"content":[{"type":"te"#,
        );
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(
            results.len(),
            1,
            "Only the complete first line should parse"
        );
        assert_eq!(results.get("h1").unwrap(), "Valid line.");
        assert!(!results.contains_key("h2"));
    }

    #[test]
    fn parse_jsonl_unicode_in_summary() {
        // Summary contains CJK characters and emoji — should be preserved exactly
        let summary = "代码解析模块 🦀 parses Rust source files";
        let jsonl = format!(
            r#"{{"custom_id":"h1","result":{{"type":"succeeded","message":{{"content":[{{"type":"text","text":"{}"}}]}}}}}}"#,
            summary
        );
        let results = parse_batch_results_jsonl(&jsonl);
        assert_eq!(results.len(), 1);
        assert_eq!(results.get("h1").unwrap(), summary);
    }

    #[test]
    fn parse_jsonl_very_long_summary() {
        // 100k char summary → stored without truncation
        let long_text: String = "x".repeat(100_000);
        let jsonl = format!(
            r#"{{"custom_id":"h1","result":{{"type":"succeeded","message":{{"content":[{{"type":"text","text":"{}"}}]}}}}}}"#,
            long_text
        );
        let results = parse_batch_results_jsonl(&jsonl);
        assert_eq!(results.len(), 1);
        assert_eq!(results.get("h1").unwrap().len(), 100_000);
    }

    #[test]
    fn parse_jsonl_duplicate_custom_ids() {
        // Two lines with the same custom_id → HashMap keeps last (1 result)
        let jsonl = concat!(
            r#"{"custom_id":"same","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"First."}]}}}"#,
            "\n",
            r#"{"custom_id":"same","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"Second."}]}}}"#,
        );
        let results = parse_batch_results_jsonl(jsonl);
        assert_eq!(
            results.len(),
            1,
            "Duplicate custom_ids collapse to one entry"
        );
        assert_eq!(
            results.get("same").unwrap(),
            "Second.",
            "HashMap last-write-wins keeps the second entry"
        );
    }

    #[test]
    fn parse_jsonl_null_message_on_succeeded() {
        // "message":null on a succeeded result → no result stored
        // (This is the explicit adversarial variant — succeeded type but null message)
        let jsonl = r#"{"custom_id":"h1","result":{"type":"succeeded","message":null}}"#;
        let results = parse_batch_results_jsonl(jsonl);
        assert!(
            results.is_empty(),
            "succeeded + null message should produce no result"
        );
    }
}
