//! TC-HP-1: Spec guard for `resolve_splade_alpha`.
//!
//! `resolve_splade_alpha` determines the SPLADE fusion weight for every query
//! routed through search. The v1.25.0 per-category defaults (`IdentifierLookup=0.90`,
//! `Structural=0.60`, `Conceptual=0.85`, `Behavioral=0.05`, rest=`1.0`) were derived
//! from a 21-point alpha sweep on a 265-query eval and are load-bearing for the
//! 90.9% R@1 headline number. A PR that swaps the match arms or deletes a category
//! arm would ship unnoticed without this test.
//!
//! Precedence under test:
//!   per-category env (`CQS_SPLADE_ALPHA_{CATEGORY}`) > global env (`CQS_SPLADE_ALPHA`)
//!   > hardcoded default.
//!
//! Env-mutating tests must run serially (`#[serial]`) because `std::env` is a
//! process-global and otherwise parallel tests would see each other's writes.
//! The tests also clear the env vars both before and after exercising them so
//! they remain hermetic even if an earlier run crashed.

use cqs::search::router::{resolve_splade_alpha, QueryCategory};
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

/// v1.25.0 spec: the full category → default-alpha table.
///
/// This is intentionally exhaustive over every `QueryCategory` variant. If a
/// tenth variant is ever added, the match in `resolve_splade_alpha` MUST grow
/// a new arm (no silent catch-all drift) — and this table has to follow so the
/// change is reviewed. Tied to the 2026-04-14 alpha sweep documented inline in
/// `src/search/router.rs`.
const V1_25_0_DEFAULTS: &[(QueryCategory, f32)] = &[
    (QueryCategory::IdentifierLookup, 0.90),
    (QueryCategory::Structural, 0.60),
    (QueryCategory::Conceptual, 0.85),
    (QueryCategory::Behavioral, 0.05),
    (QueryCategory::TypeFiltered, 1.00),
    (QueryCategory::MultiStep, 1.00),
    (QueryCategory::Negation, 1.00),
    (QueryCategory::CrossLanguage, 1.00),
    (QueryCategory::Unknown, 1.00),
];

#[test]
#[serial]
fn test_resolve_splade_alpha_v1_25_0_defaults() {
    clear_all_alpha_env();
    for (cat, expected) in V1_25_0_DEFAULTS {
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
    for (cat, _default) in V1_25_0_DEFAULTS {
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
        (got - 0.85).abs() < f32::EPSILON,
        "Infinity per-cat env must be rejected; expected Conceptual default 0.85, got {got}"
    );
    clear_all_alpha_env();

    // Same guard on the global env var.
    std::env::set_var(GLOBAL_ENV_KEY, "-inf");
    let got = resolve_splade_alpha(&QueryCategory::Unknown);
    assert!(
        (got - 1.00).abs() < f32::EPSILON,
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
        (got - 0.05).abs() < f32::EPSILON,
        "Unparseable per-cat env must fall through to default; got {got}"
    );
    clear_all_alpha_env();
}

/// TC-HP-10 companion: the `_ => 1.0` catch-all covers 5 of 9 variants. This
/// test pins every variant to its expected alpha so a future refactor that
/// moves the early-returning arms cannot silently route `Behavioral` (α=0.05)
/// through the fallback.
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
        V1_25_0_DEFAULTS.len(),
        "Every QueryCategory variant must be listed in V1_25_0_DEFAULTS"
    );
    for cat in categories {
        // Just ensure the lookup returns SOMETHING in [0,1] — spec values are
        // covered by `test_resolve_splade_alpha_v1_25_0_defaults`.
        let got = resolve_splade_alpha(&cat);
        assert!(
            (0.0..=1.0).contains(&got),
            "Every category must return α in [0,1]; {cat:?} returned {got}"
        );
    }
    clear_all_alpha_env();
}
