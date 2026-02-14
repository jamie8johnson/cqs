//! HTML to Markdown conversion using `fast_html2md`.

use std::path::Path;

use anyhow::Result;

/// Convert HTML source to Markdown.
///
/// Uses `fast_html2md::rewrite_html` for the heavy lifting.
/// Returns an error if the conversion produces no content.
pub fn html_to_markdown(source: &str) -> Result<String> {
    let _span = tracing::info_span!("html_to_markdown").entered();

    let markdown = html2md::rewrite_html(source, false);

    if markdown.trim().is_empty() {
        tracing::warn!("HTML conversion produced empty output");
        anyhow::bail!("HTML conversion produced no content");
    }

    tracing::info!(bytes = markdown.len(), "HTML converted to markdown");
    Ok(markdown)
}

/// Convert an HTML file to Markdown.
///
/// Reads the file at `path` and converts its HTML content to Markdown.
/// This is the path-based wrapper used by `FORMAT_TABLE`; the string-based
/// [`html_to_markdown`] is still used directly by `chm` and `webhelp`.
pub fn html_file_to_markdown(path: &Path) -> Result<String> {
    let _span = tracing::info_span!("html_file_to_markdown", path = %path.display()).entered();
    let source = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;
    html_to_markdown(&source)
}
