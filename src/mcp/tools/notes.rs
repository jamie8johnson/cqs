//! Notes tools - add, update, remove notes in project memory

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::Path;

use crate::note::{parse_notes, rewrite_notes_file};

use super::super::server::McpServer;

/// Re-parse and re-index notes after a file mutation.
/// Returns (indexed_count, optional_error_string).
fn reindex_notes(server: &McpServer, notes_path: &Path) -> (usize, Option<String>) {
    match parse_notes(notes_path) {
        Ok(notes) if !notes.is_empty() => match server.index_notes(&notes, notes_path) {
            Ok(count) => (count, None),
            Err(e) => {
                tracing::warn!("Failed to index notes: {}", e);
                (0, Some(e.to_string()))
            }
        },
        Ok(_) => (0, None),
        Err(e) => {
            tracing::warn!("Failed to parse notes after mutation: {}", e);
            (0, Some(e.to_string()))
        }
    }
}

/// Build a text preview (first 100 chars or full text).
fn text_preview(text: &str) -> String {
    text.char_indices()
        .nth(100)
        .map(|(i, _)| format!("{}...", &text[..i]))
        .unwrap_or_else(|| text.to_string())
}

/// Add a note to project memory
pub fn tool_add_note(server: &McpServer, arguments: Value) -> Result<Value> {
    let text = arguments
        .get("text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'text' argument"))?;

    // Validate text length
    if text.is_empty() {
        bail!("Note text cannot be empty");
    }
    if text.len() > 2000 {
        bail!("Note text too long: {} bytes (max 2000)", text.len());
    }

    let sentiment: f32 = arguments
        .get("sentiment")
        .and_then(|s| s.as_f64())
        .map(|s| (s as f32).clamp(-1.0, 1.0))
        .unwrap_or(0.0);

    let mentions: Vec<String> = arguments
        .get("mentions")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    if let Some(s) = v.as_str() {
                        if s.is_empty() {
                            tracing::debug!("Ignoring empty mention string");
                            None
                        } else {
                            Some(s.to_string())
                        }
                    } else {
                        tracing::debug!(value = ?v, "Ignoring non-string mention value");
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let note_entry = crate::note::NoteEntry {
        sentiment,
        text: text.to_string(),
        mentions,
    };

    let notes_path = server.project_root.join("docs/notes.toml");

    // Create docs dir if needed
    if let Some(parent) = notes_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create docs directory")?;
    }

    // Create file with header if it doesn't exist
    if !notes_path.exists() {
        std::fs::write(
            &notes_path,
            "# Notes - unified memory for AI collaborators\n# sentiment: -1.0 (pain) to +1.0 (gain)\n",
        )
        .context("Failed to create notes.toml")?;

        // Set restrictive permissions on Unix (0600 = owner read/write only).
        // Notes may contain sensitive observations about the codebase.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&notes_path, perms)
                .context("Failed to set notes.toml permissions")?;
        }
    }

    // Use rewrite_notes_file for atomic write with exclusive locking and TOML integrity.
    // This re-serializes the entire file, guaranteeing valid TOML output.
    rewrite_notes_file(&notes_path, |entries| {
        entries.push(note_entry.clone());
        Ok(())
    })
    .context("Failed to add note")?;

    // Re-parse and re-index all notes so the new one is immediately searchable
    let (indexed, index_error) = reindex_notes(server, &notes_path);

    tracing::info!("Note added successfully");

    let sentiment_label = if sentiment < -0.3 {
        "warning"
    } else if sentiment > 0.3 {
        "pattern"
    } else {
        "observation"
    };

    let mut result = serde_json::json!({
        "status": "added",
        "type": sentiment_label,
        "sentiment": sentiment,
        "text_preview": text_preview(text),
        "file": "docs/notes.toml",
        "indexed": indexed > 0,
        "total_notes": indexed
    });

    // Include index error in response if indexing failed
    if let Some(err) = index_error {
        result["index_error"] = serde_json::json!(server.sanitize_error_message(&err));
    }

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}

/// Update an existing note in project memory
pub fn tool_update_note(server: &McpServer, arguments: Value) -> Result<Value> {
    let text = arguments
        .get("text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'text' argument"))?;

    if text.is_empty() {
        bail!("Note text cannot be empty");
    }

    let new_text: Option<&str> = arguments.get("new_text").and_then(|t| t.as_str());
    let new_sentiment: Option<f32> = arguments
        .get("new_sentiment")
        .and_then(|s| s.as_f64())
        .map(|s| (s as f32).clamp(-1.0, 1.0));
    let new_mentions: Option<Vec<String>> = arguments
        .get("new_mentions")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        });

    if new_text.is_none() && new_sentiment.is_none() && new_mentions.is_none() {
        bail!("At least one of new_text, new_sentiment, or new_mentions must be provided");
    }

    if let Some(t) = new_text {
        if t.is_empty() {
            bail!("new_text cannot be empty");
        }
        if t.len() > 2000 {
            bail!("new_text too long: {} bytes (max 2000)", t.len());
        }
    }

    let notes_path = server.project_root.join("docs/notes.toml");
    if !notes_path.exists() {
        bail!("No notes.toml found. Use cqs_add_note to create notes first.");
    }

    let text_trimmed = text.trim();
    let text_owned = new_text.map(|s| s.to_string());
    let new_sentiment_owned = new_sentiment;
    let new_mentions_owned = new_mentions;

    rewrite_notes_file(&notes_path, |entries| {
        let entry = entries
            .iter_mut()
            .find(|e| e.text.trim() == text_trimmed)
            .ok_or_else(|| {
                crate::note::NoteError::NotFound(format!(
                    "No note with text: '{}'",
                    text_preview(text_trimmed)
                ))
            })?;

        if let Some(ref t) = text_owned {
            entry.text = t.clone();
        }
        if let Some(s) = new_sentiment_owned {
            entry.sentiment = s;
        }
        if let Some(ref m) = new_mentions_owned {
            entry.mentions = m.clone();
        }
        Ok(())
    })
    .context("Failed to update note")?;

    let (indexed, index_error) = reindex_notes(server, &notes_path);

    tracing::info!("Note updated successfully");

    let final_text = new_text.unwrap_or(text);
    let mut result = serde_json::json!({
        "status": "updated",
        "text_preview": text_preview(final_text),
        "file": "docs/notes.toml",
        "indexed": indexed > 0,
        "total_notes": indexed
    });

    if let Some(err) = index_error {
        result["index_error"] = serde_json::json!(server.sanitize_error_message(&err));
    }

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}

/// Remove a note from project memory
pub fn tool_remove_note(server: &McpServer, arguments: Value) -> Result<Value> {
    let text = arguments
        .get("text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'text' argument"))?;

    if text.is_empty() {
        bail!("Note text cannot be empty");
    }

    let notes_path = server.project_root.join("docs/notes.toml");
    if !notes_path.exists() {
        bail!("No notes.toml found");
    }

    let text_trimmed = text.trim();
    let mut removed_text = String::new();

    rewrite_notes_file(&notes_path, |entries| {
        let before_len = entries.len();
        let pos = entries
            .iter()
            .position(|e| e.text.trim() == text_trimmed)
            .ok_or_else(|| {
                crate::note::NoteError::NotFound(format!(
                    "No note with text: '{}'",
                    text_preview(text_trimmed)
                ))
            })?;

        removed_text = entries[pos].text.clone();
        entries.remove(pos);

        debug_assert_eq!(entries.len(), before_len - 1);
        Ok(())
    })
    .context("Failed to remove note")?;

    let (indexed, index_error) = reindex_notes(server, &notes_path);

    tracing::info!("Note removed successfully");

    let mut result = serde_json::json!({
        "status": "removed",
        "text_preview": text_preview(&removed_text),
        "file": "docs/notes.toml",
        "indexed": indexed > 0,
        "total_notes": indexed
    });

    if let Some(err) = index_error {
        result["index_error"] = serde_json::json!(server.sanitize_error_message(&err));
    }

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}
