//! Source file rewriter for doc comments.
//!
//! Re-parses files, finds insertion points (decorator-aware), detects existing
//! doc comments, applies edits bottom-up, and writes atomically.

use std::ops::Range;
use std::path::Path;

use crate::doc_writer::formats::{doc_format_for, format_doc_comment, InsertionPosition};
use crate::doc_writer::DocCommentResult;
use crate::language::Language;
use crate::parser::Parser;

/// Errors that can occur during doc comment rewriting.
#[derive(Debug, thiserror::Error)]
pub enum DocWriterError {
    /// IO error reading or writing files.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Tree-sitter parse error.
    #[error("Parser error: {0}")]
    Parser(#[from] crate::parser::ParserError),
    /// The target function was not found in the re-parsed file.
    #[error("Function not found in file: {0}")]
    FunctionNotFound(String),
}

/// A resolved edit: what to remove and what to insert at a specific line.
#[derive(Debug)]
struct ResolvedEdit {
    /// 0-based line range to remove (existing doc). `None` means insert-only.
    remove_range: Option<Range<usize>>,
    /// 0-based line index to insert new doc lines before.
    insert_at: usize,
    /// Formatted doc comment lines (each ends with `\n`).
    new_lines: Vec<String>,
}

/// Find the 1-based line number where a doc comment should be inserted.
/// For `BeforeFunction`: scans upward from the line above the function,
/// skipping blank lines and decorator/attribute lines (`@`, `#[`, `#![`, `[`).
/// Returns the line number above the first decorator (or `line_start` if none).
/// For `InsideBody` (Python): returns `line_start + 1` (line after `def`).
pub fn find_insertion_point(line_start: usize, file_lines: &[&str], language: Language) -> usize {
    let _span = tracing::debug_span!("find_insertion_point", line_start, %language).entered();
    // RB-12: empty file_lines would panic on index access below
    if file_lines.is_empty() || line_start == 0 {
        return line_start;
    }

    let format = doc_format_for(language);

    match format.position {
        InsertionPosition::InsideBody => {
            // Insert on the line after the def/function header
            line_start + 1
        }
        InsertionPosition::BeforeFunction => {
            if line_start <= 1 {
                return line_start;
            }

            // Start at the line above the function (0-based index)
            let mut idx = line_start - 2; // line_start is 1-based, so -2 for 0-based line above
            if idx >= file_lines.len() {
                return line_start;
            }
            let mut seen_decorator = false;

            // Scan upward: skip decorator/attribute lines (and blank lines
            // between decorators). Stop at blank lines that aren't adjacent
            // to decorators.
            loop {
                let trimmed = file_lines[idx].trim();

                let is_decorator = trimmed.starts_with('@')
                    || trimmed.starts_with("#[")
                    || trimmed.starts_with("#![")
                    || trimmed.starts_with('[');

                if is_decorator {
                    seen_decorator = true;
                    if idx == 0 {
                        return 1; // Insert at very top of file
                    }
                    idx -= 1;
                } else if trimmed.is_empty() && seen_decorator {
                    // Blank line between decorators — skip it
                    if idx == 0 {
                        return 1;
                    }
                    idx -= 1;
                } else {
                    // Non-decorator line (or blank line with no decorator seen).
                    // Insert after this line.
                    return idx + 2; // Convert back to 1-based
                }
            }
        }
    }
}

/// Detect an existing doc comment range near the insertion point.
/// For `BeforeFunction`: scans upward from `insertion_line - 1` looking for
/// consecutive lines matching the language's doc prefix (e.g., `///` for Rust).
/// Returns the 0-based line range to remove.
/// For `InsideBody` (Python): checks if the line at `insertion_line` starts
/// with `"""` or `'''`, finds the closing delimiter, and returns the range.
pub fn detect_existing_doc_range(
    insertion_line: usize,
    file_lines: &[&str],
    language: Language,
) -> Option<Range<usize>> {
    let _span =
        tracing::debug_span!("detect_existing_doc_range", insertion_line, %language).entered();
    let format = doc_format_for(language);

    match format.position {
        InsertionPosition::InsideBody => {
            // Python docstrings: check the line at insertion_line (1-based)
            let idx = insertion_line.checked_sub(1)?; // convert to 0-based
            if idx >= file_lines.len() {
                return None;
            }

            let trimmed = file_lines[idx].trim();
            let delimiter = if trimmed.starts_with("\"\"\"") {
                "\"\"\""
            } else if trimmed.starts_with("'''") {
                "'''"
            } else {
                return None;
            };

            // Single-line docstring: """text""" on one line
            if trimmed.len() > 6
                && trimmed.ends_with(delimiter)
                && trimmed[3..trimmed.len() - 3].contains(|c: char| !c.is_whitespace())
            {
                return Some(idx..idx + 1);
            }

            // Multi-line: find closing delimiter
            for (end_idx, line) in file_lines.iter().enumerate().skip(idx + 1) {
                if line.trim().ends_with(delimiter) {
                    return Some(idx..end_idx + 1);
                }
            }

            // Opening delimiter without closing — treat as no doc
            None
        }
        InsertionPosition::BeforeFunction => {
            // RB-13: bounds check — insertion_line is 1-based, need at least 2 lines
            if insertion_line < 2 || file_lines.is_empty() {
                return None;
            }

            // Determine what prefix to look for
            let doc_prefix = if !format.line_prefix.is_empty() {
                format.line_prefix.trim_end()
            } else if !format.prefix.is_empty() {
                // Block-style (/** ... */). Look for the opening marker.
                format.prefix.trim_end()
            } else {
                return None;
            };

            // Scan upward from the line above insertion_line
            let start_idx = insertion_line - 2; // 0-based index of line above insertion
            if start_idx >= file_lines.len() {
                return None;
            }
            let trimmed = file_lines[start_idx].trim();

            // For line-prefix-based docs (///, #, -- |, etc.)
            if !format.line_prefix.is_empty() {
                if !trimmed.starts_with(doc_prefix) {
                    return None;
                }

                // Found at least one doc line. Scan upward for contiguous block.
                let mut top = start_idx;
                while top > 0 {
                    let above = file_lines[top - 1].trim();
                    if above.starts_with(doc_prefix) {
                        top -= 1;
                    } else {
                        break;
                    }
                }

                Some(top..start_idx + 1)
            } else {
                // Block-style: look for closing suffix on start_idx line, then
                // scan upward for the opening prefix
                let suffix = format.suffix.trim_end();

                if !trimmed.ends_with(suffix) && !trimmed.starts_with(doc_prefix) {
                    return None;
                }

                // Find the line with the opening prefix
                let mut top = start_idx;
                while top > 0 {
                    if file_lines[top].trim().starts_with(doc_prefix) {
                        break;
                    }
                    top -= 1;
                }

                if file_lines[top].trim().starts_with(doc_prefix) {
                    Some(top..start_idx + 1)
                } else {
                    None
                }
            }
        }
    }
}

/// Rewrite a source file by inserting or replacing doc comments.
/// Re-parses the file with tree-sitter to get current chunk positions, matches
/// each edit to a chunk by function name, computes insertion points and existing
/// doc ranges, then applies all edits bottom-up to preserve line numbers.
/// Uses atomic write (temp file + rename) with cross-device fallback.
/// Returns the number of functions that were successfully documented.
pub fn rewrite_file(
    path: &Path,
    edits: &[DocCommentResult],
    parser: &Parser,
) -> Result<usize, DocWriterError> {
    let _span = tracing::info_span!("rewrite_file", file = %path.display()).entered();

    if edits.is_empty() {
        return Ok(0);
    }

    // RB-33 / PB-7: Exclusive lock prevents concurrent modifications during
    // the read->parse->edit->write cycle. Uses a separate .cqs-lock file
    // (same pattern as notes.rs) so the source file isn't held open, which
    // would conflict with the atomic-write (temp + rename) below.
    // Advisory lock: other cqs processes cooperate, external editors may not.
    let lock_path = path.with_extension("cqs-lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| {
            DocWriterError::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {}", lock_path.display(), e),
            ))
        })?;
    lock_file.lock()?;

    // Read current file content
    let content = std::fs::read_to_string(path)?;
    let file_lines: Vec<&str> = content.lines().collect();

    // Re-parse to get current chunk positions.
    // RB-14: All edits for a single file must share the same language.
    // If mixed, warn and filter to only edits matching the first language.
    let language = edits[0].language;
    if edits.iter().any(|e| e.language != language) {
        tracing::warn!(
            file = %path.display(),
            expected = %language,
            "Mixed languages in doc edits for one file — using {}", language
        );
    }
    let chunks = match parser.parse_source(&content, language, path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, file = %path.display(), "Failed to parse file for doc rewrite");
            return Err(DocWriterError::Parser(e));
        }
    };

    // Resolve each edit to a line-level operation
    let mut resolved: Vec<ResolvedEdit> = Vec::new();

    for edit in edits {
        // RB-14: skip edits with mismatched language
        if edit.language != language {
            continue;
        }
        // Find matching chunk by name. If multiple matches, prefer the one
        // closest to the edit's original line_start.
        let matching_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.name == edit.function_name)
            .collect();

        let chunk = if matching_chunks.is_empty() {
            tracing::warn!(
                function = %edit.function_name,
                file = %path.display(),
                "Function not found in re-parsed file, skipping"
            );
            continue;
        } else if matching_chunks.len() == 1 {
            matching_chunks[0]
        } else {
            // Disambiguate by closest line_start to the edit's original
            matching_chunks
                .iter()
                .min_by_key(|c| (c.line_start as isize - edit.line_start as isize).unsigned_abs())
                .expect("matching_chunks guaranteed non-empty by else-if guard")
        };

        let line_start = chunk.line_start as usize;
        let insertion_line = find_insertion_point(line_start, &file_lines, language);

        let existing_range = detect_existing_doc_range(insertion_line, &file_lines, language);

        // Skip if function already has an adequate doc comment (>= 30 chars)
        // This prevents re-writing docs on every run when the cache still has the entry
        if let Some(ref range) = existing_range {
            let existing_doc: String = file_lines[range.clone()]
                .iter()
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join("\n");
            if existing_doc.len() >= 30 {
                tracing::debug!(
                    function = %edit.function_name,
                    "Function already has adequate doc, skipping"
                );
                continue;
            }
        }

        // Detect indentation from the chunk's first line
        let chunk_line_idx = line_start.saturating_sub(1); // 0-based
        let indent = if chunk_line_idx < file_lines.len() {
            let line = file_lines[chunk_line_idx];
            let stripped = line.trim_start();
            &line[..line.len() - stripped.len()]
        } else {
            ""
        };

        // For InsideBody (Python), use body indentation (one level deeper)
        let format = doc_format_for(language);
        let effective_indent = if format.position == InsertionPosition::InsideBody {
            // Detect body indent from the line after the def
            let body_idx = line_start; // 0-based index of line after def (line_start is 1-based)
            if body_idx < file_lines.len() && !file_lines[body_idx].trim().is_empty() {
                let body_line = file_lines[body_idx];
                let stripped = body_line.trim_start();
                body_line[..body_line.len() - stripped.len()].to_string()
            } else {
                // Fallback: original indent + 4 spaces
                format!("{indent}    ")
            }
        } else {
            indent.to_string()
        };

        let formatted = format_doc_comment(
            &edit.generated_doc,
            language,
            &effective_indent,
            &edit.function_name,
        );

        if formatted.is_empty() {
            continue;
        }

        let new_lines: Vec<String> = formatted.lines().map(|l| format!("{l}\n")).collect();

        // Compute 0-based insert position
        let insert_at_0 = insertion_line.saturating_sub(1);

        tracing::debug!(
            function = %edit.function_name,
            insert_at = insertion_line,
            existing_doc = existing_range.is_some(),
            "Resolved doc edit"
        );

        resolved.push(ResolvedEdit {
            remove_range: existing_range,
            insert_at: insert_at_0,
            new_lines,
        });
    }

    // RB-19: Log when edits are skipped (not found, adequate doc, empty format, etc.)
    let skipped = edits.len() - resolved.len();
    if skipped > 0 {
        tracing::info!(
            file = %path.display(),
            total = edits.len(),
            skipped,
            resolved = resolved.len(),
            "Skipped doc edits (not found, adequate doc, or empty)"
        );
    }

    if resolved.is_empty() {
        return Ok(0);
    }

    // Sort edits by line number descending (bottom-up) so earlier edits
    // don't shift line numbers for later ones.
    resolved.sort_by(|a, b| b.insert_at.cmp(&a.insert_at));

    // Apply edits to a mutable line buffer
    let mut lines: Vec<String> = content.lines().map(|l| format!("{l}\n")).collect();
    // Preserve trailing newline state
    if content.ends_with('\n') && !lines.is_empty() {
        // lines() already stripped trailing, but our format added \n back — correct
    } else if !content.ends_with('\n') && !lines.is_empty() {
        // File didn't end with newline; remove the extra \n we added to last line
        if let Some(last) = lines.last_mut() {
            if last.ends_with('\n') {
                last.pop();
            }
        }
    }

    let count = resolved.len();

    for edit in &resolved {
        // Remove existing doc lines first (if any)
        if let Some(ref range) = edit.remove_range {
            if range.start < lines.len() {
                let end = range.end.min(lines.len());
                lines.drain(range.start..end);
            }
        }

        // Compute effective insert position after removal
        let insert_at = if let Some(ref range) = edit.remove_range {
            // After removing lines, the insertion point shifts up
            edit.insert_at
                .saturating_sub(range.end.saturating_sub(range.start))
                .min(lines.len())
        } else {
            edit.insert_at.min(lines.len())
        };

        // Insert new doc lines
        for (i, line) in edit.new_lines.iter().enumerate() {
            lines.insert(insert_at + i, line.clone());
        }
    }

    // Atomic write: temp file in same directory + rename
    let result_content: String = lines.concat();
    atomic_write(path, result_content.as_bytes())?;

    tracing::debug!(file = %path.display(), count, "Wrote doc comments");

    Ok(count)
}

/// Write bytes to a file atomically: write to a temp file in the same
/// directory, then rename.
///
/// PB-4: On cross-device rename failure, falls back to copy-to-same-dir-then-rename
/// instead of a bare `fs::write`. This copies the original file to a backup in
/// the same directory, writes the new content to the original path, and removes
/// the backup on success. If the write fails, the backup is renamed back.
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), std::io::Error> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let suffix = crate::temp_suffix();
    let temp_path = dir.join(format!(".cqs-doc-{}-{}.tmp", std::process::id(), suffix));

    if let Err(e) = std::fs::write(&temp_path, data) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e);
    }

    match std::fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            // Rename can fail cross-device (EXDEV on Unix, ERROR_NOT_SAME_DEVICE on Windows).
            // Fall back to: backup original → write new → remove backup.
            let _ = std::fs::remove_file(&temp_path);

            let backup_path = dir.join(format!(".cqs-doc-{}-{}.bak", std::process::id(), suffix));

            // If the original exists, back it up so we can restore on failure
            let has_backup = if path.exists() {
                std::fs::copy(path, &backup_path)
                    .map(|_| true)
                    .map_err(|e| {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Cross-device fallback: failed to create backup"
                        );
                        e
                    })?
            } else {
                false
            };

            match std::fs::write(path, data) {
                Ok(()) => {
                    if has_backup {
                        let _ = std::fs::remove_file(&backup_path);
                    }
                    Ok(())
                }
                Err(write_err) => {
                    // Restore from backup if we made one
                    if has_backup {
                        let _ = std::fs::rename(&backup_path, path);
                    }
                    tracing::warn!(
                        path = %path.display(),
                        rename_error = %rename_err,
                        write_error = %write_err,
                        "Atomic write failed: both rename and fallback write failed"
                    );
                    Err(write_err)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    use crate::doc_writer::DocCommentResult;
    use crate::language::Language;

    /// Constructs a DocCommentResult containing metadata about a generated documentation comment.
    /// # Arguments
    /// * `file` - The path to the source file being documented
    /// * `function_name` - The name of the function for which documentation was generated
    /// * `generated_doc` - The content of the generated documentation comment
    /// * `language` - The programming language of the source file
    /// * `line_start` - The line number where the documentation comment begins
    /// * `had_existing_doc` - Whether the function previously had documentation
    /// # Returns
    /// A new `DocCommentResult` struct populated with the provided arguments and a placeholder content hash.
    fn make_edit(
        file: &Path,
        function_name: &str,
        generated_doc: &str,
        language: Language,
        line_start: usize,
        had_existing_doc: bool,
    ) -> DocCommentResult {
        DocCommentResult {
            file: file.to_path_buf(),
            function_name: function_name.to_string(),
            content_hash: "test_hash".to_string(),
            generated_doc: generated_doc.to_string(),
            language,
            line_start,
            had_existing_doc,
        }
    }

    // ── find_insertion_point tests ────────────────────────────────────

    #[test]
    fn test_insertion_point_plain_function() {
        let lines = vec!["", "fn hello() {", "}", ""];
        // Function at line 2 (1-based), no decorators above
        let point = find_insertion_point(2, &lines, Language::Rust);
        assert_eq!(point, 2, "Should insert right before function");
    }

    #[test]
    fn test_insertion_point_with_attributes() {
        let lines = vec![
            "use std::fmt;",
            "",
            "#[derive(Debug)]",
            "#[cfg(test)]",
            "fn hello() {",
            "}",
        ];
        // Function at line 5 (1-based), two attributes above + blank line
        let point = find_insertion_point(5, &lines, Language::Rust);
        // Should insert above #[derive(Debug)] — after "use std::fmt;" (line 1)
        assert_eq!(point, 2, "Should insert above first attribute");
    }

    #[test]
    fn test_insertion_point_python_inside_body() {
        let lines = vec!["def hello():", "    pass"];
        // Function at line 1 (1-based), Python inserts inside body
        let point = find_insertion_point(1, &lines, Language::Python);
        assert_eq!(point, 2, "Should insert on line after def");
    }

    #[test]
    fn test_insertion_point_with_at_decorator() {
        let lines = vec![
            "import os",
            "",
            "@staticmethod",
            "@decorator",
            "def hello():",
            "    pass",
        ];
        // Function at line 5, but Python uses InsideBody
        let point = find_insertion_point(5, &lines, Language::Python);
        assert_eq!(point, 6, "Python inserts inside body, ignores decorators");
    }

    #[test]
    fn test_insertion_point_first_line_of_file() {
        let lines = vec!["fn hello() {", "}"];
        let point = find_insertion_point(1, &lines, Language::Rust);
        assert_eq!(point, 1, "Should insert at line 1 when function is first");
    }

    #[test]
    fn test_insertion_point_attribute_at_top_of_file() {
        let lines = vec!["#[test]", "fn hello() {", "}"];
        let point = find_insertion_point(2, &lines, Language::Rust);
        assert_eq!(point, 1, "Should insert at line 1 above attribute at top");
    }

    // ── detect_existing_doc_range tests ──────────────────────────────

    #[test]
    fn test_detect_no_existing_doc() {
        let lines = vec!["use std::fmt;", "", "fn hello() {", "}"];
        let range = detect_existing_doc_range(3, &lines, Language::Rust);
        assert!(range.is_none(), "No doc comment should be detected");
    }

    #[test]
    fn test_detect_rust_doc_comment() {
        let lines = vec!["/// Does a thing.", "/// More detail.", "fn hello() {", "}"];
        // insertion_line=1 (1-based), scan upward from line 0
        // Actually: find_insertion_point for line 3 would give 1 because doc is above
        // But detect_existing_doc_range starts from insertion_line-1
        let range = detect_existing_doc_range(3, &lines, Language::Rust);
        assert_eq!(range, Some(0..2), "Should detect two-line /// block");
    }

    #[test]
    fn test_detect_single_line_rust_doc() {
        let lines = vec!["/// Short.", "fn hello() {", "}"];
        let range = detect_existing_doc_range(2, &lines, Language::Rust);
        assert_eq!(range, Some(0..1), "Should detect single-line /// doc");
    }

    #[test]
    fn test_detect_python_docstring_single_line() {
        let lines = vec!["def hello():", "    \"\"\"Does a thing.\"\"\"", "    pass"];
        // InsideBody: insertion_line=2 (1-based, the line after def)
        let range = detect_existing_doc_range(2, &lines, Language::Python);
        assert_eq!(range, Some(1..2), "Should detect single-line docstring");
    }

    #[test]
    fn test_detect_python_docstring_multiline() {
        let lines = vec![
            "def hello():",
            "    \"\"\"",
            "    Does a thing.",
            "    \"\"\"",
            "    pass",
        ];
        let range = detect_existing_doc_range(2, &lines, Language::Python);
        assert_eq!(range, Some(1..4), "Should detect multi-line docstring");
    }

    #[test]
    fn test_detect_no_python_docstring() {
        let lines = vec!["def hello():", "    pass"];
        let range = detect_existing_doc_range(2, &lines, Language::Python);
        assert!(range.is_none(), "No docstring present");
    }

    // ── rewrite_file integration tests ───────────────────────────────

    #[test]
    fn test_rewrite_rust_undocumented_function() {
        let source = "fn hello() {\n    println!(\"hi\");\n}\n";
        let mut tmp = NamedTempFile::with_suffix(".rs").unwrap();
        write!(tmp, "{source}").unwrap();
        tmp.flush().unwrap();

        let parser = Parser::new().unwrap();
        let edit = make_edit(
            tmp.path(),
            "hello",
            "Prints a greeting.",
            Language::Rust,
            1,
            false,
        );

        let count = rewrite_file(tmp.path(), &[edit], &parser).unwrap();
        assert_eq!(count, 1);

        let result = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            result.contains("/// Prints a greeting."),
            "Should contain doc comment, got:\n{result}"
        );
        assert!(
            result.find("/// Prints a greeting.").unwrap() < result.find("fn hello()").unwrap(),
            "Doc should appear before function"
        );
    }

    #[test]
    fn test_rewrite_rust_replace_thin_doc() {
        let source = "/// Short\nfn hello() {\n    println!(\"hi\");\n}\n";
        let mut tmp = NamedTempFile::with_suffix(".rs").unwrap();
        write!(tmp, "{source}").unwrap();
        tmp.flush().unwrap();

        let parser = Parser::new().unwrap();
        let edit = make_edit(
            tmp.path(),
            "hello",
            "Prints a friendly greeting to stdout.",
            Language::Rust,
            2,
            true,
        );

        let count = rewrite_file(tmp.path(), &[edit], &parser).unwrap();
        assert_eq!(count, 1);

        let result = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            !result.contains("/// Short"),
            "Old thin doc should be removed, got:\n{result}"
        );
        assert!(
            result.contains("/// Prints a friendly greeting to stdout."),
            "New doc should be inserted, got:\n{result}"
        );
    }

    #[test]
    fn test_rewrite_rust_with_decorators() {
        let source = "#[derive(Debug)]\n#[cfg(test)]\nfn hello() {\n    println!(\"hi\");\n}\n";
        let mut tmp = NamedTempFile::with_suffix(".rs").unwrap();
        write!(tmp, "{source}").unwrap();
        tmp.flush().unwrap();

        let parser = Parser::new().unwrap();
        let edit = make_edit(
            tmp.path(),
            "hello",
            "Prints a greeting.",
            Language::Rust,
            3,
            false,
        );

        let count = rewrite_file(tmp.path(), &[edit], &parser).unwrap();
        assert_eq!(count, 1);

        let result = std::fs::read_to_string(tmp.path()).unwrap();
        let doc_pos = result.find("/// Prints a greeting.").unwrap();
        let attr_pos = result.find("#[derive(Debug)]").unwrap();
        let fn_pos = result.find("fn hello()").unwrap();
        assert!(
            doc_pos < attr_pos,
            "Doc should be above #[derive], got:\n{result}"
        );
        assert!(
            attr_pos < fn_pos,
            "Attributes should be between doc and fn, got:\n{result}"
        );
    }

    #[test]
    fn test_rewrite_python_inside_body() {
        let source = "def hello():\n    pass\n";
        let mut tmp = NamedTempFile::with_suffix(".py").unwrap();
        write!(tmp, "{source}").unwrap();
        tmp.flush().unwrap();

        let parser = Parser::new().unwrap();
        let edit = make_edit(
            tmp.path(),
            "hello",
            "Prints a greeting.",
            Language::Python,
            1,
            false,
        );

        let count = rewrite_file(tmp.path(), &[edit], &parser).unwrap();
        assert_eq!(count, 1);

        let result = std::fs::read_to_string(tmp.path()).unwrap();
        let def_pos = result.find("def hello():").unwrap();
        let doc_pos = result.find("\"\"\"").unwrap();
        assert!(
            doc_pos > def_pos,
            "Docstring should be inside body (after def), got:\n{result}"
        );
    }

    #[test]
    fn test_rewrite_multiple_functions_bottom_up() {
        let source = "\
fn alpha() {
    println!(\"a\");
}

fn beta() {
    println!(\"b\");
}

fn gamma() {
    println!(\"c\");
}
";
        let mut tmp = NamedTempFile::with_suffix(".rs").unwrap();
        write!(tmp, "{source}").unwrap();
        tmp.flush().unwrap();

        let parser = Parser::new().unwrap();
        let edits = vec![
            make_edit(
                tmp.path(),
                "alpha",
                "First function.",
                Language::Rust,
                1,
                false,
            ),
            make_edit(
                tmp.path(),
                "gamma",
                "Third function.",
                Language::Rust,
                9,
                false,
            ),
        ];

        let count = rewrite_file(tmp.path(), &edits, &parser).unwrap();
        assert_eq!(count, 2, "Should modify two functions");

        let result = std::fs::read_to_string(tmp.path()).unwrap();

        // Verify both docs are present
        assert!(
            result.contains("/// First function."),
            "Alpha doc missing:\n{result}"
        );
        assert!(
            result.contains("/// Third function."),
            "Gamma doc missing:\n{result}"
        );

        // Verify beta is untouched (no doc added)
        let beta_pos = result.find("fn beta()").unwrap();
        let before_beta = &result[..beta_pos];
        assert!(
            !before_beta.ends_with("/// "),
            "Beta should not get a doc comment"
        );

        // Verify ordering: alpha doc < alpha fn < beta fn < gamma doc < gamma fn
        let alpha_doc = result.find("/// First function.").unwrap();
        let alpha_fn = result.find("fn alpha()").unwrap();
        let gamma_doc = result.find("/// Third function.").unwrap();
        let gamma_fn = result.find("fn gamma()").unwrap();
        assert!(alpha_doc < alpha_fn, "Alpha doc should be before alpha fn");
        assert!(alpha_fn < gamma_doc, "Alpha fn should be before gamma doc");
        assert!(gamma_doc < gamma_fn, "Gamma doc should be before gamma fn");
    }

    #[test]
    fn test_rewrite_function_not_found() {
        let source = "fn hello() {\n    println!(\"hi\");\n}\n";
        let mut tmp = NamedTempFile::with_suffix(".rs").unwrap();
        write!(tmp, "{source}").unwrap();
        tmp.flush().unwrap();

        let parser = Parser::new().unwrap();
        let edit = make_edit(
            tmp.path(),
            "nonexistent",
            "This function does not exist.",
            Language::Rust,
            1,
            false,
        );

        let count = rewrite_file(tmp.path(), &[edit], &parser).unwrap();
        assert_eq!(count, 0, "Should return 0 when function not found");

        // Verify file is unchanged
        let result = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(result, source, "File should be unchanged");
    }

    // TC-5: Same-name function disambiguation (two `new()` in different impl blocks)
    #[test]
    fn test_rewrite_disambiguates_same_name_functions() {
        let source = "\
struct Alpha;

impl Alpha {
    fn new() -> Self {
        Alpha
    }
}

struct Beta;

impl Beta {
    fn new() -> Self {
        Beta
    }
}
";
        let mut tmp = NamedTempFile::with_suffix(".rs").unwrap();
        write!(tmp, "{source}").unwrap();
        tmp.flush().unwrap();

        let parser = Parser::new().unwrap();
        // Target the second `new` (Beta::new at line 13)
        let edit = make_edit(
            tmp.path(),
            "new",
            "Creates a new Beta instance.",
            Language::Rust,
            13,
            false,
        );

        let count = rewrite_file(tmp.path(), &[edit], &parser).unwrap();
        assert_eq!(count, 1, "Should document exactly one function");

        let result = std::fs::read_to_string(tmp.path()).unwrap();
        // Doc should appear near "impl Beta", not "impl Alpha"
        let beta_pos = result.find("impl Beta").unwrap();
        let doc_pos = result.find("Creates a new Beta").unwrap();
        let alpha_pos = result.find("impl Alpha").unwrap();
        assert!(
            doc_pos > alpha_pos,
            "Doc should not be near Alpha, got:\n{result}"
        );
        assert!(
            doc_pos > beta_pos || doc_pos < beta_pos + 50,
            "Doc should be near Beta impl, got:\n{result}"
        );
    }

    // TC-3: Adequate doc skip path (>= 30 chars)
    #[test]
    fn test_rewrite_skips_adequate_doc() {
        let source = "/// This is a long enough doc comment for the function.\nfn hello() {\n    println!(\"hi\");\n}\n";
        let mut tmp = NamedTempFile::with_suffix(".rs").unwrap();
        write!(tmp, "{source}").unwrap();
        tmp.flush().unwrap();

        let parser = Parser::new().unwrap();
        let edit = make_edit(
            tmp.path(),
            "hello",
            "Replacement doc that should not appear.",
            Language::Rust,
            2,
            true,
        );

        let count = rewrite_file(tmp.path(), &[edit], &parser).unwrap();
        assert_eq!(count, 0, "Should skip function with adequate doc");

        let result = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            !result.contains("Replacement"),
            "Original doc should be preserved, got:\n{result}"
        );
        assert!(
            result.contains("This is a long enough"),
            "Original doc should remain"
        );
    }

    // TC-8: Empty edits array
    #[test]
    fn test_rewrite_empty_edits_returns_zero() {
        let source = "fn hello() {\n    println!(\"hi\");\n}\n";
        let mut tmp = NamedTempFile::with_suffix(".rs").unwrap();
        write!(tmp, "{source}").unwrap();
        tmp.flush().unwrap();

        let parser = Parser::new().unwrap();
        let count = rewrite_file(tmp.path(), &[], &parser).unwrap();
        assert_eq!(count, 0, "Empty edits should return 0");

        let result = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(result, source, "File should be unchanged with empty edits");
    }
}
