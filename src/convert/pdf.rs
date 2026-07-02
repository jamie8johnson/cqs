//! PDF to Markdown conversion via Python `pymupdf4llm`.
//!
//! Shells out to a Python converter that uses the `pymupdf4llm` library for
//! high-quality PDF conversion preserving layout, tables, and headings. The
//! script is embedded into the binary at compile time (`include_str!`) and
//! materialized to a temp file at exec time, so there is no CWD / binary-relative
//! filesystem search — that search was a TOCTOU symlink-swap surface and made the
//! executed script depend on ambient filesystem layout. The `CQS_PDF_SCRIPT` env
//! var remains a deliberate operator escape hatch.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The PDF converter script, compiled into the binary. Materialized to a temp
/// file at exec time — see [`resolve_pdf_script`]. Kept in sync with the on-disk
/// `scripts/pdf_to_md.py` (the same file this embeds).
const PDF_SCRIPT: &str = include_str!("../../scripts/pdf_to_md.py");

/// A resolved PDF converter script, ready to exec.
///
/// - `Embedded` owns the [`tempfile::TempPath`] the compiled-in script was
///   written to; it MUST outlive the child process (dropping it deletes the
///   file). This is the default — no filesystem search.
/// - `Override` holds an operator-supplied on-disk path from `CQS_PDF_SCRIPT`,
///   already validated by the extension check and (on unix) [`validate_script_safety`].
#[derive(Debug)]
enum ResolvedPdfScript {
    Embedded(tempfile::TempPath),
    Override(PathBuf),
}

impl ResolvedPdfScript {
    /// The filesystem path to hand the Python interpreter.
    fn path(&self) -> &Path {
        match self {
            ResolvedPdfScript::Embedded(p) => p.as_ref(),
            ResolvedPdfScript::Override(p) => p.as_path(),
        }
    }
}

/// Convert a PDF file to Markdown by shelling out to the Python converter.
/// Uses the embedded `pdf_to_md.py` (materialized to a temp file) by default, or
/// the operator-supplied script named by `CQS_PDF_SCRIPT`. Requires `python3` and
/// `pip install pymupdf4llm`.
pub fn pdf_to_markdown(path: &Path) -> Result<String> {
    let _span = tracing::info_span!("pdf_to_markdown", path = %path.display()).entered();

    // Resolve the converter script. `script` owns the embedded temp file (when
    // that variant applies), so it must stay in scope until the child exits.
    let script = resolve_pdf_script()?;

    let python = find_python()?;

    // Bound PDF stdout. Default 100 MiB cap, env-overridable via
    // CQS_PDF_MAX_BYTES. A pathological / hostile PDF can produce arbitrary
    // stdout that .output() would buffer unbounded. Mirrors the spawn+take
    // pattern in train_data::git_diff_tree.
    use std::io::Read;
    use std::process::Stdio;
    let max_bytes: usize = std::env::var("CQS_PDF_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100 * 1024 * 1024);
    let mut child = std::process::Command::new(&python)
        .arg("--")
        .arg(script.path())
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run `{}`. Is Python installed?", python))?;
    let mut stdout_buf = Vec::with_capacity(64 * 1024);
    let mut stderr_buf = Vec::with_capacity(8 * 1024);
    if let Some(s) = child.stdout.take() {
        let _ = s.take((max_bytes as u64) + 1).read_to_end(&mut stdout_buf);
    }
    if let Some(s) = child.stderr.take() {
        let _ = s
            .take(1024 * 1024) // 1 MiB cap on stderr
            .read_to_end(&mut stderr_buf);
    }
    let status = child.wait().context("Failed to await PDF converter")?;
    if stdout_buf.len() > max_bytes {
        anyhow::bail!(
            "PDF converter stdout exceeded CQS_PDF_MAX_BYTES ({} bytes)",
            max_bytes
        );
    }

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_buf);
        if stderr.contains("pymupdf4llm not installed") {
            tracing::warn!("pymupdf4llm not installed");
            anyhow::bail!("pymupdf4llm not installed. Run: pip install pymupdf4llm");
        }
        tracing::warn!(stderr = %stderr, "PDF conversion failed");
        anyhow::bail!("PDF conversion failed: {}", stderr.trim());
    }

    let markdown =
        String::from_utf8(stdout_buf).context("PDF converter produced non-UTF-8 output")?;

    if markdown.trim().is_empty() {
        tracing::warn!(path = %path.display(), "PDF produced no text (possibly image-only)");
        anyhow::bail!("PDF produced no text output");
    }

    tracing::info!(path = %path.display(), bytes = markdown.len(), "PDF text extracted");
    Ok(markdown)
}

/// Resolve the PDF converter script to exec.
///
/// Default: the embedded [`PDF_SCRIPT`], materialized to a temp file. There is
/// no CWD / binary-relative filesystem search, so there is no TOCTOU symlink-swap
/// window between validate and exec and no dependence on ambient filesystem
/// layout.
///
/// Operator escape hatch: `CQS_PDF_SCRIPT`. The named path is rejected unless it
/// has a `.py` extension and (on unix) is owned by the current effective user and
/// is not group/world writable, so a daemon inheriting a stale env pointing at
/// `/tmp/evil.py` bails rather than executing attacker code. If the env var names
/// a file that does not exist, the resolver falls back to the trusted embedded
/// script rather than failing the conversion.
fn resolve_pdf_script() -> Result<ResolvedPdfScript> {
    if let Ok(script) = std::env::var("CQS_PDF_SCRIPT") {
        tracing::warn!(script = %script, "Using custom PDF script from CQS_PDF_SCRIPT env var");
        let p = PathBuf::from(&script);
        // Extension-only check. This prevents trivial misuse (e.g., running
        // a .sh or .exe) but does NOT prevent a malicious .py script placed
        // via .envrc or shell profile in a cloned repository. See SECURITY.md
        // for the full threat model of CQS_PDF_SCRIPT.
        if p.extension().is_none_or(|e| e != "py") {
            anyhow::bail!("CQS_PDF_SCRIPT must have .py extension (got: {}).", script);
        }
        if p.exists() {
            // Enforce ownership + permissions on unix before trusting.
            #[cfg(unix)]
            validate_script_safety(&p)
                .with_context(|| format!("CQS_PDF_SCRIPT safety check failed for {}", script))?;
            return Ok(ResolvedPdfScript::Override(p));
        }
        tracing::warn!(path = %script, "CQS_PDF_SCRIPT set but file not found; using embedded script");
    }

    // Default: materialize the embedded script to a temp file and exec THAT.
    // The TempPath is carried inside ResolvedPdfScript so the file survives until
    // the child process has read it (dropping it deletes the file).
    let mut script_file = tempfile::NamedTempFile::new()
        .context("failed to create temp file for embedded PDF script")?;
    script_file
        .write_all(PDF_SCRIPT.as_bytes())
        .context("failed to write embedded PDF script to temp file")?;
    script_file
        .flush()
        .context("failed to flush embedded PDF script temp file")?;
    Ok(ResolvedPdfScript::Embedded(script_file.into_temp_path()))
}

/// Validate that the script is owned by the current effective user and is not
/// group/world writable. A daemon inheriting a stale env pointing at
/// `/tmp/evil.py` must not execute attacker code.
#[cfg(unix)]
fn validate_script_safety(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(path).with_context(|| format!("metadata for {}", path.display()))?;
    // SAFETY: geteuid() is always safe — no state, no args, no errors.
    let effective_uid = unsafe { libc::geteuid() };
    if md.uid() != effective_uid {
        anyhow::bail!(
            "CQS_PDF_SCRIPT must be owned by current user (script uid={}, euid={})",
            md.uid(),
            effective_uid
        );
    }
    let mode = md.mode();
    if mode & 0o022 != 0 {
        anyhow::bail!(
            "CQS_PDF_SCRIPT must not be group- or world-writable (mode={:o})",
            mode & 0o777
        );
    }
    Ok(())
}

/// Find a working Python interpreter (delegates to shared `convert::find_python`).
fn find_python() -> Result<String> {
    super::find_python()
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
        /// Set an environment variable, returning a guard that restores the
        /// previous value on drop.
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            EnvGuard { key, prev }
        }

        /// Remove an environment variable, returning a guard that restores
        /// the previous value on drop.
        fn unset(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            EnvGuard { key, prev }
        }
    }

    impl Drop for EnvGuard {
        /// Restore the variable to its previous value, or remove it if it
        /// was unset before the guard was created.
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// With no `CQS_PDF_SCRIPT` and no `scripts/pdf_to_md.py` anywhere on the
    /// filesystem (empty CWD), resolution must succeed via the embedded script —
    /// proving the old CWD / binary-relative search is gone. The materialized
    /// temp file must carry the exact compiled-in content.
    #[test]
    #[serial_test::serial]
    fn resolve_pdf_script_uses_embedded_without_filesystem_script() {
        let empty_dir = tempfile::TempDir::new().unwrap();
        let prev_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(empty_dir.path()).unwrap();

        let _guard = EnvGuard::unset("CQS_PDF_SCRIPT");

        let result = resolve_pdf_script();

        // Restore CWD before asserting (so cleanup doesn't interfere).
        std::env::set_current_dir(&prev_dir).unwrap();

        let resolved = result.expect("embedded script must resolve with no fs script present");
        match &resolved {
            ResolvedPdfScript::Embedded(_) => {}
            ResolvedPdfScript::Override(p) => {
                panic!("expected embedded script, got override: {}", p.display())
            }
        }
        // The temp file exists and contains the compiled-in script byte-for-byte.
        let on_disk = fs::read_to_string(resolved.path())
            .expect("materialized embedded script should be readable");
        assert_eq!(
            on_disk, PDF_SCRIPT,
            "materialized temp file must contain the include_str! content"
        );
        // Sanity: the embedded content is the real converter, not a stub.
        assert!(
            PDF_SCRIPT.contains("pymupdf4llm"),
            "embedded script should be the pymupdf4llm converter"
        );
    }

    /// CQS_PDF_SCRIPT pointing to an existing, safe .py file → used verbatim as
    /// the override (not the embedded script).
    #[test]
    #[serial_test::serial]
    fn resolve_pdf_script_env_override_used() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("my_script.py");
        fs::write(&script, "# placeholder").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 0o644 — owner writable only; passes the safety check regardless
            // of the running user's umask.
            fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        let _guard = EnvGuard::set("CQS_PDF_SCRIPT", script.to_str().unwrap());

        let resolved = resolve_pdf_script().expect("override should resolve");
        match resolved {
            ResolvedPdfScript::Override(p) => assert_eq!(p, script),
            ResolvedPdfScript::Embedded(_) => panic!("expected override, got embedded script"),
        }
    }

    /// CQS_PDF_SCRIPT set but the file does not exist → falls back to the trusted
    /// embedded script (deterministically, since there is no CWD search anymore).
    #[test]
    #[serial_test::serial]
    fn resolve_pdf_script_missing_override_falls_through_to_embedded() {
        let dir = tempfile::TempDir::new().unwrap();
        let ghost = dir.path().join("does_not_exist.py");
        assert!(!ghost.exists());

        let _guard = EnvGuard::set("CQS_PDF_SCRIPT", ghost.to_str().unwrap());

        let resolved =
            resolve_pdf_script().expect("missing override must fall through to embedded");
        match resolved {
            ResolvedPdfScript::Embedded(_) => {}
            ResolvedPdfScript::Override(p) => {
                panic!(
                    "expected embedded fallthrough, got override: {}",
                    p.display()
                )
            }
        }
    }

    /// CQS_PDF_SCRIPT with a non-.py extension is rejected.
    #[test]
    #[serial_test::serial]
    fn resolve_pdf_script_env_var_non_py_extension_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("converter.sh");
        fs::write(&script, "#!/bin/sh\necho hello").unwrap();

        let _guard = EnvGuard::set("CQS_PDF_SCRIPT", script.to_str().unwrap());

        let result = resolve_pdf_script();
        assert!(result.is_err(), "non-.py extension should be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains(".py extension"),
            "error should mention .py requirement, got: {}",
            msg
        );
    }

    // World-writable override script is rejected.
    #[test]
    #[serial_test::serial]
    #[cfg(unix)]
    fn resolve_pdf_script_world_writable_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("world_writable.py");
        fs::write(&script, "# evil").unwrap();
        // World-writable: mode 0o666.
        fs::set_permissions(&script, std::fs::Permissions::from_mode(0o666)).unwrap();

        let _guard = EnvGuard::set("CQS_PDF_SCRIPT", script.to_str().unwrap());

        let result = resolve_pdf_script();
        assert!(result.is_err(), "world-writable script must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("writable") || msg.contains("mode"),
            "error should mention writable/mode, got: {}",
            msg
        );
    }

    // User-owned, non-world-writable override script passes.
    #[test]
    #[serial_test::serial]
    #[cfg(unix)]
    fn resolve_pdf_script_user_owned_accepted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("safe.py");
        fs::write(&script, "# placeholder").unwrap();
        fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _guard = EnvGuard::set("CQS_PDF_SCRIPT", script.to_str().unwrap());

        let result = resolve_pdf_script();
        assert!(
            result.is_ok(),
            "user-owned non-writable script must be accepted, got: {:?}",
            result.as_ref().err()
        );
    }

    // Group-writable override script is rejected.
    #[test]
    #[serial_test::serial]
    #[cfg(unix)]
    fn resolve_pdf_script_group_writable_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("group_writable.py");
        fs::write(&script, "# evil").unwrap();
        // Group-writable: mode 0o664.
        fs::set_permissions(&script, std::fs::Permissions::from_mode(0o664)).unwrap();

        let _guard = EnvGuard::set("CQS_PDF_SCRIPT", script.to_str().unwrap());

        let result = resolve_pdf_script();
        assert!(result.is_err(), "group-writable script must be rejected");
    }
}
