//! CHM to Markdown conversion via `7z` extraction + HTML conversion.
//!
//! CHM (Compiled HTML Help) files are Microsoft archives containing HTML pages.
//! We extract with `7z`, convert each HTML page, and merge into a single Markdown document.

use std::path::Path;

use anyhow::Result;

/// Convert a CHM file to Markdown.
///
/// 1. Extracts the CHM archive to a temp directory using `7z`
/// 2. Finds all HTML/HTM files in the extracted content
/// 3. Converts each page to Markdown
/// 4. Merges all pages with `---` separators
///
/// Requires `7z` (p7zip-full) to be installed.
pub fn chm_to_markdown(path: &Path) -> Result<String> {
    let _span = tracing::info_span!("chm_to_markdown", path = %path.display()).entered();

    let sevenzip = find_7z()?;
    let temp_dir = tempfile::tempdir()?;

    let path_str = path.to_string_lossy();
    let output_arg = format!("-o{}", temp_dir.path().display());
    let output = std::process::Command::new(&sevenzip)
        .args(["x", path_str.as_ref(), output_arg.as_str(), "-y"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(path = %path.display(), stderr = %stderr, "7z extraction failed");
        anyhow::bail!(
            "7z extraction failed for {}: {}",
            path.display(),
            stderr.trim()
        );
    }

    // Collect all HTML pages, sorted by name for consistent ordering
    let mut pages: Vec<_> = walkdir::WalkDir::new(temp_dir.path())
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
        tracing::warn!(path = %path.display(), "CHM contained no HTML files");
        anyhow::bail!("CHM archive contained no HTML files");
    }

    let mut all_markdown = Vec::new();

    for entry in &pages {
        let bytes = match std::fs::read(entry.path()) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    path = %entry.path().display(),
                    error = %e,
                    "Failed to read CHM page"
                );
                continue;
            }
        };
        // Lossy UTF-8 for old Windows-1252 encoded files
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
                    "Skipping empty CHM page"
                );
            }
        }
    }

    if all_markdown.is_empty() {
        tracing::warn!(path = %path.display(), pages = pages.len(), "CHM produced no content from any page");
        anyhow::bail!("CHM produced no content");
    }

    // Merge all pages with separator
    let merged = all_markdown.join("\n\n---\n\n");
    tracing::info!(
        path = %path.display(),
        pages = all_markdown.len(),
        bytes = merged.len(),
        "CHM converted"
    );
    Ok(merged)
}

/// Find a working `7z` executable.
fn find_7z() -> Result<String> {
    for name in &["7z", "7za", "p7zip"] {
        if std::process::Command::new(name)
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return Ok(name.to_string());
        }
    }
    anyhow::bail!("7z not found. Install: sudo apt install p7zip-full")
}
