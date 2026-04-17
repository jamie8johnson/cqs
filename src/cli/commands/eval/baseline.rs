//! Baseline diff support for `cqs eval --baseline X`.
//!
//! Loads a previously saved `EvalReport` from disk, diffs its R@1 / R@5 /
//! R@20 numbers against the current run (overall + per-category), and
//! flags any per-category drop greater than `--tolerance` percentage
//! points as a regression. The CLI exits non-zero when the returned
//! `DiffReport.regressions` is non-empty so this can drop straight into
//! CI as a quality gate.
//!
//! Why per-category and not just overall: a 50-query category dropping
//! 5pp can be invisible in a 500-query overall (1pp ripple). The whole
//! point of the gate is to catch local regressions before they bleed
//! into aggregate metrics, so the gate fires on per-category deltas.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::runner::EvalReport;

/// Per-K delta (current minus baseline) expressed in percentage points.
///
/// A negative value means the current run is *worse* than baseline at
/// that K. Stored as `f64` so 0.0 round-trips exactly through JSON and
/// the diff math doesn't accumulate float noise on small deltas.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct KDelta {
    pub r1: f64,
    pub r5: f64,
    pub r20: f64,
}

/// One per-category R@K regression past tolerance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Regression {
    pub category: String,
    /// One of "R@1" / "R@5" / "R@20".
    pub metric: String,
    pub baseline_value: f64,
    pub current_value: f64,
    /// Negative = regression. Always (current - baseline) in percentage points.
    pub delta_pp: f64,
}

/// Lightweight slice of metadata we surface in the diff header. Pulled from
/// the loaded baseline so the user can spot model / version drift without
/// opening the JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BaselineMeta {
    pub cqs_version: String,
    pub index_model: String,
    pub query_file: String,
    pub overall_n: usize,
}

/// The full diff between current run and a saved baseline.
///
/// Reported in two forms:
///   - text via `print_diff_report` (human-readable, with `(±N.Npp)`
///     deltas and a regression summary at the end)
///   - JSON via `serde_json::to_string_pretty(&report)` (CI-friendly)
///
/// Caller decides whether to exit non-zero based on `regressions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DiffReport {
    pub baseline_path: PathBuf,
    pub baseline_meta: BaselineMeta,
    pub current_meta: BaselineMeta,
    pub overall_delta: KDelta,
    /// Categories present in EITHER baseline OR current. Categories only on
    /// one side get a half-populated entry (the missing side reads as 0).
    pub by_category_delta: BTreeMap<String, KDelta>,
    pub regressions: Vec<Regression>,
    /// Tolerance (percentage points) the regressions list was filtered with.
    pub tolerance_pp: f64,
    /// Drift warnings (e.g. baseline was saved with a different model). These
    /// are advisory — they don't gate exit, but they do print so a CI log
    /// shows them.
    pub warnings: Vec<String>,
}

/// Fraction in [0,1] → percentage points (0..=100).
fn to_pp(fraction: f64) -> f64 {
    fraction * 100.0
}

/// Compare `current` against the `EvalReport` saved at `baseline_path`.
///
/// `tolerance_pp` is the per-category R@K drop (in percentage points) that
/// is tolerated without triggering a regression entry. A drop of exactly
/// `tolerance_pp` is allowed; anything strictly greater is a regression.
///
/// Loads the baseline JSON, diffs it, and returns the structured report.
/// Does not print or exit — the caller (`cmd_eval`) handles output and
/// exit code so this stays unit-testable without subprocess dance.
pub(crate) fn compare_against_baseline(
    current: &EvalReport,
    baseline_path: &Path,
    tolerance_pp: f64,
) -> Result<DiffReport> {
    let _span =
        tracing::info_span!("compare_against_baseline", path = %baseline_path.display()).entered();

    let raw = std::fs::read_to_string(baseline_path).with_context(|| {
        format!(
            "Failed to read baseline file at {}. \
             Did you forget to run `cqs eval ... --save {}` first?",
            baseline_path.display(),
            baseline_path.display()
        )
    })?;
    let baseline: EvalReport = serde_json::from_str(&raw).with_context(|| {
        format!(
            "Failed to parse baseline JSON at {}. \
             The file must be a `cqs eval --save` output.",
            baseline_path.display()
        )
    })?;

    let mut warnings = Vec::new();
    if baseline.cqs_version != current.cqs_version {
        warnings.push(format!(
            "cqs version drift: baseline={}, current={}. Diff is still meaningful but be aware index/scoring may have changed.",
            baseline.cqs_version, current.cqs_version
        ));
    }
    if baseline.index_model != current.index_model {
        warnings.push(format!(
            "index model drift: baseline={}, current={}. Comparing across models is rarely apples-to-apples.",
            baseline.index_model, current.index_model
        ));
    }
    for w in &warnings {
        tracing::warn!(warning = %w, "baseline drift");
    }

    let overall_delta = KDelta {
        r1: to_pp(current.overall.r_at_1) - to_pp(baseline.overall.r_at_1),
        r5: to_pp(current.overall.r_at_5) - to_pp(baseline.overall.r_at_5),
        r20: to_pp(current.overall.r_at_20) - to_pp(baseline.overall.r_at_20),
    };

    // Union of category keys from both sides so a category present in only
    // one of them still shows up in the diff.
    let mut all_cats: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for k in baseline.by_category.keys() {
        all_cats.insert(k.as_str());
    }
    for k in current.by_category.keys() {
        all_cats.insert(k.as_str());
    }

    let mut by_category_delta: BTreeMap<String, KDelta> = BTreeMap::new();
    let mut regressions: Vec<Regression> = Vec::new();

    for cat in all_cats {
        let b = baseline.by_category.get(cat);
        let c = current.by_category.get(cat);

        let (b_r1, b_r5, b_r20) = b
            .map(|s| (to_pp(s.r_at_1), to_pp(s.r_at_5), to_pp(s.r_at_20)))
            .unwrap_or((0.0, 0.0, 0.0));
        let (c_r1, c_r5, c_r20) = c
            .map(|s| (to_pp(s.r_at_1), to_pp(s.r_at_5), to_pp(s.r_at_20)))
            .unwrap_or((0.0, 0.0, 0.0));

        let delta = KDelta {
            r1: c_r1 - b_r1,
            r5: c_r5 - b_r5,
            r20: c_r20 - b_r20,
        };

        // Only flag as regression if the category exists on the baseline side.
        // A new category in the current run with no baseline counterpart isn't
        // a regression — there's nothing to regress *from*.
        if b.is_some() {
            for (metric, delta_val, b_val, c_val) in [
                ("R@1", delta.r1, b_r1, c_r1),
                ("R@5", delta.r5, b_r5, c_r5),
                ("R@20", delta.r20, b_r20, c_r20),
            ] {
                // Strictly greater than tolerance is a regression. Allows
                // `--tolerance 0` to mean "any drop fails".
                if -delta_val > tolerance_pp {
                    regressions.push(Regression {
                        category: cat.to_string(),
                        metric: metric.to_string(),
                        baseline_value: b_val,
                        current_value: c_val,
                        delta_pp: delta_val,
                    });
                }
            }
        }

        by_category_delta.insert(cat.to_string(), delta);
    }

    Ok(DiffReport {
        baseline_path: baseline_path.to_path_buf(),
        baseline_meta: BaselineMeta {
            cqs_version: baseline.cqs_version.clone(),
            index_model: baseline.index_model.clone(),
            query_file: baseline.query_file.clone(),
            overall_n: baseline.overall.n,
        },
        current_meta: BaselineMeta {
            cqs_version: current.cqs_version.clone(),
            index_model: current.index_model.clone(),
            query_file: current.query_file.clone(),
            overall_n: current.overall.n,
        },
        overall_delta,
        by_category_delta,
        regressions,
        tolerance_pp,
        warnings,
    })
}

/// Format a delta in percentage points with a sign and the `pp` suffix:
/// positive `(+1.2pp)`, negative `(-0.8pp)`, exactly zero `(±0.0pp)`.
///
/// One decimal place keeps the column tidy; reading two decimals on
/// percentage points adds noise without information.
fn format_delta(delta: f64) -> String {
    if delta.abs() < 0.05 {
        // Anything that rounds to ±0.0 prints as ±0.0pp so the eye reads
        // "no change" instead of "0.04 — wait is that a drop?".
        "(\u{00b1}0.0pp)".to_string()
    } else if delta > 0.0 {
        format!("(+{:.1}pp)", delta)
    } else {
        format!("({:.1}pp)", delta)
    }
}

/// Render the diff to stdout. Mirrors the spec's text shape so a user
/// running locally and a CI log render the same output.
pub(crate) fn print_diff_report(report: &DiffReport, json: bool) {
    if json {
        // unwrap is fine here — DiffReport derives Serialize over plain types
        // (no maps with non-string keys, no custom serializers) so it cannot
        // fail to serialize.
        match serde_json::to_string_pretty(report) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to serialize DiffReport as JSON");
                eprintln!("error: failed to serialize diff report as JSON: {e}");
            }
        }
        return;
    }

    println!(
        "=== eval diff: {} (N={}) ===",
        report.current_meta.query_file, report.current_meta.overall_n
    );
    println!("CURRENT vs {}", report.baseline_path.display());
    if report.baseline_meta.cqs_version != report.current_meta.cqs_version
        || report.baseline_meta.index_model != report.current_meta.index_model
        || report.baseline_meta.overall_n != report.current_meta.overall_n
    {
        println!(
            "  baseline: cqs={} model={} N={}",
            report.baseline_meta.cqs_version,
            report.baseline_meta.index_model,
            report.baseline_meta.overall_n,
        );
        println!(
            "  current : cqs={} model={} N={}",
            report.current_meta.cqs_version,
            report.current_meta.index_model,
            report.current_meta.overall_n,
        );
    }
    println!();

    for w in &report.warnings {
        println!("warning: {}", w);
    }
    if !report.warnings.is_empty() {
        println!();
    }

    println!(
        "OVERALL: R@1 {} R@5 {} R@20 {}",
        format_delta(report.overall_delta.r1),
        format_delta(report.overall_delta.r5),
        format_delta(report.overall_delta.r20),
    );
    println!();

    if !report.by_category_delta.is_empty() {
        println!(
            "{:<28} {:>12} {:>12} {:>12}",
            "category", "R@1 (\u{0394})", "R@5 (\u{0394})", "R@20 (\u{0394})"
        );
        for (cat, delta) in &report.by_category_delta {
            println!(
                "{:<28} {:>12} {:>12} {:>12}",
                cat,
                format_delta(delta.r1),
                format_delta(delta.r5),
                format_delta(delta.r20),
            );
        }
        println!();
    }

    if report.regressions.is_empty() {
        println!(
            "(no regressions beyond tolerance \u{00b1}{:.1}pp)",
            report.tolerance_pp
        );
    } else {
        println!(
            "REGRESSIONS (exit 1) — tolerance \u{00b1}{:.1}pp:",
            report.tolerance_pp
        );
        for reg in &report.regressions {
            println!(
                "  {:<24} {:>5}  baseline {:>5.1}% \u{2192} current {:>5.1}% {}",
                reg.category,
                reg.metric,
                reg.baseline_value,
                reg.current_value,
                format_delta(reg.delta_pp),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use super::super::runner::{CategoryStats, Overall};

    /// Build a deterministic dummy report. Per-category numbers are passed
    /// in as `(name, r_at_1, r_at_5, r_at_20)` so each test is self-explanatory.
    fn make_report(
        overall: (f64, f64, f64),
        cats: &[(&str, f64, f64, f64)],
        version: &str,
        model: &str,
    ) -> EvalReport {
        let n = cats.len().max(1);
        let mut by_category = BTreeMap::new();
        for (name, r1, r5, r20) in cats {
            by_category.insert(
                (*name).to_string(),
                CategoryStats {
                    n: 1,
                    r_at_1: *r1,
                    r_at_5: *r5,
                    r_at_20: *r20,
                },
            );
        }
        EvalReport {
            query_count: n,
            skipped: 0,
            elapsed_secs: 1.0,
            queries_per_sec: 1.0,
            overall: Overall {
                n,
                r_at_1: overall.0,
                r_at_5: overall.1,
                r_at_20: overall.2,
            },
            by_category,
            index_model: model.to_string(),
            cqs_version: version.to_string(),
            query_file: "noop.json".into(),
            limit: 20,
            category_filter: None,
        }
    }

    /// Save a report to a tempfile so the comparator can read it back.
    fn save_to_tmp(report: &EvalReport) -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("tmp");
        std::fs::write(tmp.path(), serde_json::to_vec_pretty(report).unwrap()).unwrap();
        tmp
    }

    /// Sanity: a diff against itself is all zeros and zero regressions.
    /// Pins the basic round-trip — save current, compare current to it,
    /// expect a no-op diff.
    #[test]
    fn test_diff_self_is_zero() {
        let r = make_report(
            (0.40, 0.60, 0.80),
            &[("cat_a", 0.50, 0.70, 0.90)],
            "1.27.0",
            "bge-large",
        );
        let tmp = save_to_tmp(&r);
        let diff = compare_against_baseline(&r, tmp.path(), 0.5).unwrap();
        assert_eq!(diff.overall_delta.r1, 0.0);
        assert_eq!(diff.overall_delta.r5, 0.0);
        assert_eq!(diff.overall_delta.r20, 0.0);
        assert!(diff.regressions.is_empty());
        assert!(diff.warnings.is_empty());
    }

    /// Pin: tolerance comparison is strict-greater. A drop *exactly equal*
    /// to tolerance is allowed; only strictly greater drops are flagged.
    #[test]
    fn test_diff_tolerance_is_strict_greater() {
        let baseline = make_report((0.40, 0.60, 0.80), &[("a", 0.40, 0.60, 0.80)], "1.27", "m");
        // current.r1 = 0.395 → drop of 0.5pp == tolerance, must NOT regress
        let current = make_report(
            (0.395, 0.60, 0.80),
            &[("a", 0.395, 0.60, 0.80)],
            "1.27",
            "m",
        );
        let tmp = save_to_tmp(&baseline);
        let diff = compare_against_baseline(&current, tmp.path(), 0.5).unwrap();
        assert!(
            diff.regressions.is_empty(),
            "drop == tolerance should NOT regress, got: {:?}",
            diff.regressions
        );
    }

    /// `format_delta` rounds to one decimal and uses ± for ~zero.
    #[test]
    fn test_format_delta_shape() {
        assert_eq!(format_delta(0.0), "(\u{00b1}0.0pp)");
        assert_eq!(format_delta(1.2), "(+1.2pp)");
        assert_eq!(format_delta(-0.8), "(-0.8pp)");
        // Anything that rounds to 0.0 reads as ±0.0pp:
        assert_eq!(format_delta(0.04), "(\u{00b1}0.0pp)");
        assert_eq!(format_delta(-0.04), "(\u{00b1}0.0pp)");
    }
}
