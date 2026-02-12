//! HTML to Markdown conversion using `fast_html2md`.

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
