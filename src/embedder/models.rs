//! Embedding model configuration: presets, resolution, config-file parsing.

use serde::Deserialize;

/// Configuration for an embedding model.
///
/// Defines everything needed to download, load, and use an ONNX embedding model:
/// repository location, file paths, dimensions, and text prefixes.
#[derive(Debug, Clone)]
pub struct ModelConfig {
    /// Short human-readable name (e.g. "e5-base", "bge-large")
    pub name: String,
    /// HuggingFace repo ID (e.g. "intfloat/e5-base-v2")
    pub repo: String,
    /// Path to ONNX model file within the HuggingFace repo (always forward-slash separated,
    /// e.g., `"onnx/model.onnx"`). Not a filesystem path -- HuggingFace Hub resolves this.
    pub onnx_path: String,
    /// Path to tokenizer file within the repo
    pub tokenizer_path: String,
    /// Embedding dimension (1024 for BGE-large, 768 for E5-base)
    pub dim: usize,
    /// Maximum input sequence length in tokens
    pub max_seq_length: usize,
    /// Prefix prepended to queries (e.g. "query: " for E5)
    pub query_prefix: String,
    /// Prefix prepended to documents (e.g. "passage: " for E5)
    pub doc_prefix: String,
}

/// Default model repo ID. Must match `ModelConfig::default_model().repo`.
/// Kept as a const for use in store validation and metadata (which need compile-time strings).
pub const DEFAULT_MODEL_REPO: &str = "BAAI/bge-large-en-v1.5";

/// Default embedding dimension. Must match `ModelConfig::default_model().dim`.
/// Kept as a const for use in test helpers and compile-time array sizing.
pub const DEFAULT_DIM: usize = 1024;

impl ModelConfig {
    /// The project default model. Single source of truth for all fallback paths.
    ///
    /// Change this ONE function to switch the default model for the entire project.
    /// Everything else (DEFAULT_MODEL_REPO, EMBEDDING_DIM, ModelInfo::default(),
    /// serde defaults, resolve() fallbacks) derives from this.
    pub fn default_model() -> Self {
        Self::bge_large()
    }

    /// E5-base-v2: 768-dim, 512 tokens. Lightweight preset.
    pub fn e5_base() -> Self {
        Self {
            name: "e5-base".to_string(),
            repo: "intfloat/e5-base-v2".to_string(),
            onnx_path: "onnx/model.onnx".to_string(),
            tokenizer_path: "tokenizer.json".to_string(),
            dim: 768,
            max_seq_length: 512,
            query_prefix: "query: ".to_string(),
            doc_prefix: "passage: ".to_string(),
        }
    }

    /// v9-200k LoRA: E5-base fine-tuned with call-graph false-negative filtering.
    /// 768-dim, 512 tokens. 90.5% R@1 on expanded eval (296 queries, 7 languages).
    pub fn v9_200k() -> Self {
        Self {
            name: "v9-200k".to_string(),
            repo: "jamie8johnson/e5-base-v2-code-search".to_string(),
            onnx_path: "model.onnx".to_string(),
            tokenizer_path: "tokenizer.json".to_string(),
            dim: 768,
            max_seq_length: 512,
            query_prefix: "query: ".to_string(),
            doc_prefix: "passage: ".to_string(),
        }
    }

    /// BGE-large-en-v1.5: 1024-dim, 512 tokens. Higher quality, slower.
    pub fn bge_large() -> Self {
        Self {
            name: "bge-large".to_string(),
            repo: "BAAI/bge-large-en-v1.5".to_string(),
            onnx_path: "onnx/model.onnx".to_string(),
            tokenizer_path: "tokenizer.json".to_string(),
            dim: 1024,
            max_seq_length: 512,
            query_prefix: "Represent this sentence for searching relevant passages: ".to_string(),
            doc_prefix: String::new(),
        }
    }

    /// Look up a preset by short name ("e5-base") or repo ID ("intfloat/e5-base-v2").
    ///
    /// Returns `None` for unknown names.
    pub const PRESET_NAMES: &'static [&'static str] = &["e5-base", "v9-200k", "bge-large"];

    pub fn from_preset(name: &str) -> Option<Self> {
        match name {
            "e5-base" | "intfloat/e5-base-v2" => Some(Self::e5_base()),
            "v9-200k" | "jamie8johnson/e5-base-v2-code-search" => Some(Self::v9_200k()),
            "bge-large" | "BAAI/bge-large-en-v1.5" => Some(Self::bge_large()),
            _ => None,
        }
    }

    /// Resolve model config from (in priority order): CLI flag, env var, config file, default.
    ///
    /// Unknown preset names log a warning and fall back to default.
    pub fn resolve(cli_model: Option<&str>, config_embedding: Option<&EmbeddingConfig>) -> Self {
        let _span = tracing::info_span!("resolve_model_config").entered();

        // 1. CLI flag (highest priority)
        if let Some(name) = cli_model {
            if let Some(cfg) = Self::from_preset(name) {
                tracing::info!(model = %cfg.name, source = "cli", "Resolved model config");
                return cfg;
            }
            tracing::warn!(
                model = name,
                "Unknown model from CLI flag, falling back to default"
            );
            return Self::default_model();
        }

        // 2. Environment variable
        if let Ok(env_val) = std::env::var("CQS_EMBEDDING_MODEL") {
            if !env_val.is_empty() {
                if let Some(cfg) = Self::from_preset(&env_val) {
                    tracing::info!(model = %cfg.name, source = "env", "Resolved model config");
                    return cfg;
                }
                tracing::warn!(
                    model = %env_val,
                    "Unknown CQS_EMBEDDING_MODEL env var value, falling back to default"
                );
                return Self::default_model();
            }
        }

        // 3. Config file
        if let Some(embedding_cfg) = config_embedding {
            if let Some(cfg) = Self::from_preset(&embedding_cfg.model) {
                tracing::info!(model = %cfg.name, source = "config", "Resolved model config");
                return cfg;
            }
            // Not a known preset — check if custom fields are present
            let has_repo = embedding_cfg.repo.is_some();
            let has_dim = embedding_cfg.dim.is_some();
            if has_repo && has_dim {
                let dim = embedding_cfg.dim.expect("guarded by has_dim");
                if dim == 0 {
                    tracing::warn!(model = %embedding_cfg.model, "Custom model has dim=0, falling back to default");
                    return Self::default_model();
                }

                // SEC-28: Validate repo format — must be "org/model" without injection chars
                let repo = embedding_cfg.repo.as_ref().expect("guarded by has_repo");
                if !repo.contains('/')
                    || repo.contains('"')
                    || repo.contains('\n')
                    || repo.contains('\\')
                    || repo.contains(' ')
                    || repo.starts_with('/')
                    || repo.contains("..")
                {
                    tracing::warn!(
                        %repo,
                        "Custom model repo contains invalid characters, falling back to default"
                    );
                    return Self::default_model();
                }

                // SEC-20: Validate custom paths don't contain traversal
                let onnx_path = embedding_cfg
                    .onnx_path
                    .clone()
                    .unwrap_or_else(|| "onnx/model.onnx".to_string());
                let tokenizer_path = embedding_cfg
                    .tokenizer_path
                    .clone()
                    .unwrap_or_else(|| "tokenizer.json".to_string());
                for (label, path) in [
                    ("onnx_path", &onnx_path),
                    ("tokenizer_path", &tokenizer_path),
                ] {
                    if path.contains("..") || std::path::Path::new(path).is_absolute() {
                        tracing::warn!(%label, %path, "Custom model path contains traversal or is absolute, falling back to default");
                        return Self::default_model();
                    }
                }

                let cfg = Self {
                    name: embedding_cfg.model.clone(),
                    repo: embedding_cfg.repo.clone().expect("guarded by has_repo"),
                    onnx_path,
                    tokenizer_path,
                    dim,
                    max_seq_length: embedding_cfg.max_seq_length.unwrap_or(512),
                    query_prefix: embedding_cfg.query_prefix.clone().unwrap_or_default(),
                    doc_prefix: embedding_cfg.doc_prefix.clone().unwrap_or_default(),
                };
                tracing::info!(model = %cfg.name, source = "config-custom", "Resolved custom model config");
                return cfg;
            }
            tracing::warn!(
                model = %embedding_cfg.model,
                has_repo,
                has_dim,
                "Unknown model in config and missing required custom fields (repo, dim), falling back to default"
            );
        }

        // 4. Default — BGE-large since v1.9.0
        tracing::info!(
            model = "bge-large",
            source = "default",
            "Resolved model config"
        );
        Self::default_model()
    }

    /// Apply env var overrides to a resolved ModelConfig.
    /// CQS_MAX_SEQ_LENGTH overrides max_seq_length (for large-context models via CQS_ONNX_DIR).
    /// CQS_EMBEDDING_DIM overrides dim (for custom models where dim detection isn't automatic).
    pub fn apply_env_overrides(mut self) -> Self {
        if let Ok(val) = std::env::var("CQS_MAX_SEQ_LENGTH") {
            if let Ok(seq) = val.parse::<usize>() {
                tracing::info!(max_seq_length = seq, "CQS_MAX_SEQ_LENGTH override active");
                self.max_seq_length = seq;
            }
        }
        if let Ok(val) = std::env::var("CQS_EMBEDDING_DIM") {
            if let Ok(dim) = val.parse::<usize>() {
                if dim > 0 {
                    tracing::info!(dim, "CQS_EMBEDDING_DIM override active");
                    self.dim = dim;
                }
            }
        }
        self
    }
}

/// Config-file section for embedding model settings.
///
/// Parsed from `[embedding]` in the cqs config file.
/// All fields except `model` are optional — preset names fill them automatically.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingConfig {
    /// Model name or preset (default: "bge-large")
    #[serde(default = "default_model_name")]
    pub model: String,
    /// HuggingFace repo ID (required for custom models)
    pub repo: Option<String>,
    /// ONNX model path within repo
    pub onnx_path: Option<String>,
    /// Tokenizer path within repo
    pub tokenizer_path: Option<String>,
    /// Embedding dimension (required for custom models)
    pub dim: Option<usize>,
    /// Max sequence length
    pub max_seq_length: Option<usize>,
    /// Query prefix
    pub query_prefix: Option<String>,
    /// Document prefix
    pub doc_prefix: Option<String>,
}

fn default_model_name() -> String {
    ModelConfig::default_model().name
}

/// Model metadata for index initialization.
///
/// Construct via `ModelInfo::new()` with explicit name + dim, or
/// `ModelInfo::default()` for tests only (BGE-large, 1024-dim).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub dimensions: usize,
    pub version: String,
}

impl ModelInfo {
    /// Create ModelInfo with explicit model name and dimension.
    ///
    /// This is the preferred constructor for production code. The name and dim
    /// come from the Embedder at runtime.
    pub fn new(name: impl Into<String>, dim: usize) -> Self {
        ModelInfo {
            name: name.into(),
            dimensions: dim,
            version: "2".to_string(),
        }
    }

    /// Create ModelInfo with default model name and a specific dimension.
    ///
    /// Convenience for callers that only vary dimension (e.g., `Embedder::embedding_dim()`).
    pub fn with_dim(dim: usize) -> Self {
        Self::new(DEFAULT_MODEL_REPO, dim)
    }
}

impl Default for ModelInfo {
    /// Test-only default: BGE-large with default dim (1024).
    ///
    /// Production code should use `ModelInfo::new()` or `ModelInfo::with_dim()`.
    fn default() -> Self {
        ModelInfo {
            name: DEFAULT_MODEL_REPO.to_string(),
            dimensions: DEFAULT_DIM,
            version: "2".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mutex to serialize tests that manipulate CQS_EMBEDDING_MODEL env var.
    /// Env vars are process-global — concurrent test threads race on set/remove.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    // ===== Preset tests =====

    #[test]
    fn test_e5_base_preset() {
        let cfg = ModelConfig::e5_base();
        assert_eq!(cfg.name, "e5-base");
        assert_eq!(cfg.repo, "intfloat/e5-base-v2");
        assert_eq!(cfg.dim, 768);
        assert_eq!(cfg.max_seq_length, 512);
        assert_eq!(cfg.query_prefix, "query: ");
        assert_eq!(cfg.doc_prefix, "passage: ");
        assert_eq!(cfg.onnx_path, "onnx/model.onnx");
        assert_eq!(cfg.tokenizer_path, "tokenizer.json");
    }

    #[test]
    fn test_bge_large_preset() {
        let cfg = ModelConfig::bge_large();
        assert_eq!(cfg.name, "bge-large");
        assert_eq!(cfg.repo, "BAAI/bge-large-en-v1.5");
        assert_eq!(cfg.dim, 1024);
        assert_eq!(cfg.max_seq_length, 512);
        assert_eq!(
            cfg.query_prefix,
            "Represent this sentence for searching relevant passages: "
        );
        assert_eq!(cfg.doc_prefix, "");
    }

    #[test]
    fn test_v9_200k_preset() {
        let cfg = ModelConfig::v9_200k();
        assert_eq!(cfg.name, "v9-200k");
        assert_eq!(cfg.repo, "jamie8johnson/e5-base-v2-code-search");
        assert_eq!(cfg.dim, 768);
        assert_eq!(cfg.onnx_path, "model.onnx");
        assert_eq!(cfg.query_prefix, "query: ");
        assert_eq!(cfg.doc_prefix, "passage: ");
    }

    // ===== from_preset tests =====

    #[test]
    fn test_from_preset_short_name() {
        assert!(ModelConfig::from_preset("e5-base").is_some());
        assert!(ModelConfig::from_preset("v9-200k").is_some());
        assert!(ModelConfig::from_preset("bge-large").is_some());
    }

    #[test]
    fn test_from_preset_repo_id() {
        let cfg = ModelConfig::from_preset("intfloat/e5-base-v2").unwrap();
        assert_eq!(cfg.name, "e5-base");

        let cfg = ModelConfig::from_preset("jamie8johnson/e5-base-v2-code-search").unwrap();
        assert_eq!(cfg.name, "v9-200k");

        let cfg = ModelConfig::from_preset("BAAI/bge-large-en-v1.5").unwrap();
        assert_eq!(cfg.name, "bge-large");
    }

    #[test]
    fn test_from_preset_unknown() {
        assert!(ModelConfig::from_preset("unknown-model").is_none());
        assert!(ModelConfig::from_preset("").is_none());
    }

    // ===== resolve tests =====

    #[test]
    fn test_resolve_default() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // Clear env to ensure we get default
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "bge-large");
    }

    #[test]
    fn test_resolve_env_by_name() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "bge-large");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "bge-large");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_env_by_repo_id() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "BAAI/bge-large-en-v1.5");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "bge-large");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_cli_overrides_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "bge-large");
        let cfg = ModelConfig::resolve(Some("e5-base"), None);
        assert_eq!(cfg.name, "e5-base");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_unknown_env_warns_and_defaults() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "nonexistent-model");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "bge-large"); // falls back to default
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_unknown_cli_warns_and_defaults() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let cfg = ModelConfig::resolve(Some("nonexistent"), None);
        assert_eq!(cfg.name, "bge-large");
    }

    #[test]
    fn test_resolve_config_preset() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "bge-large".to_string(),
            repo: None,
            onnx_path: None,
            tokenizer_path: None,
            dim: None,
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let cfg = ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(cfg.name, "bge-large");
    }

    #[test]
    fn test_resolve_config_custom_model() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "my-custom".to_string(),
            repo: Some("my-org/my-model".to_string()),
            onnx_path: Some("model.onnx".to_string()),
            tokenizer_path: None,
            dim: Some(384),
            max_seq_length: Some(256),
            query_prefix: Some("search: ".to_string()),
            doc_prefix: None,
        };
        let cfg = ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(cfg.name, "my-custom");
        assert_eq!(cfg.repo, "my-org/my-model");
        assert_eq!(cfg.dim, 384);
        assert_eq!(cfg.max_seq_length, 256);
        assert_eq!(cfg.onnx_path, "model.onnx");
        assert_eq!(cfg.tokenizer_path, "tokenizer.json"); // default
        assert_eq!(cfg.query_prefix, "search: ");
        assert_eq!(cfg.doc_prefix, ""); // default
    }

    #[test]
    fn test_resolve_config_unknown_missing_fields_defaults() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "unknown".to_string(),
            repo: None, // missing required field
            onnx_path: None,
            tokenizer_path: None,
            dim: None, // missing required field
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let cfg = ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(cfg.name, "bge-large"); // falls back
    }

    // ===== EmbeddingConfig serde tests =====

    #[test]
    fn test_embedding_config_default_model() {
        let json = r#"{}"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.model, "bge-large");
    }

    #[test]
    fn test_embedding_config_explicit_model() {
        let json = r#"{"model": "bge-large"}"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.model, "bge-large");
    }

    #[test]
    fn test_embedding_config_custom_fields() {
        let json = r#"{
            "model": "custom",
            "repo": "org/model",
            "dim": 384,
            "query_prefix": "q: "
        }"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.model, "custom");
        assert_eq!(cfg.repo.unwrap(), "org/model");
        assert_eq!(cfg.dim.unwrap(), 384);
        assert_eq!(cfg.query_prefix.unwrap(), "q: ");
        assert!(cfg.doc_prefix.is_none());
    }

    #[test]
    fn test_resolve_empty_env_ignored() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_EMBEDDING_MODEL", "");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "bge-large");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_cli_overrides_config() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "bge-large".to_string(),
            repo: None,
            onnx_path: None,
            tokenizer_path: None,
            dim: None,
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let cfg = ModelConfig::resolve(Some("e5-base"), Some(&embedding_cfg));
        assert_eq!(cfg.name, "e5-base");
    }

    // ===== TC-31: multi-model dim-threading (ModelConfig) =====

    #[test]
    fn tc31_resolve_config_dim_zero_falls_back_to_default() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // TC-31.8: Custom config with dim=0 should be rejected and fall back to e5_base.
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = EmbeddingConfig {
            model: "zero-dim-model".to_string(),
            repo: Some("org/zero-dim".to_string()),
            onnx_path: None,
            tokenizer_path: None,
            dim: Some(0),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let cfg = ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(
            cfg.name, "bge-large",
            "dim=0 should cause fallback to default bge-large"
        );
        assert_eq!(cfg.dim, 1024, "Fallback should have BGE-large dim=1024");
    }

    // ===== TC-43: SEC-20 path traversal rejection tests =====

    #[test]
    fn test_sec20_onnx_path_traversal_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "evil-model".to_string(),
            repo: Some("evil/model".to_string()),
            onnx_path: Some("../../../etc/passwd".to_string()),
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name, "bge-large",
            "Traversal in onnx_path should fall back to default"
        );
    }

    #[test]
    fn test_sec20_tokenizer_path_traversal_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "evil-model".to_string(),
            repo: Some("evil/model".to_string()),
            onnx_path: Some("model.onnx".to_string()),
            tokenizer_path: Some("../../secret/tokenizer.json".to_string()),
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name, "bge-large",
            "Traversal in tokenizer_path should fall back to default"
        );
    }

    #[test]
    fn test_sec20_absolute_onnx_path_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "evil-model".to_string(),
            repo: Some("evil/model".to_string()),
            onnx_path: Some("/etc/passwd".to_string()),
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name, "bge-large",
            "Absolute onnx_path should fall back to default"
        );
    }

    #[test]
    fn test_sec20_valid_custom_paths_accepted() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "safe-model".to_string(),
            repo: Some("org/safe-model".to_string()),
            onnx_path: Some("onnx/model.onnx".to_string()),
            tokenizer_path: Some("tokenizer.json".to_string()),
            dim: Some(384),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name, "safe-model",
            "Valid paths should be accepted"
        );
        assert_eq!(resolved.onnx_path, "onnx/model.onnx");
        assert_eq!(resolved.tokenizer_path, "tokenizer.json");
    }

    #[test]
    fn test_sec20_dotdot_in_middle_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "tricky".to_string(),
            repo: Some("org/tricky".to_string()),
            onnx_path: Some("models/../../../etc/shadow".to_string()),
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name, "bge-large",
            ".. anywhere in path should fall back"
        );
    }

    // ===== SEC-28: repo validation tests =====

    #[test]
    fn test_sec28_repo_no_slash_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "bad-repo".to_string(),
            repo: Some("no-slash-repo".to_string()),
            onnx_path: None,
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name, "bge-large",
            "Repo without slash should fall back to default"
        );
    }

    #[test]
    fn test_sec28_repo_traversal_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "traversal-repo".to_string(),
            repo: Some("../../other-repo/model".to_string()),
            onnx_path: None,
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name, "bge-large",
            "Repo with .. should fall back to default"
        );
    }

    #[test]
    fn test_sec28_repo_absolute_path_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = EmbeddingConfig {
            model: "abs-repo".to_string(),
            repo: Some("/etc/passwd/model".to_string()),
            onnx_path: None,
            tokenizer_path: None,
            dim: Some(768),
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let resolved = ModelConfig::resolve(None, Some(&cfg));
        assert_eq!(
            resolved.name, "bge-large",
            "Repo starting with / should fall back to default"
        );
    }

    /// Consistency check: DEFAULT_MODEL_REPO and DEFAULT_DIM must match default_model().
    /// If you change default_model() to point at a different preset, update these consts too.
    #[test]
    fn test_default_model_consts_consistent() {
        let dm = ModelConfig::default_model();
        assert_eq!(
            dm.repo,
            super::DEFAULT_MODEL_REPO,
            "DEFAULT_MODEL_REPO must match default_model().repo"
        );
        assert_eq!(
            dm.dim,
            super::DEFAULT_DIM,
            "DEFAULT_DIM must match default_model().dim"
        );
        assert_eq!(
            dm.dim,
            crate::EMBEDDING_DIM,
            "EMBEDDING_DIM must match default_model().dim"
        );
    }
}
