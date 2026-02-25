//! Read command for cqs
//!
//! Reads a file with context from notes injected as comments.
//! Respects audit mode (skips notes if active).
//!
//! Core logic is in shared functions (`validate_and_read_file`,
//! `build_file_note_header`, `build_focused_output`) so batch mode
//! can reuse them without duplicating ~200 lines.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use cqs::audit::load_audit_state;
use cqs::note::{parse_notes, path_matches_mention, Note};
use cqs::parser::ChunkType;
use cqs::store::Store;
use cqs::{compute_hints, FunctionHints, COMMON_TYPES};

use crate::cli::find_project_root;

// ─── Shared core functions ──────────────────────────────────────────────────

/// Validate path (traversal, size) and read file contents.
///
/// Returns `(file_path, content)` where `file_path` is root.join(path).
pub(crate) fn validate_and_read_file(root: &Path, path: &str) -> Result<(PathBuf, String)> {
    let file_path = root.join(path);

    if !file_path.exists() {
        bail!("File not found: {}", path);
    }

    // Path traversal protection (dunce strips Windows UNC prefix automatically)
    let canonical = dunce::canonicalize(&file_path)
        .with_context(|| format!("Failed to canonicalize path: {}", path))?;
    let project_canonical =
        dunce::canonicalize(root).context("Failed to canonicalize project root")?;
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

    let content = std::fs::read_to_string(&canonical).context("Failed to read file")?;
    Ok((file_path, content))
}

/// Build note-injection header for a full file read.
///
/// Returns `(header_string, notes_injected)`.
pub(crate) fn build_file_note_header(
    path: &str,
    file_path: &Path,
    audit_state: &cqs::audit::AuditMode,
    notes: &[Note],
) -> (String, bool) {
    let mut header = String::new();
    let mut notes_injected = false;

    if let Some(status) = audit_state.status_line() {
        header.push_str(&format!("// {}\n//\n", status));
    }

    if !audit_state.is_active() {
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
            notes_injected = true;
            header.push_str("// ┌─────────────────────────────────────────────────────────────┐\n");
            header.push_str("// │ [cqs] Context from notes.toml                              │\n");
            header.push_str("// └─────────────────────────────────────────────────────────────┘\n");
            for n in relevant {
                if let Some(first_line) = n.text.lines().next() {
                    header.push_str(&format!(
                        "// [{}] {}\n",
                        n.sentiment_label(),
                        first_line.trim()
                    ));
                }
            }
            header.push_str("//\n");
        }
    }

    (header, notes_injected)
}

/// Result of a focused read operation.
pub(crate) struct FocusedReadResult {
    pub output: String,
    pub hints: Option<FunctionHints>,
}

/// Build focused-read output: header + hints + notes + target + type deps.
///
/// Shared between CLI `cmd_read --focus` and batch `dispatch_read --focus`.
pub(crate) fn build_focused_output(
    store: &Store,
    focus: &str,
    root: &Path,
    audit_state: &cqs::audit::AuditMode,
    notes: &[Note],
) -> Result<FocusedReadResult> {
    let resolved = cqs::resolve_target(store, focus)?;
    let chunk = &resolved.chunk;
    let rel_file = cqs::rel_display(&chunk.file, root);

    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "// [cqs] Focused read: {} ({}:{}-{})\n",
        chunk.name, rel_file, chunk.line_start, chunk.line_end
    ));

    // Hints (function/method only)
    let hints = if chunk.chunk_type.is_callable() {
        match compute_hints(store, &chunk.name, None) {
            Ok(h) => Some(h),
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

    // Audit mode status
    if let Some(status) = audit_state.status_line() {
        output.push_str(&format!("// {}\n", status));
    }

    // Note injection (skip in audit mode)
    if !audit_state.is_active() {
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

    // Target function
    output.push_str("\n// --- Target ---\n");
    if let Some(ref doc) = chunk.doc {
        output.push_str(doc);
        output.push('\n');
    }
    output.push_str(&chunk.content);
    output.push('\n');

    // Type dependencies
    let type_deps = match store.get_types_used_by(&chunk.name) {
        Ok(pairs) => pairs,
        Err(e) => {
            tracing::warn!(function = %chunk.name, error = %e, "Failed to query type deps");
            Vec::new()
        }
    };
    let mut seen_types = std::collections::HashSet::new();
    let filtered_types: Vec<cqs::store::TypeUsage> = type_deps
        .into_iter()
        .filter(|t| !COMMON_TYPES.contains(t.type_name.as_str()))
        .filter(|t| seen_types.insert(t.type_name.clone()))
        .collect();
    tracing::debug!(
        type_count = filtered_types.len(),
        "Type deps for focused read"
    );

    // Batch lookup instead of N+1 queries (CQ-15)
    let type_names: Vec<&str> = filtered_types
        .iter()
        .map(|t| t.type_name.as_str())
        .collect();
    let batch_results = store
        .search_by_names_batch(&type_names, 5)
        .unwrap_or_default();

    for t in &filtered_types {
        let type_name = &t.type_name;
        let edge_kind = &t.edge_kind;
        if let Some(results) = batch_results.get(type_name.as_str()) {
            let type_def = results.iter().find(|r| {
                r.chunk.name == *type_name
                    && matches!(
                        r.chunk.chunk_type,
                        ChunkType::Struct
                            | ChunkType::Enum
                            | ChunkType::Trait
                            | ChunkType::Interface
                            | ChunkType::Class
                    )
            });
            if let Some(r) = type_def {
                let dep_rel = cqs::rel_display(&r.chunk.file, root);
                let kind_label = if edge_kind.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", edge_kind)
                };
                output.push_str(&format!(
                    "\n// --- Type: {}{} ({}:{}-{}) ---\n",
                    r.chunk.name, kind_label, dep_rel, r.chunk.line_start, r.chunk.line_end
                ));
                output.push_str(&r.chunk.content);
                output.push('\n');
            }
        }
    }

    Ok(FocusedReadResult { output, hints })
}

// ─── CLI commands ───────────────────────────────────────────────────────────

/// Handle read command
pub(crate) fn cmd_read(path: &str, focus: Option<&str>, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_read", path).entered();

    // Focused read mode
    if let Some(focus) = focus {
        return cmd_read_focused(focus, json);
    }

    let root = find_project_root();
    let (file_path, content) = validate_and_read_file(&root, path)?;

    // Build note header
    let cqs_dir = cqs::resolve_index_dir(&root);
    let audit_mode = load_audit_state(&cqs_dir);
    let notes_path = root.join("docs/notes.toml");
    let notes = if notes_path.exists() {
        parse_notes(&notes_path).unwrap_or_default()
    } else {
        vec![]
    };

    let (header, _notes_injected) = build_file_note_header(path, &file_path, &audit_mode, &notes);

    let enriched = if header.is_empty() {
        content
    } else {
        format!("{}{}", header, content)
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

    let audit_mode = load_audit_state(&cqs_dir);
    let notes_path = root.join("docs/notes.toml");
    let notes = if notes_path.exists() {
        parse_notes(&notes_path).unwrap_or_default()
    } else {
        vec![]
    };

    let result = build_focused_output(&store, focus, &root, &audit_mode, &notes)?;

    if json {
        let mut json_val = serde_json::json!({
            "focus": focus,
            "content": result.output,
        });
        if let Some(ref h) = result.hints {
            json_val["hints"] = serde_json::json!({
                "caller_count": h.caller_count,
                "test_count": h.test_count,
                "no_callers": h.caller_count == 0,
                "no_tests": h.test_count == 0,
            });
        }
        println!("{}", serde_json::to_string_pretty(&json_val)?);
    } else {
        print!("{}", result.output);
    }

    Ok(())
}
