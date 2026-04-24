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

    // ===== TC-ADV-1.29-7: NaN / Inf bytes in embedding blobs =====
    //
    // `embedding_slice` is a zero-copy cast from stored bytes to `&[f32]`.
    // The only validation is `bytes.len() == expected_dim * 4` — there is
    // no numeric sanity check. If an upstream bug wrote NaN or Inf into
    // the store (e.g. a broken normalize_l2 producing NaN from a zero
    // vector on some future model), those values ride through read paths
    // silently and poison cosine scores downstream.
    //
    // These tests pin current behaviour so a future "reject non-finite
    // reads" guard is a deliberate change.

    /// A blob containing `f32::NAN` values passes through `embedding_slice`
    /// unmodified. AUDIT-FOLLOWUP (TC-ADV-1.29-7): if we add a finite-check
    /// on read, flip this to assert an error.
    #[test]
    fn test_embedding_slice_passes_nan_bytes_through() {
        let data = vec![f32::NAN; crate::EMBEDDING_DIM];
        let bytes = bytemuck::cast_slice::<f32, u8>(&data);
        let slice = embedding_slice(bytes, crate::EMBEDDING_DIM)
            .expect("current behaviour: NaN bytes pass the length check");
        assert_eq!(slice.len(), crate::EMBEDDING_DIM);
        assert!(
            slice.iter().all(|v| v.is_nan()),
            "AUDIT-FOLLOWUP (TC-ADV-1.29-7): NaN bytes silently pass through"
        );
    }

    /// `f32::INFINITY` and `f32::NEG_INFINITY` both pass through. Same
    /// rationale — would also break cosine similarity if any storage path
    /// produced them.
    #[test]
    fn test_embedding_slice_passes_inf_bytes_through() {
        let mut data = vec![0.0f32; crate::EMBEDDING_DIM];
        data[0] = f32::INFINITY;
        data[1] = f32::NEG_INFINITY;
        let bytes = bytemuck::cast_slice::<f32, u8>(&data);
        let slice = embedding_slice(bytes, crate::EMBEDDING_DIM)
            .expect("current behaviour: Inf bytes pass the length check");
        assert!(
            slice[0].is_infinite() && slice[0].is_sign_positive(),
            "+Inf must pass through unmodified — got {}",
            slice[0]
        );
        assert!(
            slice[1].is_infinite() && slice[1].is_sign_negative(),
            "-Inf must pass through unmodified — got {}",
            slice[1]
        );
    }

    /// `bytes_to_embedding` has identical validation to `embedding_slice`
    /// (length-only, no finite-check). Pins that the owned-copy path
    /// returns the same values verbatim.
    #[test]
    fn test_bytes_to_embedding_passes_nan_inf_through() {
        let mut data = vec![0.0f32; crate::EMBEDDING_DIM];
        data[0] = f32::NAN;
        data[1] = f32::INFINITY;
        data[2] = f32::NEG_INFINITY;
        let bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&data).to_vec();
        let v = bytes_to_embedding(&bytes, crate::EMBEDDING_DIM)
            .expect("current behaviour: NaN/Inf bytes pass the length check");
        assert!(v[0].is_nan());
        assert!(v[1].is_infinite() && v[1].is_sign_positive());
        assert!(v[2].is_infinite() && v[2].is_sign_negative());
    }
}
