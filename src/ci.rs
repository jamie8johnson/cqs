//! CI pipeline analysis — composable diff review + dead code + gate logic.
//!
//! Combines [`review_diff`] impact analysis, dead code detection filtered to
//! diff-touched files, and configurable gate thresholds with CI exit codes.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;

use crate::diff_parse::parse_unified_diff;
use crate::impact::RiskLevel;
use crate::review::{review_diff, ReviewResult, RiskSummary};
use crate::store::DeadConfidence;
use crate::Store;

/// Gate threshold level — determines when CI fails.
#[derive(Debug, Clone, Copy, serde::Serialize)]
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
#[derive(Debug, serde::Serialize)]
pub struct GateResult {
    /// The threshold that was applied
    pub threshold: GateThreshold,
    /// Whether the gate passed
    pub passed: bool,
    /// Human-readable reasons for failure (empty if passed)
    pub reasons: Vec<String>,
}

/// Dead code found in files touched by the diff.
#[derive(Debug, serde::Serialize)]
pub struct DeadInDiff {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub confidence: String,
}

/// Complete CI analysis report.
#[derive(Debug, serde::Serialize)]
pub struct CiReport {
    /// Full review result (impact + risk + notes + staleness)
    pub review: ReviewResult,
    /// Dead code in files touched by the diff
    pub dead_in_diff: Vec<DeadInDiff>,
    /// Gate evaluation result
    pub gate: GateResult,
}

/// Run CI analysis on a unified diff.
///
/// Composes:
/// 1. `review_diff()` — impact analysis + risk scoring + notes + staleness
/// 2. `find_dead_code()` — filtered to files touched by the diff
/// 3. Gate evaluation — configurable threshold
pub fn run_ci_analysis(
    store: &Store,
    diff_text: &str,
    root: &Path,
    threshold: GateThreshold,
) -> Result<CiReport> {
    let _span = tracing::info_span!("run_ci_analysis", ?threshold).entered();

    // 1. Full review (impact + risk + notes + stale)
    let review = match review_diff(store, diff_text, root)? {
        Some(r) => r,
        None => {
            tracing::info!("No indexed functions affected by diff");
            return Ok(CiReport {
                review: empty_review(),
                dead_in_diff: Vec::new(),
                gate: GateResult {
                    threshold,
                    passed: true,
                    reasons: Vec::new(),
                },
            });
        }
    };

    // 2. Dead code in diff files
    let hunks = parse_unified_diff(diff_text);
    let diff_files: HashSet<&str> = hunks.iter().map(|h| h.file.as_str()).collect();

    let dead_in_diff = match store.find_dead_code(true) {
        Ok((confident, possibly_pub)) => {
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
                    file: crate::rel_display(&d.chunk.file, root),
                    line_start: d.chunk.line_start,
                    confidence: match d.confidence {
                        DeadConfidence::High => "high",
                        DeadConfidence::Medium => "medium",
                        DeadConfidence::Low => "low",
                    }
                    .to_string(),
                })
                .collect();
            tracing::info!(
                dead_in_diff = dead.len(),
                diff_files = diff_files.len(),
                "Dead code scan complete"
            );
            dead
        }
        Err(e) => {
            tracing::warn!(error = %e, "Dead code detection failed, skipping");
            Vec::new()
        }
    };

    // 3. Gate evaluation
    let gate = evaluate_gate(&review.risk_summary, threshold);
    if !gate.passed {
        tracing::info!(
            threshold = ?threshold,
            reasons = ?gate.reasons,
            "CI gate failed"
        );
    }

    Ok(CiReport {
        review,
        dead_in_diff,
        gate,
    })
}

/// Evaluate whether the CI gate passes for the given risk summary.
fn evaluate_gate(risk: &RiskSummary, threshold: GateThreshold) -> GateResult {
    let (passed, reasons) = match threshold {
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
        let gate = evaluate_gate(&risk, GateThreshold::High);
        assert!(gate.passed);
        assert!(gate.reasons.is_empty());
    }

    #[test]
    fn test_gate_high_fails_on_high_risk() {
        let risk = make_summary(2, 1, 0);
        let gate = evaluate_gate(&risk, GateThreshold::High);
        assert!(!gate.passed);
        assert_eq!(gate.reasons.len(), 1);
        assert!(gate.reasons[0].contains("2 high-risk"));
    }

    #[test]
    fn test_gate_medium_fails_on_medium() {
        let risk = make_summary(0, 1, 5);
        let gate = evaluate_gate(&risk, GateThreshold::Medium);
        assert!(!gate.passed);
        assert_eq!(gate.reasons.len(), 1);
        assert!(gate.reasons[0].contains("medium-risk"));
    }

    #[test]
    fn test_gate_medium_reports_both_high_and_medium() {
        let risk = make_summary(2, 3, 1);
        let gate = evaluate_gate(&risk, GateThreshold::Medium);
        assert!(!gate.passed);
        assert_eq!(gate.reasons.len(), 2);
        assert!(gate.reasons[0].contains("high-risk"));
        assert!(gate.reasons[1].contains("medium-risk"));
    }

    #[test]
    fn test_gate_off_always_passes() {
        let risk = make_summary(10, 5, 0);
        let gate = evaluate_gate(&risk, GateThreshold::Off);
        assert!(gate.passed);
        assert!(gate.reasons.is_empty());
    }

    #[test]
    fn test_gate_all_low_passes_any_threshold() {
        let risk = make_summary(0, 0, 10);
        assert!(evaluate_gate(&risk, GateThreshold::High).passed);
        assert!(evaluate_gate(&risk, GateThreshold::Medium).passed);
        assert!(evaluate_gate(&risk, GateThreshold::Off).passed);
    }

    #[test]
    fn test_empty_review_has_low_risk() {
        let review = empty_review();
        assert_eq!(review.risk_summary.overall, RiskLevel::Low);
        assert!(review.changed_functions.is_empty());
        assert!(review.affected_callers.is_empty());
        assert!(review.affected_tests.is_empty());
    }
}
