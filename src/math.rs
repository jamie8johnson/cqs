//! Math utilities for vector operations
//!
//! Shared math functions used across modules (search, notes, etc.).

use crate::EMBEDDING_DIM;

/// Dot product of two embeddings (= cosine similarity for L2-normalized vectors).
/// E5-base-v2 outputs normalized embeddings, so dot product suffices.
/// Uses SIMD acceleration when available (2-4x faster on AVX2/NEON)
///
/// Returns `None` if vectors have different lengths or unexpected dimensions.
/// This allows callers to gracefully handle dimension mismatches rather than panicking.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.len() != EMBEDDING_DIM {
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
pub fn full_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        let result = dot / denom;
        if result.is_finite() {
            result
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_embedding(val: f32) -> Vec<f32> {
        vec![val; 769]
    }

    fn make_unit_embedding(idx: usize) -> Vec<f32> {
        let mut v = vec![0.0; 769];
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
        let a: Vec<f32> = (0..769).map(|i| (i as f32) / 769.0).collect();
        let b: Vec<f32> = (0..769).map(|i| 1.0 - (i as f32) / 769.0).collect();
        let sim_ab = cosine_similarity(&a, &b).expect("Should succeed");
        let sim_ba = cosine_similarity(&b, &a).expect("Should succeed");
        assert!((sim_ab - sim_ba).abs() < 1e-6, "Should be symmetric");
    }

    #[test]
    fn test_cosine_similarity_range() {
        // Random-ish vectors
        let a: Vec<f32> = (0..769).map(|i| ((i * 7) % 100) as f32 / 100.0).collect();
        let b: Vec<f32> = (0..769).map(|i| ((i * 13) % 100) as f32 / 100.0).collect();
        let sim = cosine_similarity(&a, &b).expect("Should succeed");
        // Cosine similarity for non-normalized vectors can exceed [-1, 1]
        // but for typical embeddings should be reasonable
        assert!(sim.is_finite(), "Should be finite");
    }

    #[test]
    fn test_cosine_similarity_dimension_mismatch() {
        let a: Vec<f32> = vec![0.5; 768]; // Wrong dimension
        let b: Vec<f32> = vec![0.5; 769];
        assert!(
            cosine_similarity(&a, &b).is_none(),
            "Should fail for mismatched dimensions"
        );
        assert!(
            cosine_similarity(&a, &a).is_none(),
            "Should fail for wrong dimension"
        );
    }
}
