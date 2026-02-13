//! Review command — comprehensive diff review context
//!
//! Composes impact analysis + gather context + notes + risk scoring
//! into a single structured review payload.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::diff_parse::parse_unified_diff;
use crate::impact::{
    analyze_diff_impact_with_graph, compute_risk_batch, map_hunks_to_functions, CallerDetail,
    DiffTestInfo, RiskLevel, RiskScore,
};
use crate::note::path_matches_mention;
use crate::Store;

/// Result of a comprehensive diff review.
#[derive(Debug, serde::Serialize)]
pub struct ReviewResult {
    /// Functions changed by the diff
    pub changed_functions: Vec<ReviewedFunction>,
    /// All callers affected by the changes (uses impact's CallerDetail directly)
    pub affected_callers: Vec<CallerDetail>,
    /// Tests affected by or suggested for the changes (uses impact's DiffTestInfo directly)
    pub affected_tests: Vec<DiffTestInfo>,
    /// Notes relevant to changed files
    pub relevant_notes: Vec<NoteEntry>,
    /// Aggregated risk summary
    pub risk_summary: RiskSummary,
    /// Files that are stale in the index (if any)
    pub stale_warning: Option<Vec<String>>,
    /// Non-fatal warnings encountered during review
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A changed function with its risk assessment.
#[derive(Debug, serde::Serialize)]
pub struct ReviewedFunction {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub risk: RiskScore,
}

/// A note relevant to the review.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteEntry {
    pub text: String,
    pub sentiment: f32,
    pub matching_files: Vec<String>,
}

/// Aggregated risk counts.
#[derive(Debug, serde::Serialize)]
pub struct RiskSummary {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub overall: RiskLevel,
}

/// Analyze a unified diff and produce a comprehensive review.
///
/// Steps:
/// 1. Parse diff -> changed functions
/// 2. Load call graph + test chunks (once, shared by impact + risk)
/// 3. Impact analysis -> callers + tests
/// 4. Risk scoring -> per-function risk
/// 5. Note matching -> relevant notes for changed files (non-fatal)
/// 6. Staleness check -> warn if changed files are stale (non-fatal)
pub fn review_diff(store: &Store, diff_text: &str, root: &Path) -> Result<Option<ReviewResult>> {
    let _span = tracing::info_span!("review_diff").entered();
    let mut warnings: Vec<String> = Vec::new();

    // 1. Parse hunks
    let hunks = parse_unified_diff(diff_text);
    if hunks.is_empty() {
        return Ok(None);
    }

    // 2. Map hunks to functions
    let changed = map_hunks_to_functions(store, &hunks);
    if changed.is_empty() {
        return Ok(None);
    }

    // 3. Load call graph and test chunks once — used by both impact and risk
    let graph = store.get_call_graph()?;
    let test_chunks = store.find_test_chunks()?;

    // 4. Impact analysis (reuses pre-loaded graph + test_chunks)
    let impact = analyze_diff_impact_with_graph(store, changed, &graph, &test_chunks)?;

    // 5. Compute risk scores for changed functions (reuses same graph + test_chunks)
    let changed_names: Vec<&str> = impact
        .changed_functions
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    let risk_scores = compute_risk_batch(&changed_names, &graph, &test_chunks);

    // 6. Build reviewed functions with risk
    let reviewed_functions: Vec<ReviewedFunction> = impact
        .changed_functions
        .iter()
        .zip(risk_scores)
        .map(|(cf, risk)| ReviewedFunction {
            name: cf.name.clone(),
            file: cf.file.clone(),
            line_start: cf.line_start,
            risk,
        })
        .collect();

    // 7. Match notes to changed files (non-fatal: warning on failure)
    let changed_files: HashSet<&str> = impact
        .changed_functions
        .iter()
        .map(|f| f.file.as_str())
        .collect();
    let relevant_notes = match match_notes(store, &changed_files) {
        Ok(notes) => notes,
        Err(e) => {
            let msg = format!("Failed to load notes for review: {e}");
            tracing::warn!("{}", msg);
            warnings.push(msg);
            Vec::new()
        }
    };

    // 8. Staleness check (non-fatal: warning on failure)
    let origins: Vec<&str> = changed_files.iter().copied().collect();
    let stale_warning = match store.check_origins_stale(&origins, root) {
        Ok(stale) if stale.is_empty() => None,
        Ok(stale) => Some(stale.into_iter().collect()),
        Err(e) => {
            let msg = format!("Failed to check staleness: {e}");
            tracing::warn!("{}", msg);
            warnings.push(msg);
            None
        }
    };

    // 9. Build risk summary
    let risk_summary = build_risk_summary(&reviewed_functions);

    // 10. Relativize paths in impact types for display
    let affected_callers: Vec<CallerDetail> = impact
        .all_callers
        .into_iter()
        .map(|mut c| {
            c.file = PathBuf::from(crate::rel_display(&c.file, root));
            c
        })
        .collect();

    let affected_tests: Vec<DiffTestInfo> = impact
        .all_tests
        .into_iter()
        .map(|mut t| {
            t.file = PathBuf::from(crate::rel_display(&t.file, root));
            t
        })
        .collect();

    Ok(Some(ReviewResult {
        changed_functions: reviewed_functions,
        affected_callers,
        affected_tests,
        relevant_notes,
        risk_summary,
        stale_warning,
        warnings,
    }))
}

/// Match notes to a set of changed file paths.
///
/// Returns an error if notes cannot be loaded (caller decides how to handle).
fn match_notes(store: &Store, changed_files: &HashSet<&str>) -> Result<Vec<NoteEntry>> {
    let _span = tracing::info_span!("match_notes").entered();

    let notes = store.list_notes_summaries()?;

    Ok(notes
        .into_iter()
        .filter_map(|note| {
            let matching: Vec<String> = changed_files
                .iter()
                .filter(|file| {
                    note.mentions
                        .iter()
                        .any(|mention| path_matches_mention(file, mention))
                })
                .map(|f| f.to_string())
                .collect();

            if matching.is_empty() {
                None
            } else {
                Some(NoteEntry {
                    text: note.text,
                    sentiment: note.sentiment,
                    matching_files: matching,
                })
            }
        })
        .collect())
}

/// Build aggregated risk summary from reviewed functions.
fn build_risk_summary(functions: &[ReviewedFunction]) -> RiskSummary {
    let high = functions
        .iter()
        .filter(|f| f.risk.risk_level == RiskLevel::High)
        .count();
    let medium = functions
        .iter()
        .filter(|f| f.risk.risk_level == RiskLevel::Medium)
        .count();
    let low = functions
        .iter()
        .filter(|f| f.risk.risk_level == RiskLevel::Low)
        .count();

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
