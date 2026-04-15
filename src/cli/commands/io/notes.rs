//! Notes command for cqs
//!
//! Lists and manages notes from docs/notes.toml.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

use cqs::{parse_notes, rewrite_notes_file, NoteEntry, NOTES_HEADER};

use crate::cli::{find_project_root, Cli};

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

/// JSON output for note mutation commands (add, update, remove).
#[derive(Debug, serde::Serialize)]
struct NoteMutationOutput {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    note_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sentiment: Option<f32>,
    text_preview: String,
    file: String,
    indexed: bool,
    total_notes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_error: Option<String>,
}

/// A single note entry in the list output.
#[derive(Debug, serde::Serialize)]
struct NoteListEntry {
    id: String,
    sentiment: f32,
    #[serde(rename = "type")]
    note_type: String,
    text: String,
    mentions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stale_mentions: Option<Vec<String>>,
}

/// Notes subcommands
#[derive(clap::Subcommand)]
pub(crate) enum NotesCommand {
    /// List all notes with sentiment and mentions
    List {
        /// Show only warnings (negative sentiment)
        #[arg(long)]
        warnings: bool,
        /// Show only patterns (positive sentiment)
        #[arg(long)]
        patterns: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Check mentions for staleness (verifies files exist and symbols are in index)
        #[arg(long)]
        check: bool,
    },
    /// Add a note to project memory
    Add {
        /// Note text
        text: String,
        /// Sentiment (-1, -0.5, 0, 0.5, 1)
        #[arg(long, default_value = "0", allow_negative_numbers = true)]
        sentiment: f32,
        /// File paths or concepts this note relates to (comma-separated)
        #[arg(long, value_delimiter = ',')]
        mentions: Option<Vec<String>>,
        /// Skip re-indexing after adding (useful for batch operations)
        #[arg(long)]
        no_reindex: bool,
    },
    /// Update an existing note (find by exact text match)
    Update {
        /// Exact text of the note to update
        text: String,
        /// New text
        #[arg(long)]
        new_text: Option<String>,
        /// New sentiment (-1, -0.5, 0, 0.5, 1)
        #[arg(long, allow_negative_numbers = true)]
        new_sentiment: Option<f32>,
        /// New mentions (replaces all, comma-separated)
        #[arg(long, value_delimiter = ',')]
        new_mentions: Option<Vec<String>>,
        /// Skip re-indexing after update
        #[arg(long)]
        no_reindex: bool,
    },
    /// Remove a note by exact text match
    Remove {
        /// Exact text of the note to remove
        text: String,
        /// Skip re-indexing after removal
        #[arg(long)]
        no_reindex: bool,
    },
}

/// Handle `notes list` — requires a readonly CommandContext for staleness checks.
pub(crate) fn cmd_notes(ctx: &crate::cli::CommandContext, subcmd: &NotesCommand) -> Result<()> {
    let _span = tracing::info_span!("cmd_notes").entered();
    match subcmd {
        NotesCommand::List {
            warnings,
            patterns,
            json,
            check,
        } => cmd_notes_list(ctx, *warnings, *patterns, *json, *check),
        // Mutations delegated to cmd_notes_mutate (Group A, no CommandContext)
        _ => anyhow::bail!("internal: notes dispatch routing bug — please file an issue"),
    }
}

/// Handle `notes add|update|remove` — opens one read-write store for reindex,
/// avoiding the extra readonly connection that CommandContext would create.
pub(crate) fn cmd_notes_mutate(cli: &Cli, subcmd: &NotesCommand) -> Result<()> {
    let _span = tracing::info_span!("cmd_notes_mutate").entered();
    match subcmd {
        NotesCommand::Add {
            text,
            sentiment,
            mentions,
            no_reindex,
        } => cmd_notes_add(cli, text, *sentiment, mentions.as_deref(), *no_reindex),
        NotesCommand::Update {
            text,
            new_text,
            new_sentiment,
            new_mentions,
            no_reindex,
        } => cmd_notes_update(
            cli,
            text,
            new_text.as_deref(),
            *new_sentiment,
            new_mentions.as_deref(),
            *no_reindex,
        ),
        NotesCommand::Remove { text, no_reindex } => cmd_notes_remove(cli, text, *no_reindex),
        NotesCommand::List { .. } => {
            anyhow::bail!("internal: notes dispatch routing bug — please file an issue")
        }
    }
}

/// Re-parse and re-index notes after a file mutation, reusing an existing store.
fn reindex_notes(root: &std::path::Path, store: &cqs::Store) -> (usize, Option<String>) {
    let notes_path = root.join("docs/notes.toml");
    match parse_notes(&notes_path) {
        Ok(notes) if !notes.is_empty() => match cqs::index_notes(&notes, &notes_path, store) {
            Ok(count) => (count, None),
            Err(e) => (0, Some(format!("Failed to index notes: {}", e))),
        },
        Ok(_) => (0, None),
        Err(e) => (0, Some(format!("Failed to parse notes: {}", e))),
    }
}

/// Open a read-write store for notes mutations that need to reindex.
fn open_rw_store(root: &std::path::Path) -> Result<cqs::Store> {
    let index_path = cqs::resolve_index_dir(root).join("index.db");
    cqs::Store::open(&index_path)
        .map_err(|e| anyhow::anyhow!("Failed to open index at {}: {}", index_path.display(), e))
}

/// Build a text preview (first 100 chars or full text).
fn text_preview(text: &str) -> String {
    text.char_indices()
        .nth(100)
        .map(|(i, _)| format!("{}...", &text[..i]))
        .unwrap_or_else(|| text.to_string())
}

/// Ensure docs/notes.toml exists, creating it with header if needed.
fn ensure_notes_file(root: &std::path::Path) -> Result<PathBuf> {
    let notes_path = root.join("docs/notes.toml");
    if let Some(parent) = notes_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create docs directory")?;
    }
    if !notes_path.exists() {
        std::fs::write(&notes_path, NOTES_HEADER).context("Failed to create notes.toml")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&notes_path, perms)
                .context("Failed to set notes.toml permissions")?;
        }
    }
    Ok(notes_path)
}

/// Add a note: validate text/sentiment, append to notes.toml, optionally reindex.
fn cmd_notes_add(
    cli: &Cli,
    text: &str,
    sentiment: f32,
    mentions: Option<&[String]>,
    no_reindex: bool,
) -> Result<()> {
    if text.is_empty() {
        bail!("Note text cannot be empty");
    }
    if text.len() > 2000 {
        bail!("Note text too long: {} bytes (max 2000)", text.len());
    }

    let sentiment = sentiment.clamp(-1.0, 1.0);
    let mentions: Vec<String> = mentions
        .unwrap_or(&[])
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect();

    let note_entry = NoteEntry {
        sentiment,
        text: text.to_string(),
        mentions,
    };

    let root = find_project_root();
    let notes_path = ensure_notes_file(&root)?;

    rewrite_notes_file(&notes_path, |entries| {
        entries.push(note_entry.clone());
        Ok(())
    })
    .context("Failed to add note")?;

    let (indexed, index_error) = if no_reindex {
        (0, None)
    } else {
        match open_rw_store(&root) {
            Ok(store) => reindex_notes(root.as_path(), &store),
            Err(e) => (0, Some(format!("{e}"))),
        }
    };

    let sentiment_label = if sentiment < -0.3 {
        "warning"
    } else if sentiment > 0.3 {
        "pattern"
    } else {
        "observation"
    };

    if cli.json {
        let result = NoteMutationOutput {
            status: "added".into(),
            note_type: Some(sentiment_label.into()),
            sentiment: Some(sentiment),
            text_preview: text_preview(text),
            file: "docs/notes.toml".into(),
            indexed: indexed > 0,
            total_notes: indexed,
            index_error,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "Added {} (sentiment: {:+.1}): {}",
            sentiment_label,
            sentiment,
            text_preview(text)
        );
        if indexed > 0 {
            println!("Indexed {} notes.", indexed);
        }
        if let Some(err) = index_error {
            tracing::warn!(error = %err, "Note operation warning");
        }
    }

    Ok(())
}

/// Update a note: match by text, apply new text/sentiment/mentions, optionally reindex.
fn cmd_notes_update(
    cli: &Cli,
    text: &str,
    new_text: Option<&str>,
    new_sentiment: Option<f32>,
    new_mentions: Option<&[String]>,
    no_reindex: bool,
) -> Result<()> {
    if text.is_empty() {
        bail!("Note text cannot be empty");
    }
    if new_text.is_none() && new_sentiment.is_none() && new_mentions.is_none() {
        bail!("At least one of --new-text, --new-sentiment, or --new-mentions must be provided");
    }
    if let Some(t) = new_text {
        if t.is_empty() {
            bail!("--new-text cannot be empty");
        }
        if t.len() > 2000 {
            bail!("--new-text too long: {} bytes (max 2000)", t.len());
        }
    }

    let root = find_project_root();
    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        bail!("No notes.toml found. Use 'cqs notes add' to create notes first.");
    }

    let text_trimmed = text.trim();
    let new_text_owned = new_text.map(|s| s.to_string());
    let new_sentiment_clamped = new_sentiment.map(|s| s.clamp(-1.0, 1.0));
    let new_mentions_owned = new_mentions.map(|m| {
        m.iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
    });

    rewrite_notes_file(&notes_path, |entries| {
        let entry = entries
            .iter_mut()
            .find(|e| e.text.trim() == text_trimmed)
            .ok_or_else(|| {
                cqs::NoteError::NotFound(format!(
                    "No note with text: '{}'",
                    text_preview(text_trimmed)
                ))
            })?;

        if let Some(ref t) = new_text_owned {
            entry.text = t.clone();
        }
        if let Some(s) = new_sentiment_clamped {
            entry.sentiment = s;
        }
        if let Some(ref m) = new_mentions_owned {
            entry.mentions = m.clone();
        }
        Ok(())
    })
    .context("Failed to update note")?;

    let (indexed, index_error) = if no_reindex {
        (0, None)
    } else {
        match open_rw_store(&root) {
            Ok(store) => reindex_notes(root.as_path(), &store),
            Err(e) => (0, Some(format!("{e}"))),
        }
    };

    let final_text = new_text.unwrap_or(text);
    if cli.json {
        let result = NoteMutationOutput {
            status: "updated".into(),
            note_type: None,
            sentiment: None,
            text_preview: text_preview(final_text),
            file: "docs/notes.toml".into(),
            indexed: indexed > 0,
            total_notes: indexed,
            index_error,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Updated: {}", text_preview(final_text));
        if indexed > 0 {
            println!("Indexed {} notes.", indexed);
        }
        if let Some(err) = index_error {
            tracing::warn!(error = %err, "Note operation warning");
        }
    }

    Ok(())
}

/// Remove a note by matching its text content, optionally reindex after.
fn cmd_notes_remove(cli: &Cli, text: &str, no_reindex: bool) -> Result<()> {
    if text.is_empty() {
        bail!("Note text cannot be empty");
    }

    let root = find_project_root();
    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        bail!("No notes.toml found");
    }

    let text_trimmed = text.trim();
    let mut removed_text = String::new();

    rewrite_notes_file(&notes_path, |entries| {
        let pos = entries
            .iter()
            .position(|e| e.text.trim() == text_trimmed)
            .ok_or_else(|| {
                cqs::NoteError::NotFound(format!(
                    "No note with text: '{}'",
                    text_preview(text_trimmed)
                ))
            })?;

        removed_text = entries[pos].text.clone();
        entries.remove(pos);
        Ok(())
    })
    .context("Failed to remove note")?;

    let (indexed, index_error) = if no_reindex {
        (0, None)
    } else {
        match open_rw_store(&root) {
            Ok(store) => reindex_notes(root.as_path(), &store),
            Err(e) => (0, Some(format!("{e}"))),
        }
    };

    if cli.json {
        let result = NoteMutationOutput {
            status: "removed".into(),
            note_type: None,
            sentiment: None,
            text_preview: text_preview(&removed_text),
            file: "docs/notes.toml".into(),
            indexed: indexed > 0,
            total_notes: indexed,
            index_error,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Removed: {}", text_preview(&removed_text));
        if indexed > 0 {
            println!("Indexed {} notes.", indexed);
        }
        if let Some(err) = index_error {
            tracing::warn!(error = %err, "Note operation warning");
        }
    }

    Ok(())
}

/// List notes from docs/notes.toml
fn cmd_notes_list(
    ctx: &crate::cli::CommandContext,
    warnings_only: bool,
    patterns_only: bool,
    json: bool,
    check: bool,
) -> Result<()> {
    let root = &ctx.root;
    let notes_path = root.join("docs/notes.toml");

    if !notes_path.exists() {
        bail!("No notes file found at docs/notes.toml. Run 'cqs init' or create it manually.");
    }

    let notes = parse_notes(&notes_path)?;

    if notes.is_empty() {
        println!("No notes found.");
        return Ok(());
    }

    // Staleness check (requires store)
    let staleness: std::collections::HashMap<String, Vec<String>> = if check {
        cqs::suggest::check_note_staleness(&ctx.store, root)?
            .into_iter()
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    // Filter
    let filtered: Vec<_> = notes
        .iter()
        .filter(|n| {
            if warnings_only {
                n.is_warning()
            } else if patterns_only {
                n.is_pattern()
            } else {
                true
            }
        })
        .collect();

    if json || ctx.cli.json {
        let json_notes: Vec<NoteListEntry> = filtered
            .iter()
            .map(|n| {
                let note_type = if n.is_warning() {
                    "warning"
                } else if n.is_pattern() {
                    "pattern"
                } else {
                    "neutral"
                };
                let stale_mentions = if check {
                    Some(staleness.get(&n.text).cloned().unwrap_or_default())
                } else {
                    None
                };
                NoteListEntry {
                    id: n.id.clone(),
                    sentiment: n.sentiment,
                    note_type: note_type.into(),
                    text: n.text.clone(),
                    mentions: n.mentions.clone(),
                    stale_mentions,
                }
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_notes)?);
        return Ok(());
    }

    // Human-readable output
    let total = notes.len();
    let warn_count = notes.iter().filter(|n| n.is_warning()).count();
    let pat_count = notes.iter().filter(|n| n.is_pattern()).count();
    let neutral_count = total - warn_count - pat_count;

    println!(
        "{} notes ({} warnings, {} patterns, {} neutral)\n",
        total, warn_count, pat_count, neutral_count
    );

    for note in &filtered {
        let sentiment_marker = format!("[{:+.1}]", note.sentiment);

        // Truncate text for display (char-safe)
        let preview = if note.text.chars().count() > 120 {
            let end = note
                .text
                .char_indices()
                .nth(117)
                .map(|(i, _)| i)
                .unwrap_or(note.text.len());
            format!("{}...", &note.text[..end])
        } else {
            note.text.clone()
        };

        let mentions = if note.mentions.is_empty() {
            String::new()
        } else {
            format!("  mentions: {}", note.mentions.join(", "))
        };

        print!("  {} {}", sentiment_marker, preview);
        if check {
            if let Some(stale) = staleness.get(&note.text) {
                print!("  [STALE: {}]", stale.join(", "));
            }
        }
        println!();
        if !mentions.is_empty() {
            println!("  {}", mentions);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_mutation_output_add() {
        let output = NoteMutationOutput {
            status: "added".into(),
            note_type: Some("warning".into()),
            sentiment: Some(-0.5),
            text_preview: "some note text".into(),
            file: "docs/notes.toml".into(),
            indexed: true,
            total_notes: 5,
            index_error: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "added");
        assert_eq!(json["type"], "warning");
        assert_eq!(json["sentiment"], -0.5);
        assert_eq!(json["text_preview"], "some note text");
        assert_eq!(json["indexed"], true);
        assert_eq!(json["total_notes"], 5);
        assert!(json.get("index_error").is_none());
    }

    #[test]
    fn note_mutation_output_remove_no_type() {
        let output = NoteMutationOutput {
            status: "removed".into(),
            note_type: None,
            sentiment: None,
            text_preview: "deleted note".into(),
            file: "docs/notes.toml".into(),
            indexed: false,
            total_notes: 0,
            index_error: Some("store not found".into()),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "removed");
        assert!(json.get("type").is_none());
        assert!(json.get("sentiment").is_none());
        assert_eq!(json["index_error"], "store not found");
    }

    #[test]
    fn note_list_entry_serialization() {
        let entry = NoteListEntry {
            id: "note:0".into(),
            sentiment: -1.0,
            note_type: "warning".into(),
            text: "This is broken".into(),
            mentions: vec!["search.rs".into()],
            stale_mentions: Some(vec!["old_file.rs".into()]),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["id"], "note:0");
        assert_eq!(json["type"], "warning");
        assert_eq!(json["sentiment"], -1.0);
        assert_eq!(json["mentions"][0], "search.rs");
        assert_eq!(json["stale_mentions"][0], "old_file.rs");
    }

    #[test]
    fn note_list_entry_no_stale() {
        let entry = NoteListEntry {
            id: "note:1".into(),
            sentiment: 0.0,
            note_type: "neutral".into(),
            text: "just an observation".into(),
            mentions: vec![],
            stale_mentions: None,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("stale_mentions").is_none());
    }
}
