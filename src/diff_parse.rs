//! Unified diff parser for `cqs impact-diff`
//!
//! Extracts changed file paths and line ranges from `git diff` output.

use std::sync::LazyLock;

use regex::Regex;

/// Compiled once, reused across all calls to `parse_unified_diff`
static HUNK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@@ [^@]* \+(\d+)(?:,(\d+))? @@").expect("hardcoded hunk regex"));

/// A single hunk from a unified diff — one changed region in one file
#[derive(Debug, Clone)]
pub struct DiffHunk {
    /// Relative file path (from `+++ b/...`)
    pub file: String,
    /// Start line in the new version (1-based)
    pub start: u32,
    /// Number of lines in the new version (half-open: covers `start..start+count`)
    pub count: u32,
}

/// Parse unified diff output into hunks.
///
/// Handles standard `git diff` output:
/// - Splits on `diff --git` boundaries
/// - Extracts file path from `+++ b/...`
/// - Extracts line ranges from `@@ ... +start,count @@`
/// - Skips binary files and deleted files (`+++ /dev/null`)
/// - Defaults count to 1 when omitted (`@@ +start @@`)
pub fn parse_unified_diff(input: &str) -> Vec<DiffHunk> {
    if input.is_empty() {
        return Vec::new();
    }

    // Normalize CRLF for Windows git output (bare \r from classic Mac too)
    let input = if input.contains('\r') {
        std::borrow::Cow::Owned(input.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        std::borrow::Cow::Borrowed(input)
    };

    let mut hunks = Vec::new();
    let mut current_file: Option<String> = None;

    for line in input.lines() {
        // New file boundary
        if let Some(path) = line.strip_prefix("+++ ") {
            if path == "/dev/null" {
                // Deleted file — no new-side hunks
                current_file = None;
            } else if let Some(rel) = path.strip_prefix("b/") {
                current_file = Some(rel.to_string());
            } else {
                // Non-standard format, use as-is
                current_file = Some(path.to_string());
            }
            continue;
        }

        // Skip binary files
        if line.starts_with("Binary files ") {
            current_file = None;
            continue;
        }

        // Hunk header
        if let Some(file) = &current_file {
            if let Some(caps) = HUNK_RE.captures(line) {
                let start: u32 = match caps[1].parse() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::warn!(
                            line = line,
                            file = file.as_str(),
                            "Could not parse hunk start line number, defaulting to 1"
                        );
                        1
                    }
                };
                let count: u32 = caps
                    .get(2)
                    .map(|m| match m.as_str().parse() {
                        Ok(v) => v,
                        Err(_) => {
                            tracing::warn!(
                                line = line,
                                file = file.as_str(),
                                "Could not parse hunk count, defaulting to 1"
                            );
                            1
                        }
                    })
                    .unwrap_or(1);
                hunks.push(DiffHunk {
                    file: file.clone(),
                    start,
                    count,
                });
            }
        }
    }

    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_unified_diff_basic() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,5 @@ fn main() {
     let x = 1;
+    let y = 2;
+    let z = 3;
";
        let hunks = parse_unified_diff(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file, "src/main.rs");
        assert_eq!(hunks[0].start, 10);
        assert_eq!(hunks[0].count, 5);
    }

    #[test]
    fn test_parse_unified_diff_new_file() {
        let diff = "\
diff --git a/src/new.rs b/src/new.rs
new file mode 100644
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,15 @@
+fn hello() {}
";
        let hunks = parse_unified_diff(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file, "src/new.rs");
        assert_eq!(hunks[0].start, 1);
        assert_eq!(hunks[0].count, 15);
    }

    #[test]
    fn test_parse_unified_diff_deleted_file() {
        let diff = "\
diff --git a/src/old.rs b/src/old.rs
deleted file mode 100644
--- a/src/old.rs
+++ /dev/null
@@ -1,10 +0,0 @@
-fn old() {}
";
        let hunks = parse_unified_diff(diff);
        assert!(hunks.is_empty(), "Deleted files should produce no hunks");
    }

    #[test]
    fn test_parse_unified_diff_binary() {
        let diff = "\
diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
";
        let hunks = parse_unified_diff(diff);
        assert!(hunks.is_empty(), "Binary files should be skipped");
    }

    #[test]
    fn test_parse_unified_diff_multiple_hunks() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,3 +10,4 @@ fn foo() {
     let x = 1;
+    let y = 2;
@@ -50,2 +51,3 @@ fn bar() {
     let a = 1;
+    let b = 2;
";
        let hunks = parse_unified_diff(diff);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].start, 10);
        assert_eq!(hunks[0].count, 4);
        assert_eq!(hunks[1].start, 51);
        assert_eq!(hunks[1].count, 3);
    }

    #[test]
    fn test_parse_unified_diff_count_omitted() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1 +1 @@
-old line
+new line
";
        let hunks = parse_unified_diff(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].count, 1, "Missing count should default to 1");
    }

    #[test]
    fn test_parse_unified_diff_empty_input() {
        let hunks = parse_unified_diff("");
        assert!(hunks.is_empty());
    }

    #[test]
    fn test_parse_unified_diff_rename() {
        let diff = "\
diff --git a/src/old_name.rs b/src/new_name.rs
similarity index 90%
rename from src/old_name.rs
rename to src/new_name.rs
--- a/src/old_name.rs
+++ b/src/new_name.rs
@@ -5,3 +5,4 @@ fn renamed() {
     let x = 1;
+    let y = 2;
";
        let hunks = parse_unified_diff(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(
            hunks[0].file, "src/new_name.rs",
            "Should use the new file name"
        );
    }
}
