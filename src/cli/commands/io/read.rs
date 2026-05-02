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

// ─── Shared core functions ──────────────────────────────────────────────────

/// Validate path (traversal, size) and read file contents.
/// Returns `(file_path, content)` where `file_path` is root.join(path).
///
/// SEC-D.5: existence check is folded into the same "Invalid path" rejection
/// as the traversal check so a daemon client can't use distinguishable
/// error messages as a path-existence oracle for files outside the project
/// root (e.g. `/home/other/.ssh/id_rsa`).
pub(crate) fn validate_and_read_file(root: &Path, path: &str) -> Result<(PathBuf, String)> {
    let file_path = root.join(path);

    // Path traversal protection FIRST so a missing file outside the root
    // and a missing file inside the root produce identical error messages.
    // dunce::canonicalize follows symlinks and resolves filesystem case,
    // so starts_with is correct on NTFS / APFS. dunce strips the Windows
    // UNC `\\?\` prefix automatically.
    //
    // Both `Failed to canonicalize` and `Path traversal not allowed`
    // collapse into the same opaque "Invalid path" so the rejection
    // paths are indistinguishable to the client.
    let canonical = dunce::canonicalize(&file_path).map_err(|_| anyhow::anyhow!("Invalid path"))?;
    let project_canonical = dunce::canonicalize(root).context("Invalid project root")?;
    if !canonical.starts_with(&project_canonical) {
        bail!("Invalid path");
    }
    if !file_path.exists() {
        bail!("Invalid path");
    }

    // P3 #107: env-overridable via CQS_READ_MAX_FILE_SIZE (default 10 MiB).
    let max_file_size = crate::cli::limits::read_max_file_size();
    let metadata = std::fs::metadata(&file_path).context("Failed to read file metadata")?;
    if metadata.len() > max_file_size {
        bail!(
            "File too large: {} bytes (max {} bytes; CQS_READ_MAX_FILE_SIZE)",
            metadata.len(),
            max_file_size
        );
    }

    let content = std::fs::read_to_string(&canonical).context("Failed to read file")?;
    Ok((file_path, content))
}

/// Build note-injection header for a full file read.
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
    /// P2.23: human-readable warnings emitted when an upstream batch query
    /// returned `Err`. The previous `unwrap_or_else(_, HashMap::new())`
    /// silently dropped type-definition lookups; agents now see exactly
    /// what was missed instead of inferring it from absent JSON keys.
    pub warnings: Vec<String>,
}

/// Build focused-read output: header + hints + notes + target + type deps.
/// Shared between CLI `cmd_read --focus` and batch `dispatch_read --focus`.
pub(crate) fn build_focused_output<Mode>(
    store: &Store<Mode>,
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

    // Type dependencies.
    // P2 #65: usize::MAX preserves existing "all rows" behaviour. The display
    // path filters by COMMON_TYPES then truncates inline downstream.
    // TODO: cap at e.g. 50 once we measure typical edge count per chunk.
    let type_deps = match store.get_types_used_by(&chunk.name, usize::MAX) {
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
    // P2.23: capture batch failure as a structured warning rather than
    // silently empty the map. Type definitions still get omitted (the
    // dependency surface is best-effort), but downstream JSON callers
    // now see a `warnings` entry telling them why.
    let mut warnings: Vec<String> = Vec::new();
    let batch_results = match store.search_by_names_batch(&type_names, 5) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-lookup type definitions for focused read");
            warnings.push(format!(
                "search_by_names_batch failed: {e}; type definitions omitted"
            ));
            std::collections::HashMap::new()
        }
    };

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

    Ok(FocusedReadResult {
        output,
        hints,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

/// JSON output for a full file read.
#[derive(Debug, serde::Serialize)]
struct ReadOutput {
    path: String,
    content: String,
}

/// Hints about caller/test coverage for a focused function.
#[derive(Debug, serde::Serialize)]
struct ReadHints {
    caller_count: usize,
    test_count: usize,
    no_callers: bool,
    no_tests: bool,
}

/// JSON output for a focused read.
#[derive(Debug, serde::Serialize)]
struct FocusedReadJsonOutput {
    focus: String,
    content: String,
    /// SEC-V1.30.1-1: every chunk-returning JSON output must carry a
    /// trust_level. `read --focus` reads from the project store only
    /// (no reference-store fan-in), so this is always "user-code".
    /// SECURITY.md's mitigation contract is that agents can branch
    /// safely on this field; the `read --focus` path was missing it.
    trust_level: &'static str,
    /// SEC-V1.30.1-1: parallel field to chunk JSON. `read --focus`
    /// content is delivered as a single concatenated string, not a
    /// per-chunk list, so there is no per-chunk array — a single
    /// empty array satisfies the schema-stability contract.
    injection_flags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hints: Option<ReadHints>,
    /// P2.23: warnings emitted by the underlying assembly (e.g.
    /// `search_by_names_batch` failed). Mirrors `SummaryOutput::warnings`
    /// per EH-V1.29-9 — agents need to distinguish "no type deps" from
    /// "type-deps lookup failed silently".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

// ─── CLI commands ───────────────────────────────────────────────────────────

pub(crate) fn cmd_read(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    path: &str,
    focus: Option<&str>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_read", path).entered();

    // Focused read mode
    if let Some(focus) = focus {
        return cmd_read_focused(ctx, focus, json);
    }

    let root = &ctx.root;
    let (file_path, content) = validate_and_read_file(root, path)?;

    // Build note header
    let cqs_dir = &ctx.cqs_dir;
    let audit_mode = load_audit_state(cqs_dir);
    let notes_path = root.join("docs/notes.toml");
    let notes = if notes_path.exists() {
        parse_notes(&notes_path).unwrap_or_else(|e| {
            tracing::warn!(path = %notes_path.display(), error = %e, "Failed to parse notes.toml");
            vec![]
        })
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
        let result = ReadOutput {
            path: path.to_string(),
            content: enriched,
        };
        crate::cli::json_envelope::emit_json(&result)?;
    } else {
        print!("{}", enriched);
    }

    Ok(())
}

fn cmd_read_focused(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    focus: &str,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_read_focused", %focus).entered();

    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;

    let audit_mode = load_audit_state(cqs_dir);
    let notes_path = root.join("docs/notes.toml");
    let notes = if notes_path.exists() {
        parse_notes(&notes_path).unwrap_or_else(|e| {
            tracing::warn!(path = %notes_path.display(), error = %e, "Failed to parse notes.toml in focused read");
            vec![]
        })
    } else {
        vec![]
    };

    let result = build_focused_output(store, focus, root, &audit_mode, &notes)?;

    if json {
        let hints = result.hints.as_ref().map(|h| ReadHints {
            caller_count: h.caller_count,
            test_count: h.test_count,
            no_callers: h.caller_count == 0,
            no_tests: h.test_count == 0,
        });
        let output = FocusedReadJsonOutput {
            focus: focus.to_string(),
            content: result.output,
            trust_level: "user-code",
            injection_flags: Vec::new(),
            hints,
            warnings: result.warnings.clone(),
        };
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        // P2.23: surface warnings on stderr so non-JSON callers also see them.
        for w in &result.warnings {
            eprintln!("warning: {w}");
        }
        print!("{}", result.output);
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
    fn read_output_serialization() {
        let output = ReadOutput {
            path: "src/lib.rs".into(),
            content: "fn main() {}".into(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["path"], "src/lib.rs");
        assert_eq!(json["content"], "fn main() {}");
    }

    #[test]
    fn focused_read_output_with_hints() {
        let output = FocusedReadJsonOutput {
            focus: "search".into(),
            content: "fn search() { ... }".into(),
            trust_level: "user-code",
            injection_flags: Vec::new(),
            hints: Some(ReadHints {
                caller_count: 3,
                test_count: 2,
                no_callers: false,
                no_tests: false,
            }),
            warnings: Vec::new(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["focus"], "search");
        assert_eq!(json["hints"]["caller_count"], 3);
        assert_eq!(json["hints"]["test_count"], 2);
        assert_eq!(json["hints"]["no_callers"], false);
        assert_eq!(json["hints"]["no_tests"], false);
        // P2.23: warnings field omitted when empty.
        assert!(json.get("warnings").is_none());
    }

    #[test]
    fn focused_read_output_no_hints() {
        let output = FocusedReadJsonOutput {
            focus: "MyStruct".into(),
            content: "struct MyStruct {}".into(),
            trust_level: "user-code",
            injection_flags: Vec::new(),
            hints: None,
            warnings: Vec::new(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["focus"], "MyStruct");
        assert!(json.get("hints").is_none());
    }

    /// P2.23 regression-pin: `warnings` populated when batch lookup fails.
    /// Verified at the JSON-shape level here; the production wiring goes
    /// through `build_focused_output` which has integration coverage.
    #[test]
    fn focused_read_output_with_warnings() {
        let output = FocusedReadJsonOutput {
            focus: "MyStruct".into(),
            content: "struct MyStruct {}".into(),
            trust_level: "user-code",
            injection_flags: Vec::new(),
            hints: None,
            warnings: vec!["search_by_names_batch failed: db locked".into()],
        };
        let json = serde_json::to_value(&output).unwrap();
        let warns = json["warnings"].as_array().unwrap();
        assert_eq!(warns.len(), 1);
        assert!(warns[0].as_str().unwrap().contains("db locked"));
    }

    /// SEC-V1.30.1-1: SECURITY.md:57 promises `read --focus` JSON carries
    /// `trust_level: "user-code" | "reference-code"` and `injection_flags: []`.
    /// Before this fix, the doc was lying — `FocusedReadJsonOutput` had
    /// neither field. This regression-pin keeps the contract honest: future
    /// removal of these fields would silently break SECURITY.md again.
    #[test]
    fn focused_read_output_emits_sec_trust_level_and_injection_flags() {
        let output = FocusedReadJsonOutput {
            focus: "f".into(),
            content: "fn f() {}".into(),
            trust_level: "user-code",
            injection_flags: Vec::new(),
            hints: None,
            warnings: Vec::new(),
        };
        let json = serde_json::to_value(&output).unwrap();
        // Both fields must serialize (no skip_serializing_if on these).
        assert_eq!(
            json["trust_level"], "user-code",
            "SECURITY.md:57 promises trust_level on read --focus JSON"
        );
        let flags = json["injection_flags"].as_array().expect(
            "SECURITY.md:57 promises injection_flags on read --focus JSON; must serialize as array",
        );
        assert!(flags.is_empty(), "no per-content heuristics fired yet");
    }

    /// TC-ADV-V1.33-9: file exceeding `CQS_READ_MAX_FILE_SIZE` is rejected
    /// with the documented error message including both the actual size
    /// and the cap. The size-cap branch is the only DoS-prevention layer
    /// for arbitrary-content reads — a flipped comparison sign would slip
    /// through CI without a regression test.
    #[test]
    fn read_rejects_oversized_file() {
        use std::sync::Mutex;
        // CQS_READ_MAX_FILE_SIZE is process-global; serialise the env edit.
        static READ_SIZE_LOCK: Mutex<()> = Mutex::new(());
        let _guard = READ_SIZE_LOCK.lock().unwrap();

        let dir = tempfile::TempDir::new().unwrap();
        let big_path = dir.path().join("big.rs");
        // 100 bytes; cap below is 50.
        std::fs::write(&big_path, vec![b'a'; 100]).unwrap();

        let prev = std::env::var("CQS_READ_MAX_FILE_SIZE").ok();
        std::env::set_var("CQS_READ_MAX_FILE_SIZE", "50");
        let err = super::validate_and_read_file(dir.path(), "big.rs")
            .expect_err("oversized file must error");
        // Restore env regardless of assert outcome.
        match prev {
            Some(v) => std::env::set_var("CQS_READ_MAX_FILE_SIZE", v),
            None => std::env::remove_var("CQS_READ_MAX_FILE_SIZE"),
        }

        let msg = err.to_string();
        assert!(
            msg.contains("File too large"),
            "error must mention 'File too large', got {msg:?}"
        );
        assert!(
            msg.contains("100"),
            "error must include actual size (100), got {msg:?}"
        );
        assert!(
            msg.contains("50"),
            "error must include the cap (50), got {msg:?}"
        );
        assert!(
            msg.contains("CQS_READ_MAX_FILE_SIZE"),
            "error must name the env var so users can tune, got {msg:?}"
        );
    }

    /// SEC-D.5: `validate_and_read_file` must produce identical error text
    /// for "file outside project root" and "file not found" so a daemon
    /// client can't probe filesystem layout via distinguishable messages.
    /// Before the fix, missing-inside-root returned "File not found: X" and
    /// outside-root returned "Path traversal not allowed: X" — agents (and
    /// attackers) could distinguish.
    #[test]
    fn read_rejects_path_outside_root_with_same_message_as_nonexistent() {
        let root_dir = tempfile::TempDir::new().unwrap();
        // A path whose canonical resolution lands outside the project root.
        // The traversal goes up enough levels to escape the temp dir.
        let outside_err = super::validate_and_read_file(root_dir.path(), "../../../etc/hostname")
            .unwrap_err()
            .to_string();
        // A path that simply doesn't exist within the root.
        let nonexistent_err =
            super::validate_and_read_file(root_dir.path(), "definitely_missing_xyz.rs")
                .unwrap_err()
                .to_string();
        assert_eq!(
            outside_err, nonexistent_err,
            "outside-root and nonexistent paths must produce indistinguishable errors \
             (no path-existence oracle), got outside={outside_err:?} nonexistent={nonexistent_err:?}"
        );
        assert_eq!(
            outside_err, "Invalid path",
            "expected opaque rejection text 'Invalid path', got: {outside_err:?}"
        );
    }
}
