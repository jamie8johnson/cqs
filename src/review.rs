//! Review command — comprehensive diff review context
//!
//! Composes impact analysis + gather context + notes + risk scoring
//! into a single structured review payload.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;

use crate::diff_parse::parse_unified_diff;
use crate::impact::{
    analyze_diff_impact, compute_risk_batch, map_hunks_to_functions, RiskLevel, RiskScore,
};
use crate::note::path_matches_mention;
use crate::Store;

/// Result of a comprehensive diff review.
#[derive(Debug, serde::Serialize)]
pub struct ReviewResult {
    /// Functions changed by the diff
    pub changed_functions: Vec<ReviewedFunction>,
    /// All callers affected by the changes
    pub affected_callers: Vec<CallerEntry>,
    /// Tests affected by or suggested for the changes
    pub affected_tests: Vec<TestEntry>,
    /// Notes relevant to changed files
    pub relevant_notes: Vec<NoteEntry>,
    /// Aggregated risk summary
    pub risk_summary: RiskSummary,
    /// Files that are stale in the index (if any)
    pub stale_warning: Option<Vec<String>>,
}

/// A changed function with its risk assessment.
#[derive(Debug, serde::Serialize)]
pub struct ReviewedFunction {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub risk: RiskScore,
}

/// A caller affected by the changes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CallerEntry {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub call_line: u32,
}

/// A test affected by the changes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TestEntry {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub via: String,
    pub call_depth: usize,
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
/// 1. Parse diff → changed functions
/// 2. Impact analysis → callers + tests
/// 3. Note matching → relevant notes for changed files
/// 4. Risk scoring → per-function risk
/// 5. Staleness check → warn if changed files are stale
pub fn review_diff(store: &Store, diff_text: &str, root: &Path) -> Result<Option<ReviewResult>> {
    let _span = tracing::info_span!("review_diff").entered();

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

    // 3. Impact analysis
    let impact = analyze_diff_impact(store, changed)?;

    // 4. Load call graph and test chunks for risk scoring
    let graph = store.get_call_graph().map_err(|e| {
        tracing::warn!(error = %e, "Failed to load call graph for risk scoring");
        e
    })?;
    let test_chunks = store.find_test_chunks().map_err(|e| {
        tracing::warn!(error = %e, "Failed to load test chunks for risk scoring");
        e
    })?;

    // 5. Compute risk scores for changed functions
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

    // 7. Match notes to changed files
    let changed_files: HashSet<&str> = impact
        .changed_functions
        .iter()
        .map(|f| f.file.as_str())
        .collect();
    let relevant_notes = match_notes(store, &changed_files);

    // 8. Staleness check
    let origins: Vec<&str> = changed_files.iter().copied().collect();
    let stale_warning = match store.check_origins_stale(&origins, root) {
        Ok(stale) if stale.is_empty() => None,
        Ok(stale) => Some(stale.into_iter().collect()),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check staleness");
            None
        }
    };

    // 9. Build risk summary
    let risk_summary = build_risk_summary(&reviewed_functions);

    // 10. Convert impact types to review types
    let affected_callers: Vec<CallerEntry> = impact
        .all_callers
        .iter()
        .map(|c| CallerEntry {
            name: c.name.clone(),
            file: crate::rel_display(&c.file, root),
            line: c.line,
            call_line: c.call_line,
        })
        .collect();

    let affected_tests: Vec<TestEntry> = impact
        .all_tests
        .iter()
        .map(|t| TestEntry {
            name: t.name.clone(),
            file: crate::rel_display(&t.file, root),
            line: t.line,
            via: t.via.clone(),
            call_depth: t.call_depth,
        })
        .collect();

    Ok(Some(ReviewResult {
        changed_functions: reviewed_functions,
        affected_callers,
        affected_tests,
        relevant_notes,
        risk_summary,
        stale_warning,
    }))
}

/// Match notes to a set of changed file paths.
fn match_notes(store: &Store, changed_files: &HashSet<&str>) -> Vec<NoteEntry> {
    let _span = tracing::info_span!("match_notes").entered();

    let notes = match store.list_notes_summaries() {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load notes for review");
            return Vec::new();
        }
    };

    notes
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
        .collect()
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
