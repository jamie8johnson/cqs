//! Embedding generation with ort + tokenizers

use std::path::{Path, PathBuf};
use ndarray::Array2;
use ort::session::Session;
use ort::ep::ExecutionProvider as OrtExecutionProvider;
use thiserror::Error;

// Model configuration
const MODEL_REPO: &str = "nomic-ai/nomic-embed-text-v1.5";
const MODEL_FILE: &str = "onnx/model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

// SHA256 checksums for model verification (update when model changes)
const MODEL_SHA256: &str = ""; // TODO: Fill after first download
const TOKENIZER_SHA256: &str = ""; // TODO: Fill after first download

#[derive(Error, Debug)]
pub enum EmbedderError {
    #[error("Model not found: {0}")]
    ModelNotFound(String),
    #[error("Tokenizer error: {0}")]
    TokenizerError(String),
    #[error("Inference failed: {0}")]
    InferenceFailed(String),
    #[error("Checksum mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("Query cannot be empty")]
    EmptyQuery,
    #[error("HuggingFace Hub error: {0}")]
    HfHubError(String),
}

impl From<ort::Error> for EmbedderError {
    fn from(e: ort::Error) -> Self {
        EmbedderError::InferenceFailed(e.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct Embedding(pub Vec<f32>);

#[derive(Debug, Clone, Copy)]
pub enum ExecutionProvider {
    CUDA { device_id: i32 },
    TensorRT { device_id: i32 },
    CPU,
}

impl std::fmt::Display for ExecutionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionProvider::CUDA { device_id } => write!(f, "CUDA (device {})", device_id),
            ExecutionProvider::TensorRT { device_id } => write!(f, "TensorRT (device {})", device_id),
            ExecutionProvider::CPU => write!(f, "CPU"),
        }
    }
}

pub struct Embedder {
    session: Session,
    tokenizer: tokenizers::Tokenizer,
    provider: ExecutionProvider,
    max_length: usize,
    batch_size: usize,
}

impl Embedder {
    pub fn new() -> Result<Self, EmbedderError> {
        let (model_path, tokenizer_path) = ensure_model()?;
        let provider = select_provider();
        let session = create_session(&model_path, provider)?;
        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?;

        let batch_size = match provider {
            ExecutionProvider::CPU => 4,
            _ => 16,
        };

        Ok(Self {
            session,
            tokenizer,
            provider,
            max_length: 8192,
            batch_size,
        })
    }

    /// Embed documents (code chunks). Adds "search_document: " prefix.
    pub fn embed_documents(&mut self, texts: &[&str]) -> Result<Vec<Embedding>, EmbedderError> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| format!("search_document: {}", t))
            .collect();
        self.embed_batch(&prefixed)
    }

    /// Embed a query. Adds "search_query: " prefix.
    pub fn embed_query(&mut self, text: &str) -> Result<Embedding, EmbedderError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(EmbedderError::EmptyQuery);
        }
        let prefixed = format!("search_query: {}", text);
        let results = self.embed_batch(&[prefixed])?;
        Ok(results.into_iter().next().unwrap())
    }

    /// Get the execution provider being used
    pub fn provider(&self) -> ExecutionProvider {
        self.provider
    }

    /// Get the batch size
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    /// Warm up the model with a dummy inference
    pub fn warm(&mut self) -> Result<(), EmbedderError> {
        let _ = self.embed_query("warmup")?;
        Ok(())
    }

    fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Embedding>, EmbedderError> {
        use ort::value::Tensor;

        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?;

        // Prepare inputs - note INT32 (i32) for ONNX model compatibility
        let input_ids: Vec<Vec<i32>> = encodings
            .iter()
            .map(|e| e.get_ids().iter().map(|&id| id as i32).collect())
            .collect();
        let attention_mask: Vec<Vec<i32>> = encodings
            .iter()
            .map(|e| e.get_attention_mask().iter().map(|&m| m as i32).collect())
            .collect();

        // Pad to max length in batch
        let max_len = input_ids
            .iter()
            .map(|v| v.len())
            .max()
            .unwrap_or(0)
            .min(self.max_length);

        // Create padded arrays
        let input_ids_arr = pad_2d_i32(&input_ids, max_len, 0);
        let attention_mask_arr = pad_2d_i32(&attention_mask, max_len, 0);

        // Create tensors
        let input_ids_tensor = Tensor::from_array(input_ids_arr)?;
        let attention_mask_tensor = Tensor::from_array(attention_mask_arr)?;

        // Run inference
        let outputs = self.session.run(ort::inputs![
            "input_ids" => input_ids_tensor,
            "attention_mask" => attention_mask_tensor,
        ])?;

        // Use sentence_embedding directly - it's pre-pooled
        // try_extract_tensor returns (Shape, &[T])
        let (_shape, data) = outputs["sentence_embedding"].try_extract_tensor::<f32>()?;

        // L2 normalize each embedding
        let batch_size = texts.len();
        // nomic-embed-text-v1.5 always outputs 768-dim embeddings
        let embedding_dim = 768;
        let mut results = Vec::with_capacity(batch_size);

        for i in 0..batch_size {
            let start = i * embedding_dim;
            let end = start + embedding_dim;
            let v: Vec<f32> = data[start..end].to_vec();
            results.push(Embedding(normalize_l2(v)));
        }

        Ok(results)
    }
}

/// Download model and tokenizer from HuggingFace Hub
fn ensure_model() -> Result<(PathBuf, PathBuf), EmbedderError> {
    use hf_hub::api::sync::Api;

    let api = Api::new().map_err(|e| EmbedderError::HfHubError(e.to_string()))?;
    let repo = api.model(MODEL_REPO.to_string());

    let model_path = repo
        .get(MODEL_FILE)
        .map_err(|e| EmbedderError::HfHubError(e.to_string()))?;
    let tokenizer_path = repo
        .get(TOKENIZER_FILE)
        .map_err(|e| EmbedderError::HfHubError(e.to_string()))?;

    // Verify checksums (skip if not configured)
    if !MODEL_SHA256.is_empty() {
        verify_checksum(&model_path, MODEL_SHA256)?;
    }
    if !TOKENIZER_SHA256.is_empty() {
        verify_checksum(&tokenizer_path, TOKENIZER_SHA256)?;
    }

    Ok((model_path, tokenizer_path))
}

/// Verify file checksum using blake3
fn verify_checksum(path: &Path, expected: &str) -> Result<(), EmbedderError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| EmbedderError::ModelNotFound(e.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| EmbedderError::ModelNotFound(e.to_string()))?;
    let actual = hasher.finalize().to_hex().to_string();

    if actual != expected {
        return Err(EmbedderError::ChecksumMismatch {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Select the best available execution provider
fn select_provider() -> ExecutionProvider {
    use ort::ep::{CUDA, TensorRT};

    // Try CUDA first
    let cuda = CUDA::default();
    if cuda.is_available().unwrap_or(false) {
        return ExecutionProvider::CUDA { device_id: 0 };
    }

    // Try TensorRT
    let tensorrt = TensorRT::default();
    if tensorrt.is_available().unwrap_or(false) {
        return ExecutionProvider::TensorRT { device_id: 0 };
    }

    ExecutionProvider::CPU
}

/// Create an ort session with the specified provider
fn create_session(model_path: &Path, provider: ExecutionProvider) -> Result<Session, EmbedderError> {
    use ort::ep::{CUDA, TensorRT};

    let builder = Session::builder()?;

    let session = match provider {
        ExecutionProvider::CUDA { device_id } => {
            builder
                .with_execution_providers([CUDA::default().with_device_id(device_id).build()])?
                .commit_from_file(model_path)?
        }
        ExecutionProvider::TensorRT { device_id } => {
            builder
                .with_execution_providers([
                    TensorRT::default().with_device_id(device_id).build(),
                    // Fallback to CUDA for unsupported ops
                    CUDA::default().with_device_id(device_id).build(),
                ])?
                .commit_from_file(model_path)?
        }
        ExecutionProvider::CPU => builder.commit_from_file(model_path)?,
    };

    Ok(session)
}

/// Pad 2D sequences to a fixed length
fn pad_2d_i32(inputs: &[Vec<i32>], max_len: usize, pad_value: i32) -> Array2<i32> {
    let batch_size = inputs.len();
    let mut arr = Array2::from_elem((batch_size, max_len), pad_value);
    for (i, seq) in inputs.iter().enumerate() {
        for (j, &val) in seq.iter().take(max_len).enumerate() {
            arr[[i, j]] = val;
        }
    }
    arr
}

/// L2 normalize a vector
fn normalize_l2(v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        v
    } else {
        v.into_iter().map(|x| x / norm).collect()
    }
}
