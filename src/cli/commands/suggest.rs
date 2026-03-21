//! Suggest command — auto-detect note-worthy patterns

use anyhow::Result;
use colored::Colorize;

/// Generates and optionally applies code quality suggestions for the current project.
///
/// Analyzes the codebase to produce a list of suggestions for improvement. Can output results in JSON format or human-readable text, and optionally apply the suggestions to project files.
///
/// # Arguments
///
/// * `json` - If true, output suggestions in JSON format; otherwise use human-readable text
/// * `apply` - If true, apply the suggestions to the codebase; if false, perform a dry-run display only
///
/// # Returns
///
/// Returns `Ok(())` on success, or an error if project opening, suggestion generation, or application fails.
///
/// # Errors
///
/// Returns an error if the project store cannot be opened, suggestion generation fails, or applying suggestions encounters an error.
pub(crate) fn cmd_suggest(json: bool, apply: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_suggest", apply).entered();

    let (store, root, _) = crate::cli::open_project_store()?;
    let suggestions = cqs::suggest::suggest_notes(&store, &root)?;

    if suggestions.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No suggestions — codebase looks clean.");
        }
        return Ok(());
    }

    if json {
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
        println!("{}", serde_json::to_string_pretty(&json_val)?);

        if apply {
            apply_suggestions(&suggestions, &root, &store)?;
        }
    } else if apply {
        apply_suggestions(&suggestions, &root, &store)?;
        println!(
            "Applied {} suggestion{}.",
            suggestions.len(),
            if suggestions.len() == 1 { "" } else { "s" }
        );
    } else {
        // Dry-run: display suggestions
        println!("{} ({}):", "Suggested notes".bold(), suggestions.len());
        println!();
        for s in &suggestions {
            let sentiment_str = match s.sentiment {
                v if v <= -0.5 => format!("[{}]", format!("{:.1}", v).red()),
                v if v >= 0.5 => format!("[{}]", format!("{:.1}", v).green()),
                v => format!("[{:.1}]", v),
            };
            println!("  {} {} ({})", sentiment_str, s.text, s.reason.dimmed());
            if !s.mentions.is_empty() {
                println!("    mentions: {}", s.mentions.join(", ").dimmed());
            }
        }
        println!();
        println!("Run {} to add these notes.", "cqs suggest --apply".bold());
    }

    Ok(())
}

/// Applies suggested notes to the notes file and re-indexes them in the store.
///
/// This function takes a collection of suggested notes, converts them into note entries, appends them to the notes.toml file, and then re-indexes all notes in the store to reflect the changes.
///
/// # Arguments
///
/// * `suggestions` - A slice of suggested notes to apply
/// * `root` - The root directory path where docs/notes.toml is located
/// * `store` - The store instance used to index the notes
///
/// # Returns
///
/// Returns `Ok(())` on success, or an error if file operations or indexing fail.
///
/// # Errors
///
/// Returns an error if the notes file cannot be read, written, parsed, or if indexing the notes fails.
fn apply_suggestions(
    suggestions: &[cqs::suggest::SuggestedNote],
    root: &std::path::Path,
    store: &cqs::Store,
) -> Result<()> {
    let notes_path = root.join("docs/notes.toml");

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

    // Re-index notes
    let notes = cqs::parse_notes(&notes_path)?;
    cqs::index_notes(&notes, &notes_path, store)?;

    Ok(())
}
