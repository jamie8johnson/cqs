//! Notes tool - add notes to project memory

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::note::parse_notes;

use super::super::server::McpServer;

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

    // Build TOML entry - escape all strings properly
    let mentions_toml = if mentions.is_empty() {
        String::new()
    } else {
        format!(
            "\nmentions = [{}]",
            mentions
                .iter()
                .map(|m| {
                    format!(
                        "\"{}\"",
                        m.replace('\\', "\\\\")
                            .replace('\"', "\\\"")
                            .replace('\n', "\\n")
                            .replace('\r', "\\r")
                            .replace('\t', "\\t")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    // Escape text for TOML - use single-line strings with escape sequences
    // (avoids triple-quote edge cases)
    let text_toml = format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('\"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    );

    let entry = format!(
        "\n[[note]]\nsentiment = {:.1}\ntext = {}{}\n",
        sentiment, text_toml, mentions_toml
    );

    // Append to notes.toml
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

    // Append entry
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&notes_path)
            .context("Failed to open notes.toml")?;
        file.write_all(entry.as_bytes())
            .context("Failed to write note")?;
        file.sync_all().context("Failed to sync note to disk")?;
    }

    // Re-parse and re-index all notes so the new one is immediately searchable
    let (indexed, index_error) = match parse_notes(&notes_path) {
        Ok(notes) if !notes.is_empty() => match server.index_notes(&notes, &notes_path) {
            Ok(count) => (count, None),
            Err(e) => {
                tracing::warn!("Failed to index notes: {}", e);
                (0, Some(e.to_string()))
            }
        },
        Ok(_) => (0, None),
        Err(e) => {
            tracing::warn!("Failed to parse notes after adding: {}", e);
            (0, Some(e.to_string()))
        }
    };

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
        "text_preview": text.char_indices().nth(100).map(|(i, _)| format!("{}...", &text[..i])).unwrap_or_else(|| text.to_string()),
        "file": "docs/notes.toml",
        "indexed": indexed > 0,
        "total_notes": indexed
    });

    // Include index error in response if indexing failed
    if let Some(err) = index_error {
        result["index_error"] = serde_json::json!(err);
    }

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}
