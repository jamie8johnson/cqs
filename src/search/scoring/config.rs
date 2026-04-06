//! Central scoring configuration constants.

/// Central configuration for all search scoring constants.
/// Consolidates name matching tiers, note boost factor, importance
/// demotion weights, and parent boost parameters into one struct.
/// Use `ScoringConfig::DEFAULT` everywhere — no scattered magic numbers.
pub(crate) struct ScoringConfig {
    pub name_exact: f32,
    pub name_contains: f32,
    pub name_contained_by: f32,
    pub name_max_overlap: f32,
    pub note_boost_factor: f32,
    pub importance_test: f32,
    pub importance_private: f32,
    pub parent_boost_per_child: f32,
    pub parent_boost_cap: f32,
    /// Weight for dense score in sparse-dense fusion (0.0-1.0).
    /// 0.7 = cosine-heavy (default), 0.3 = SPLADE-heavy.
    /// 1-splade_alpha is the weight for the sparse score.
    pub splade_alpha: f32,
}

impl ScoringConfig {
    pub const DEFAULT: Self = Self {
        name_exact: 1.0,
        name_contains: 0.8,
        name_contained_by: 0.6,
        name_max_overlap: 0.5,
        note_boost_factor: 0.15,
        importance_test: 0.70,
        importance_private: 0.80,
        parent_boost_per_child: 0.05,
        parent_boost_cap: 1.15,
        splade_alpha: 0.7,
    };

    /// Build a `ScoringConfig` by layering optional overrides on top of defaults.
    /// Callers: currently test-only; wiring into scoring pipeline is a follow-up.
    #[allow(dead_code)]
    pub fn with_overrides(overrides: &crate::config::ScoringOverrides) -> Self {
        Self {
            name_exact: overrides.name_exact.unwrap_or(Self::DEFAULT.name_exact),
            name_contains: overrides
                .name_contains
                .unwrap_or(Self::DEFAULT.name_contains),
            name_contained_by: overrides
                .name_contained_by
                .unwrap_or(Self::DEFAULT.name_contained_by),
            name_max_overlap: overrides
                .name_max_overlap
                .unwrap_or(Self::DEFAULT.name_max_overlap),
            note_boost_factor: overrides
                .note_boost_factor
                .unwrap_or(Self::DEFAULT.note_boost_factor),
            importance_test: overrides
                .importance_test
                .unwrap_or(Self::DEFAULT.importance_test),
            importance_private: overrides
                .importance_private
                .unwrap_or(Self::DEFAULT.importance_private),
            parent_boost_per_child: overrides
                .parent_boost_per_child
                .unwrap_or(Self::DEFAULT.parent_boost_per_child),
            parent_boost_cap: overrides
                .parent_boost_cap
                .unwrap_or(Self::DEFAULT.parent_boost_cap),
            splade_alpha: overrides.splade_alpha.unwrap_or(Self::DEFAULT.splade_alpha),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScoringOverrides;

    #[test]
    fn with_overrides_uses_defaults_when_empty() {
        let overrides = ScoringOverrides::default();
        let cfg = ScoringConfig::with_overrides(&overrides);
        assert!((cfg.name_exact - ScoringConfig::DEFAULT.name_exact).abs() < f32::EPSILON);
        assert!(
            (cfg.note_boost_factor - ScoringConfig::DEFAULT.note_boost_factor).abs() < f32::EPSILON
        );
        assert!(
            (cfg.importance_test - ScoringConfig::DEFAULT.importance_test).abs() < f32::EPSILON
        );
    }

    #[test]
    fn with_overrides_applies_partial_overrides() {
        let overrides = ScoringOverrides {
            name_exact: Some(0.9),
            note_boost_factor: Some(0.25),
            ..Default::default()
        };
        let cfg = ScoringConfig::with_overrides(&overrides);
        assert!((cfg.name_exact - 0.9).abs() < f32::EPSILON);
        assert!((cfg.note_boost_factor - 0.25).abs() < f32::EPSILON);
        // Untouched fields keep defaults
        assert!((cfg.name_contains - ScoringConfig::DEFAULT.name_contains).abs() < f32::EPSILON);
        assert!(
            (cfg.importance_private - ScoringConfig::DEFAULT.importance_private).abs()
                < f32::EPSILON
        );
    }
}
