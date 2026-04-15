//! Suggest command — auto-detect note-worthy patterns
//!
//! Core struct is [`SuggestEntry`]; build with [`build_suggest_entries`].
//! CLI uses text output for human display, batch serializes with `serde_json::to_value()`.

use anyhow::Result;
use colored::Colorize;

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub(crate) struct SuggestEntry {
    pub text: String,
    pub sentiment: f64,
    pub mentions: Vec<String>,
    pub reason: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct SuggestOutput {
    pub suggestions: Vec<SuggestEntry>,
    pub count: usize,
    pub applied: bool,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build typed suggest entries from lib-level suggestions.
pub(crate) fn build_suggest_entries(
    suggestions: &[cqs::suggest::SuggestedNote],
) -> Vec<SuggestEntry> {
    let _span = tracing::info_span!("build_suggest_entries", count = suggestions.len()).entered();

    suggestions
        .iter()
        .map(|s| SuggestEntry {
            text: s.text.clone(),
            sentiment: s.sentiment as f64,
            mentions: s.mentions.clone(),
            reason: s.reason.clone(),
        })
        .collect()
}

/// Build the full suggest output (entries + metadata).
pub(crate) fn build_suggest_output(
    suggestions: &[cqs::suggest::SuggestedNote],
    applied: bool,
) -> SuggestOutput {
    let _span =
        tracing::info_span!("build_suggest_output", count = suggestions.len(), applied).entered();

    SuggestOutput {
        count: suggestions.len(),
        applied,
        suggestions: build_suggest_entries(suggestions),
    }
}

// ---------------------------------------------------------------------------
// CLI command
// ---------------------------------------------------------------------------

pub(crate) fn cmd_suggest(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    json: bool,
    apply: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_suggest", apply).entered();

    let store = &ctx.store;
    let root = &ctx.root;
    let suggestions = cqs::suggest::suggest_notes(store, root)?;

    if suggestions.is_empty() {
        if json {
            let output = build_suggest_output(&suggestions, false);
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("No suggestions — codebase looks clean.");
        }
        return Ok(());
    }

    if json {
        if apply {
            apply_suggestions(&suggestions, root, &ctx.cqs_dir)?;
        }
        let output = build_suggest_output(&suggestions, apply);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if apply {
        apply_suggestions(&suggestions, root, &ctx.cqs_dir)?;
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

/// Applies suggested notes to the notes file and re-indexes them.
///
/// The `CommandContext` holds a `Store<ReadOnly>` (suggest is a Group B
/// read-only command), but `--apply` needs to write. This helper opens
/// a scoped `Store<ReadWrite>` just for the reindex, mirroring the
/// pattern used by `cmd_gc` and `cmd_notes_mutate`. The typestate
/// enforces that the write path compiles only when we explicitly
/// promote the store; an accidental call to a write method on `ctx.store`
/// is now a compile error.
fn apply_suggestions(
    suggestions: &[cqs::suggest::SuggestedNote],
    root: &std::path::Path,
    cqs_dir: &std::path::Path,
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

    // Re-index notes. Opens a read-write store for the mutation — the
    // CLI already holds a read-only handle via CommandContext, but the
    // #946 typestate won't let us use it here.
    let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    let rw_store = cqs::Store::open(&index_path)?;
    let notes = cqs::parse_notes(&notes_path)?;
    cqs::index_notes(&notes, &notes_path, &rw_store)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_entry_serialization() {
        let entry = SuggestEntry {
            text: "Missing error handling in parser".into(),
            sentiment: -0.5,
            mentions: vec!["parser.rs".into()],
            reason: "No Result propagation".into(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["text"], "Missing error handling in parser");
        assert_eq!(json["sentiment"], -0.5);
        assert_eq!(json["mentions"][0], "parser.rs");
        assert_eq!(json["reason"], "No Result propagation");
    }

    #[test]
    fn suggest_output_empty() {
        let output = SuggestOutput {
            suggestions: vec![],
            count: 0,
            applied: false,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["count"], 0);
        assert_eq!(json["applied"], false);
        assert!(json["suggestions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn suggest_output_with_entries() {
        let output = SuggestOutput {
            suggestions: vec![SuggestEntry {
                text: "note".into(),
                sentiment: 0.5,
                mentions: vec![],
                reason: "pattern".into(),
            }],
            count: 1,
            applied: true,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["count"], 1);
        assert_eq!(json["applied"], true);
        assert_eq!(json["suggestions"][0]["text"], "note");
    }
}
