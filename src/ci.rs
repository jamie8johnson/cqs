//! CI pipeline analysis — composable diff review + dead code + gate logic.
//!
//! Combines [`review_diff`] impact analysis, dead code detection filtered to
//! diff-touched files, and configurable gate thresholds with CI exit codes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::AnalysisError;

use crate::diff_parse::parse_unified_diff;
use crate::impact::RiskLevel;
use crate::review::{review_diff_overlay, ReviewResult, RiskSummary};
use crate::store::DeadConfidence;
use crate::worktree_overlay::WorktreeOverlay;
use crate::Store;

/// Gate threshold level — determines when CI fails.
#[derive(Debug, Clone, Copy, serde::Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum GateThreshold {
    /// Fail if any High-risk function is detected
    High,
    /// Fail if any Medium or High risk function is detected
    Medium,
    /// Never fail — report only
    Off,
}

/// Result of gate evaluation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GateResult {
    /// The threshold that was applied
    pub threshold: GateThreshold,
    /// Whether the gate passed
    pub passed: bool,
    /// Human-readable reasons for failure (empty if passed)
    pub reasons: Vec<String>,
}

/// Dead code found in files touched by the diff.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeadInDiff {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line_start: u32,
    pub confidence: DeadConfidence,
}

/// Complete CI analysis report.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CiReport {
    /// Full review result (impact + risk + notes + staleness)
    pub review: ReviewResult,
    /// Dead code in files touched by the diff
    pub dead_in_diff: Vec<DeadInDiff>,
    /// Whether the dead-code scan actually ran successfully. When `false`,
    /// `dead_in_diff` is empty because the scan failed — not because no dead
    /// code exists. The gate fails the build on scan failure unless the
    /// threshold is `Off`, so CI can't silently green-light a broken index.
    /// Field is omitted from JSON on the happy path via `skip_serializing_if`
    /// so agents see no shape change — the failure signal is also surfaced in
    /// `gate.reasons`.
    #[serde(skip_serializing_if = "crate::serde_helpers::is_true")]
    pub dead_scan_ok: bool,
    /// Gate evaluation result
    pub gate: GateResult,
}

/// Run CI analysis on a unified diff.
///
/// Plain entry point: no worktree overlay. The full logic lives in
/// [`run_ci_analysis_overlay`]; this delegates with `None` so the parent-truth
/// byte shape is unchanged (participation is discarded). The CLI direct path
/// (`cmd_ci` text) and every existing caller go through here.
///
/// Composes:
/// 1. `review_diff()` — impact analysis + risk scoring + notes + staleness
/// 2. `find_dead_code()` — filtered to files touched by the diff
/// 3. Gate evaluation — configurable threshold
pub fn run_ci_analysis<Mode>(
    store: &Store<Mode>,
    diff_text: &str,
    root: &Path,
    threshold: GateThreshold,
) -> Result<CiReport, AnalysisError> {
    Ok(run_ci_analysis_overlay(store, diff_text, root, threshold, None)?.0)
}

/// Worktree-overlay-aware [`run_ci_analysis`]. Identical to
/// [`run_ci_analysis`] when `overlay` is `None`. When `Some`, BOTH bundled
/// sections reflect the worktree delta:
///
/// - **review** is computed via [`review_diff_overlay`] — only its
///   `affected_callers` section is overlaid (the same mask+union `cqs review`
///   applies); its `affected_tests` + per-function risk scores stay parent-truth.
///   So the review component is `"callers-only"`.
/// - **dead_in_diff** is recomputed over the merged caller graph via
///   [`crate::store::apply_dead_overlay`] (the same Direction A/B merge `cqs dead`
///   applies, BEFORE the diff-file filter), so a worktree call that flips a
///   diff-file function dead⇄live is reflected. The dead component is `"full"`.
///
/// Returns `(CiReport, overlay_participated)`. Participation is `true` iff the
/// review-overlay merge OR the dead-overlay merge consulted the delta for THIS
/// diff. Every parent-truth early-return (`review` maps no indexed function) and
/// every no-overlay call reports `false`.
///
/// Composite-marker honesty (the daemon adapter's concern): `cqs ci` bundles a
/// `"callers-only"` component (review) and a `"full"` component (dead). The
/// weakest component bounds the claim, so the daemon emits the combined
/// `_meta.overlay_graph = "callers-only"` marker when this returns
/// `participated == true` — NEVER `"full"`, which would over-promise the
/// review's tests/risk sections as delta-aware (PR2/PR3 precedent: a partial
/// overlay must never claim `"full"`).
pub fn run_ci_analysis_overlay<Mode>(
    store: &Store<Mode>,
    diff_text: &str,
    root: &Path,
    threshold: GateThreshold,
    overlay: Option<&WorktreeOverlay>,
) -> Result<(CiReport, bool), AnalysisError> {
    let _span =
        tracing::info_span!("run_ci_analysis", ?threshold, overlay = overlay.is_some()).entered();

    // 1. Full review (impact + risk + notes + stale). The overlay (when present)
    //    merges ONLY the direct-callers section; `review_participated` records
    //    whether it changed that section for this diff.
    let (review_opt, review_participated) = review_diff_overlay(store, diff_text, root, overlay)?;
    let review = match review_opt {
        Some(r) => r,
        None => {
            // Parent-truth early-return (no indexed function affected). The review
            // overlay reports `participated == false` here by construction, and the
            // dead scan below is skipped, so the whole report is parent-truth: no
            // marker downstream.
            tracing::info!("No indexed functions affected by diff");
            return Ok((
                CiReport {
                    review: empty_review(),
                    dead_in_diff: Vec::new(),
                    dead_scan_ok: true,
                    gate: GateResult {
                        threshold,
                        passed: true,
                        reasons: Vec::new(),
                    },
                },
                false,
            ));
        }
    };

    // 2. Dead code in diff files
    let hunks = parse_unified_diff(diff_text);
    let diff_file_strings: Vec<String> = hunks
        .iter()
        .map(|h| h.file.to_string_lossy().into_owned())
        .collect();
    let diff_files: HashSet<&str> = diff_file_strings.iter().map(|s| s.as_str()).collect();

    let mut dead_participated = false;
    let (dead_in_diff, dead_scan_ok) = match store.find_dead_code(true) {
        Ok((mut confident, mut possibly_pub)) => {
            // Worktree-overlay merge over the parent dead populations (BEFORE the
            // diff-file filter so a delta-driven flip is reflected). `cqs ci`'s
            // dead scan uses include_pub=true and reports every confidence, so the
            // merge runs with the same include_pub=true and `Low` floor — the
            // shared lib merge (`cqs dead` drives the identical call) so the two
            // surfaces cannot drift.
            if let Some(ov) = overlay {
                dead_participated = crate::store::apply_dead_overlay(
                    store,
                    ov,
                    &mut confident,
                    &mut possibly_pub,
                    true,
                    DeadConfidence::Low,
                )?;
            }
            let dead: Vec<DeadInDiff> = confident
                .into_iter()
                .chain(possibly_pub)
                .filter(|d| {
                    // Use Path::ends_with for component-level matching
                    // (not string suffix — "foobar.rs" must not match "bar.rs")
                    diff_files.iter().any(|f| d.chunk.file.ends_with(f))
                })
                .map(|d| DeadInDiff {
                    name: d.chunk.name.clone(),
                    file: PathBuf::from(crate::rel_display(&d.chunk.file, root)),
                    line_start: d.chunk.line_start,
                    confidence: d.confidence,
                })
                .collect();
            tracing::info!(
                dead_in_diff = dead.len(),
                diff_files = diff_files.len(),
                "Dead code scan complete"
            );
            (dead, true)
        }
        Err(e) => {
            // Record the scan failure so the gate fails loud — a scan failure
            // with no flag would let `evaluate_gate` green-light a CI run whose
            // index was unreadable.
            tracing::error!(error = %e, "Dead code detection failed — CI treating as a gate failure");
            (Vec::new(), false)
        }
    };

    // 3. Gate evaluation
    let gate = evaluate_gate(&review.risk_summary, threshold, dead_scan_ok);
    if !gate.passed {
        tracing::info!(
            threshold = ?threshold,
            reasons = ?gate.reasons,
            "CI gate failed"
        );
    }

    // Participation is true if EITHER bundled component consulted the delta. The
    // daemon adapter gates the (composite) marker on this bool.
    let participated = review_participated || dead_participated;
    Ok((
        CiReport {
            review,
            dead_in_diff,
            dead_scan_ok,
            gate,
        },
        participated,
    ))
}

/// Evaluate whether the CI gate passes for the given risk summary.
///
/// When `dead_scan_ok == false`, the gate fails unless the threshold is
/// `Off`. A broken dead-code scan is a broken build, not "0 dead code,
/// gate passed".
fn evaluate_gate(risk: &RiskSummary, threshold: GateThreshold, dead_scan_ok: bool) -> GateResult {
    let (mut passed, mut reasons) = match threshold {
        GateThreshold::High => {
            if risk.high > 0 {
                (
                    false,
                    vec![format!("{} high-risk function(s) detected", risk.high)],
                )
            } else {
                (true, Vec::new())
            }
        }
        GateThreshold::Medium => {
            let mut reasons = Vec::new();
            if risk.high > 0 {
                reasons.push(format!("{} high-risk function(s)", risk.high));
            }
            if risk.medium > 0 {
                reasons.push(format!("{} medium-risk function(s)", risk.medium));
            }
            (reasons.is_empty(), reasons)
        }
        GateThreshold::Off => (true, Vec::new()),
    };

    // Short-circuit: broken scan fails the gate for High/Medium thresholds.
    // `Off` means "never fail" — respect that even if the scan exploded.
    if !dead_scan_ok && !matches!(threshold, GateThreshold::Off) {
        passed = false;
        reasons.push("Dead-code scan failed — treating as gate failure".to_string());
    }

    GateResult {
        threshold,
        passed,
        reasons,
    }
}

/// Construct an empty ReviewResult for diffs with no indexed functions.
fn empty_review() -> ReviewResult {
    ReviewResult {
        changed_functions: Vec::new(),
        affected_callers: Vec::new(),
        affected_tests: Vec::new(),
        relevant_notes: Vec::new(),
        risk_summary: RiskSummary {
            high: 0,
            medium: 0,
            low: 0,
            overall: RiskLevel::Low,
        },
        stale_warning: None,
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a risk summary from counts of high, medium, and low priority items.
    ///
    /// Determines the overall risk level based on the presence of high-priority items first, then medium-priority items, with low as the default. All counts are included in the returned summary regardless of the overall level.
    ///
    /// # Arguments
    ///
    /// * `high` - Number of high-priority risk items
    /// * `medium` - Number of medium-priority risk items
    /// * `low` - Number of low-priority risk items
    ///
    /// # Returns
    ///
    /// A `RiskSummary` containing the item counts and computed overall risk level.
    fn make_summary(high: usize, medium: usize, low: usize) -> RiskSummary {
        let overall = if high > 0 {
            RiskLevel::High
        } else if medium > 0 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };
        RiskSummary {
            high,
            medium,
            low,
            overall,
        }
    }

    #[test]
    fn test_gate_high_passes_when_no_high_risk() {
        let risk = make_summary(0, 3, 5);
        let gate = evaluate_gate(&risk, GateThreshold::High, true);
        assert!(gate.passed);
        assert!(gate.reasons.is_empty());
    }

    #[test]
    fn test_gate_high_fails_on_high_risk() {
        let risk = make_summary(2, 1, 0);
        let gate = evaluate_gate(&risk, GateThreshold::High, true);
        assert!(!gate.passed);
        assert_eq!(gate.reasons.len(), 1);
        assert!(gate.reasons[0].contains("2 high-risk"));
    }

    #[test]
    fn test_gate_medium_fails_on_medium() {
        let risk = make_summary(0, 1, 5);
        let gate = evaluate_gate(&risk, GateThreshold::Medium, true);
        assert!(!gate.passed);
        assert_eq!(gate.reasons.len(), 1);
        assert!(gate.reasons[0].contains("medium-risk"));
    }

    #[test]
    fn test_gate_medium_reports_both_high_and_medium() {
        let risk = make_summary(2, 3, 1);
        let gate = evaluate_gate(&risk, GateThreshold::Medium, true);
        assert!(!gate.passed);
        assert_eq!(gate.reasons.len(), 2);
        assert!(gate.reasons[0].contains("high-risk"));
        assert!(gate.reasons[1].contains("medium-risk"));
    }

    #[test]
    fn test_gate_off_always_passes() {
        let risk = make_summary(10, 5, 0);
        let gate = evaluate_gate(&risk, GateThreshold::Off, true);
        assert!(gate.passed);
        assert!(gate.reasons.is_empty());
    }

    #[test]
    fn test_gate_all_low_passes_any_threshold() {
        let risk = make_summary(0, 0, 10);
        assert!(evaluate_gate(&risk, GateThreshold::High, true).passed);
        assert!(evaluate_gate(&risk, GateThreshold::Medium, true).passed);
        assert!(evaluate_gate(&risk, GateThreshold::Off, true).passed);
    }

    #[test]
    fn test_empty_review_has_low_risk() {
        let review = empty_review();
        assert_eq!(review.risk_summary.overall, RiskLevel::Low);
        assert!(review.changed_functions.is_empty());
        assert!(review.affected_callers.is_empty());
        assert!(review.affected_tests.is_empty());
    }

    /// A broken dead-code scan must fail the gate for High/Medium thresholds,
    /// regardless of the review risk summary, rather than letting
    /// `evaluate_gate` see an empty `dead_in_diff` and green-light the build.
    #[test]
    fn test_gate_fails_when_dead_scan_broken_high_threshold() {
        let risk = make_summary(0, 0, 0);
        let gate = evaluate_gate(&risk, GateThreshold::High, false);
        assert!(!gate.passed);
        assert_eq!(gate.reasons.len(), 1);
        assert!(gate.reasons[0].contains("Dead-code scan failed"));
    }

    #[test]
    fn test_gate_fails_when_dead_scan_broken_medium_threshold() {
        let risk = make_summary(0, 0, 5);
        let gate = evaluate_gate(&risk, GateThreshold::Medium, false);
        assert!(!gate.passed);
        assert!(gate
            .reasons
            .iter()
            .any(|r| r.contains("Dead-code scan failed")));
    }

    /// `Off` means "never fail" — respect that even if the scan exploded.
    /// Operators explicitly asked for permissive mode.
    #[test]
    fn test_gate_off_passes_even_when_dead_scan_broken() {
        let risk = make_summary(10, 5, 0);
        let gate = evaluate_gate(&risk, GateThreshold::Off, false);
        assert!(gate.passed);
        assert!(gate.reasons.is_empty());
    }
}
