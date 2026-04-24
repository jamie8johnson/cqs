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
    // Audit P2 #37: `-snl` disables symbolic-link creation during extraction.
    // CHM is a CAB-based archive; a malicious file can embed a symlink whose
    // target is `../../escape`, and without `-snl` the extracted on-disk
    // entry IS the symlink — any subsequent file write through that path
    // escapes `temp_dir`. With `-snl`, 7z either skips the link or stores
    // it as a regular file containing the link target string.
    let output = std::process::Command::new(&sevenzip)
        .args(["x", "-snl", "--"])
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

    // Zip-slip containment: verify all extracted files are inside temp_dir.
    // See `verify_extraction_safety` for the per-entry rules (audit P2 #37).
    verify_extraction_safety(temp_dir.path())?;

    // P3 #106: shared cap honoring CQS_CONVERT_MAX_PAGES.
    let max_pages = crate::limits::doc_max_pages();

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

    if pages.len() > max_pages {
        tracing::warn!(
            path = %path.display(),
            total = pages.len(),
            limit = max_pages,
            "CHM page count exceeds limit, truncating; bump CQS_CONVERT_MAX_PAGES if needed"
        );
        pages.truncate(max_pages);
    }

    let mut merged = String::new();

    // RM-V1.29-5: cap per-page reads so a pathological archive with a single
    // huge "page" can't OOM the process. The outer archive-size check in
    // `convert/mod.rs` doesn't bound the per-file read.
    let max_page_bytes = crate::limits::convert_page_bytes();

    for entry in &pages {
        let bytes = {
            use std::io::Read;
            let file = match std::fs::File::open(entry.path()) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(
                        path = %entry.path().display(),
                        error = %e,
                        "Failed to open CHM page"
                    );
                    continue;
                }
            };
            let mut buf = Vec::new();
            if let Err(e) = file.take(max_page_bytes).read_to_end(&mut buf) {
                tracing::warn!(
                    path = %entry.path().display(),
                    error = %e,
                    "Failed to read CHM page"
                );
                continue;
            }
            // Warn once if we hit the cap exactly — the page may have been truncated.
            if buf.len() as u64 == max_page_bytes {
                tracing::warn!(
                    path = %entry.path().display(),
                    cap_bytes = max_page_bytes,
                    "CHM page hit per-page byte cap, content may be truncated; \
                     bump CQS_CONVERT_PAGE_BYTES if needed"
                );
            }
            buf
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

/// Walk every entry under `extract_root` and reject anything that escapes it.
///
/// Audit P2 #37 hardening:
///   * Any symbolic link in the extraction is fatal — a benign CHM/CAB
///     archive does not contain symbolic links.
///   * Any path that cannot be canonicalized (e.g. a dangling symlink, a
///     permission failure) is fatal — broken paths in extracted output are
///     themselves an attack signal, not a benign curiosity to skip.
///   * Any canonical path that does not start with the canonicalized
///     `extract_root` is fatal — classic zip-slip.
///
/// Extracted into a standalone helper so the bail behavior can be unit
/// tested without needing a real CHM archive on disk (building a malicious
/// CHM blob is impractical; the symlink-extraction code path is what we
/// actually care about and it does not depend on `7z`).
fn verify_extraction_safety(extract_root: &Path) -> Result<()> {
    let canonical_root = dunce::canonicalize(extract_root).with_context(|| {
        format!(
            "Failed to canonicalize extraction root: {}",
            extract_root.display()
        )
    })?;
    for entry in walkdir::WalkDir::new(extract_root)
        .into_iter()
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(error = %err, "Skipping entry during zip-slip check due to walkdir error");
                None
            }
        })
    {
        // A symlink in extracted output is itself an attack signal.
        if let Ok(md) = entry.path().symlink_metadata() {
            if md.file_type().is_symlink() {
                anyhow::bail!(
                    "CHM extraction produced symlink (rejected for security): {}",
                    entry.path().display()
                );
            }
        }
        match dunce::canonicalize(entry.path()) {
            Ok(canonical) => {
                if !canonical.starts_with(&canonical_root) {
                    anyhow::bail!(
                        "CHM archive contains path traversal: {}",
                        entry.path().display()
                    );
                }
            }
            Err(e) => {
                // Broken symlink / permission failure → attack signal, bail.
                anyhow::bail!(
                    "CHM extraction produced an entry that cannot be canonicalized \
                     (treating as attack signal): {} ({})",
                    entry.path().display(),
                    e
                );
            }
        }
    }
    Ok(())
}

/// Find a working `7z` executable.
/// Checks that the candidate actually executes successfully (exit code 0 or
/// recognizable help output). This prevents accidentally running an unrelated
/// binary that happens to share the name.
///
/// SEC-V1.25-10: Rejects any binary whose resolved location is in a
/// user-writable directory (e.g. /tmp, /var/tmp, or a world-writable dir).
/// This blocks PATH injection where an attacker drops `7z` in a writable
/// directory earlier in PATH.
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
        // Resolve the candidate to an absolute path so we can vet the parent directory.
        // If the name already is absolute (Windows ProgramFiles paths), we use it directly;
        // otherwise consult PATH.
        let resolved = std::path::PathBuf::from(name);
        let resolved = if resolved.is_absolute() {
            if resolved.is_file() {
                Some(resolved)
            } else {
                None
            }
        } else {
            super::resolve_on_path(name)
        };
        let Some(resolved) = resolved else { continue };
        if !super::is_safe_executable_path(&resolved) {
            tracing::warn!(
                candidate = %name,
                path = %resolved.display(),
                "Refusing 7z candidate in user-writable directory (SEC-V1.25-10)"
            );
            continue;
        }
        match std::process::Command::new(&resolved)
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) if status.success() || status.code() == Some(0) => {
                return Ok(resolved.to_string_lossy().to_string());
            }
            _ => continue,
        }
    }
    anyhow::bail!(
        "7z not found (or only available in a user-writable directory that cqs refuses to trust). \
         Install: `sudo apt install p7zip-full` (Linux), `brew install p7zip` (macOS), or 7-Zip (Windows)"
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

    /// Audit P2 #37: a symlink inside an "extracted" archive must cause
    /// `verify_extraction_safety` to bail with a clear error. Building a
    /// real malicious CHM is impractical (would need a fixture binary in
    /// CAB format); since #37 hardens the post-extraction walk, we test
    /// that walk directly against a manually-constructed extract dir.
    #[test]
    #[cfg(unix)]
    fn verify_extraction_safety_rejects_relative_symlink_to_escape() {
        let extract_dir = tempfile::tempdir().unwrap();
        let escape_link = extract_dir.path().join("inner.html");
        // Mirror the attack pattern an attacker would embed in a malicious CHM.
        std::os::unix::fs::symlink("../../escape", &escape_link).unwrap();

        let result = verify_extraction_safety(extract_dir.path());
        let err = result.expect_err("symlink in extraction must be rejected");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("symlink") || msg.contains("path traversal"),
            "error should explain the rejection (symlink/traversal); got: {msg}"
        );
    }

    /// Audit P2 #37: a broken symlink (target doesn't exist) must cause the
    /// walk to bail — the prior code logged a warning and silently skipped.
    /// A broken symlink in extracted archive output is itself an attack
    /// signal: a benign archive does not contain dangling references.
    #[test]
    #[cfg(unix)]
    fn verify_extraction_safety_bails_on_broken_symlink() {
        let extract_dir = tempfile::tempdir().unwrap();
        let broken = extract_dir.path().join("dangling.html");
        std::os::unix::fs::symlink("/this/path/does/not/exist/anywhere", &broken).unwrap();

        let result = verify_extraction_safety(extract_dir.path());
        let err = result.expect_err("broken symlink must be rejected, not skipped");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("symlink")
                || msg.contains("canonicalize")
                || msg.contains("attack signal"),
            "error should not be silent; got: {msg}"
        );
    }

    /// Plain extracted files inside the temp dir must not trigger the
    /// safety walk — guards against the symlink check accidentally
    /// rejecting legitimate output.
    #[test]
    fn verify_extraction_safety_accepts_plain_files() {
        let extract_dir = tempfile::tempdir().unwrap();
        std::fs::write(extract_dir.path().join("page1.html"), b"<html>ok</html>").unwrap();
        std::fs::create_dir(extract_dir.path().join("sub")).unwrap();
        std::fs::write(
            extract_dir.path().join("sub/page2.html"),
            b"<html>ok</html>",
        )
        .unwrap();

        verify_extraction_safety(extract_dir.path())
            .expect("plain file extraction must pass the safety walk");
    }
}
