//! Web help site to Markdown conversion.
//!
//! Web help sites (e.g., AuthorIT, MadCap Flare) are directories containing
//! multiple HTML pages under a `content/` subdirectory, plus assets (css, js,
//! fonts, images) that we skip.
//!
//! Detection: a directory containing a `content/` subdirectory with `.html` files.

use std::path::Path;

use anyhow::Result;

/// Default content subdirectory for WebHelp sites.
const WEBHELP_CONTENT_DIR: &str = "content";

/// Check if a directory looks like a web help site.
/// Heuristic: has a `content/` subdirectory containing at least one `.html` file.
pub fn is_webhelp_dir(dir: &Path) -> bool {
    // Reject symlinks to prevent traversal outside trusted directories
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return false;
    }
    let content_dir = dir.join(WEBHELP_CONTENT_DIR);
    if !content_dir.is_dir() {
        return false;
    }
    // Check for at least one HTML file anywhere under content/
    walkdir::WalkDir::new(&content_dir)
        .into_iter()
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(error = %err, "Skipping entry during webhelp detection due to walkdir error");
                None
            }
        })
        .any(|e| {
            e.file_type().is_file()
                && e.path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
                    .unwrap_or(false)
        })
}

/// Convert a web help site directory to a single merged Markdown document.
/// Walks `content/` for HTML files, converts each page, merges with separators.
/// Skips asset directories (css/, js/, fonts/, images/).
pub fn webhelp_to_markdown(dir: &Path) -> Result<String> {
    let _span = tracing::info_span!("webhelp_to_markdown", dir = %dir.display()).entered();

    let content_dir = dir.join(WEBHELP_CONTENT_DIR);
    // SEC-29: Reject symlinked content_dir to prevent traversal outside trusted directories.
    // is_webhelp_dir already checks `dir` itself, but content_dir could be a symlink too.
    if content_dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        anyhow::bail!(
            "Web help content/ directory is a symlink (rejected for security): {}",
            content_dir.display()
        );
    }
    if !content_dir.is_dir() {
        anyhow::bail!(
            "Web help directory has no content/ subdirectory: {}",
            dir.display()
        );
    }

    // Maximum number of pages to process from a single web help site.
    const MAX_PAGES: usize = 1000;

    // Collect all HTML pages under content/, sorted for consistent ordering.
    // Skip symlinks (SEC-11) to prevent symlink traversal attacks.
    let mut pages: Vec<_> = walkdir::WalkDir::new(&content_dir)
        .into_iter()
        .filter_entry(|e| !e.path_is_symlink())
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(error = %err, "Skipping web help page due to walkdir error");
                None
            }
        })
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
                .unwrap_or(false)
        })
        .collect();
    pages.sort_by_key(|e| e.path().to_path_buf());

    if pages.is_empty() {
        anyhow::bail!("No HTML files found in {}", content_dir.display());
    }

    if pages.len() > MAX_PAGES {
        tracing::warn!(
            dir = %dir.display(),
            total = pages.len(),
            limit = MAX_PAGES,
            "Web help page count exceeds limit, truncating"
        );
        pages.truncate(MAX_PAGES);
    }

    tracing::info!(
        dir = %dir.display(),
        pages = pages.len(),
        "Found web help pages"
    );

    let mut merged = String::new();
    let mut page_count = 0usize;
    const MAX_WEBHELP_BYTES: usize = 50 * 1024 * 1024; // 50MB

    for entry in &pages {
        let bytes = match std::fs::read(entry.path()) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    path = %entry.path().display(),
                    error = %e,
                    "Failed to read web help page"
                );
                continue;
            }
        };
        let html = String::from_utf8_lossy(&bytes);

        match super::html::html_to_markdown(&html) {
            Ok(md) if !md.trim().is_empty() => {
                if !merged.is_empty() {
                    merged.push_str("\n\n---\n\n");
                }
                merged.push_str(&md);
                page_count += 1;
                // RM-3: Guard against unbounded concatenation
                if merged.len() > MAX_WEBHELP_BYTES {
                    tracing::warn!(
                        bytes = merged.len(),
                        pages = page_count,
                        "Webhelp output exceeds 50MB limit, truncating"
                    );
                    break;
                }
            }
            Ok(_) => {} // skip empty pages
            Err(e) => {
                tracing::debug!(
                    path = %entry.path().display(),
                    error = %e,
                    "Skipping empty web help page"
                );
            }
        }
    }

    if merged.is_empty() {
        anyhow::bail!("Web help produced no content from {} pages", pages.len());
    }

    tracing::info!(
        dir = %dir.display(),
        pages = page_count,
        bytes = merged.len(),
        "Web help converted"
    );
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_webhelp_dir_returns_false_for_empty_dir() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        assert!(
            !is_webhelp_dir(dir.path()),
            "empty directory should not be detected as a webhelp dir"
        );
    }

    #[test]
    fn test_is_webhelp_dir_returns_false_for_dir_without_html() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        // Create a content/ subdir but put no HTML files in it.
        let content = dir.path().join("content");
        std::fs::create_dir(&content).expect("should create content dir");
        std::fs::write(content.join("readme.txt"), "not html").expect("should write file");
        assert!(
            !is_webhelp_dir(dir.path()),
            "directory with content/ but no HTML files should not be detected as webhelp"
        );
    }

    #[test]
    fn test_is_webhelp_dir_returns_true_for_webhelp_layout() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let content = dir.path().join("content");
        std::fs::create_dir(&content).expect("should create content dir");
        std::fs::write(content.join("index.html"), "<p>hello</p>").expect("should write html");
        assert!(
            is_webhelp_dir(dir.path()),
            "directory with content/*.html should be detected as webhelp"
        );
    }
}
