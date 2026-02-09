//! Notes command for cqs
//!
//! Lists and manages notes from docs/notes.toml.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

use cqs::{parse_notes, rewrite_notes_file, Embedder, NoteEntry, NOTES_HEADER};

use crate::cli::{find_project_root, Cli};

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

/// Handle notes subcommands
pub(crate) fn cmd_notes(cli: &Cli, subcmd: &NotesCommand) -> Result<()> {
    match subcmd {
        NotesCommand::List { warnings, patterns } => cmd_notes_list(cli, *warnings, *patterns),
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
    }
}

/// Re-parse and re-index notes after a file mutation.
fn reindex_notes_cli(root: &std::path::Path) -> (usize, Option<String>) {
    let notes_path = root.join("docs/notes.toml");
    match parse_notes(&notes_path) {
        Ok(notes) if !notes.is_empty() => {
            let embedder = match Embedder::new() {
                Ok(e) => e,
                Err(e) => return (0, Some(format!("Failed to create embedder: {}", e))),
            };
            let index_path = cqs::resolve_index_dir(root).join("index.db");
            let store = match cqs::Store::open(&index_path) {
                Ok(s) => s,
                Err(e) => return (0, Some(format!("Failed to open index: {}", e))),
            };
            match cqs::index_notes(&notes, &notes_path, &embedder, &store) {
                Ok(count) => (count, None),
                Err(e) => (0, Some(format!("Failed to index notes: {}", e))),
            }
        }
        Ok(_) => (0, None),
        Err(e) => (0, Some(format!("Failed to parse notes: {}", e))),
    }
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

/// Add a note
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
        reindex_notes_cli(&root)
    };

    let sentiment_label = if sentiment < -0.3 {
        "warning"
    } else if sentiment > 0.3 {
        "pattern"
    } else {
        "observation"
    };

    if cli.json {
        let mut result = serde_json::json!({
            "status": "added",
            "type": sentiment_label,
            "sentiment": sentiment,
            "text_preview": text_preview(text),
            "file": "docs/notes.toml",
            "indexed": indexed > 0,
            "total_notes": indexed
        });
        if let Some(err) = index_error {
            result["index_error"] = serde_json::json!(err);
        }
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
            eprintln!("Warning: {}", err);
        }
    }

    Ok(())
}

/// Update a note
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
        reindex_notes_cli(&root)
    };

    let final_text = new_text.unwrap_or(text);
    if cli.json {
        let mut result = serde_json::json!({
            "status": "updated",
            "text_preview": text_preview(final_text),
            "file": "docs/notes.toml",
            "indexed": indexed > 0,
            "total_notes": indexed
        });
        if let Some(err) = index_error {
            result["index_error"] = serde_json::json!(err);
        }
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Updated: {}", text_preview(final_text));
        if indexed > 0 {
            println!("Indexed {} notes.", indexed);
        }
        if let Some(err) = index_error {
            eprintln!("Warning: {}", err);
        }
    }

    Ok(())
}

/// Remove a note
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
        reindex_notes_cli(&root)
    };

    if cli.json {
        let mut result = serde_json::json!({
            "status": "removed",
            "text_preview": text_preview(&removed_text),
            "file": "docs/notes.toml",
            "indexed": indexed > 0,
            "total_notes": indexed
        });
        if let Some(err) = index_error {
            result["index_error"] = serde_json::json!(err);
        }
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Removed: {}", text_preview(&removed_text));
        if indexed > 0 {
            println!("Indexed {} notes.", indexed);
        }
        if let Some(err) = index_error {
            eprintln!("Warning: {}", err);
        }
    }

    Ok(())
}

/// List notes from docs/notes.toml
fn cmd_notes_list(cli: &Cli, warnings_only: bool, patterns_only: bool) -> Result<()> {
    let root = find_project_root();
    let notes_path = root.join("docs/notes.toml");

    if !notes_path.exists() {
        bail!("No notes file found at docs/notes.toml. Run 'cqs init' or create it manually.");
    }

    let notes = parse_notes(&notes_path)?;

    if notes.is_empty() {
        println!("No notes found.");
        return Ok(());
    }

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

    if cli.json {
        let json_notes: Vec<_> = filtered
            .iter()
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "sentiment": n.sentiment,
                    "type": if n.is_warning() { "warning" } else if n.is_pattern() { "pattern" } else { "neutral" },
                    "text": n.text,
                    "mentions": n.mentions,
                })
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

        // Truncate text for display
        let preview = if note.text.len() > 120 {
            format!("{}...", &note.text[..117])
        } else {
            note.text.clone()
        };

        let mentions = if note.mentions.is_empty() {
            String::new()
        } else {
            format!("  mentions: {}", note.mentions.join(", "))
        };

        println!("  {} {}", sentiment_marker, preview);
        if !mentions.is_empty() {
            println!("  {}", mentions);
        }
    }

    Ok(())
}
