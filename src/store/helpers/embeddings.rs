//! Embedding serialization and deserialization.

use crate::embedder::Embedding;

use super::error::StoreError;

/// Convert embedding to bytes for storage.
///
/// Returns an error if embedding doesn't match `expected_dim` dimensions.
/// Storing wrong-sized embeddings would corrupt the index.
///
/// NOTE: `embedding_slice` and `bytes_to_embedding` return `Option`, while this
/// returns `Result`. Kept as `Result` because 3 callers use `.collect::<Result<Vec<_>, _>>()?`
/// and changing would be invasive. (AD-40)
pub fn embedding_to_bytes(
    embedding: &Embedding,
    expected_dim: usize,
) -> Result<Vec<u8>, StoreError> {
    if embedding.len() != expected_dim {
        return Err(StoreError::Runtime(format!(
            "Embedding dimension mismatch: expected {}, got {}. This indicates a bug in the embedder.",
            expected_dim,
            embedding.len()
        )));
    }
    Ok(bytemuck::cast_slice::<f32, u8>(embedding.as_slice()).to_vec())
}

/// Zero-copy view of embedding bytes as f32 slice (for hot paths)
///
/// Returns `Err(StoreError::EmbeddingBlobMismatch)` if byte length doesn't match `expected_dim * 4`.
pub fn embedding_slice(bytes: &[u8], expected_dim: usize) -> Result<&[f32], StoreError> {
    let expected_bytes = expected_dim * 4;
    if bytes.len() != expected_bytes {
        return Err(StoreError::EmbeddingBlobMismatch {
            expected: expected_dim,
            expected_bytes,
            actual_bytes: bytes.len(),
        });
    }
    Ok(bytemuck::cast_slice(bytes))
}

/// Convert embedding bytes to owned Vec (when ownership needed)
///
/// Returns `Err(StoreError::EmbeddingBlobMismatch)` if byte length doesn't match `expected_dim * 4` bytes.
/// This prevents silently using corrupted/truncated embeddings.
pub fn bytes_to_embedding(bytes: &[u8], expected_dim: usize) -> Result<Vec<f32>, StoreError> {
    let expected_bytes = expected_dim * 4;
    if bytes.len() != expected_bytes {
        return Err(StoreError::EmbeddingBlobMismatch {
            expected: expected_dim,
            expected_bytes,
            actual_bytes: bytes.len(),
        });
    }
    Ok(bytemuck::cast_slice::<u8, f32>(bytes).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_slice_768_dim() {
        let data = vec![0.0f32; crate::EMBEDDING_DIM];
        let bytes = bytemuck::cast_slice::<f32, u8>(&data);
        let result = embedding_slice(bytes, crate::EMBEDDING_DIM);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), crate::EMBEDDING_DIM);
    }

    #[test]
    fn test_embedding_slice_1024_dim() {
        let data = vec![1.0f32; 1024];
        let bytes = bytemuck::cast_slice::<f32, u8>(&data);
        let result = embedding_slice(bytes, 1024);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1024);
    }

    #[test]
    fn test_embedding_slice_wrong_dim_returns_err() {
        let data = vec![0.0f32; crate::EMBEDDING_DIM];
        let bytes = bytemuck::cast_slice::<f32, u8>(&data);
        // Ask for a different dim than what was stored
        let wrong_dim = if crate::EMBEDDING_DIM == 1024 {
            768
        } else {
            1024
        };
        let result = embedding_slice(bytes, wrong_dim);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, StoreError::EmbeddingBlobMismatch { .. }));
    }

    #[test]
    fn test_embedding_to_bytes_validates_dim() {
        let emb = Embedding::new(vec![0.0f32; crate::EMBEDDING_DIM]);
        assert!(embedding_to_bytes(&emb, crate::EMBEDDING_DIM).is_ok());
        let wrong_dim = if crate::EMBEDDING_DIM == 1024 {
            768
        } else {
            1024
        };
        assert!(embedding_to_bytes(&emb, wrong_dim).is_err());
    }

    #[test]
    fn test_bytes_to_embedding_1024_dim() {
        let data = vec![0.5f32; 1024];
        let bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&data).to_vec();
        let result = bytes_to_embedding(&bytes, 1024);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1024);
    }

    #[test]
    fn test_bytes_to_embedding_wrong_dim_returns_err() {
        let data = vec![0.5f32; 1024];
        let bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&data).to_vec();
        let result = bytes_to_embedding(&bytes, 768);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, StoreError::EmbeddingBlobMismatch { .. }));
    }
}
