//! Suggest command — auto-detect note-worthy patterns

use anyhow::Result;
use colored::Colorize;

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
    let embedder = cqs::Embedder::new()?;
    cqs::index_notes(&notes, &notes_path, &embedder, store)?;

    Ok(())
}
