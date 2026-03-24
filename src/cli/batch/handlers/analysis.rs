//! Analysis dispatch handlers: dead, health, stale, suggest, review, ci.

use anyhow::Result;

use super::super::BatchContext;
use cqs::normalize_path;
use cqs::store::DeadConfidence;

/// Identifies and reports dead code in a codebase.
///
/// Analyzes code to find functions that are never called, filtering results based on confidence level and visibility. Returns structured JSON containing categorized dead code findings.
///
/// # Arguments
///
/// * `ctx` - Batch context containing the code store and root directory path
/// * `include_pub` - Whether to include public functions in the dead code analysis
/// * `min_confidence` - Minimum confidence threshold for including results
///
/// # Returns
///
/// A JSON object with four fields:
/// - `dead`: Array of confidently identified dead functions
/// - `possibly_dead_pub`: Array of possibly dead public functions
/// - `total_dead`: Count of confidently dead functions
/// - `total_possibly_dead_pub`: Count of possibly dead public functions
///
/// Each function entry includes name, file path, line range, type, signature, language, and confidence level.
///
/// # Errors
///
/// Returns an error if the code store query fails.
pub(in crate::cli::batch) fn dispatch_dead(
    ctx: &BatchContext,
    include_pub: bool,
    min_confidence: &DeadConfidence,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_dead").entered();

    let (confident, possibly_pub) = ctx.store().find_dead_code(include_pub)?;

    let confident: Vec<_> = confident
        .into_iter()
        .filter(|d| d.confidence >= *min_confidence)
        .collect();
    let possibly_pub: Vec<_> = possibly_pub
        .into_iter()
        .filter(|d| d.confidence >= *min_confidence)
        .collect();

    let format_dead = |dead: &cqs::store::DeadFunction| {
        let confidence = dead.confidence.as_str();
        serde_json::json!({
            "name": dead.chunk.name,
            "file": cqs::rel_display(&dead.chunk.file, &ctx.root),
            "line_start": dead.chunk.line_start,
            "line_end": dead.chunk.line_end,
            "chunk_type": dead.chunk.chunk_type.to_string(),
            "signature": dead.chunk.signature,
            "language": dead.chunk.language.to_string(),
            "confidence": confidence,
        })
    };

    Ok(serde_json::json!({
        "dead": confident.iter().map(&format_dead).collect::<Vec<_>>(),
        "possibly_dead_pub": possibly_pub.iter().map(&format_dead).collect::<Vec<_>>(),
        "total_dead": confident.len(),
        "total_possibly_dead_pub": possibly_pub.len(),
    }))
}

/// Dispatches a request to identify stale and missing files in the batch store.
///
/// Retrieves the file set from the batch context and queries the store for files whose modification times have changed or are no longer present on disk. Returns a JSON report containing lists of stale files with their stored and current modification times, missing files, and summary statistics.
///
/// # Arguments
///
/// * `ctx` - The batch context containing the store and file set information.
///
/// # Returns
///
/// A JSON object containing:
/// - `stale`: Array of stale files with their origin path, stored mtime, and current mtime
/// - `missing`: Array of missing file paths
/// - `total_indexed`: Total number of indexed files
/// - `stale_count`: Count of stale files
/// - `missing_count`: Count of missing files
///
/// # Errors
///
/// Returns an error if the file set cannot be retrieved from the context or if the store query fails.
pub(in crate::cli::batch) fn dispatch_stale(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stale").entered();

    let file_set = ctx.file_set()?;
    let report = ctx.store().list_stale_files(&file_set)?;

    let stale_json: Vec<_> = report
        .stale
        .iter()
        .map(|f| {
            serde_json::json!({
                "origin": normalize_path(&f.file),
                "stored_mtime": f.stored_mtime,
                "current_mtime": f.current_mtime,
            })
        })
        .collect();

    let missing_json: Vec<_> = report
        .missing
        .iter()
        .map(|path| serde_json::json!(normalize_path(path)))
        .collect();

    Ok(serde_json::json!({
        "stale": stale_json,
        "missing": missing_json,
        "total_indexed": report.total_indexed,
        "stale_count": report.stale.len(),
        "missing_count": report.missing.len(),
    }))
}

/// Performs a health check on the batch processing system and returns the results as JSON.
///
/// This function executes a comprehensive health check that validates the store, file set, and CQS directory, then serializes the health report to a JSON value for reporting purposes.
///
/// # Arguments
///
/// * `ctx` - The batch processing context containing the store, file set, and CQS directory paths.
///
/// # Returns
///
/// A `Result` containing a `serde_json::Value` representing the health check report, or an error if the health check fails or serialization fails.
///
/// # Errors
///
/// Returns an error if retrieving the file set fails, if the health check itself fails, or if serializing the report to JSON fails.
pub(in crate::cli::batch) fn dispatch_health(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_health").entered();

    let file_set = ctx.file_set()?;
    let report = cqs::health::health_check(&ctx.store(), &file_set, &ctx.cqs_dir)?;

    Ok(serde_json::to_value(&report)?)
}

/// Suggests notes from codebase patterns and optionally applies them.
pub(in crate::cli::batch) fn dispatch_suggest(
    ctx: &BatchContext,
    apply: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_suggest", apply).entered();

    let suggestions = cqs::suggest::suggest_notes(&ctx.store(), &ctx.root)?;

    if apply && !suggestions.is_empty() {
        let notes_path = ctx.root.join("docs/notes.toml");
        let entries: Vec<cqs::NoteEntry> = suggestions
            .iter()
            .map(|s| cqs::NoteEntry {
                sentiment: s.sentiment,
                text: s.text.clone(),
                mentions: s.mentions.clone(),
            })
            .collect();
        cqs::rewrite_notes_file(&notes_path, |notes| {
            notes.extend(entries);
            Ok(())
        })?;
        let notes = cqs::parse_notes(&notes_path)?;
        cqs::index_notes(&notes, &notes_path, &ctx.store())?;
    }

    let json_val: Vec<_> = suggestions
        .iter()
        .map(|s| {
            serde_json::json!({
                "text": s.text,
                "sentiment": s.sentiment,
                "mentions": s.mentions,
                "reason": s.reason,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "suggestions": json_val,
        "total": suggestions.len(),
        "applied": apply,
    }))
}

/// Runs a diff-aware review and returns results as JSON.
///
/// Executes `git diff` against the given base ref (or HEAD) and runs the
/// review pipeline: diff impact, risk scoring, note matching, staleness.
pub(in crate::cli::batch) fn dispatch_review(
    ctx: &BatchContext,
    base: Option<&str>,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_review", ?base).entered();

    let diff_text = crate::cli::commands::run_git_diff(base)?;
    let result = cqs::review_diff(&ctx.store(), &diff_text, &ctx.root)?;

    match result {
        None => Ok(serde_json::json!({
            "changed_functions": [],
            "affected_callers": [],
            "affected_tests": [],
            "risk_summary": { "overall": "low", "high": 0, "medium": 0, "low": 0 },
        })),
        Some(mut review) => {
            // Apply token budget if specified
            if let Some(budget) = tokens {
                crate::cli::commands::review::apply_token_budget_public(&mut review, budget, true);
            }
            let mut output: serde_json::Value = serde_json::to_value(&review)?;
            if let Some(budget) = tokens {
                output["token_budget"] = serde_json::json!(budget);
            }
            Ok(output)
        }
    }
}

/// Runs CI analysis (review + dead code + gate) and returns results as JSON.
///
/// Note: In batch mode, gate failure is reported in the JSON output rather than
/// causing a process exit, since the batch session must continue.
pub(in crate::cli::batch) fn dispatch_ci(
    ctx: &BatchContext,
    base: Option<&str>,
    gate: &crate::cli::GateThreshold,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_ci", ?gate).entered();

    let diff_text = crate::cli::commands::run_git_diff(base)?;
    let mut report = cqs::ci::run_ci_analysis(&ctx.store(), &diff_text, &ctx.root, *gate)?;

    // Apply token budget if specified
    if let Some(budget) = tokens {
        crate::cli::commands::ci::apply_ci_token_budget(&mut report.review, budget);
    }

    let mut output: serde_json::Value = serde_json::to_value(&report)?;
    if let Some(budget) = tokens {
        output["token_budget"] = serde_json::json!(budget);
    }
    Ok(output)
}
