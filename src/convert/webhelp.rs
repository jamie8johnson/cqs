//! Web help site to Markdown conversion.
//!
//! Web help sites (e.g., AuthorIT, MadCap Flare) are directories containing
//! multiple HTML pages under a `content/` subdirectory, plus assets (css, js,
//! fonts, images) that we skip.
//!
//! Detection: a directory containing a `content/` subdirectory with `.html` files.

use std::path::Path;

use anyhow::Result;

/// Check if a directory looks like a web help site.
///
/// Heuristic: has a `content/` subdirectory containing at least one `.html` file.
pub fn is_webhelp_dir(dir: &Path) -> bool {
    let content_dir = dir.join("content");
    if !content_dir.is_dir() {
        return false;
    }
    // Check for at least one HTML file anywhere under content/
    walkdir::WalkDir::new(&content_dir)
        .into_iter()
        .filter_map(|e| e.ok())
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
///
/// Walks `content/` for HTML files, converts each page, merges with separators.
/// Skips asset directories (css/, js/, fonts/, images/).
pub fn webhelp_to_markdown(dir: &Path) -> Result<String> {
    let _span = tracing::info_span!("webhelp_to_markdown", dir = %dir.display()).entered();

    let content_dir = dir.join("content");
    if !content_dir.is_dir() {
        anyhow::bail!(
            "Web help directory has no content/ subdirectory: {}",
            dir.display()
        );
    }

    // Collect all HTML pages under content/, sorted for consistent ordering
    let mut pages: Vec<_> = walkdir::WalkDir::new(&content_dir)
        .into_iter()
        .filter_map(|e| e.ok())
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

    tracing::info!(
        dir = %dir.display(),
        pages = pages.len(),
        "Found web help pages"
    );

    let mut all_markdown = Vec::new();

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
                all_markdown.push(md);
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

    if all_markdown.is_empty() {
        anyhow::bail!("Web help produced no content from {} pages", pages.len());
    }

    let merged = all_markdown.join("\n\n---\n\n");
    tracing::info!(
        dir = %dir.display(),
        pages = all_markdown.len(),
        bytes = merged.len(),
        "Web help converted"
    );
    Ok(merged)
}
