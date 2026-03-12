//! PDF to Markdown conversion via Python `pymupdf4llm`.
//!
//! Shells out to `scripts/pdf_to_md.py` which uses the `pymupdf4llm` library
//! for high-quality PDF conversion preserving layout, tables, and headings.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Convert a PDF file to Markdown by shelling out to the Python converter.
///
/// Looks for `scripts/pdf_to_md.py` relative to CWD, or via `CQS_PDF_SCRIPT` env var.
/// Requires `python3` and `pip install pymupdf4llm`.
pub fn pdf_to_markdown(path: &Path) -> Result<String> {
    let _span = tracing::info_span!("pdf_to_markdown", path = %path.display()).entered();

    let script = find_pdf_script()?;

    let python = find_python()?;

    let output = std::process::Command::new(&python)
        .arg("--")
        .arg(&script)
        .arg(path)
        .output()
        .with_context(|| format!("Failed to run `{}`. Is Python installed?", python))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("pymupdf4llm not installed") {
            tracing::warn!("pymupdf4llm not installed");
            anyhow::bail!("pymupdf4llm not installed. Run: pip install pymupdf4llm");
        }
        tracing::warn!(stderr = %stderr, "PDF conversion failed");
        anyhow::bail!("PDF conversion failed: {}", stderr.trim());
    }

    let markdown =
        String::from_utf8(output.stdout).context("PDF converter produced non-UTF-8 output")?;

    if markdown.trim().is_empty() {
        tracing::warn!(path = %path.display(), "PDF produced no text (possibly image-only)");
        anyhow::bail!("PDF produced no text output");
    }

    tracing::info!(path = %path.display(), bytes = markdown.len(), "PDF text extracted");
    Ok(markdown)
}

/// Locate the PDF conversion script.
///
/// Search order:
/// 1. `CQS_PDF_SCRIPT` environment variable
/// 2. `scripts/pdf_to_md.py` relative to CWD
/// 3. Relative to the cqs binary location
fn find_pdf_script() -> Result<String> {
    // Check env var first
    if let Ok(script) = std::env::var("CQS_PDF_SCRIPT") {
        tracing::warn!(script = %script, "Using custom PDF script from CQS_PDF_SCRIPT env var");
        let p = PathBuf::from(&script);
        if p.extension().is_none_or(|e| e != "py") {
            tracing::warn!(
                script = %script,
                "CQS_PDF_SCRIPT does not have .py extension — ensure this is intentional"
            );
        }
        if p.exists() {
            return Ok(script);
        }
        tracing::warn!(path = %script, "CQS_PDF_SCRIPT set but file not found");
    }

    let mut candidates = vec![PathBuf::from("scripts/pdf_to_md.py")];
    if let Some(exe_relative) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("../scripts/pdf_to_md.py")))
    {
        candidates.push(exe_relative);
    }

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.to_string_lossy().to_string());
        }
    }

    anyhow::bail!(
        "scripts/pdf_to_md.py not found. \
         Run cqs convert from the project root, or set CQS_PDF_SCRIPT env var."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: remove CQS_PDF_SCRIPT from environment and restore on drop.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            EnvGuard { key, prev }
        }

        fn unset(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            EnvGuard { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// CQS_PDF_SCRIPT pointing to an existing .py file → returned immediately.
    #[test]
    #[serial_test::serial]
    fn test_find_pdf_script_env_var_existing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("my_script.py");
        fs::write(&script, "# placeholder").unwrap();

        let _guard = EnvGuard::set("CQS_PDF_SCRIPT", script.to_str().unwrap());

        let result = find_pdf_script();
        assert!(
            result.is_ok(),
            "should succeed when env var points to existing file"
        );
        assert_eq!(result.unwrap(), script.to_str().unwrap());
    }

    /// CQS_PDF_SCRIPT set but file does not exist → falls through to candidate scan,
    /// which also finds nothing → error.
    #[test]
    #[serial_test::serial]
    fn test_find_pdf_script_env_var_missing_file_falls_through() {
        let dir = tempfile::TempDir::new().unwrap();
        let ghost = dir.path().join("does_not_exist.py");
        // Ensure the file definitely doesn't exist
        assert!(!ghost.exists());

        let _guard = EnvGuard::set("CQS_PDF_SCRIPT", ghost.to_str().unwrap());

        // CWD is unlikely to have scripts/pdf_to_md.py in a temp context,
        // and the binary-relative path also won't have it. Expect failure.
        let result = find_pdf_script();
        // May succeed if the real scripts/pdf_to_md.py happens to exist relative to CWD,
        // so we only assert the env-var path itself is NOT the returned value.
        if let Ok(found) = &result {
            assert_ne!(
                found,
                ghost.to_str().unwrap(),
                "env-var ghost path must not be returned"
            );
        }
        // Not asserting Err here because it depends on CWD having scripts/pdf_to_md.py.
    }

    /// CQS_PDF_SCRIPT not set and scripts/pdf_to_md.py exists relative to CWD → found.
    #[test]
    #[serial_test::serial]
    fn test_find_pdf_script_cwd_relative_path() {
        let dir = tempfile::TempDir::new().unwrap();
        // Create scripts/pdf_to_md.py inside the temp dir
        let scripts_dir = dir.path().join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        fs::write(scripts_dir.join("pdf_to_md.py"), "# placeholder").unwrap();

        let prev_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let _guard = EnvGuard::unset("CQS_PDF_SCRIPT");

        let result = find_pdf_script();

        // Restore CWD before asserting (so cleanup doesn't interfere)
        std::env::set_current_dir(&prev_dir).unwrap();

        assert!(
            result.is_ok(),
            "should find scripts/pdf_to_md.py relative to CWD"
        );
        let found = result.unwrap();
        assert!(
            found.contains("pdf_to_md.py"),
            "returned path should contain pdf_to_md.py, got: {}",
            found
        );
    }

    /// No env var, no scripts/pdf_to_md.py in CWD, no binary-relative path → error.
    #[test]
    #[serial_test::serial]
    fn test_find_pdf_script_not_found_returns_error() {
        let empty_dir = tempfile::TempDir::new().unwrap();
        let prev_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(empty_dir.path()).unwrap();

        let _guard = EnvGuard::unset("CQS_PDF_SCRIPT");

        let result = find_pdf_script();

        std::env::set_current_dir(&prev_dir).unwrap();

        // May succeed if the binary is run from a dir containing scripts/pdf_to_md.py.
        // We assert the error message is informative if it fails.
        if let Err(e) = result {
            let msg = e.to_string();
            assert!(
                msg.contains("pdf_to_md.py") || msg.contains("CQS_PDF_SCRIPT"),
                "error message should mention the script name or env var, got: {}",
                msg
            );
        }
    }

    /// CQS_PDF_SCRIPT with a non-.py extension produces a warning but still succeeds if file exists.
    #[test]
    #[serial_test::serial]
    fn test_find_pdf_script_env_var_non_py_extension_accepted_if_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("converter.sh");
        fs::write(&script, "#!/bin/sh\necho hello").unwrap();

        let _guard = EnvGuard::set("CQS_PDF_SCRIPT", script.to_str().unwrap());

        let result = find_pdf_script();
        assert!(
            result.is_ok(),
            "non-.py extension with existing file should still succeed (with a warning)"
        );
    }
}

/// Find a working Python interpreter.
///
/// Tries `python3` first, falls back to `python`. Validates that the candidate
/// exits cleanly with `--version` to avoid running unrelated binaries.
fn find_python() -> Result<String> {
    for name in &["python3", "python"] {
        match std::process::Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {
                return Ok(name.to_string());
            }
            _ => continue,
        }
    }
    anyhow::bail!(
        "Python not found. Install `python3` (Linux: `sudo apt install python3`, macOS: `brew install python`)"
    )
}
