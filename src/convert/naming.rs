//! Title extraction and collision-safe kebab-case filename generation.

use std::path::Path;

/// Extract the document title from Markdown content.
///
/// Priority:
/// 1. First `# ` (H1) heading
/// 2. First `## ` (H2) heading
/// 3. First non-empty, non-heading line
/// 4. Source filename stem as fallback
pub fn extract_title(markdown: &str, source_path: &Path) -> String {
    let fallback = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string();

    // Try H1 first
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("# ") {
            if !heading.starts_with('#') {
                let title = heading.trim().to_string();
                if !title.is_empty() {
                    return title;
                }
            }
        }
    }

    // Try H2
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            if !heading.starts_with('#') {
                let title = heading.trim().to_string();
                if !title.is_empty() {
                    return title;
                }
            }
        }
    }

    // Try first non-empty line
    for line in markdown.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            let candidate = trimmed.to_string();
            // Only use if it looks like a title (short enough)
            if candidate.len() <= 120 {
                return candidate;
            }
        }
    }

    fallback
}

/// Convert a title string to a kebab-case filename with `.md` extension.
///
/// - Lowercases everything
/// - Keeps alphanumeric characters, spaces, and hyphens
/// - Replaces parentheses content: `(v2024)` → `v2024`
/// - Collapses whitespace into single hyphens
/// - Strips leading/trailing hyphens
///
/// # Examples
///
/// ```
/// use cqs::convert::naming::title_to_filename;
/// assert_eq!(title_to_filename("AVEVA MES Client User Guide"), "aveva-mes-client-user-guide.md");
/// assert_eq!(title_to_filename("Historian Admin Guide (v2024)"), "historian-admin-guide-v2024.md");
/// ```
pub fn title_to_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .flat_map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![' ']
            }
        })
        .collect();

    let parts: Vec<&str> = cleaned.split_whitespace().collect();
    if parts.is_empty() {
        return "untitled.md".to_string();
    }

    let kebab = parts.join("-");
    // Strip leading/trailing hyphens that might result from punctuation-only words
    let kebab = kebab.trim_matches('-');
    format!("{}.md", kebab)
}

/// Resolve filename conflicts with multiple strategies.
///
/// 1. If no conflict, use as-is
/// 2. Append source filename stem as disambiguator
/// 3. Append numeric suffix (-2, -3, etc.)
pub fn resolve_conflict(filename: &str, source_path: &Path, output_dir: &Path) -> String {
    let path = output_dir.join(filename);
    if !path.exists() {
        return filename.to_string();
    }

    let stem = filename.trim_end_matches(".md");

    // Try disambiguating with source filename stem
    let source_stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_lowercase();
    let source_stem_clean: String = source_stem
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();

    if !source_stem_clean.is_empty() {
        let candidate = format!("{}-{}.md", stem, source_stem_clean);
        if !output_dir.join(&candidate).exists() {
            return candidate;
        }
    }

    // Numeric suffix fallback
    for i in 2..=100 {
        let candidate = format!("{}-{}.md", stem, i);
        if !output_dir.join(&candidate).exists() {
            return candidate;
        }
    }

    // Last resort: include random disambiguator
    format!("{}-{:08x}.md", stem, rand::random::<u32>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_title_to_filename_basic() {
        assert_eq!(
            title_to_filename("AVEVA MES Client User Guide"),
            "aveva-mes-client-user-guide.md"
        );
    }

    #[test]
    fn test_title_to_filename_with_parens() {
        assert_eq!(
            title_to_filename("Historian Admin Guide (v2024)"),
            "historian-admin-guide-v2024.md"
        );
    }

    #[test]
    fn test_title_to_filename_special_chars() {
        assert_eq!(
            title_to_filename("User's Guide: Installation & Setup"),
            "user-s-guide-installation-setup.md"
        );
    }

    #[test]
    fn test_title_to_filename_empty() {
        assert_eq!(title_to_filename(""), "untitled.md");
    }

    #[test]
    fn test_title_to_filename_already_kebab() {
        assert_eq!(
            title_to_filename("already-kebab-case"),
            "already-kebab-case.md"
        );
    }

    #[test]
    fn test_title_to_filename_unicode() {
        // Unicode chars are lowercased properly (not skipped by to_ascii_lowercase)
        assert_eq!(title_to_filename("Über Handbuch"), "über-handbuch.md");
        assert_eq!(title_to_filename("Ångström Guide"), "ångström-guide.md");
    }

    #[test]
    fn test_extract_title_h1() {
        let md = "# My Document\n\nSome content.";
        assert_eq!(extract_title(md, Path::new("doc.pdf")), "My Document");
    }

    #[test]
    fn test_extract_title_h2_fallback() {
        let md = "Some preamble\n## Getting Started\n\nContent.";
        assert_eq!(extract_title(md, Path::new("doc.pdf")), "Getting Started");
    }

    #[test]
    fn test_extract_title_filename_fallback() {
        let md = "";
        assert_eq!(
            extract_title(md, Path::new("HistorianAdmin.pdf")),
            "HistorianAdmin"
        );
    }

    #[test]
    fn test_extract_title_first_line_fallback() {
        let md = "AVEVA Historian Administration Guide\n\nMore content";
        assert_eq!(
            extract_title(md, Path::new("doc.pdf")),
            "AVEVA Historian Administration Guide"
        );
    }

    #[test]
    fn test_resolve_conflict_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_conflict("test.md", Path::new("doc.pdf"), dir.path());
        assert_eq!(result, "test.md");
    }

    #[test]
    fn test_resolve_conflict_with_collision() {
        let dir = tempfile::tempdir().unwrap();
        // Create existing file
        std::fs::write(dir.path().join("test.md"), "existing").unwrap();

        let result = resolve_conflict("test.md", Path::new("MyDoc.pdf"), dir.path());
        assert_eq!(result, "test-mydoc.md");
    }

    #[test]
    fn test_resolve_conflict_numeric_fallback() {
        let dir = tempfile::tempdir().unwrap();
        // Create both the base and source-disambiguated files
        std::fs::write(dir.path().join("test.md"), "existing").unwrap();
        std::fs::write(dir.path().join("test-mydoc.md"), "existing").unwrap();

        let result = resolve_conflict("test.md", Path::new("MyDoc.pdf"), dir.path());
        assert_eq!(result, "test-2.md");
    }
}
