//! HTML to Markdown conversion using `fast_html2md`.

use std::path::Path;

use anyhow::Result;

/// Convert HTML source to Markdown.
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

/// Converts an HTML file to Markdown format.
///
/// Reads the HTML file from the specified path and converts its contents to Markdown. The file size must not exceed the configured maximum limit.
/// This is the path-based wrapper used by `FORMAT_TABLE`; the string-based
/// [`html_to_markdown`] is still used directly by `chm` and `webhelp`.
///
/// SHL-V1.29-10: the size cap used to be a local `MAX_CONVERT_FILE_SIZE = 100 MB`
/// duplicated in `convert::markdown_passthrough`. Both now route through
/// `crate::limits::convert_file_size()` so `CQS_CONVERT_MAX_FILE_SIZE`
/// tunes both single-file converters in lockstep.
///
/// # Arguments
/// * `path` - Path to the HTML file to convert
/// # Returns
/// Returns a `Result` containing the converted Markdown string, or an error if the file cannot be read or converted.
/// # Errors
/// Returns an error if:
/// * The file cannot be accessed or its metadata cannot be retrieved
/// * The file exceeds the maximum allowed file size
/// * The file cannot be read as UTF-8 text
/// * The HTML to Markdown conversion fails
pub fn html_file_to_markdown(path: &Path) -> Result<String> {
    let _span = tracing::info_span!("html_file_to_markdown", path = %path.display()).entered();
    let max_bytes = crate::limits::convert_file_size();
    let meta = std::fs::metadata(path)
        .map_err(|e| anyhow::anyhow!("Failed to stat {}: {}", path.display(), e))?;
    if meta.len() > max_bytes {
        anyhow::bail!(
            "File {} exceeds {} MB size limit",
            path.display(),
            max_bytes / 1024 / 1024,
        );
    }
    let source = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;
    html_to_markdown(&source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_to_markdown_basic_paragraph() {
        let html = "<p>Hello, world!</p>";
        let result = html_to_markdown(html).expect("should convert simple paragraph");
        assert!(
            result.contains("Hello, world!"),
            "converted markdown should contain the paragraph text"
        );
    }

    #[test]
    fn test_html_to_markdown_heading() {
        let html = "<h1>My Heading</h1><p>Some text.</p>";
        let result = html_to_markdown(html).expect("should convert heading and paragraph");
        assert!(
            result.contains("My Heading"),
            "converted markdown should contain the heading text"
        );
        assert!(
            result.contains("Some text."),
            "converted markdown should contain the paragraph text"
        );
    }

    #[test]
    fn test_html_to_markdown_empty_returns_error() {
        // Completely empty / whitespace-only HTML produces no content.
        let result = html_to_markdown("   ");
        assert!(result.is_err(), "empty HTML should return an error");
    }
}
