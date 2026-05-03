//! TC-HP-1: Spec guard for `resolve_splade_alpha`.
//!
//! `resolve_splade_alpha` determines the SPLADE fusion weight for every query
//! routed through search. The current per-category defaults (encoded in
//! [`PER_CATEGORY_DEFAULTS`] below) were derived from the 2026-05-03
//! EmbeddingGemma alpha sweep on the v3.v2 fixtures (109 test + 109 dev,
//! gemma slot at 13,359 chunks). Earlier sweeps tuned for BGE-large
//! (2026-04-15 / v1.26.0, refined 2026-04-16 / v1.29.0); the comment block
//! at the top of `src/search/router.rs::define_query_categories!` carries
//! the per-variant rationale tied to the sweep numbers. A PR that swaps the
//! match arms or deletes a category arm would ship unnoticed without this
//! test.
//!
//! Precedence under test:
//!   per-category env (`CQS_SPLADE_ALPHA_{CATEGORY}`) > global env (`CQS_SPLADE_ALPHA`)
//!   > hardcoded default.
//!
//! Env-mutating tests must run serially (`#[serial]`) because `std::env` is a
//! process-global and otherwise parallel tests would see each other's writes.
//! The tests also clear the env vars both before and after exercising them so
//! they remain hermetic even if an earlier run crashed.

use cqs::search::router::{classify_query, resolve_splade_alpha, QueryCategory};
use serial_test::serial;

/// Per-category env var keys. Kept in sync with `resolve_splade_alpha`'s
/// `format!("CQS_SPLADE_ALPHA_{}", category)` — `QueryCategory::Display` upper-cased.
const PER_CAT_ENV_KEYS: &[&str] = &[
    "CQS_SPLADE_ALPHA_IDENTIFIER_LOOKUP",
    "CQS_SPLADE_ALPHA_STRUCTURAL",
    "CQS_SPLADE_ALPHA_BEHAVIORAL",
    "CQS_SPLADE_ALPHA_CONCEPTUAL",
    "CQS_SPLADE_ALPHA_MULTI_STEP",
    "CQS_SPLADE_ALPHA_NEGATION",
    "CQS_SPLADE_ALPHA_TYPE_FILTERED",
    "CQS_SPLADE_ALPHA_CROSS_LANGUAGE",
    "CQS_SPLADE_ALPHA_UNKNOWN",
];

const GLOBAL_ENV_KEY: &str = "CQS_SPLADE_ALPHA";

/// Clear every alpha-related env var so a single test's state cannot leak.
fn clear_all_alpha_env() {
    std::env::remove_var(GLOBAL_ENV_KEY);
    for key in PER_CAT_ENV_KEYS {
        std::env::remove_var(key);
    }
}

/// v1.26.0 spec: the full category → default-alpha table.
///
/// This is intentionally exhaustive over every `QueryCategory` variant. If a
/// tenth variant is ever added, the match in `resolve_splade_alpha` MUST grow
/// a new arm (no silent catch-all drift) — and this table has to follow so the
/// change is reviewed. Tied to the 2026-04-15 alpha sweep documented inline in
/// `src/search/router.rs`.
/// Per-category SPLADE alpha defaults — spec table.
///
/// History:
/// - v1.26.0 (2026-04-15): initial sweep on BGE-large, dirty index.
/// - v1.28.3: R@5 re-sweep on cleaned index — Behavioral 0.00→0.80, MultiStep 1.00→0.10.
/// - v1.29.0 (2026-04-16): v3 sweep — CrossLanguage 1.00→0.10.
/// - 2026-05-03 (this table): EmbeddingGemma re-sweep on v3.v2 fixtures —
///   Structural 0.90→0.60, Behavioral 0.80→1.00, Conceptual 0.70→0.80,
///   TypeFiltered 1.00→0.00, CrossLanguage 0.10→0.70. See router.rs for
///   per-variant rationale tied to the sweep numbers.
const PER_CATEGORY_DEFAULTS: &[(QueryCategory, f32)] = &[
    (QueryCategory::IdentifierLookup, 1.00),
    // EmbeddingGemma v3.v2 sweep change (2026-05-03): 0.90 → 0.60.
    (QueryCategory::Structural, 0.60),
    // EmbeddingGemma v3.v2 sweep change (2026-05-03): 0.70 → 0.80.
    (QueryCategory::Conceptual, 0.80),
    // v1.28.3 R@5 re-sweep change: 0.00 → 0.80.
    // EmbeddingGemma v3.v2 sweep change (2026-05-03): 0.80 → 1.00.
    (QueryCategory::Behavioral, 1.00),
    (QueryCategory::Negation, 0.80),
    // EmbeddingGemma v3.v2 sweep change (2026-05-03): 1.00 → 0.00.
    (QueryCategory::TypeFiltered, 0.00),
    // v1.28.3 R@5 re-sweep change: 1.00 → 0.10.
    (QueryCategory::MultiStep, 0.10),
    // v3 sweep change (2026-04-16): 1.00 → 0.10.
    // EmbeddingGemma v3.v2 sweep change (2026-05-03): 0.10 → 0.70.
    (QueryCategory::CrossLanguage, 0.70),
    // EmbeddingGemma v3.v2 sweep change (2026-05-03): 1.00 → 0.80.
    // Unknown is the catch-all where misclassified queries land; flat α=0.80
    // is the joint mean-R@5 optimum from the global sweep.
    (QueryCategory::Unknown, 0.80),
];

#[test]
#[serial]
fn test_resolve_splade_alpha_per_category_defaults() {
    clear_all_alpha_env();
    for (cat, expected) in PER_CATEGORY_DEFAULTS {
        let got = resolve_splade_alpha(cat);
        assert!(
            (got - expected).abs() < f32::EPSILON,
            "Category {cat:?}: expected default α={expected}, got {got}. \
             This table is a spec, not a reference — do not update it without \
             a corresponding alpha-sweep update in docs."
        );
    }
    clear_all_alpha_env();
}

#[test]
#[serial]
fn test_resolve_splade_alpha_per_category_env_override() {
    clear_all_alpha_env();
    // Per-category override wins over the default for its own category.
    std::env::set_var("CQS_SPLADE_ALPHA_CONCEPTUAL", "0.3");
    let got = resolve_splade_alpha(&QueryCategory::Conceptual);
    assert!(
        (got - 0.3).abs() < f32::EPSILON,
        "Expected per-cat override α=0.3 for Conceptual, got {got}"
    );

    // Other categories are untouched by a different category's env var.
    let struct_alpha = resolve_splade_alpha(&QueryCategory::Structural);
    assert!(
        (struct_alpha - 0.60).abs() < f32::EPSILON,
        "Unrelated category should still use its default; got {struct_alpha}"
    );
    clear_all_alpha_env();
}

#[test]
#[serial]
fn test_resolve_splade_alpha_global_env_override() {
    clear_all_alpha_env();
    std::env::set_var(GLOBAL_ENV_KEY, "0.25");
    // Every variant should pick up the global override when no per-cat override is set.
    for (cat, _default) in PER_CATEGORY_DEFAULTS {
        let got = resolve_splade_alpha(cat);
        assert!(
            (got - 0.25).abs() < f32::EPSILON,
            "Category {cat:?}: expected global override α=0.25, got {got}"
        );
    }
    clear_all_alpha_env();
}

#[test]
#[serial]
fn test_resolve_splade_alpha_precedence_per_cat_over_global() {
    clear_all_alpha_env();
    // Both env vars set: per-category must win.
    std::env::set_var(GLOBAL_ENV_KEY, "0.25");
    std::env::set_var("CQS_SPLADE_ALPHA_BEHAVIORAL", "0.10");
    let got = resolve_splade_alpha(&QueryCategory::Behavioral);
    assert!(
        (got - 0.10).abs() < f32::EPSILON,
        "Per-cat env must beat global env; expected 0.10, got {got}"
    );
    // A category without a per-cat override still sees the global.
    let unknown = resolve_splade_alpha(&QueryCategory::Unknown);
    assert!(
        (unknown - 0.25).abs() < f32::EPSILON,
        "Category without per-cat env should see global override; got {unknown}"
    );
    clear_all_alpha_env();
}

#[test]
#[serial]
fn test_resolve_splade_alpha_rejects_nan_falls_back_to_default() {
    clear_all_alpha_env();
    // NaN is non-finite — the resolver must ignore it and fall through to the default.
    std::env::set_var("CQS_SPLADE_ALPHA_STRUCTURAL", "NaN");
    let got = resolve_splade_alpha(&QueryCategory::Structural);
    assert!(
        (got - 0.60).abs() < f32::EPSILON,
        "NaN per-cat env must be rejected; expected Structural default 0.60, got {got}"
    );
    clear_all_alpha_env();
}

#[test]
#[serial]
fn test_resolve_splade_alpha_rejects_infinity_falls_back_to_default() {
    clear_all_alpha_env();
    std::env::set_var("CQS_SPLADE_ALPHA_CONCEPTUAL", "inf");
    let got = resolve_splade_alpha(&QueryCategory::Conceptual);
    assert!(
        (got - 0.80).abs() < f32::EPSILON,
        "Infinity per-cat env must be rejected; expected Conceptual default 0.80, got {got}"
    );
    clear_all_alpha_env();

    // Same guard on the global env var.
    std::env::set_var(GLOBAL_ENV_KEY, "-inf");
    let got = resolve_splade_alpha(&QueryCategory::Unknown);
    assert!(
        (got - 0.80).abs() < f32::EPSILON,
        "Non-finite global env must fall through to default; got {got}"
    );
    clear_all_alpha_env();
}

#[test]
#[serial]
fn test_resolve_splade_alpha_clamps_out_of_range_env() {
    clear_all_alpha_env();
    // The resolver clamps finite overrides to [0.0, 1.0] so downstream code
    // never sees an alpha outside the documented contract.
    std::env::set_var("CQS_SPLADE_ALPHA_IDENTIFIER_LOOKUP", "2.5");
    let got = resolve_splade_alpha(&QueryCategory::IdentifierLookup);
    assert!(
        (got - 1.0).abs() < f32::EPSILON,
        "α=2.5 must clamp to 1.0; got {got}"
    );

    std::env::set_var("CQS_SPLADE_ALPHA_IDENTIFIER_LOOKUP", "-4.2");
    let got = resolve_splade_alpha(&QueryCategory::IdentifierLookup);
    assert!(
        got.abs() < f32::EPSILON,
        "α=-4.2 must clamp to 0.0; got {got}"
    );
    clear_all_alpha_env();
}

#[test]
#[serial]
fn test_resolve_splade_alpha_invalid_string_falls_back() {
    clear_all_alpha_env();
    // A string that does not parse as f32 is ignored, same as non-finite.
    std::env::set_var("CQS_SPLADE_ALPHA_BEHAVIORAL", "banana");
    let got = resolve_splade_alpha(&QueryCategory::Behavioral);
    assert!(
        (got - 1.00).abs() < f32::EPSILON,
        "Unparseable per-cat env must fall through to default (1.00, EmbeddingGemma sweep 2026-05-03); got {got}"
    );
    clear_all_alpha_env();
}

/// TC-HP-4: per-category SPLADE alpha routing has no end-to-end test. The
/// wiring in `dispatch_search` (`src/cli/batch/handlers/search.rs:117-139`)
/// and `cmd_query` (`src/cli/commands/search/query.rs:170-200`) composes
/// `classify_query(query)` → `resolve_splade_alpha(category)`. Both production
/// call sites do *exactly* that two-step lookup. A refactor that hardcodes
/// `alpha = 1.0` or swaps the category would survive the split-apart unit
/// tests but the composed behavior would break silently.
///
/// These tests couple the two functions and assert the alpha that would
/// flow into `SearchFilter::splade_alpha`. They do NOT construct a full
/// `BatchContext` — `dispatch_search` is `pub(crate)`, not reachable from an
/// integration test. Since both production sites resolve alpha via the
/// same two public functions we test here, coupling them in a test is
/// equivalent to coupling them in production.
mod splade_routing {
    use super::*;

    /// Run the composed `classify_query` → `resolve_splade_alpha` pipeline
    /// that both production call sites use.
    fn route(query: &str) -> (QueryCategory, f32) {
        let classification = classify_query(query);
        let alpha = resolve_splade_alpha(&classification.category);
        (classification.category, alpha)
    }

    /// Structural query → α=0.60 (EmbeddingGemma v3.v2 sweep 2026-05-03).
    #[test]
    #[serial]
    fn test_routing_structural_lands_on_alpha_0_60() {
        clear_all_alpha_env();
        let (cat, alpha) = route("functions that return Result");
        assert_eq!(cat, QueryCategory::Structural);
        assert!(
            (alpha - 0.60).abs() < f32::EPSILON,
            "Structural query should route to α=0.60, got {alpha}"
        );
        clear_all_alpha_env();
    }

    /// Behavioral query → α=1.00 (pure dense — EmbeddingGemma v3.v2 sweep 2026-05-03).
    #[test]
    #[serial]
    fn test_routing_behavioral_lands_on_alpha_1_00() {
        clear_all_alpha_env();
        let (cat, alpha) = route("validates user input");
        assert_eq!(cat, QueryCategory::Behavioral);
        assert!(
            (alpha - 1.00).abs() < f32::EPSILON,
            "Behavioral query should route to α=1.00 (EmbeddingGemma sweep), got {alpha}"
        );
        clear_all_alpha_env();
    }

    /// Conceptual query → α=0.80 (EmbeddingGemma v3.v2 sweep 2026-05-03).
    #[test]
    #[serial]
    fn test_routing_conceptual_lands_on_alpha_0_80() {
        clear_all_alpha_env();
        let (cat, alpha) = route("dependency injection pattern");
        assert_eq!(cat, QueryCategory::Conceptual);
        assert!(
            (alpha - 0.80).abs() < f32::EPSILON,
            "Conceptual query should route to α=0.80, got {alpha}"
        );
        clear_all_alpha_env();
    }

    /// Identifier lookup → α=1.00 (sparse-only fallback; NameOnly strategy is what
    /// actually fires for this category, so SPLADE alpha is moot).
    #[test]
    #[serial]
    fn test_routing_identifier_lands_on_alpha_1_00() {
        clear_all_alpha_env();
        let (cat, alpha) = route("HashMap::new");
        assert_eq!(cat, QueryCategory::IdentifierLookup);
        assert!(
            (alpha - 1.00).abs() < f32::EPSILON,
            "Identifier lookup should route to α=1.00, got {alpha}"
        );
        clear_all_alpha_env();
    }

    /// CrossLanguage → α=0.70 (EmbeddingGemma v3.v2 sweep 2026-05-03 — flipped from
    /// 0.10 because EmbeddingGemma's bilingual embeddings dominate sparse here).
    #[test]
    #[serial]
    fn test_routing_cross_language_lands_on_alpha_0_70() {
        clear_all_alpha_env();
        let (cat, alpha) = route("Python equivalent of map in Rust");
        assert_eq!(cat, QueryCategory::CrossLanguage);
        assert!(
            (alpha - 0.70).abs() < f32::EPSILON,
            "CrossLanguage should route to α=0.70, got {alpha}"
        );
        clear_all_alpha_env();
    }

    /// Negation → α=0.80 (curve flat across α=0.0-0.9 on EmbeddingGemma; kept at 0.80).
    #[test]
    #[serial]
    fn test_routing_negation_lands_on_alpha_0_80() {
        clear_all_alpha_env();
        let (cat, alpha) = route("sort without allocating");
        assert_eq!(cat, QueryCategory::Negation);
        assert!(
            (alpha - 0.80).abs() < f32::EPSILON,
            "Negation should route to α=0.80, got {alpha}"
        );
        clear_all_alpha_env();
    }

    /// TypeFiltered → α=0.00 (pure SPLADE — EmbeddingGemma v3.v2 sweep 2026-05-03).
    /// Lexical signals ("test function", "enum") dominate; dense adds noise.
    #[test]
    #[serial]
    fn test_routing_type_filtered_lands_on_alpha_0_00() {
        clear_all_alpha_env();

        let (cat, alpha) = route("all test functions");
        assert_eq!(cat, QueryCategory::TypeFiltered);
        assert!(
            alpha.abs() < f32::EPSILON,
            "TypeFiltered should route to α=0.00 (EmbeddingGemma sweep), got {alpha}"
        );

        clear_all_alpha_env();
    }

    /// Env override propagates through the composed pipeline.
    #[test]
    #[serial]
    fn test_routing_env_override_wins_through_pipeline() {
        clear_all_alpha_env();
        // Pin Behavioral to a bespoke alpha via the per-category env.
        std::env::set_var("CQS_SPLADE_ALPHA_BEHAVIORAL", "0.42");
        let (cat, alpha) = route("validates user input");
        assert_eq!(cat, QueryCategory::Behavioral);
        assert!(
            (alpha - 0.42).abs() < f32::EPSILON,
            "Env override should propagate through classify → resolve, got {alpha}"
        );
        clear_all_alpha_env();
    }
}

/// TC-HP-10 companion: every `QueryCategory` variant must resolve to a value
/// in [0,1]. Ensures a future refactor that moves the early-returning arms
/// cannot accidentally route a category outside the documented contract.
#[test]
#[serial]
fn test_resolve_splade_alpha_catch_all_coverage() {
    clear_all_alpha_env();
    // Enumerate every variant the match might see. Adding a 10th variant must
    // force a test update here.
    let categories = [
        QueryCategory::IdentifierLookup,
        QueryCategory::Structural,
        QueryCategory::Behavioral,
        QueryCategory::Conceptual,
        QueryCategory::MultiStep,
        QueryCategory::Negation,
        QueryCategory::TypeFiltered,
        QueryCategory::CrossLanguage,
        QueryCategory::Unknown,
    ];
    assert_eq!(
        categories.len(),
        PER_CATEGORY_DEFAULTS.len(),
        "Every QueryCategory variant must be listed in PER_CATEGORY_DEFAULTS"
    );
    for cat in categories {
        // Just ensure the lookup returns SOMETHING in [0,1] — spec values are
        // covered by `test_resolve_splade_alpha_v1_26_0_defaults`.
        let got = resolve_splade_alpha(&cat);
        assert!(
            (0.0..=1.0).contains(&got),
            "Every category must return α in [0,1]; {cat:?} returned {got}"
        );
    }
    clear_all_alpha_env();
}
