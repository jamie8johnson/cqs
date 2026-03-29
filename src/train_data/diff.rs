/// A hunk range from a unified diff header.
#[derive(Debug, Clone)]
pub struct HunkRange {
    pub new_start: usize,
    pub new_count: usize,
}

/// A file entry from parsed diff output, with its hunks.
#[derive(Debug)]
pub struct DiffFile {
    pub path: String,
    pub hunks: Vec<HunkRange>,
}

impl DiffFile {
    /// Total number of added lines across all hunks.
    pub fn total_added_lines(&self) -> usize {
        self.hunks.iter().map(|h| h.new_count).sum()
    }
}

/// A function span for intersection with diff hunks.
#[derive(Debug, Clone)]
pub struct FunctionSpan {
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

/// Parse a `@@ -old_start,old_count +new_start,new_count @@` header line.
///
/// Returns `None` if the line doesn't match the expected format.
pub fn parse_hunk_header(line: &str) -> Option<HunkRange> {
    // Format: @@ -A,B +C,D @@ optional context
    // or:    @@ -A +C,D @@  (count defaults to 1 when omitted)
    let line = line.trim();
    if !line.starts_with("@@") {
        return None;
    }

    // Find the +N,M or +N portion
    let plus_idx = line.find('+')?;
    let after_plus = &line[plus_idx + 1..];

    // Find the closing @@
    let end_idx = after_plus.find("@@")?;
    let range_str = after_plus[..end_idx].trim();

    if let Some((start_str, count_str)) = range_str.split_once(',') {
        let new_start = start_str.parse::<usize>().ok()?;
        let new_count = count_str.parse::<usize>().ok()?;
        Some(HunkRange {
            new_start,
            new_count,
        })
    } else {
        // No comma -- count defaults to 1
        let new_start = range_str.parse::<usize>().ok()?;
        Some(HunkRange {
            new_start,
            new_count: 1,
        })
    }
}

/// Parse unified diff output into per-file entries with hunk ranges.
///
/// Skips submodule entries (detected by "Subproject commit" lines).
/// Returns empty vec for empty input.
pub fn parse_diff_output(diff: &str) -> Vec<DiffFile> {
    let _span = tracing::info_span!("parse_diff_output").entered();
    if diff.is_empty() {
        return Vec::new();
    }

    let mut files: Vec<DiffFile> = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_hunks: Vec<HunkRange> = Vec::new();
    let mut is_submodule = false;

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            // Flush previous file
            if let Some(path) = current_path.take() {
                if !is_submodule && !current_hunks.is_empty() {
                    files.push(DiffFile {
                        path,
                        hunks: std::mem::take(&mut current_hunks),
                    });
                } else {
                    current_hunks.clear();
                }
            }
            is_submodule = false;

            // Extract path from "diff --git a/path b/path"
            // Use the b/ side (post-image)
            if let Some(b_idx) = line.rfind(" b/") {
                current_path = Some(line[b_idx + 3..].to_string());
            }
        } else if line.contains("Subproject commit") {
            is_submodule = true;
        } else if line.starts_with("@@") {
            if let Some(hunk) = parse_hunk_header(line) {
                current_hunks.push(hunk);
            }
        }
    }

    // Flush last file
    if let Some(path) = current_path {
        if !is_submodule && !current_hunks.is_empty() {
            files.push(DiffFile {
                path,
                hunks: current_hunks,
            });
        }
    }

    files
}

/// Find functions whose line ranges overlap with any diff hunk.
///
/// Deduplicates nested functions: if a nested span (e.g., closure) and its
/// enclosing parent both match, only the outermost (parent) is kept.
pub fn find_changed_functions(
    hunks: &[HunkRange],
    functions: &[FunctionSpan],
) -> Vec<FunctionSpan> {
    let _span = tracing::info_span!(
        "find_changed_functions",
        hunks = hunks.len(),
        functions = functions.len()
    )
    .entered();
    let mut matched: Vec<&FunctionSpan> = Vec::new();

    for func in functions {
        let overlaps = hunks.iter().any(|h| {
            let hunk_end = h.new_start + h.new_count.saturating_sub(1);
            // Overlap: hunk range [new_start, hunk_end] intersects [start_line, end_line]
            h.new_start <= func.end_line && hunk_end >= func.start_line
        });
        if overlaps {
            matched.push(func);
        }
    }

    // Deduplicate nested: if function A fully contains function B, keep only A
    let mut result: Vec<FunctionSpan> = Vec::new();
    for func in &matched {
        let has_parent = matched.iter().any(|other| {
            !std::ptr::eq(*other, *func)
                && other.start_line <= func.start_line
                && other.end_line >= func.end_line
        });
        if !has_parent {
            result.push((*func).clone());
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Parses a basic unified diff hunk header and validates the extracted line numbers and counts.
    ///
    /// # Arguments
    ///
    /// None. This is a unit test that uses a hardcoded hunk header string.
    ///
    /// # Returns
    ///
    /// None. This function asserts expected values and panics if they don't match.
    ///
    /// # Panics
    ///
    /// Panics if `parse_hunk_header` returns `None` (via `unwrap()`) or if any assertion fails (new_start != 12 or new_count != 8).

    #[test]
    fn parse_hunk_header_basic() {
        let line = "@@ -10,5 +12,8 @@ fn some_context";
        let hunk = super::parse_hunk_header(line).unwrap();
        assert_eq!(hunk.new_start, 12);
        assert_eq!(hunk.new_count, 8);
    }
    /// Verifies that `parse_diff_output` correctly extracts file paths and hunks from unified diff format.
    ///
    /// # Arguments
    ///
    /// This is a test function with no parameters.
    ///
    /// # Returns
    ///
    /// None. This function asserts the correctness of parsing a sample diff string containing one modified file with one hunk.

    #[test]
    fn parse_diff_extracts_files_and_hunks() {
        let diff = "diff --git a/src/foo.rs b/src/foo.rs\n--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1,3 +1,5 @@\n+new line\n context\n";
        let files = parse_diff_output(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/foo.rs");
        assert_eq!(files[0].hunks.len(), 1);
    }
    /// Tests that `find_changed_functions` correctly identifies which functions are affected by code hunks.
    ///
    /// # Arguments
    ///
    /// * `hunks` - A vector of `HunkRange` representing modified line ranges in code
    /// * `functions` - A vector of `FunctionSpan` representing function definitions with their line ranges
    ///
    /// # Returns
    ///
    /// A vector of `FunctionSpan` containing only the functions whose line ranges intersect with the modified hunks. In this test case, function "b" (lines 5-10) intersects with the hunk at lines 5-7, while function "a" (lines 1-4) does not.

    #[test]
    fn intersect_hunks_with_functions() {
        let hunks = vec![HunkRange {
            new_start: 5,
            new_count: 3,
        }];
        let functions = vec![
            FunctionSpan {
                name: "a".into(),
                start_line: 1,
                end_line: 4,
                content: "fn a()".into(),
            },
            FunctionSpan {
                name: "b".into(),
                start_line: 5,
                end_line: 10,
                content: "fn b()".into(),
            },
        ];
        let changed = find_changed_functions(&hunks, &functions);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].name, "b");
    }
    /// Tests that `find_changed_functions` correctly identifies multiple functions when a code hunk spans across function boundaries.
    ///
    /// # Arguments
    ///
    /// This is a test function with no parameters. It internally creates:
    /// - `hunks`: A vector containing a single `HunkRange` representing lines 4-7
    /// - `functions`: A vector of two `FunctionSpan` objects where the hunk overlaps both function "a" (lines 1-5) and function "b" (lines 6-10)
    ///
    /// # Returns
    ///
    /// No explicit return value. Asserts that `find_changed_functions` returns a collection with 2 elements, validating that both functions are identified as changed.

    #[test]
    fn hunk_spanning_two_functions() {
        let hunks = vec![HunkRange {
            new_start: 4,
            new_count: 4,
        }];
        let functions = vec![
            FunctionSpan {
                name: "a".into(),
                start_line: 1,
                end_line: 5,
                content: "fn a()".into(),
            },
            FunctionSpan {
                name: "b".into(),
                start_line: 6,
                end_line: 10,
                content: "fn b()".into(),
            },
        ];
        let changed = find_changed_functions(&hunks, &functions);
        assert_eq!(changed.len(), 2);
    }
    /// Tests that `find_changed_functions` returns an empty list when hunks modify lines outside of any function definitions.
    ///
    /// # Arguments
    ///
    /// This is a test function with no parameters. It internally creates:
    /// - `hunks`: A vector containing a single `HunkRange` with changes at lines 1-2
    /// - `functions`: A vector containing a single `FunctionSpan` for function "a" at lines 5-10
    ///
    /// # Returns
    ///
    /// None. This function asserts that `find_changed_functions` returns an empty collection when the modified lines do not overlap with any function spans.

    #[test]
    fn change_outside_functions_skipped() {
        let hunks = vec![HunkRange {
            new_start: 1,
            new_count: 2,
        }];
        let functions = vec![FunctionSpan {
            name: "a".into(),
            start_line: 5,
            end_line: 10,
            content: "fn a()".into(),
        }];
        let changed = find_changed_functions(&hunks, &functions);
        assert!(changed.is_empty());
    }
    /// Verifies that submodule entries are excluded from parsed diff output.
    ///
    /// # Arguments
    ///
    /// This is a test function with no parameters.
    ///
    /// # Returns
    ///
    /// Returns nothing; this is a test assertion that verifies `parse_diff_output` correctly skips submodule commit changes.
    ///
    /// # Panics
    ///
    /// Panics if the assertion fails, indicating that `parse_diff_output` incorrectly included submodule entries in its output.

    #[test]
    fn skips_submodule_entries() {
        let diff = "diff --git a/submod b/submod\n--- a/submod\n+++ b/submod\n@@ -1 +1 @@\n-Subproject commit abc\n+Subproject commit def\n";
        let files = parse_diff_output(diff);
        assert!(files.is_empty());
    }
    /// Verifies that parsing an empty diff output string returns an empty collection of files.
    ///
    /// # Arguments
    ///
    /// None
    ///
    /// # Returns
    ///
    /// None. This is a test function that asserts the expected behavior rather than returning a value.

    #[test]
    fn empty_diff_returns_empty() {
        let files = parse_diff_output("");
        assert!(files.is_empty());
    }
    /// Tests that when a code hunk matches both an outer function and a nested closure, `find_changed_functions` returns only the outermost function, deduplicating nested matches.
    ///
    /// # Arguments
    ///
    /// This is a test function with no parameters. It internally creates test data including a code hunk at line 3 and two function spans (an outer function from lines 1-10 and a nested closure from lines 2-5).
    ///
    /// # Returns
    ///
    /// Returns nothing. Asserts that the changed functions list contains exactly one entry with the name "outer".

    #[test]
    fn nested_closure_attributed_to_outer() {
        let hunks = vec![HunkRange {
            new_start: 3,
            new_count: 1,
        }];
        let functions = vec![
            FunctionSpan {
                name: "outer".into(),
                start_line: 1,
                end_line: 10,
                content: "fn outer()".into(),
            },
            FunctionSpan {
                name: "closure".into(),
                start_line: 2,
                end_line: 5,
                content: "|| {}".into(),
            },
        ];
        // Both match the hunk, but find_changed_functions deduplicates to outermost
        let changed = find_changed_functions(&hunks, &functions);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].name, "outer");
    }
    /// Verifies that the total added lines counter correctly aggregates line counts across multiple hunks in a diff. Parses a multi-hunk diff output for a single file and asserts that the total added lines equals the sum of additions from all hunks (5 lines from first hunk plus 4 from second hunk).
    ///
    /// # Arguments
    ///
    /// None. This is a test function that uses hardcoded test data.
    ///
    /// # Returns
    ///
    /// None. This function asserts equality and panics on failure.
    ///
    /// # Panics
    ///
    /// Panics if the total added lines count does not equal 9, indicating the total_added_lines() method failed to correctly sum additions across multiple hunks.

    #[test]
    fn total_diff_lines_counted() {
        let diff = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,5 @@\n+a\n+b\n c\n@@ -10,2 +12,4 @@\n+d\n+e\n";
        let files = parse_diff_output(diff);
        assert_eq!(files[0].total_added_lines(), 9); // 5 + 4
    }
}
