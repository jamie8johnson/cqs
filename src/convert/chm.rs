//! CHM to Markdown conversion via `7z` extraction + HTML conversion.
//!
//! CHM (Compiled HTML Help) files are Microsoft archives containing HTML pages.
//! We extract with `7z`, convert each HTML page, and merge into a single Markdown document.

use std::path::Path;

use anyhow::{Context, Result};

/// Convert a CHM file to Markdown.
/// 1. Extracts the CHM archive to a temp directory using `7z`
/// 2. Finds all HTML/HTM files in the extracted content
/// 3. Converts each page to Markdown
/// 4. Merges all pages with `---` separators
/// Requires `7z` (p7zip-full / brew install p7zip) to be installed.
/// ## Security
/// After extraction, all file paths are verified to be inside the temp directory
/// (zip-slip containment). Symlinks in extracted content are skipped.
pub fn chm_to_markdown(path: &Path) -> Result<String> {
    let _span = tracing::info_span!("chm_to_markdown", path = %path.display()).entered();

    let sevenzip = find_7z()?;
    let temp_dir = tempfile::tempdir()?;

    let mut output_arg = std::ffi::OsString::from("-o");
    output_arg.push(temp_dir.path());
    let output = std::process::Command::new(&sevenzip)
        .args(["x", "--"])
        .arg(path)
        .arg(&output_arg)
        .arg("-y")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "Failed to run `{}` for CHM extraction. \
                 Install: `sudo apt install p7zip-full` (Linux) or `brew install p7zip` (macOS)",
                sevenzip
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(path = %path.display(), stderr = %stderr, "7z extraction failed");
        anyhow::bail!(
            "7z extraction failed for {}: {}",
            path.display(),
            stderr.trim()
        );
    }

    // Zip-slip containment: verify all extracted files are inside temp_dir
    let canonical_temp = dunce::canonicalize(temp_dir.path()).with_context(|| {
        format!(
            "Failed to canonicalize temp dir: {}",
            temp_dir.path().display()
        )
    })?;
    for entry in walkdir::WalkDir::new(temp_dir.path())
        .into_iter()
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(error = %err, "Skipping entry during zip-slip check due to walkdir error");
                None
            }
        })
    {
        match dunce::canonicalize(entry.path()) {
            Ok(canonical) => {
                if !canonical.starts_with(&canonical_temp) {
                    anyhow::bail!(
                        "CHM archive contains path traversal: {}",
                        entry.path().display()
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %entry.path().display(),
                    error = %e,
                    "Cannot canonicalize extracted path, skipping"
                );
            }
        }
    }

    // Maximum number of pages to process from a single CHM archive.
    const MAX_PAGES: usize = 1000;

    // Collect all HTML pages, sorted by name for consistent ordering.
    // Skip symlinks (SEC-9) to prevent symlink escape attacks.
    let mut pages: Vec<_> = walkdir::WalkDir::new(temp_dir.path())
        .into_iter()
        .filter_entry(|e| !e.path_is_symlink())
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(error = %err, "Skipping CHM page due to walkdir error");
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
        tracing::warn!(path = %path.display(), "CHM contained no HTML files");
        anyhow::bail!("CHM archive contained no HTML files");
    }

    if pages.len() > MAX_PAGES {
        tracing::warn!(
            path = %path.display(),
            total = pages.len(),
            limit = MAX_PAGES,
            "CHM page count exceeds limit, truncating"
        );
        pages.truncate(MAX_PAGES);
    }

    let mut merged = String::new();

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
                if !merged.is_empty() {
                    merged.push_str("\n\n---\n\n");
                }
                merged.push_str(&md);
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

    if merged.is_empty() {
        tracing::warn!(path = %path.display(), pages = pages.len(), "CHM produced no content from any page");
        anyhow::bail!("CHM produced no content");
    }
    tracing::info!(
        path = %path.display(),
        pages = pages.len(),
        bytes = merged.len(),
        "CHM converted"
    );
    Ok(merged)
}

/// Find a working `7z` executable.
/// Checks that the candidate actually executes successfully (exit code 0 or
/// recognizable help output). This prevents accidentally running an unrelated
/// binary that happens to share the name.
fn find_7z() -> Result<String> {
    // Check common names first, then env-based Windows install paths
    let mut candidates: Vec<String> =
        vec!["7z".to_string(), "7za".to_string(), "p7zip".to_string()];
    // Check env-based Windows paths (handles non-standard install dirs)
    if let Ok(pf) = std::env::var("ProgramFiles") {
        candidates.push(format!(r"{}\7-Zip\7z.exe", pf));
    }
    if let Ok(pf) = std::env::var("ProgramFiles(x86)") {
        candidates.push(format!(r"{}\7-Zip\7z.exe", pf));
    }
    for name in &candidates {
        match std::process::Command::new(name)
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) if status.success() || status.code() == Some(0) => {
                return Ok(name.to_string());
            }
            _ => continue,
        }
    }
    anyhow::bail!(
        "7z not found. Install: `sudo apt install p7zip-full` (Linux), `brew install p7zip` (macOS), or 7-Zip (Windows)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chm_to_markdown_nonexistent_file_returns_error() {
        let path = std::path::Path::new("/nonexistent/path/does_not_exist.chm");
        let result = chm_to_markdown(path);
        assert!(
            result.is_err(),
            "chm_to_markdown should return an error for a nonexistent file"
        );
    }
}
