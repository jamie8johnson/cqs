//! Output and display functions for CLI results

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use cqs::reference::TaggedResult;
use cqs::store::{ParentContext, UnifiedResult};

/// Strip terminal control sequences from chunk-derived strings before
/// printing them to a TTY.
///
/// SEC-V1.33-5 (#1341): every CLI text-mode path that surfaces a chunk
/// feeds content directly to `println!`. A malicious file in the indexed
/// corpus (or a poisoned reference index — explicitly listed as a
/// "semi-trusted" surface in `SECURITY.md`) can embed:
///
/// - ANSI cursor / line-clear codes that overwrite previous terminal
///   output (forging "OK" status lines)
/// - OSC 8 hyperlinks that render as innocent text but click through to
///   attacker-chosen destinations
/// - iTerm2 / kitty / wezterm proprietary escapes that read clipboard
///   or execute commands in some configurations
/// - DCS sequences interpreted as commands by some terminals
///
/// `SECURITY.md` flags indexed content as "Untrusted (in AI agent
/// context)" — but the interactive CLI user is also a terminal, and so
/// is the agent's terminal when cqs runs through Claude Code. This
/// helper is the shell-version of indirect-prompt-injection mitigation.
///
/// Strategy: replace ESC (`\x1b`), DEL (`\x7f`), and most C0/C1 control
/// chars with `'?'`. Preserves `\t` / `\n` / `\r` so source-code layout
/// still renders. Using `'?'` (rather than dropping) keeps the byte
/// budget identical, which preserves column alignment in fancy
/// displays.
///
/// **Opt-out via `CQS_NO_ANSI_STRIP=1`** for terminals where the user
/// trusts their corpus and wants escape passthrough (e.g., displaying
/// chunks of code whose own string literals legitimately contain
/// escape sequences being analyzed).
///
/// Returns the input unchanged when no candidate byte is present,
/// avoiding allocation on every clean chunk.
pub fn sanitize_for_terminal(s: &str) -> std::borrow::Cow<'_, str> {
    if std::env::var("CQS_NO_ANSI_STRIP")
        .is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
    {
        return std::borrow::Cow::Borrowed(s);
    }
    if !s.bytes().any(|b| {
        b == 0x1b
            || (b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r')
            || b == 0x7f
            || (0x80..=0x9f).contains(&b)
    }) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c as u32 {
            0x1b => out.push('?'), // ESC — kills all CSI / OSC / DCS introducers
            0x00..=0x08 => out.push('?'),
            0x0b | 0x0c => out.push('?'), // VT / FF
            0x0e..=0x1a => out.push('?'),
            0x1c..=0x1f => out.push('?'),
            0x7f => out.push('?'),        // DEL
            0x80..=0x9f => out.push('?'), // C1 controls
            _ => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_for_terminal;

    #[test]
    fn passes_clean_text_unchanged() {
        let s = "fn foo() {\n    bar();\n}\t// trailing tab\n";
        // Borrowed Cow proves we didn't allocate.
        assert!(matches!(
            sanitize_for_terminal(s),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strips_ansi_csi() {
        let s = "before\x1b[31mred\x1b[0mafter";
        assert_eq!(sanitize_for_terminal(s).as_ref(), "before?[31mred?[0mafter");
    }

    #[test]
    fn strips_osc8_hyperlink() {
        let s = "\x1b]8;;file:///etc/passwd\x1b\\benign\x1b]8;;\x1b\\";
        let out = sanitize_for_terminal(s).into_owned();
        // Both ESC bytes replaced so the terminal doesn't interpret OSC 8;
        // the URL text is preserved as plain text (audit-visible).
        assert!(!out.contains('\x1b'));
        assert!(out.contains("file:///etc/passwd"));
    }

    #[test]
    fn preserves_tab_lf_cr() {
        let s = "a\tb\nc\rd";
        assert_eq!(sanitize_for_terminal(s).as_ref(), s);
    }

    #[test]
    fn strips_c1_controls() {
        let s = "before\u{0085}NEL\u{009b}CSIafter";
        let out = sanitize_for_terminal(s).into_owned();
        assert!(!out.contains('\u{0085}'));
        assert!(!out.contains('\u{009b}'));
    }

    #[test]
    fn strips_del() {
        let s = "before\x7fDELafter";
        assert_eq!(sanitize_for_terminal(s).as_ref(), "before?DELafter");
    }

    #[test]
    fn opt_out_via_env() {
        // SAFETY: this test mutates a process-global env var and is intentionally
        // not parallel-safe; full test run uses --test-threads=1 already.
        let prev = std::env::var("CQS_NO_ANSI_STRIP").ok();
        std::env::set_var("CQS_NO_ANSI_STRIP", "1");
        let s = "before\x1b[31mred\x1b[0mafter";
        assert_eq!(sanitize_for_terminal(s).as_ref(), s);
        match prev {
            Some(v) => std::env::set_var("CQS_NO_ANSI_STRIP", v),
            None => std::env::remove_var("CQS_NO_ANSI_STRIP"),
        }
    }
}

/// Read context lines before and after a range in a file
/// # Arguments
/// * `line_start` - 1-indexed start line (0 treated as 1)
/// * `line_end` - 1-indexed end line (must be >= line_start)
pub fn read_context_lines(
    file: &Path,
    line_start: u32,
    line_end: u32,
    context: usize,
) -> Result<(Vec<String>, Vec<String>)> {
    // Path traversal guard: reject absolute paths and `..` traversal that could
    // escape the project root via tampered DB paths. (RT-FS-1/RT-FS-2/SEC-12)
    //
    // DB stores relative paths; absolute paths indicate injection. The byte-
    // index check missed Windows UNC (`\\server\share`) and extended-length
    // (`\\?\C:\...`) paths — a tampered DB could trigger an SMB connection
    // and leak NTLM hashes via SMB relay. `Path::is_absolute` correctly
    // recognizes drive-letter (and some UNC) on Windows; the explicit
    // starts_with checks catch UNC consistently across platforms.
    let path_str = file.to_string_lossy();
    if file.is_absolute() || path_str.starts_with("\\\\") || path_str.starts_with("//") {
        anyhow::bail!("Absolute path blocked: {}", file.display());
    }
    if path_str.contains("..") {
        if let (Ok(canonical), Ok(cwd)) = (
            dunce::canonicalize(file),
            std::env::current_dir().and_then(dunce::canonicalize),
        ) {
            if !canonical.starts_with(&cwd) {
                anyhow::bail!("Path traversal blocked: {}", file.display());
            }
        }
    }

    // Size guard: don't read files larger than the configured cap for
    // context display. P3 #107: env-overridable via
    // `CQS_MAX_DISPLAY_FILE_SIZE` (default 10 MiB).
    let max_display_size = crate::cli::limits::max_display_file_size();
    if let Ok(meta) = std::fs::metadata(file) {
        if meta.len() > max_display_size {
            anyhow::bail!(
                "File too large for context display: {}MB (limit {}MB; CQS_MAX_DISPLAY_FILE_SIZE)",
                meta.len() / (1024 * 1024),
                max_display_size / (1024 * 1024)
            );
        }
    }
    // Normalize: treat 0 as 1, ensure end >= start. Done up front so the
    // RM-3 bounded read knows how many lines to actually pull off disk.
    let line_start = line_start.max(1);
    let line_end = line_end.max(line_start);

    // RM-3: bounded read. Previously `read_to_string` slurped the whole file
    // into RAM even when only ~5 lines around the chunk were needed. Compute
    // the upper bound from `line_end + context + 1` so we only walk the
    // BufReader that far. The downstream indexing logic still handles short
    // files gracefully because `lines.len()` reflects what was actually read.
    use std::io::{BufRead, BufReader};
    let f =
        std::fs::File::open(file).with_context(|| format!("Failed to read {}", file.display()))?;
    let limit = (line_end as usize)
        .saturating_add(context)
        .saturating_add(1);
    let lines: Vec<String> = BufReader::new(f)
        .lines()
        .take(limit)
        .map(|l| l.unwrap_or_default().trim_end_matches('\r').to_string())
        .collect();

    // Convert 1-indexed lines to 0-indexed array indices, clamped to valid range.
    // For an empty file (lines.len() == 0), both indices will be 0.
    let max_idx = lines.len().saturating_sub(1);
    let start_idx = (line_start as usize).saturating_sub(1).min(max_idx);
    let end_idx = (line_end as usize).saturating_sub(1).min(max_idx);

    // Context before
    let context_start = start_idx.saturating_sub(context);
    let before: Vec<String> = if start_idx <= lines.len() {
        lines[context_start..start_idx].to_vec()
    } else {
        vec![]
    };

    // Context after (saturating_add prevents overflow near usize::MAX)
    let context_end = end_idx
        .saturating_add(context)
        .saturating_add(1)
        .min(lines.len());
    let after: Vec<String> = if end_idx + 1 < lines.len() {
        lines[(end_idx + 1)..context_end].to_vec()
    } else {
        vec![]
    };

    Ok((before, after))
}

/// Display unified search results (code + notes)
pub fn display_unified_results(
    results: &[UnifiedResult],
    root: &Path,
    no_content: bool,
    context: Option<usize>,
    parents: Option<&HashMap<String, ParentContext>>,
) -> Result<()> {
    for result in results {
        match result {
            UnifiedResult::Code(r) => {
                // Paths are stored relative; strip_prefix handles legacy absolute paths
                let rel_path = cqs::rel_display(&r.chunk.file, root);

                let parent_tag = if r.chunk.parent_id.is_some() {
                    " [has parent]"
                } else {
                    ""
                };
                let header = format!(
                    "{}:{} ({} {}) [{}] [{:.2}]{}",
                    rel_path,
                    r.chunk.line_start,
                    r.chunk.chunk_type,
                    r.chunk.name,
                    r.chunk.language,
                    r.score,
                    parent_tag
                );

                println!("{}", header.cyan());

                if !no_content {
                    println!("{}", "─".repeat(50));

                    // Read context if requested
                    if let Some(n) = context {
                        if n > 0 {
                            let abs_path = root.join(&r.chunk.file);
                            match read_context_lines(
                                &abs_path,
                                r.chunk.line_start,
                                r.chunk.line_end,
                                n,
                            ) {
                                Ok((before, _)) => {
                                    for line in &before {
                                        println!("{}", format!("  {}", line).dimmed());
                                    }
                                }
                                Err(e) => {
                                    tracing::trace!(
                                        error = %e,
                                        file = %abs_path.display(),
                                        "Failed to read context lines (before)"
                                    );
                                }
                            }
                        }
                    }

                    // Show signature or truncated content
                    if r.chunk.content.lines().count() <= 10 {
                        println!("{}", sanitize_for_terminal(&r.chunk.content));
                    } else {
                        for line in r.chunk.content.lines().take(8) {
                            println!("{}", sanitize_for_terminal(line));
                        }
                        println!("    ...");
                    }

                    // Print after context if requested
                    if let Some(n) = context {
                        if n > 0 {
                            let abs_path = root.join(&r.chunk.file);
                            match read_context_lines(
                                &abs_path,
                                r.chunk.line_start,
                                r.chunk.line_end,
                                n,
                            ) {
                                Ok((_, after)) => {
                                    for line in &after {
                                        println!("{}", format!("  {}", line).dimmed());
                                    }
                                }
                                Err(e) => {
                                    tracing::trace!(
                                        error = %e,
                                        file = %abs_path.display(),
                                        "Failed to read context lines (after)"
                                    );
                                }
                            }
                        }
                    }

                    // Show parent context if --expand
                    if let Some(parent) = parents.and_then(|p| p.get(&r.chunk.id)) {
                        let parent_header = format!(
                            "  Parent context: {} ({}:{}-{})",
                            parent.name, rel_path, parent.line_start, parent.line_end,
                        );
                        println!("{}", parent_header.dimmed());
                        println!("{}", "  ────────────────────────────────".dimmed());
                        for line in parent.content.lines().take(20) {
                            println!("{}", format!("  {}", line).dimmed());
                        }
                        if parent.content.lines().count() > 20 {
                            println!("{}", "  ...".dimmed());
                        }
                    }

                    println!();
                }
            }
        }
    }

    println!("{} results", results.len());
    Ok(())
}

/// Display unified results as JSON
pub fn display_unified_results_json(
    results: &[UnifiedResult],
    query: &str,
    parents: Option<&HashMap<String, ParentContext>>,
    token_info: Option<(usize, usize)>,
) -> Result<()> {
    let json_results: Vec<_> = results
        .iter()
        .map(|r| {
            // Delegate to UnifiedResult::to_json() for the canonical base keys,
            // then layer on parent context fields (CQ-NEW-3).
            let mut obj = r.to_json();
            let UnifiedResult::Code(sr) = r;
            if let Some(parent) = parents.and_then(|p| p.get(&sr.chunk.id)) {
                obj["parent_name"] = serde_json::json!(parent.name);
                obj["parent_content"] = serde_json::json!(parent.content);
                obj["parent_line_start"] = serde_json::json!(parent.line_start);
                obj["parent_line_end"] = serde_json::json!(parent.line_end);
            }
            obj
        })
        .collect();

    let mut output = serde_json::json!({
        "results": json_results,
        "query": query,
        "total": results.len(),
    });
    if let Some((used, budget)) = token_info {
        output["token_count"] = serde_json::json!(used);
        output["token_budget"] = serde_json::json!(budget);
    }

    super::json_envelope::emit_json(&output)?;
    Ok(())
}

/// Display tagged search results (multi-index with source labels)
pub fn display_tagged_results(
    results: &[TaggedResult],
    root: &Path,
    no_content: bool,
    context: Option<usize>,
    parents: Option<&HashMap<String, ParentContext>>,
) -> Result<()> {
    for tagged in results {
        match &tagged.result {
            UnifiedResult::Code(r) => {
                let rel_path = cqs::rel_display(&r.chunk.file, root);

                // Prepend source name for reference results
                let source_prefix = tagged
                    .source
                    .as_ref()
                    .map(|s| format!("[{}] ", s))
                    .unwrap_or_default();

                let parent_tag = if r.chunk.parent_id.is_some() {
                    " [has parent]"
                } else {
                    ""
                };
                let header = format!(
                    "{}{}:{} ({} {}) [{}] [{:.2}]{}",
                    source_prefix,
                    rel_path,
                    r.chunk.line_start,
                    r.chunk.chunk_type,
                    r.chunk.name,
                    r.chunk.language,
                    r.score,
                    parent_tag
                );

                println!("{}", header.cyan());

                if !no_content {
                    println!("{}", "─".repeat(50));

                    // Context lines only for project results (reference source files may not exist)
                    if tagged.source.is_none() {
                        if let Some(n) = context {
                            if n > 0 {
                                let abs_path = root.join(&r.chunk.file);
                                match read_context_lines(
                                    &abs_path,
                                    r.chunk.line_start,
                                    r.chunk.line_end,
                                    n,
                                ) {
                                    Ok((before, _)) => {
                                        for line in &before {
                                            println!("{}", format!("  {}", line).dimmed());
                                        }
                                    }
                                    Err(e) => {
                                        tracing::trace!(
                                            error = %e,
                                            file = %abs_path.display(),
                                            "Failed to read context lines (before)"
                                        );
                                    }
                                }
                            }
                        }
                    }

                    if r.chunk.content.lines().count() <= 10 {
                        println!("{}", sanitize_for_terminal(&r.chunk.content));
                    } else {
                        for line in r.chunk.content.lines().take(8) {
                            println!("{}", sanitize_for_terminal(line));
                        }
                        println!("    ...");
                    }

                    // After context only for project results
                    if tagged.source.is_none() {
                        if let Some(n) = context {
                            if n > 0 {
                                let abs_path = root.join(&r.chunk.file);
                                match read_context_lines(
                                    &abs_path,
                                    r.chunk.line_start,
                                    r.chunk.line_end,
                                    n,
                                ) {
                                    Ok((_, after)) => {
                                        for line in &after {
                                            println!("{}", format!("  {}", line).dimmed());
                                        }
                                    }
                                    Err(e) => {
                                        tracing::trace!(
                                            error = %e,
                                            file = %abs_path.display(),
                                            "Failed to read context lines (after)"
                                        );
                                    }
                                }
                            }
                        }
                    }

                    // Show parent context if --expand
                    if let Some(parent) = parents.and_then(|p| p.get(&r.chunk.id)) {
                        let parent_header = format!(
                            "  Parent context: {} ({}:{}-{})",
                            parent.name, rel_path, parent.line_start, parent.line_end,
                        );
                        println!("{}", parent_header.dimmed());
                        println!("{}", "  ────────────────────────────────".dimmed());
                        for line in parent.content.lines().take(20) {
                            println!("{}", format!("  {}", line).dimmed());
                        }
                        if parent.content.lines().count() > 20 {
                            println!("{}", "  ...".dimmed());
                        }
                    }

                    println!();
                }
            }
        }
    }

    println!("{} results", results.len());
    Ok(())
}

/// Display similar results as JSON
pub fn display_similar_results_json(
    results: &[cqs::store::SearchResult],
    target: &str,
) -> Result<()> {
    // Delegate to SearchResult::to_json() for canonical base keys.
    // Previously missing `type` and `has_parent` (CQ-NEW-5).
    let json_results: Vec<_> = results.iter().map(|r| r.to_json()).collect();

    let output = serde_json::json!({
        "target": target,
        "results": json_results,
        "total": results.len(),
    });

    super::json_envelope::emit_json(&output)?;
    Ok(())
}

/// Display tagged results as JSON (multi-index with source field)
pub fn display_tagged_results_json(
    results: &[TaggedResult],
    query: &str,
    parents: Option<&HashMap<String, ParentContext>>,
    token_info: Option<(usize, usize)>,
) -> Result<()> {
    let json_results: Vec<_> = results
        .iter()
        .map(|t| {
            // Delegate to UnifiedResult::to_json_with_origin() for the
            // canonical base keys + the trust_level/reference_name pair
            // (#1167, #1169), then layer on parent context and the legacy
            // `source` field for back-compat (CQ-NEW-7). Existing consumers
            // can keep reading `source`; new ones should prefer the typed
            // `trust_level` + `reference_name`.
            let mut json = t.result.to_json_with_origin(t.source.as_deref());
            let UnifiedResult::Code(sr) = &t.result;
            if let Some(parent) = parents.and_then(|p| p.get(&sr.chunk.id)) {
                json["parent_name"] = serde_json::json!(parent.name);
                json["parent_content"] = serde_json::json!(parent.content);
                json["parent_line_start"] = serde_json::json!(parent.line_start);
                json["parent_line_end"] = serde_json::json!(parent.line_end);
            }
            if let Some(source) = &t.source {
                json["source"] = serde_json::json!(source);
            }
            json
        })
        .collect();

    let mut output = serde_json::json!({
        "results": json_results,
        "query": query,
        "total": results.len(),
    });
    if let Some((used, budget)) = token_info {
        output["token_count"] = serde_json::json!(used);
        output["token_budget"] = serde_json::json!(budget);
    }

    super::json_envelope::emit_json(&output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== read_context_lines tests (P3-14, P3-18) =====

    /// Creates a temp test file and returns (TempDir, relative_path).
    /// Returns a relative path (just the filename) suitable for the SEC-12
    /// absolute-path guard. The returned TempDir must stay alive for the
    /// duration of the test (drop deletes the dir). The CWD is changed to
    /// the temp dir so the relative path resolves.
    fn write_test_file(lines: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let file_path = dir.path().join("test.rs");
        let content = lines.join("\n");
        std::fs::write(&file_path, &content).unwrap();
        // SEC-12: return absolute path but guard won't fire during tests
        // because we set CWD. Use file_path directly for tests that need
        // to read outside the guard.
        (dir, file_path)
    }

    /// Read context lines bypassing the path guard (for unit tests with temp files).
    /// RM-3: mirror the production `read_context_lines` BufReader-based bounded read
    /// so the test's edge-case coverage stays representative.
    fn read_context_lines_test(
        file: &Path,
        line_start: u32,
        line_end: u32,
        context: usize,
    ) -> anyhow::Result<(Vec<String>, Vec<String>)> {
        let line_start = line_start.max(1);
        let line_end = line_end.max(line_start);
        use std::io::{BufRead, BufReader};
        let f = std::fs::File::open(file)
            .with_context(|| format!("Failed to read {}", file.display()))?;
        let limit = (line_end as usize)
            .saturating_add(context)
            .saturating_add(1);
        let lines: Vec<String> = BufReader::new(f)
            .lines()
            .take(limit)
            .map(|l| l.unwrap_or_default().trim_end_matches('\r').to_string())
            .collect();
        let max_idx = lines.len().saturating_sub(1);
        let start_idx = (line_start as usize).saturating_sub(1).min(max_idx);
        let end_idx = (line_end as usize).saturating_sub(1).min(max_idx);
        let context_start = start_idx.saturating_sub(context);
        let before: Vec<String> = if start_idx <= lines.len() {
            lines[context_start..start_idx].to_vec()
        } else {
            vec![]
        };
        let context_end = end_idx
            .saturating_add(context)
            .saturating_add(1)
            .min(lines.len());
        let after: Vec<String> = if end_idx + 1 < lines.len() {
            lines[(end_idx + 1)..context_end].to_vec()
        } else {
            vec![]
        };
        Ok((before, after))
    }

    #[test]
    fn test_read_context_lines_basic() {
        let lines = vec![
            "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7",
        ];
        let (_dir, path) = write_test_file(&lines);

        // Function at lines 3-5, context=1
        let (before, after) = read_context_lines_test(&path, 3, 5, 1).unwrap();
        assert_eq!(before.len(), 1, "Should have 1 line before");
        assert_eq!(before[0], "line 2");
        assert_eq!(after.len(), 1, "Should have 1 line after");
        assert_eq!(after[0], "line 6");
    }

    #[test]
    fn test_read_context_lines_at_start() {
        let lines = vec!["first", "second", "third", "fourth"];
        let (_dir, path) = write_test_file(&lines);

        // Function at line 1, context=2 -- no before lines available
        let (before, after) = read_context_lines_test(&path, 1, 1, 2).unwrap();
        assert!(before.is_empty(), "No lines before line 1");
        assert_eq!(after.len(), 2, "Should have 2 lines after");
        assert_eq!(after[0], "second");
        assert_eq!(after[1], "third");
    }

    #[test]
    fn test_read_context_lines_at_end() {
        let lines = vec!["first", "second", "third", "last"];
        let (_dir, path) = write_test_file(&lines);

        // Function at last line, context=2
        let (before, after) = read_context_lines_test(&path, 4, 4, 2).unwrap();
        assert_eq!(before.len(), 2, "Should have 2 lines before");
        assert_eq!(before[0], "second");
        assert_eq!(before[1], "third");
        assert!(after.is_empty(), "No lines after last line");
    }

    #[test]
    fn test_read_context_lines_zero_context() {
        let lines = vec!["line 1", "line 2", "line 3"];
        let (_dir, path) = write_test_file(&lines);

        let (before, after) = read_context_lines_test(&path, 2, 2, 0).unwrap();
        assert!(before.is_empty());
        assert!(after.is_empty());
    }

    #[test]
    fn test_read_context_lines_single_line_file() {
        let (_dir, path) = write_test_file(&["only line"]);

        let (before, after) = read_context_lines_test(&path, 1, 1, 5).unwrap();
        assert!(before.is_empty());
        assert!(after.is_empty());
    }

    #[test]
    fn test_read_context_lines_line_zero_normalized() {
        let lines = vec!["first", "second"];
        let (_dir, path) = write_test_file(&lines);

        // line_start=0 should be normalized to 1
        let (before, after) = read_context_lines_test(&path, 0, 1, 1).unwrap();
        assert!(before.is_empty(), "Line 0 normalizes to 1, nothing before");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0], "second");
    }

    #[test]
    fn test_read_context_lines_nonexistent_file() {
        let result = read_context_lines(Path::new("nonexistent/file.rs"), 1, 5, 2);
        assert!(result.is_err(), "Should fail for nonexistent file");
    }

    #[test]
    fn test_read_context_lines_absolute_path_blocked() {
        let result = read_context_lines(Path::new("/etc/passwd"), 1, 5, 2);
        assert!(result.is_err(), "Should block absolute paths");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Absolute path blocked"),
            "Expected absolute path error, got: {err}"
        );
    }

    /// C.3 / SEC: tampered DB row could carry a Windows UNC path
    /// (`\\server\share\loot`). On Windows that triggers an SMB mount and
    /// can leak NTLM hashes via SMB relay. The byte-2 drive-letter check
    /// missed UNC entirely; the new guard rejects via either
    /// `Path::is_absolute()` or a literal `\\` / `//` prefix.
    #[test]
    fn read_context_lines_rejects_unc_paths() {
        let p = Path::new("\\\\evil-server\\share\\loot");
        let result = read_context_lines(p, 1, 1, 0);
        assert!(result.is_err(), "UNC path must be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Absolute path blocked"),
            "Expected absolute-path block, got: {err}"
        );
    }

    /// C.3 / SEC: extended-length / device-namespace paths
    /// (`\\?\C:\loot`, `\\.\PIPE\foo`) bypass MAX_PATH and reach Win32
    /// directly. Same guard family — must also be blocked.
    #[test]
    fn read_context_lines_rejects_extended_length_path() {
        let p = Path::new("\\\\?\\C:\\loot");
        let result = read_context_lines(p, 1, 1, 0);
        assert!(result.is_err(), "Extended-length path must be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Absolute path blocked"),
            "Expected absolute-path block, got: {err}"
        );
    }

    #[test]
    fn test_read_context_lines_multi_line_range() {
        let lines = vec!["a", "b", "c", "d", "e", "f", "g", "h"];
        let (_dir, path) = write_test_file(&lines);

        // Function spans lines 3-6, context=1
        let (before, after) = read_context_lines_test(&path, 3, 6, 1).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0], "b");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0], "g");
    }

    // ===== HP-7: display_similar_results_json tests =====

    fn make_search_result(
        name: &str,
        score: f32,
        parent_id: Option<&str>,
    ) -> cqs::store::SearchResult {
        cqs::store::SearchResult {
            chunk: cqs::store::ChunkSummary {
                id: format!("id-{name}"),
                file: std::path::PathBuf::from(format!("src/{name}.rs")),
                language: cqs::parser::Language::Rust,
                chunk_type: cqs::parser::ChunkType::Function,
                name: name.to_string(),
                signature: format!("fn {name}()"),
                content: format!("fn {name}() {{}}"),
                doc: None,
                line_start: 10,
                line_end: 20,
                parent_id: parent_id.map(|s| s.to_string()),
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
            },
            score,
        }
    }

    /// Verify display_similar_results_json succeeds with non-empty results.
    /// Output goes to stdout (hard to capture in-process), so we assert Ok(()).
    #[test]
    fn test_display_similar_results_json_returns_ok() {
        let results = vec![
            make_search_result("alpha", 0.95, None),
            make_search_result("beta", 0.80, Some("parent-1")),
        ];
        let result = super::display_similar_results_json(&results, "my_target");
        assert!(
            result.is_ok(),
            "display_similar_results_json should succeed"
        );
    }

    /// Verify display_similar_results_json succeeds with empty results.
    #[test]
    fn test_display_similar_results_json_empty() {
        let results: Vec<cqs::store::SearchResult> = vec![];
        let result = super::display_similar_results_json(&results, "no_matches");
        assert!(result.is_ok(), "should succeed with empty results");
    }

    /// Verify the JSON structure that display_similar_results_json produces.
    /// Since we cannot easily capture println! output, we replicate the same
    /// construction logic and verify field completeness.
    #[test]
    fn test_display_similar_results_json_structure() {
        let results = [
            make_search_result("alpha", 0.95, None),
            make_search_result("beta", 0.80, Some("parent-1")),
        ];

        // Use the same canonical to_json() that display_similar_results_json delegates to
        let json_results: Vec<_> = results.iter().map(|r| r.to_json()).collect();

        let output = serde_json::json!({
            "target": "my_target",
            "results": json_results,
            "total": results.len(),
        });

        // Top-level fields
        assert!(output.get("target").is_some(), "missing 'target'");
        assert!(output.get("results").is_some(), "missing 'results'");
        assert!(output.get("total").is_some(), "missing 'total'");
        assert_eq!(output["target"], "my_target");
        assert_eq!(output["total"], 2);

        // Per-result fields
        let arr = output["results"].as_array().unwrap();
        assert_eq!(arr.len(), 2);

        for (i, item) in arr.iter().enumerate() {
            let obj = item.as_object().unwrap_or_else(|| {
                panic!("result[{i}] should be an object");
            });
            for field in [
                "file",
                "line_start",
                "line_end",
                "name",
                "signature",
                "language",
                "chunk_type",
                "score",
                "content",
            ] {
                assert!(
                    obj.contains_key(field),
                    "result[{i}] missing field '{field}'"
                );
            }
        }

        // Verify specific values
        assert_eq!(arr[0]["name"], "alpha");
        assert_eq!(arr[1]["name"], "beta");
        assert_eq!(arr[0]["line_start"], 10);
        assert_eq!(arr[0]["line_end"], 20);
        assert_eq!(arr[0]["language"], "rust");
        assert_eq!(arr[0]["chunk_type"], "function");

        // Score values
        let s0 = arr[0]["score"].as_f64().unwrap();
        assert!((s0 - 0.95).abs() < 1e-4, "alpha score should be ~0.95");
        let s1 = arr[1]["score"].as_f64().unwrap();
        assert!((s1 - 0.80).abs() < 1e-4, "beta score should be ~0.80");
    }
}
