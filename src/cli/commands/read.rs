//! Read command for cqs
//!
//! Reads a file with context from notes injected as comments.
//! Respects audit mode (skips notes if active).

use anyhow::{bail, Context, Result};

use cqs::audit::load_audit_state;
use cqs::compute_hints;
use cqs::extract_type_names;
use cqs::note::{parse_notes, path_matches_mention};
use cqs::parser::ChunkType;

use crate::cli::find_project_root;

use super::resolve::resolve_target;

/// Handle read command
pub(crate) fn cmd_read(path: &str, focus: Option<&str>, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_read", path).entered();
    // Focused read mode
    if let Some(focus) = focus {
        return cmd_read_focused(focus, json);
    }

    let root = find_project_root();
    let file_path = root.join(path);

    if !file_path.exists() {
        bail!("File not found: {}", path);
    }

    // Path traversal protection (dunce strips Windows UNC prefix automatically)
    let canonical = dunce::canonicalize(&file_path).context("Failed to canonicalize path")?;
    let project_canonical =
        dunce::canonicalize(&root).context("Failed to canonicalize project root")?;
    if !canonical.starts_with(&project_canonical) {
        bail!("Path traversal not allowed: {}", path);
    }

    // File size limit (10MB)
    const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
    let metadata = std::fs::metadata(&file_path).context("Failed to read file metadata")?;
    if metadata.len() > MAX_FILE_SIZE {
        bail!(
            "File too large: {} bytes (max {} bytes)",
            metadata.len(),
            MAX_FILE_SIZE
        );
    }

    let content = std::fs::read_to_string(&file_path).context("Failed to read file")?;

    // Check audit mode
    let cqs_dir = cqs::resolve_index_dir(&root);
    let audit_mode = load_audit_state(&cqs_dir);
    let mut context_header = String::new();

    if let Some(status) = audit_mode.status_line() {
        context_header.push_str(&format!("// {}\n//\n", status));
    }

    // Find relevant notes (skip if audit mode active)
    if !audit_mode.is_active() {
        let notes_path = root.join("docs/notes.toml");
        if notes_path.exists() {
            if let Ok(notes) = parse_notes(&notes_path) {
                let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let relevant: Vec<_> = notes
                    .iter()
                    .filter(|n| {
                        n.mentions
                            .iter()
                            .any(|m| m == file_name || m == path || path_matches_mention(path, m))
                    })
                    .collect();

                if !relevant.is_empty() {
                    context_header.push_str(
                        "// ┌─────────────────────────────────────────────────────────────┐\n",
                    );
                    context_header.push_str(
                        "// │ [cqs] Context from notes.toml                              │\n",
                    );
                    context_header.push_str(
                        "// └─────────────────────────────────────────────────────────────┘\n",
                    );

                    for n in relevant {
                        if let Some(first_line) = n.text.lines().next() {
                            context_header.push_str(&format!(
                                "// [{}] {}\n",
                                n.sentiment_label(),
                                first_line.trim()
                            ));
                        }
                    }
                    context_header.push_str("//\n");
                }
            }
        }
    }

    let enriched = if context_header.is_empty() {
        content
    } else {
        format!("{}{}", context_header, content)
    };

    if json {
        let result = serde_json::json!({
            "path": path,
            "content": enriched,
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print!("{}", enriched);
    }

    Ok(())
}

fn cmd_read_focused(focus: &str, json: bool) -> Result<()> {
    let (store, root, cqs_dir) = crate::cli::open_project_store()?;
    let resolved = resolve_target(&store, focus)?;
    let chunk = resolved.chunk;

    let rel_file = cqs::rel_display(&chunk.file, &root);

    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "// [cqs] Focused read: {} ({}:{}-{})\n",
        chunk.name, rel_file, chunk.line_start, chunk.line_end
    ));

    // Hints (function/method only) — compute once, reuse for JSON
    let hints = if matches!(chunk.chunk_type, ChunkType::Function | ChunkType::Method) {
        match compute_hints(&store, &chunk.name, None) {
            Ok(hints) => Some(hints),
            Err(e) => {
                tracing::warn!(function = %chunk.name, error = %e, "Failed to compute hints");
                None
            }
        }
    } else {
        None
    };
    if let Some(ref h) = hints {
        let caller_label = if h.caller_count == 0 {
            "! 0 callers".to_string()
        } else {
            format!("{} callers", h.caller_count)
        };
        let test_label = if h.test_count == 0 {
            "! 0 tests".to_string()
        } else {
            format!("{} tests", h.test_count)
        };
        output.push_str(&format!("// [cqs] {} | {}\n", caller_label, test_label));
    }

    // Note injection
    let audit_mode = load_audit_state(&cqs_dir);
    if let Some(status) = audit_mode.status_line() {
        output.push_str(&format!("// {}\n", status));
    }

    if !audit_mode.is_active() {
        let notes_path = root.join("docs/notes.toml");
        if notes_path.exists() {
            if let Ok(notes) = parse_notes(&notes_path) {
                let relevant: Vec<_> = notes
                    .iter()
                    .filter(|n| {
                        n.mentions
                            .iter()
                            .any(|m| m == &chunk.name || m == &rel_file)
                    })
                    .collect();
                for n in &relevant {
                    if let Some(first_line) = n.text.lines().next() {
                        output.push_str(&format!(
                            "// [{}] {}\n",
                            n.sentiment_label(),
                            first_line.trim()
                        ));
                    }
                }
                if !relevant.is_empty() {
                    output.push_str("//\n");
                }
            }
        }
    }

    // Target function
    output.push_str("\n// --- Target ---\n");
    if let Some(ref doc) = chunk.doc {
        output.push_str(doc);
        output.push('\n');
    }
    output.push_str(&chunk.content);
    output.push('\n');

    // Type dependencies
    let type_names = extract_type_names(&chunk.signature);
    for type_name in &type_names {
        if let Ok(results) = store.search_by_name(type_name, 5) {
            let type_def = results.iter().find(|r| {
                r.chunk.name == *type_name
                    && matches!(
                        r.chunk.chunk_type,
                        cqs::parser::ChunkType::Struct
                            | cqs::parser::ChunkType::Enum
                            | cqs::parser::ChunkType::Trait
                            | cqs::parser::ChunkType::Interface
                            | cqs::parser::ChunkType::Class
                    )
            });
            if let Some(r) = type_def {
                let dep_rel = cqs::rel_display(&r.chunk.file, &root);
                output.push_str(&format!(
                    "\n// --- Type: {} ({}:{}-{}) ---\n",
                    r.chunk.name, dep_rel, r.chunk.line_start, r.chunk.line_end
                ));
                output.push_str(&r.chunk.content);
                output.push('\n');
            }
        }
    }

    if json {
        let mut result = serde_json::json!({
            "focus": focus,
            "content": output,
        });
        if let Some(ref h) = hints {
            result["hints"] = serde_json::json!({
                "caller_count": h.caller_count,
                "test_count": h.test_count,
                "no_callers": h.caller_count == 0,
                "no_tests": h.test_count == 0,
            });
        }
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print!("{}", output);
    }

    Ok(())
}
