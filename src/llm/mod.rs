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
pub mod provider;
mod summary;

use std::time::Duration;

use serde::{Deserialize, Serialize};

// Re-export public API
pub use doc_comments::needs_doc_comment;
pub use hyde::hyde_query_pass;
pub use provider::BatchProvider;
pub use summary::llm_summary_pass;

use crate::Store;

/// An eligible chunk ready for LLM batch processing.
///
/// Contains the fields needed by both summary and HyDE passes:
/// content_hash (dedup key), content, chunk_type string, and language string.
pub(crate) struct EligibleChunk {
    pub content_hash: String,
    pub content: String,
    pub chunk_type: String,
    pub language: String,
    pub signature: String,
    pub name: String,
}

/// Scan the store for callable chunks eligible for LLM processing.
///
/// Shared between `llm_summary_pass` and `hyde_query_pass` (and `find_contrastive_neighbors`).
/// Applies the 4-condition filter: skip cached (by `purpose`), skip non-callable,
/// skip short content, skip windowed (window_idx > 0). Deduplicates by content_hash.
///
/// Returns at most `max_items` chunks. Pass `0` for unlimited.
pub(crate) fn collect_eligible_chunks(
    store: &Store,
    purpose: &str,
    max_items: usize,
) -> Result<(Vec<EligibleChunk>, usize, usize), LlmError> {
    let _span = tracing::info_span!("collect_eligible_chunks", purpose, max_items).entered();

    let mut cached = 0usize;
    let mut skipped = 0usize;
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;
    let mut items: Vec<EligibleChunk> = Vec::new();
    let mut queued_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let effective_limit = if max_items == 0 {
        usize::MAX
    } else {
        max_items
    };

    let mut batch_full = false;
    loop {
        let (chunks, next) = store.chunks_paged(cursor, PAGE_SIZE)?;
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
        let existing = store.get_summaries_by_hashes(&hashes, purpose)?;

        for cs in &chunks {
            if existing.contains_key(&cs.content_hash) {
                cached += 1;
                continue;
            }
            // Phase 5 follow-up: summarize all code chunk types, not just callable
            // ones. Structs, enums, traits, impls, classes, constants etc. all
            // benefit from a one-line summary because the dual-index router
            // routes conceptual/behavioral queries to the base (non-summary)
            // index — and that routing has zero effect when the gold answer is
            // a struct that doesn't have a summary to strip in the first place.
            //
            // NOTE: this filter is shared with hyde_query_pass. The HyDE pass
            // is currently disabled (--hyde-queries default is false) and
            // research data (training-data/research/enrichment.md) shows HyDE
            // is net-negative across most categories, so we don't worry about
            // it here. If HyDE is ever re-enabled, split this gate per-purpose
            // because HyDE should NOT broaden to type definitions.
            if !cs.chunk_type.is_code() {
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

            if queued_hashes.insert(cs.content_hash.clone()) {
                items.push(EligibleChunk {
                    content_hash: cs.content_hash.clone(),
                    content: cs.content.clone(),
                    chunk_type: cs.chunk_type.to_string(),
                    language: cs.language.to_string(),
                    signature: cs.signature.clone(),
                    name: cs.name.clone(),
                });
                if items.len() >= effective_limit {
                    batch_full = true;
                    break;
                }
            }
        }
        if batch_full {
            break;
        }
    }

    Ok((items, cached, skipped))
}

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
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

const API_BASE: &str = "https://api.anthropic.com/v1";
const API_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-haiku-4-5";
const MAX_TOKENS: u32 = 100;
/// Max content chars for LLM prompts.
/// Configurable via `CQS_LLM_MAX_CONTENT_CHARS` environment variable.
fn max_content_chars() -> usize {
    static SIZE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        match std::env::var("CQS_LLM_MAX_CONTENT_CHARS") {
            Ok(val) => match val.parse::<usize>() {
                Ok(n) if n > 0 => {
                    tracing::info!(max_chars = n, "CQS_LLM_MAX_CONTENT_CHARS override");
                    n
                }
                _ => {
                    tracing::warn!(value = %val, "Invalid CQS_LLM_MAX_CONTENT_CHARS, using default 8000");
                    8000
                }
            },
            Err(_) => 8000,
        }
    })
}
const MIN_CONTENT_CHARS: usize = 50;
const MAX_BATCH_SIZE: usize = 10_000;
/// Max tokens for HyDE query predictions (3-5 short queries).
const HYDE_MAX_TOKENS: u32 = 150;
/// Poll interval for batch completion
const BATCH_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// LLM API provider type.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmProvider {
    /// Anthropic Messages Batches API (default)
    Anthropic,
    // Future: OpenAI, Local, etc.
}

/// Resolved LLM configuration (env vars > config file > constants).
#[derive(Debug)]
pub struct LlmConfig {
    pub provider: LlmProvider,
    pub api_base: String,
    pub model: String,
    pub max_tokens: u32,
    /// Max tokens for HyDE query predictions (default: 150).
    pub hyde_max_tokens: u32,
}

impl LlmConfig {
    /// Resolve config with priority: env vars > config file > hardcoded constants.
    ///
    /// SEC-V1.25-13: Returns `Err` if `CQS_LLM_API_BASE` uses cleartext `http://`
    /// and the explicit opt-in `CQS_LLM_ALLOW_INSECURE=1` is not set. Prevents
    /// the API key from being sent in the clear on every call.
    pub fn resolve(config: &crate::config::Config) -> Result<Self, LlmError> {
        let _span = tracing::info_span!("resolve_llm_config").entered();

        let (api_base, api_base_source) = if let Ok(val) = std::env::var("CQS_LLM_API_BASE") {
            (val, "env:CQS_LLM_API_BASE")
        } else if let Ok(val) = std::env::var("CQS_API_BASE") {
            (val, "env:CQS_API_BASE")
        } else if let Some(val) = config.llm_api_base.clone() {
            (val, "config")
        } else {
            (API_BASE.to_string(), "default")
        };
        tracing::debug!(source = api_base_source, "api_base resolved");

        if !api_base.starts_with("https://") {
            // Strip userinfo from URL before logging to avoid leaking credentials
            let safe_url = api_base
                .find("://")
                .and_then(|scheme_end| {
                    let after_scheme = &api_base[scheme_end + 3..];
                    after_scheme.find('@').map(|at| {
                        format!(
                            "{}://***@{}",
                            &api_base[..scheme_end],
                            &after_scheme[at + 1..]
                        )
                    })
                })
                .unwrap_or_else(|| api_base.clone());
            // SEC-V1.25-13: cleartext http:// leaks the API key on every call.
            // A `warn!` alone is not enough — demand explicit opt-in via
            // CQS_LLM_ALLOW_INSECURE=1 for localhost testing.
            if api_base.starts_with("http://") {
                if std::env::var("CQS_LLM_ALLOW_INSECURE").as_deref() != Ok("1") {
                    tracing::error!(
                        api_base = %safe_url,
                        "CQS_LLM_API_BASE uses http:// (cleartext); refusing to send API key in clear. \
                         Set CQS_LLM_ALLOW_INSECURE=1 to override (e.g. for localhost testing)."
                    );
                    return Err(LlmError::Api {
                        status: 0,
                        message: format!(
                            "CQS_LLM_API_BASE uses http:// (cleartext): {}. API key would be sent in clear. \
                             Set CQS_LLM_ALLOW_INSECURE=1 to override (e.g. for localhost testing).",
                            safe_url
                        ),
                    });
                }
                tracing::warn!(
                    api_base = %safe_url,
                    "CQS_LLM_API_BASE is http://; CQS_LLM_ALLOW_INSECURE acknowledged — API key sent in clear"
                );
            } else {
                tracing::warn!(
                    api_base = %safe_url,
                    "LLM API base does not use HTTPS — API key will be sent in cleartext"
                );
            }
        }

        let provider = match std::env::var("CQS_LLM_PROVIDER").ok().as_deref() {
            Some("anthropic") | None => {
                tracing::debug!(
                    source = "env/default",
                    provider = "anthropic",
                    "provider resolved"
                );
                LlmProvider::Anthropic
            }
            Some(other) => {
                tracing::warn!(
                    provider = other,
                    "Unknown CQS_LLM_PROVIDER, defaulting to anthropic"
                );
                LlmProvider::Anthropic
            }
        };

        let (model, model_source) = if let Ok(val) = std::env::var("CQS_LLM_MODEL") {
            (val, "env:CQS_LLM_MODEL")
        } else if let Some(val) = config.llm_model.clone() {
            (val, "config")
        } else {
            (MODEL.to_string(), "default")
        };
        tracing::debug!(source = model_source, "model resolved");

        let (max_tokens, max_tokens_source) = match std::env::var("CQS_LLM_MAX_TOKENS") {
            Ok(s) => match s.parse::<u32>() {
                Ok(v) => (v, "env:CQS_LLM_MAX_TOKENS"),
                Err(e) => {
                    tracing::warn!(
                        value = %s,
                        error = %e,
                        "CQS_LLM_MAX_TOKENS is set but not a valid u32, falling back"
                    );
                    if let Some(v) = config.llm_max_tokens {
                        (v, "config")
                    } else {
                        (MAX_TOKENS, "default")
                    }
                }
            },
            Err(_) => {
                if let Some(v) = config.llm_max_tokens {
                    (v, "config")
                } else {
                    (MAX_TOKENS, "default")
                }
            }
        };
        tracing::debug!(source = max_tokens_source, "max_tokens resolved");

        let hyde_max_tokens = std::env::var("CQS_HYDE_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(config.llm_hyde_max_tokens)
            .unwrap_or(HYDE_MAX_TOKENS);

        Ok(Self {
            provider,
            api_base,
            model,
            max_tokens,
            hyde_max_tokens,
        })
    }
}

/// Create a batch provider from the resolved config.
///
/// EX-31/EX-34: Single factory, provider-aware. Currently only Anthropic is supported.
pub fn create_client(llm_config: LlmConfig) -> Result<LlmClient, LlmError> {
    let _span = tracing::info_span!("create_client", provider = ?llm_config.provider).entered();
    // EX-34: When adding providers, match on llm_config.provider here
    // and return the appropriate Box<dyn BatchProvider>.
    let env_var = match llm_config.provider {
        LlmProvider::Anthropic => "ANTHROPIC_API_KEY",
    };
    let api_key = std::env::var(env_var).map_err(|_| {
        LlmError::ApiKeyMissing(format!(
            "{env_var} environment variable required for LLM features"
        ))
    })?;
    LlmClient::new(&api_key, llm_config)
}

/// Claude API client for generating summaries.
pub struct LlmClient {
    http: reqwest::blocking::Client,
    api_key: String,
    llm_config: LlmConfig,
}

impl LlmClient {
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

/// Validates whether a string is a properly formatted Anthropic batch ID.
///
/// Anthropic batch IDs start with `msgbatch_`, contain only ASCII
/// alphanumeric characters or underscores, and are under 100 characters.
///
/// Other providers may have different ID formats; this function is
/// Anthropic-specific. The [`BatchProvider::is_valid_batch_id`] trait
/// method dispatches to the correct provider validation.
fn is_valid_anthropic_batch_id(id: &str) -> bool {
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
    use std::sync::Mutex;

    /// Mutex to serialize tests that manipulate CQS_LLM_* env vars.
    /// Env vars are process-global — concurrent test threads race on set/remove.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    type SavedEnv = [Option<String>; 4];

    fn save_llm_env_vars() -> SavedEnv {
        [
            std::env::var("CQS_LLM_MODEL").ok(),
            std::env::var("CQS_API_BASE").ok(),
            std::env::var("CQS_LLM_API_BASE").ok(),
            std::env::var("CQS_LLM_MAX_TOKENS").ok(),
        ]
    }

    fn restore_llm_env_vars(saved: SavedEnv) {
        let names = [
            "CQS_LLM_MODEL",
            "CQS_API_BASE",
            "CQS_LLM_API_BASE",
            "CQS_LLM_MAX_TOKENS",
        ];
        for (name, val) in names.iter().zip(saved.into_iter()) {
            match val {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn is_valid_batch_id_accepts_real_ids() {
        assert!(is_valid_anthropic_batch_id("msgbatch_abc123"));
        assert!(is_valid_anthropic_batch_id(
            "msgbatch_0123456789abcdef_ABCDEF"
        ));
    }

    #[test]
    fn is_valid_batch_id_rejects_crafted() {
        assert!(!is_valid_anthropic_batch_id("../../v1/complete"));
        assert!(!is_valid_anthropic_batch_id(
            "msgbatch_abc?redirect=evil.com"
        ));
        assert!(!is_valid_anthropic_batch_id(""));
        assert!(!is_valid_anthropic_batch_id("not_a_batch"));
        assert!(!is_valid_anthropic_batch_id(
            &("msgbatch_".to_string() + &"a".repeat(200))
        ));
    }

    #[test]
    fn llm_config_defaults_from_empty_config() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_llm_env_vars();
        std::env::remove_var("CQS_LLM_MODEL");
        std::env::remove_var("CQS_API_BASE");
        std::env::remove_var("CQS_LLM_API_BASE");
        std::env::remove_var("CQS_LLM_MAX_TOKENS");

        let config = crate::config::Config::default();
        let llm = LlmConfig::resolve(&config).unwrap();

        restore_llm_env_vars(saved);

        assert_eq!(llm.api_base, API_BASE);
        assert_eq!(llm.model, MODEL);
        assert_eq!(llm.max_tokens, MAX_TOKENS);
    }

    #[test]
    fn llm_config_from_config_file_fields() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let config = crate::config::Config {
            llm_model: Some("claude-sonnet-4-20250514".to_string()),
            llm_api_base: Some("https://custom.api/v1".to_string()),
            llm_max_tokens: Some(200),
            ..Default::default()
        };
        let llm = LlmConfig::resolve(&config).unwrap();
        assert_eq!(llm.model, "claude-sonnet-4-20250514");
        assert_eq!(llm.api_base, "https://custom.api/v1");
        assert_eq!(llm.max_tokens, 200);
    }

    #[test]
    fn llm_config_env_overrides_config_file() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let config = crate::config::Config {
            llm_model: Some("from-config".to_string()),
            llm_api_base: Some("https://from-config/v1".to_string()),
            llm_max_tokens: Some(200),
            ..Default::default()
        };

        // Set env vars (scoped to this test via unsafe + cleanup)
        std::env::set_var("CQS_LLM_MODEL", "from-env");
        std::env::set_var("CQS_API_BASE", "https://from-env/v1");
        std::env::remove_var("CQS_LLM_API_BASE"); // ensure primary is clear
        std::env::set_var("CQS_LLM_MAX_TOKENS", "500");

        let llm = LlmConfig::resolve(&config).unwrap();

        // Clean up env vars
        std::env::remove_var("CQS_LLM_MODEL");
        std::env::remove_var("CQS_API_BASE");
        std::env::remove_var("CQS_LLM_MAX_TOKENS");

        assert_eq!(llm.model, "from-env");
        assert_eq!(llm.api_base, "https://from-env/v1");
        assert_eq!(llm.max_tokens, 500);
    }

    // AD-32: CQS_LLM_API_BASE takes priority over CQS_API_BASE
    #[test]
    fn llm_config_llm_api_base_takes_precedence() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // Save all env vars that LlmConfig::resolve reads
        let saved = save_llm_env_vars();

        std::env::set_var("CQS_LLM_API_BASE", "https://primary/v1");
        std::env::set_var("CQS_API_BASE", "https://fallback/v1");
        std::env::remove_var("CQS_LLM_MODEL");
        std::env::remove_var("CQS_LLM_MAX_TOKENS");

        let config = crate::config::Config::default();
        let llm = LlmConfig::resolve(&config).unwrap();

        restore_llm_env_vars(saved);

        assert_eq!(
            llm.api_base, "https://primary/v1",
            "CQS_LLM_API_BASE should take precedence over CQS_API_BASE"
        );
    }

    // AD-32: CQS_API_BASE still works as fallback
    #[test]
    fn llm_config_api_base_fallback_still_works() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_llm_env_vars();

        std::env::remove_var("CQS_LLM_API_BASE");
        std::env::set_var("CQS_API_BASE", "https://legacy/v1");
        std::env::remove_var("CQS_LLM_MODEL");
        std::env::remove_var("CQS_LLM_MAX_TOKENS");

        let config = crate::config::Config::default();
        let llm = LlmConfig::resolve(&config).unwrap();

        restore_llm_env_vars(saved);

        assert_eq!(
            llm.api_base, "https://legacy/v1",
            "CQS_API_BASE should work as fallback when CQS_LLM_API_BASE is not set"
        );
    }

    // SEC-V1.25-13: http:// is rejected unless CQS_LLM_ALLOW_INSECURE=1 is also set.
    #[test]
    fn llm_config_rejects_http_without_allow_insecure() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_llm_env_vars();
        let saved_allow = std::env::var("CQS_LLM_ALLOW_INSECURE").ok();

        std::env::set_var("CQS_LLM_API_BASE", "http://localhost:8080/v1");
        std::env::remove_var("CQS_LLM_ALLOW_INSECURE");
        std::env::remove_var("CQS_API_BASE");
        std::env::remove_var("CQS_LLM_MODEL");
        std::env::remove_var("CQS_LLM_MAX_TOKENS");

        let config = crate::config::Config::default();
        let result = LlmConfig::resolve(&config);

        restore_llm_env_vars(saved);
        match saved_allow {
            Some(v) => std::env::set_var("CQS_LLM_ALLOW_INSECURE", v),
            None => std::env::remove_var("CQS_LLM_ALLOW_INSECURE"),
        }

        assert!(
            result.is_err(),
            "http:// LLM API base must be rejected without CQS_LLM_ALLOW_INSECURE=1"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("cleartext") || err_msg.contains("http://"),
            "error should mention cleartext http://, got: {}",
            err_msg
        );
    }

    // SEC-V1.25-13: With CQS_LLM_ALLOW_INSECURE=1 the http:// override is allowed.
    #[test]
    fn llm_config_allows_http_with_allow_insecure() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_llm_env_vars();
        let saved_allow = std::env::var("CQS_LLM_ALLOW_INSECURE").ok();

        std::env::set_var("CQS_LLM_API_BASE", "http://localhost:8080/v1");
        std::env::set_var("CQS_LLM_ALLOW_INSECURE", "1");
        std::env::remove_var("CQS_API_BASE");
        std::env::remove_var("CQS_LLM_MODEL");
        std::env::remove_var("CQS_LLM_MAX_TOKENS");

        let config = crate::config::Config::default();
        let result = LlmConfig::resolve(&config);

        restore_llm_env_vars(saved);
        match saved_allow {
            Some(v) => std::env::set_var("CQS_LLM_ALLOW_INSECURE", v),
            None => std::env::remove_var("CQS_LLM_ALLOW_INSECURE"),
        }

        assert!(
            result.is_ok(),
            "CQS_LLM_ALLOW_INSECURE=1 should permit http:// base"
        );
        assert_eq!(result.unwrap().api_base, "http://localhost:8080/v1");
    }

    #[test]
    fn llm_config_invalid_max_tokens_env_falls_through() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let config = crate::config::Config {
            llm_max_tokens: Some(300),
            ..Default::default()
        };

        std::env::set_var("CQS_LLM_MAX_TOKENS", "not_a_number");
        let llm = LlmConfig::resolve(&config).unwrap();
        std::env::remove_var("CQS_LLM_MAX_TOKENS");

        // Invalid env var should fall through to config value
        assert_eq!(llm.max_tokens, 300);
    }

    // ===== TC-21: JSONL parsing tests =====

    /// Helper: parse JSONL body into a HashMap<custom_id, text>, replicating
    /// the inline logic from `LlmClient::fetch_batch_results`.
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
