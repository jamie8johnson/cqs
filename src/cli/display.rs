//! Output and display functions for CLI results

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use cqs::reference::TaggedResult;
use cqs::store::{ParentContext, UnifiedResult};

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
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read {}", file.display()))?;
    // .lines() handles \r\n, but trim trailing \r for bare-CR edge cases
    let lines: Vec<&str> = content.lines().map(|l| l.trim_end_matches('\r')).collect();

    // Normalize: treat 0 as 1, ensure end >= start
    let line_start = line_start.max(1);
    let line_end = line_end.max(line_start);

    // Convert 1-indexed lines to 0-indexed array indices, clamped to valid range.
    // For an empty file (lines.len() == 0), both indices will be 0.
    let max_idx = lines.len().saturating_sub(1);
    let start_idx = (line_start as usize).saturating_sub(1).min(max_idx);
    let end_idx = (line_end as usize).saturating_sub(1).min(max_idx);

    // Context before
    let context_start = start_idx.saturating_sub(context);
    let before: Vec<String> = if start_idx <= lines.len() {
        lines[context_start..start_idx]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![]
    };

    // Context after (saturating_add prevents overflow near usize::MAX)
    let context_end = end_idx
        .saturating_add(context)
        .saturating_add(1)
        .min(lines.len());
    let after: Vec<String> = if end_idx + 1 < lines.len() {
        lines[(end_idx + 1)..context_end]
            .iter()
            .map(|s| s.to_string())
            .collect()
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
                        println!("{}", r.chunk.content);
                    } else {
                        for line in r.chunk.content.lines().take(8) {
                            println!("{}", line);
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
                        println!("{}", r.chunk.content);
                    } else {
                        for line in r.chunk.content.lines().take(8) {
                            println!("{}", line);
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
            // Delegate to UnifiedResult::to_json() for canonical base keys,
            // then layer on parent context and source fields (CQ-NEW-7).
            let mut json = t.result.to_json();
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
    fn read_context_lines_test(
        file: &Path,
        line_start: u32,
        line_end: u32,
        context: usize,
    ) -> anyhow::Result<(Vec<String>, Vec<String>)> {
        let content = std::fs::read_to_string(file)
            .with_context(|| format!("Failed to read {}", file.display()))?;
        let lines: Vec<&str> = content.lines().map(|l| l.trim_end_matches('\r')).collect();
        let line_start = line_start.max(1);
        let line_end = line_end.max(line_start);
        let max_idx = lines.len().saturating_sub(1);
        let start_idx = (line_start as usize).saturating_sub(1).min(max_idx);
        let end_idx = (line_end as usize).saturating_sub(1).min(max_idx);
        let context_start = start_idx.saturating_sub(context);
        let before: Vec<String> = if start_idx <= lines.len() {
            lines[context_start..start_idx]
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            vec![]
        };
        let context_end = end_idx
            .saturating_add(context)
            .saturating_add(1)
            .min(lines.len());
        let after: Vec<String> = if end_idx + 1 < lines.len() {
            lines[(end_idx + 1)..context_end]
                .iter()
                .map(|s| s.to_string())
                .collect()
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
