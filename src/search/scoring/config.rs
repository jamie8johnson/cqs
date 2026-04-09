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
    };

    // CQ-7: `with_overrides` removed — was dead code (`#[allow(dead_code)]`).
    // Scoring overrides from `[scoring]` in config are applied per-field where needed
    // (e.g., `rrf_k` via `set_rrf_k_from_config`). If full config-driven scoring is
    // added later, re-introduce this method with actual production callers.
}
