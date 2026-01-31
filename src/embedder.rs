//! Embedding generation with ort + tokenizers

use thiserror::Error;

#[derive(Error, Debug)]
pub enum EmbedderError {
    #[error("Model not found: {0}")]
    ModelNotFound(String),
    #[error("Inference failed: {0}")]
    InferenceFailed(String),
}

pub struct Embedder {
    // TODO: ort session, tokenizer
}

impl Embedder {
    pub fn new() -> Result<Self, EmbedderError> {
        todo!("implement embedder initialization")
    }

    pub fn embed_documents(&self, _texts: &[&str]) -> Result<Vec<Embedding>, EmbedderError> {
        todo!("implement document embedding")
    }

    pub fn embed_query(&self, _text: &str) -> Result<Embedding, EmbedderError> {
        todo!("implement query embedding")
    }

    pub fn provider(&self) -> ExecutionProvider {
        ExecutionProvider::CPU
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
