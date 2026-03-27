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
    /// Path to ONNX model file within the repo
    pub onnx_path: String,
    /// Path to tokenizer file within the repo
    pub tokenizer_path: String,
    /// Embedding dimension (768 for E5-base, 1024 for BGE-large)
    pub dim: usize,
    /// Maximum input sequence length in tokens
    pub max_seq_length: usize,
    /// Prefix prepended to queries (e.g. "query: " for E5)
    pub query_prefix: String,
    /// Prefix prepended to documents (e.g. "passage: " for E5)
    pub doc_prefix: String,
}

/// Default model repository ID. Single source of truth — used by ModelConfig, Store, and validation.
pub const DEFAULT_MODEL_REPO: &str = "intfloat/e5-base-v2";

impl ModelConfig {
    /// E5-base-v2: 768-dim, 512 tokens. The project default.
    pub fn e5_base() -> Self {
        Self {
            name: "e5-base".to_string(),
            repo: DEFAULT_MODEL_REPO.to_string(),
            onnx_path: "onnx/model.onnx".to_string(),
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
    pub fn from_preset(name: &str) -> Option<Self> {
        match name {
            "e5-base" | "intfloat/e5-base-v2" => Some(Self::e5_base()),
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
            return Self::e5_base();
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
                return Self::e5_base();
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
                    return Self::e5_base();
                }
                let cfg = Self {
                    name: embedding_cfg.model.clone(),
                    repo: embedding_cfg.repo.clone().expect("guarded by has_repo"),
                    onnx_path: embedding_cfg
                        .onnx_path
                        .clone()
                        .unwrap_or_else(|| "onnx/model.onnx".to_string()),
                    tokenizer_path: embedding_cfg
                        .tokenizer_path
                        .clone()
                        .unwrap_or_else(|| "tokenizer.json".to_string()),
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

        // 4. Default
        tracing::info!(
            model = "e5-base",
            source = "default",
            "Resolved model config"
        );
        Self::e5_base()
    }
}

/// Config-file section for embedding model settings.
///
/// Parsed from `[embedding]` in the cqs config file.
/// All fields except `model` are optional — preset names fill them automatically.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingConfig {
    /// Model name or preset (default: "e5-base")
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
    "e5-base".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ===== from_preset tests =====

    #[test]
    fn test_from_preset_short_name() {
        assert!(ModelConfig::from_preset("e5-base").is_some());
        assert!(ModelConfig::from_preset("bge-large").is_some());
    }

    #[test]
    fn test_from_preset_repo_id() {
        let cfg = ModelConfig::from_preset("intfloat/e5-base-v2").unwrap();
        assert_eq!(cfg.name, "e5-base");

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
        // Clear env to ensure we get default
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "e5-base");
    }

    #[test]
    fn test_resolve_env_by_name() {
        std::env::set_var("CQS_EMBEDDING_MODEL", "bge-large");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "bge-large");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_env_by_repo_id() {
        std::env::set_var("CQS_EMBEDDING_MODEL", "BAAI/bge-large-en-v1.5");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "bge-large");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_cli_overrides_env() {
        std::env::set_var("CQS_EMBEDDING_MODEL", "bge-large");
        let cfg = ModelConfig::resolve(Some("e5-base"), None);
        assert_eq!(cfg.name, "e5-base");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_unknown_env_warns_and_defaults() {
        std::env::set_var("CQS_EMBEDDING_MODEL", "nonexistent-model");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "e5-base"); // falls back to default
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_unknown_cli_warns_and_defaults() {
        let cfg = ModelConfig::resolve(Some("nonexistent"), None);
        assert_eq!(cfg.name, "e5-base");
    }

    #[test]
    fn test_resolve_config_preset() {
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
        assert_eq!(cfg.name, "e5-base"); // falls back
    }

    // ===== EmbeddingConfig serde tests =====

    #[test]
    fn test_embedding_config_default_model() {
        let json = r#"{}"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.model, "e5-base");
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
        std::env::set_var("CQS_EMBEDDING_MODEL", "");
        let cfg = ModelConfig::resolve(None, None);
        assert_eq!(cfg.name, "e5-base");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn test_resolve_cli_overrides_config() {
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
            cfg.name, "e5-base",
            "dim=0 should cause fallback to default e5-base"
        );
        assert_eq!(cfg.dim, 768, "Fallback should have dim=768");
    }
}
