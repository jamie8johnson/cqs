//! Math utilities for vector operations
//!
//! Shared math functions used across modules (search, notes, etc.).

/// Dot product of two embeddings (= cosine similarity for L2-normalized vectors).
/// Uses SIMD acceleration when available (2-4x faster on AVX2/NEON).
/// **Assumes L2-normalized inputs.** For zero-norm vectors, returns `Some(0.0)` (the
/// dot product is technically correct, but undefined as cosine similarity). Use
/// [`full_cosine_similarity`] when inputs may not be normalized.
/// Returns `None` if vectors have different lengths or are empty.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    use simsimd::SpatialSimilarity;
    let score = f32::dot(a, b).unwrap_or_else(|| {
        // Fallback for unsupported architectures - accumulate in f64 for precision
        a.iter()
            .zip(b)
            .map(|(&x, &y)| (x as f64) * (y as f64))
            .sum::<f64>()
    }) as f32;
    if score.is_finite() {
        Some(score)
    } else {
        None
    }
}

/// Full cosine similarity with norm computation.
/// Used for cross-store comparison where vectors may not share normalization
/// and may have arbitrary dimensions (not necessarily EMBEDDING_DIM).
/// Returns `None` on dimension mismatch, empty vectors, or zero-norm denominator.
/// This matches the `Option<f32>` convention used by [`cosine_similarity`].
pub fn full_cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        if a.len() != b.len() {
            tracing::warn!(
                a_len = a.len(),
                b_len = b.len(),
                "full_cosine_similarity: dimension mismatch"
            );
        }
        return None;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let xd = *x as f64;
        let yd = *y as f64;
        dot += xd * yd;
        norm_a += xd * xd;
        norm_b += yd * yd;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        None
    } else {
        let result = (dot / denom) as f32;
        if result.is_finite() {
            Some(result)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a vector embedding by repeating a single float value `EMBEDDING_DIM` times.
    /// # Arguments
    /// * `val` - The float value to repeat in the embedding vector
    /// # Returns
    /// A `Vec<f32>` of length `EMBEDDING_DIM` where every element equals `val`
    fn make_embedding(val: f32) -> Vec<f32> {
        vec![val; crate::EMBEDDING_DIM]
    }

    /// Creates a one-hot encoded embedding vector of dimension `EMBEDDING_DIM`.
    /// # Arguments
    /// * `idx` - The index position where the value should be set to 1.0
    /// # Returns
    /// A vector of `EMBEDDING_DIM` f32 values with all elements initialized to 0.0 except at position `idx` which is set to 1.0.
    /// # Panics
    /// Panics if `idx` >= `EMBEDDING_DIM`.
    fn make_unit_embedding(idx: usize) -> Vec<f32> {
        let mut v = vec![0.0; crate::EMBEDDING_DIM];
        v[idx] = 1.0;
        v
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = make_embedding(0.5);
        let sim = cosine_similarity(&a, &a).expect("Should succeed for valid embeddings");
        // Identical vectors should have high similarity
        assert!(sim > 0.99, "Expected ~1.0, got {}", sim);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = make_unit_embedding(0);
        let b = make_unit_embedding(1);
        let sim = cosine_similarity(&a, &b).expect("Should succeed for valid embeddings");
        // Orthogonal unit vectors should have 0 similarity
        assert!(sim.abs() < 0.01, "Expected ~0, got {}", sim);
    }

    #[test]
    fn test_cosine_similarity_symmetric() {
        let a: Vec<f32> = (0..crate::EMBEDDING_DIM)
            .map(|i| (i as f32) / crate::EMBEDDING_DIM as f32)
            .collect();
        let b: Vec<f32> = (0..crate::EMBEDDING_DIM)
            .map(|i| 1.0 - (i as f32) / crate::EMBEDDING_DIM as f32)
            .collect();
        let sim_ab = cosine_similarity(&a, &b).expect("Should succeed");
        let sim_ba = cosine_similarity(&b, &a).expect("Should succeed");
        assert!((sim_ab - sim_ba).abs() < 1e-6, "Should be symmetric");
    }

    #[test]
    fn test_cosine_similarity_range() {
        // Random-ish vectors
        let a: Vec<f32> = (0..crate::EMBEDDING_DIM)
            .map(|i| ((i * 7) % 100) as f32 / 100.0)
            .collect();
        let b: Vec<f32> = (0..crate::EMBEDDING_DIM)
            .map(|i| ((i * 13) % 100) as f32 / 100.0)
            .collect();
        let sim = cosine_similarity(&a, &b).expect("Should succeed");
        // Cosine similarity for non-normalized vectors can exceed [-1, 1]
        // but for typical embeddings should be reasonable
        assert!(sim.is_finite(), "Should be finite");
    }

    #[test]
    fn test_cosine_similarity_dimension_mismatch() {
        let a: Vec<f32> = vec![0.5; 100];
        let b: Vec<f32> = vec![0.5; crate::EMBEDDING_DIM];
        assert!(
            cosine_similarity(&a, &b).is_none(),
            "Should fail for mismatched dimensions"
        );
        // Same-length non-EMBEDDING_DIM vectors should succeed (dimension-agnostic)
        assert!(
            cosine_similarity(&a, &a).is_some(),
            "Same-length vectors should succeed regardless of dimension"
        );
    }

    // ===== Adversarial embedding tests =====

    #[test]
    fn cosine_nan_embedding() {
        let nan_emb = vec![f32::NAN; crate::EMBEDDING_DIM];
        let normal_emb = make_embedding(0.5);
        assert!(
            cosine_similarity(&nan_emb, &normal_emb).is_none(),
            "NaN embedding vs normal should return None"
        );
        assert!(
            cosine_similarity(&normal_emb, &nan_emb).is_none(),
            "Normal vs NaN embedding should return None"
        );
    }

    #[test]
    fn cosine_inf_embedding() {
        let mut inf_emb = make_embedding(0.5);
        inf_emb[42] = f32::INFINITY;
        let normal_emb = make_embedding(0.5);
        assert!(
            cosine_similarity(&inf_emb, &normal_emb).is_none(),
            "Vector with Inf value vs normal should return None"
        );
    }

    #[test]
    fn cosine_zero_norm_vector() {
        let zero_emb = make_embedding(0.0);
        let normal_emb = make_embedding(0.5);
        // dot product of zero vector with anything = 0.0, which is finite
        // so this may return Some(0.0) — the point is it must not panic or return NaN
        let result = cosine_similarity(&zero_emb, &normal_emb);
        match result {
            None => {} // acceptable
            Some(v) => assert!(v.is_finite(), "Zero-norm result must be finite, got {v}"),
        }
    }

    #[test]
    fn cosine_negative_inf_embedding() {
        let mut neg_inf_emb = make_embedding(0.5);
        neg_inf_emb[0] = f32::NEG_INFINITY;
        let normal_emb = make_embedding(0.5);
        assert!(
            cosine_similarity(&neg_inf_emb, &normal_emb).is_none(),
            "Vector with NEG_INFINITY vs normal should return None"
        );
    }

    #[test]
    fn cosine_subnormal_values() {
        // Subnormal (denormalized) floats: very close to zero but nonzero
        let subnormal_emb = make_embedding(f32::MIN_POSITIVE / 2.0);
        let result = cosine_similarity(&subnormal_emb, &subnormal_emb);
        match result {
            None => {} // acceptable — product of subnormals can underflow to 0
            Some(v) => assert!(v.is_finite(), "Subnormal result must be finite, got {v}"),
        }
    }

    // ===== full_cosine_similarity tests (TC-24, TC-29) =====

    #[test]
    fn full_cosine_normal_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let sim = full_cosine_similarity(&a, &b).unwrap();
        // Expected: (4+10+18) / (sqrt(14) * sqrt(77)) ≈ 0.9746
        assert!(
            (sim - 0.9746).abs() < 0.001,
            "Expected ~0.9746, got {}",
            sim
        );
    }

    #[test]
    fn full_cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = full_cosine_similarity(&a, &b).unwrap();
        assert!(
            sim.abs() < 1e-6,
            "Orthogonal vectors should have ~0 similarity, got {}",
            sim
        );
    }

    #[test]
    fn full_cosine_identical_vectors() {
        let a = vec![3.0, 4.0, 5.0];
        let sim = full_cosine_similarity(&a, &a).unwrap();
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "Identical vectors should have similarity ~1.0, got {}",
            sim
        );
    }

    #[test]
    fn full_cosine_zero_norm_vector() {
        // TC-29: zero-norm vector should return None
        let zero = vec![0.0, 0.0, 0.0];
        let normal = vec![1.0, 2.0, 3.0];
        assert_eq!(
            full_cosine_similarity(&zero, &normal),
            None,
            "Zero-norm vector should return None"
        );
        assert_eq!(
            full_cosine_similarity(&normal, &zero),
            None,
            "Normal vs zero-norm should return None"
        );
        assert_eq!(
            full_cosine_similarity(&zero, &zero),
            None,
            "Both zero-norm should return None"
        );
    }

    #[test]
    fn full_cosine_nan_input() {
        let nan_vec = vec![f32::NAN, 1.0, 2.0];
        let normal = vec![1.0, 2.0, 3.0];
        assert_eq!(
            full_cosine_similarity(&nan_vec, &normal),
            None,
            "NaN input should return None"
        );
    }

    #[test]
    fn full_cosine_mismatched_dimensions() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        assert_eq!(
            full_cosine_similarity(&a, &b),
            None,
            "Mismatched dimensions should return None"
        );
    }
}
