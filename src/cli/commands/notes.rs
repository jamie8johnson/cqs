//! Notes command for cqs
//!
//! Lists and manages notes from docs/notes.toml.

use anyhow::{bail, Result};

use cqs::parse_notes;

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
}

/// Handle notes subcommands
pub(crate) fn cmd_notes(cli: &Cli, subcmd: &NotesCommand) -> Result<()> {
    match subcmd {
        NotesCommand::List { warnings, patterns } => cmd_notes_list(cli, *warnings, *patterns),
    }
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
