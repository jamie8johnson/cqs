//! Shared resolver for f32 scoring knobs (audit P2.90 — issue #1132).
//!
//! Single source of truth for every scoring knob: name, env var,
//! default, valid range, cache contract. Adding a new knob is one row
//! in [`SCORING_KNOBS`].
//!
//! # Resolution order
//!
//! For each knob, [`resolve_knob`] walks:
//!
//! 1. Config override (set via [`set_overrides_from_config`])
//! 2. Environment variable (per-knob — `None` skips this step)
//! 3. Default value
//!
//! # Caching contract
//!
//! Each knob declares `cache: bool`:
//!
//! - `true` — value cached at first read across the process. Faster, but
//!   means env-var changes after the first call are ignored. Use for
//!   knobs that don't need to track per-search env changes.
//! - `false` — re-resolved on every call. Required for knobs swept by
//!   `evals/run_sweep.py`, where each run mutates `CQS_<KNOB>` between
//!   queries within the same `cqs` process.

use std::collections::HashMap;
use std::sync::OnceLock;

/// One row in [`SCORING_KNOBS`].
#[derive(Debug, Clone, Copy)]
pub struct ScoringKnob {
    /// Knob name. Matches the TOML key in `[scoring]` and the
    /// argument to [`resolve_knob`].
    pub name: &'static str,
    /// Optional environment-variable name. `None` means the knob is
    /// only settable via config / default.
    pub env_var: Option<&'static str>,
    /// Default if neither config nor env override is set.
    pub default: f32,
    /// Inclusive lower bound; values below this clamp to `min`.
    pub min: f32,
    /// Inclusive upper bound; values above this clamp to `max`.
    pub max: f32,
    /// Whether to cache the resolved value process-wide. `false` is
    /// required for sweep-contract knobs (see module-level docs).
    pub cache: bool,
}

/// All f32 scoring knobs. Adding a knob is one row here plus one
/// `resolve_knob` call at the consumer site.
///
/// Bounds (`min`, `max`) are enforced both at config-load clamping
/// (`Config::clamp_values`) and at resolve time (out-of-range env values
/// fall back to `default`). Names match the TOML keys under `[scoring]`.
pub static SCORING_KNOBS: &[ScoringKnob] = &[
    ScoringKnob {
        name: "rrf_k",
        env_var: Some("CQS_RRF_K"),
        default: 60.0,
        min: 1.0,
        max: 1000.0,
        cache: true,
    },
    // type_boost: `cache: false` is load-bearing — `evals/run_sweep.py`
    // mutates `CQS_TYPE_BOOST` between queries within the same `cqs`
    // process and expects every call to re-read the env. min just above
    // 0.0 rejects `0` / negatives that would zero out scores.
    ScoringKnob {
        name: "type_boost",
        env_var: Some("CQS_TYPE_BOOST"),
        default: 1.2,
        min: 0.0001,
        max: 100.0,
        cache: false,
    },
    // Score-tier knobs (mirror the `ScoringConfig::DEFAULT` consts in
    // `src/search/scoring/config.rs`). Consumed via
    // `ScoringConfig::current()` — the override path now flows from
    // `[scoring]` config → `resolve_knob` → live ScoringConfig snapshot.
    ScoringKnob {
        name: "name_exact",
        env_var: None,
        default: 1.0,
        min: 0.0,
        max: 2.0,
        cache: true,
    },
    ScoringKnob {
        name: "name_contains",
        env_var: None,
        default: 0.8,
        min: 0.0,
        max: 2.0,
        cache: true,
    },
    ScoringKnob {
        name: "name_contained_by",
        env_var: None,
        default: 0.6,
        min: 0.0,
        max: 2.0,
        cache: true,
    },
    ScoringKnob {
        name: "name_max_overlap",
        env_var: None,
        default: 0.5,
        min: 0.0,
        max: 2.0,
        cache: true,
    },
    ScoringKnob {
        name: "note_boost_factor",
        env_var: None,
        default: 0.15,
        min: 0.0,
        max: 1.0,
        cache: true,
    },
    ScoringKnob {
        name: "importance_test",
        env_var: None,
        default: 0.70,
        min: 0.0,
        max: 1.0,
        cache: true,
    },
    ScoringKnob {
        name: "importance_private",
        env_var: None,
        default: 0.80,
        min: 0.0,
        max: 1.0,
        cache: true,
    },
    ScoringKnob {
        name: "parent_boost_per_child",
        env_var: None,
        default: 0.05,
        min: 0.0,
        max: 0.5,
        cache: true,
    },
    ScoringKnob {
        name: "parent_boost_cap",
        env_var: None,
        default: 1.15,
        min: 1.0,
        max: 2.0,
        cache: true,
    },
];

/// Config-override map, populated once via [`set_overrides_from_config`].
/// Read-only after first set (OnceLock semantics).
static CONFIG_OVERRIDES: OnceLock<HashMap<&'static str, f32>> = OnceLock::new();

/// Cached resolved values for `cache: true` knobs. Populated lazily on
/// the first call to [`resolve_knob`] for any cached knob — at which
/// point [`CONFIG_OVERRIDES`] and env vars are read for every cached
/// knob and frozen for the rest of the process lifetime.
static CACHED: OnceLock<HashMap<&'static str, f32>> = OnceLock::new();

/// Look up a knob by name. Panics on unknown name — every consumer
/// should reference a name that appears in [`SCORING_KNOBS`].
pub fn knob(name: &str) -> &'static ScoringKnob {
    SCORING_KNOBS
        .iter()
        .find(|k| k.name == name)
        .unwrap_or_else(|| panic!("unknown scoring knob: {name}"))
}

/// Resolve the current value of a scoring knob.
///
/// See module-level docs for resolution order and caching semantics.
pub fn resolve_knob(name: &str) -> f32 {
    let knob = knob(name);
    if knob.cache {
        *CACHED
            .get_or_init(|| {
                SCORING_KNOBS
                    .iter()
                    .filter(|k| k.cache)
                    .map(|k| (k.name, resolve_uncached(k)))
                    .collect()
            })
            .get(knob.name)
            .expect("knob present in CACHED — initialized by get_or_init above")
    } else {
        resolve_uncached(knob)
    }
}

fn resolve_uncached(knob: &ScoringKnob) -> f32 {
    if let Some(&v) = CONFIG_OVERRIDES.get().and_then(|m| m.get(knob.name)) {
        // AC-V1.33-4: match env-path validation — reject NaN/Inf and out-of-range
        // before the value can flow into BM25/RRF math (`f32::clamp` propagates NaN).
        if v.is_finite() && v >= knob.min && v <= knob.max {
            return v;
        }
        tracing::warn!(
            knob = knob.name,
            value = v,
            min = knob.min,
            max = knob.max,
            fallback = knob.default,
            "scoring knob config override out of range or non-finite — using default"
        );
        return knob.default;
    }
    if let Some(env_name) = knob.env_var {
        if let Ok(raw) = std::env::var(env_name) {
            match raw.parse::<f32>() {
                Ok(v) if v.is_finite() && v >= knob.min && v <= knob.max => return v,
                Ok(v) => tracing::warn!(
                    env = env_name,
                    raw = %raw,
                    parsed = v,
                    fallback = knob.default,
                    "scoring knob env value out of range or non-finite — using default"
                ),
                Err(e) => tracing::warn!(
                    env = env_name,
                    raw = %raw,
                    error = %e,
                    fallback = knob.default,
                    "scoring knob env value not parseable as f32 — using default"
                ),
            }
        }
    }
    knob.default
}

/// Populate the config-override map from a `[scoring]` map.
///
/// Must be called once before the first [`resolve_knob`] call to any
/// `cache: true` knob (currently: from CLI dispatch, before searches
/// run). Subsequent calls are no-ops (OnceLock).
///
/// Unknown keys (not matching any knob in [`SCORING_KNOBS`]) are
/// logged at WARN. Out-of-range values are clamped to `[min, max]`
/// at resolve time, not here.
pub fn set_overrides_from_config(overrides: &HashMap<String, f32>) {
    let _ = CONFIG_OVERRIDES.set(build_override_map(overrides));
}

/// Pure helper extracted from [`set_overrides_from_config`] for unit
/// testing without `OnceLock` pollution. Filters `overrides` down to
/// known knob names and warns on unknown keys.
fn build_override_map(overrides: &HashMap<String, f32>) -> HashMap<&'static str, f32> {
    let mut map: HashMap<&'static str, f32> = HashMap::new();
    for knob in SCORING_KNOBS.iter() {
        if let Some(&v) = overrides.get(knob.name) {
            map.insert(knob.name, v);
        }
    }
    for unknown in overrides.keys() {
        if !SCORING_KNOBS.iter().any(|k| k.name == unknown.as_str()) {
            tracing::warn!(
                key = %unknown,
                "Unknown key in [scoring] config — no such knob in SCORING_KNOBS"
            );
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn knob_lookup_finds_rrf_k() {
        let k = knob("rrf_k");
        assert_eq!(k.name, "rrf_k");
        assert_eq!(k.default, 60.0);
        assert!(k.cache);
    }

    #[test]
    #[should_panic(expected = "unknown scoring knob: nonexistent")]
    fn knob_lookup_panics_on_unknown() {
        let _ = knob("nonexistent");
    }

    #[test]
    #[serial]
    fn resolve_uncached_returns_default_when_env_unset() {
        // SAFETY: removing an env that may not exist is a no-op.
        std::env::remove_var("CQS_RRF_K");
        let k = knob("rrf_k");
        assert_eq!(resolve_uncached(k), 60.0);
    }

    #[test]
    #[serial]
    fn resolve_uncached_reads_env_when_set() {
        std::env::set_var("CQS_RRF_K", "42.0");
        let k = knob("rrf_k");
        assert_eq!(resolve_uncached(k), 42.0);
        std::env::remove_var("CQS_RRF_K");
    }

    #[test]
    #[serial]
    fn resolve_uncached_falls_back_on_unparseable_env() {
        std::env::set_var("CQS_RRF_K", "not-a-number");
        let k = knob("rrf_k");
        assert_eq!(resolve_uncached(k), 60.0);
        std::env::remove_var("CQS_RRF_K");
    }

    #[test]
    #[serial]
    fn resolve_uncached_falls_back_on_out_of_range_env() {
        // 5000 > max (1000) → fall back to default
        std::env::set_var("CQS_RRF_K", "5000.0");
        let k = knob("rrf_k");
        assert_eq!(resolve_uncached(k), 60.0);
        std::env::remove_var("CQS_RRF_K");
    }

    #[test]
    #[serial]
    fn resolve_uncached_falls_back_on_non_finite_env() {
        std::env::set_var("CQS_RRF_K", "inf");
        let k = knob("rrf_k");
        assert_eq!(resolve_uncached(k), 60.0);
        std::env::remove_var("CQS_RRF_K");
    }

    #[test]
    fn build_override_map_keeps_known_drops_unknown() {
        // Pure function, no OnceLock side effects — safe to call
        // freely from tests. `set_overrides_from_config` is tested
        // indirectly via the config-load integration tests in
        // `src/config.rs`.
        let mut map = HashMap::new();
        map.insert("rrf_k".to_string(), 99.0);
        map.insert("name_exact".to_string(), 1.5);
        map.insert("nonexistent_knob".to_string(), 1.0);

        let built = build_override_map(&map);
        assert_eq!(built.get("rrf_k"), Some(&99.0));
        assert_eq!(built.get("name_exact"), Some(&1.5));
        assert!(!built.contains_key("nonexistent_knob"));
    }

    #[test]
    fn build_override_map_empty_input_returns_empty() {
        let empty = HashMap::new();
        let built = build_override_map(&empty);
        assert!(built.is_empty());
    }
}
