//! Central scoring configuration.
//!
//! `ScoringConfig` is the per-search snapshot of every score-tier knob
//! (name match tiers, note boost factor, importance demotion weights,
//! parent boost). The values come from
//! [`crate::search::scoring::knob::resolve_knob`] — adding a new knob
//! is one row in `SCORING_KNOBS`, not a field here.
//!
//! Consumers should call [`ScoringConfig::current`] to get the live
//! snapshot (cached process-wide) and read fields off the result.
//! `DEFAULT` is preserved as a const so tests and reference paths can
//! anchor against the unchanged baseline values without going through
//! the resolver.

use std::sync::OnceLock;

/// Per-search snapshot of score-tier knobs.
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
    /// Baseline values. Mirrors the `default` column on each
    /// score-tier row in
    /// [`crate::search::scoring::knob::SCORING_KNOBS`]. Kept as a
    /// const so test assertions and pre-resolver callers can anchor
    /// against the unchanged defaults.
    #[allow(dead_code)]
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

    /// Live snapshot of all score-tier knobs, resolved through
    /// [`crate::search::scoring::knob::resolve_knob`]. Cached
    /// process-wide on first call (every score-tier knob is
    /// `cache: true` in `SCORING_KNOBS`).
    ///
    /// Returns `&'static Self` so callers can store the reference
    /// across a search without copying the struct.
    pub fn current() -> &'static Self {
        static CURRENT: OnceLock<ScoringConfig> = OnceLock::new();
        CURRENT.get_or_init(|| {
            use super::knob::resolve_knob;
            Self {
                name_exact: resolve_knob("name_exact"),
                name_contains: resolve_knob("name_contains"),
                name_contained_by: resolve_knob("name_contained_by"),
                name_max_overlap: resolve_knob("name_max_overlap"),
                note_boost_factor: resolve_knob("note_boost_factor"),
                importance_test: resolve_knob("importance_test"),
                importance_private: resolve_knob("importance_private"),
                parent_boost_per_child: resolve_knob("parent_boost_per_child"),
                parent_boost_cap: resolve_knob("parent_boost_cap"),
            }
        })
    }
}
