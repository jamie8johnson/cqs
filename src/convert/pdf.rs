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
        .args([&script, &path.to_string_lossy().to_string()])
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

/// Find a working Python interpreter.
///
/// Tries `python3` first, falls back to `python`.
fn find_python() -> Result<String> {
    for name in &["python3", "python"] {
        if std::process::Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return Ok(name.to_string());
        }
    }
    anyhow::bail!(
        "Python not found. Install `python3` (Linux: `sudo apt install python3`, macOS: `brew install python`)"
    )
}
