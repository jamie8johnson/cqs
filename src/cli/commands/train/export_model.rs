//! Export a HuggingFace model to ONNX format for use with cqs.

use std::path::Path;

/// Find a working Python interpreter (delegates to shared `convert::find_python`).
fn find_python() -> anyhow::Result<String> {
    cqs::convert::find_python()
}

/// Strict allowlist for HuggingFace repo IDs.
/// A denylist would have to enumerate `\r` (CR), `[`, `]` (TOML table reopen),
/// `=`, `#`, and surrounding whitespace — all of which `write_model_toml`
/// interpolates verbatim into model.toml. HuggingFace repo IDs are documented
/// as `[A-Za-z0-9._/-]` only, so we reject anything outside that set instead.
/// Also reject leading `-` to prevent
/// arg-confusion in the optimum subprocess that consumes `repo` next.
fn validate_repo_id(repo: &str) -> anyhow::Result<()> {
    if !repo.contains('/')
        || repo.starts_with('-')
        || !repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    {
        anyhow::bail!(
            "Invalid repo ID format. Expected: org/model-name with characters \
             [A-Za-z0-9._/-] only (e.g. intfloat/e5-base-v2)"
        );
    }
    // Forbid `..` even though `.` and `/` are individually allowed.
    // optimum tries to interpret `--model` as a local path before falling
    // through to HF Hub; `org/../../etc/secrets` would walk up from cwd. HF
    // Hub itself rejects `..`; mirror that contract here.
    if repo.contains("..") {
        anyhow::bail!(
            "Invalid repo ID: contains `..`. HF Hub repo IDs cannot contain \
             parent-directory references."
        );
    }
    Ok(())
}

pub(crate) fn cmd_export_model(
    repo: &str,
    output: &Path,
    dim_override: Option<usize>,
) -> anyhow::Result<()> {
    let _span = tracing::info_span!("export_model", repo).entered();

    // Canonicalize output path
    let output = dunce::canonicalize(output).unwrap_or_else(|_| output.to_path_buf());

    validate_repo_id(repo)?;

    println!("Exporting {} to ONNX...", repo);

    // Find a working Python interpreter first
    let python = find_python()?;

    // Bound subprocess output. Both `Command::output()` calls buffer
    // stdout/stderr with a MAX_BYTES cap — `optimum.exporters.onnx` can print
    // multi-MB progress logs on a large export and a wedged HF download can
    // grow RAM for hours. Operators only need the tail of stderr for
    // diagnostics; stdout is purely informational.
    use std::io::Read;
    use std::process::Stdio;

    /// Read up to `max` bytes from the spawned child's stdout+stderr,
    /// then wait. Returns (status, stdout_buf, stderr_buf, stdout_truncated).
    fn run_capped(
        cmd: &mut std::process::Command,
        max_stdout: u64,
        max_stderr: u64,
    ) -> std::io::Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>, bool)> {
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut stdout_buf = Vec::with_capacity(8 * 1024);
        let mut stderr_buf = Vec::with_capacity(4 * 1024);
        if let Some(s) = child.stdout.take() {
            let _ = s.take(max_stdout + 1).read_to_end(&mut stdout_buf);
        }
        if let Some(s) = child.stderr.take() {
            let _ = s.take(max_stderr).read_to_end(&mut stderr_buf);
        }
        let status = child.wait()?;
        let truncated = stdout_buf.len() as u64 > max_stdout;
        Ok((status, stdout_buf, stderr_buf, truncated))
    }

    // Check Python deps. Cheap probe — 4 KiB stdout + 16 KiB stderr is plenty.
    let (check_status, _check_out, check_err, _) = run_capped(
        std::process::Command::new(&python)
            .args(["-c", "import optimum; import sentence_transformers"]),
        4 * 1024,
        16 * 1024,
    )?;
    if !check_status.success() {
        let stderr = String::from_utf8_lossy(&check_err);
        anyhow::bail!(
            "Missing Python dependencies. Install with:\n  \
             pip install optimum sentence-transformers\n\n\
             Python stderr:\n{}",
            stderr.trim()
        );
    }

    // Export via optimum. 16 MiB stdout / 1 MiB stderr — plenty of headroom
    // for legitimate progress logs but cuts off a wedged subprocess before
    // it OOMs the indexer.
    let (export_status, _export_out, export_err, export_truncated) = run_capped(
        std::process::Command::new(&python).args([
            "-m",
            "optimum.exporters.onnx",
            "--model",
            repo,
            "--task",
            "feature-extraction",
            "--opset",
            "11",
            &output.to_string_lossy(),
        ]),
        16 * 1024 * 1024,
        1024 * 1024,
    )?;

    if export_truncated {
        tracing::warn!(
            cap_bytes = 16 * 1024 * 1024,
            "ONNX export stdout exceeded cap; truncated. Subsequent diagnostics may be incomplete."
        );
    }
    if !export_status.success() {
        let stderr = String::from_utf8_lossy(&export_err);
        anyhow::bail!("ONNX export failed:\n{}", stderr);
    }

    // Resolve embedding dimension and write model.toml
    let resolved_dim = resolve_dim(dim_override, &output);
    write_model_toml(&output, repo, resolved_dim)?;

    println!("Exported to {}", output.display());
    if resolved_dim.is_some() {
        println!("Edit model.toml to set prefixes, then copy to your cqs.toml");
    } else {
        println!("Edit model.toml to set dim and prefixes, then copy to your cqs.toml");
    }
    tracing::info!(output = %output.display(), "Model exported");
    Ok(())
}

/// Resolve embedding dimension: --dim override > config.json auto-detect > None.
fn resolve_dim(dim_override: Option<usize>, output_dir: &Path) -> Option<usize> {
    let _span = tracing::info_span!("resolve_dim").entered();
    if let Some(d) = dim_override {
        tracing::info!(dim = d, "Using --dim override");
        return Some(d);
    }
    let detected = std::fs::read_to_string(output_dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|j| j["hidden_size"].as_u64())
        .map(|n| n as usize);
    match detected {
        Some(d) => {
            tracing::info!(dim = d, "Auto-detected dim from config.json hidden_size");
            println!("Auto-detected embedding dimension: {d}");
        }
        None => {
            tracing::warn!("Could not auto-detect dim from config.json; use --dim to specify");
        }
    }
    detected
}

/// Write model.toml template with resolved dimension.
fn write_model_toml(
    output_dir: &Path,
    repo: &str,
    resolved_dim: Option<usize>,
) -> anyhow::Result<()> {
    let toml_path = output_dir.join("model.toml");
    let dim_line = match resolved_dim {
        Some(d) => format!("dim = {d}"),
        None => {
            "# dim = ???  # Could not auto-detect; use --dim or check config.json for hidden_size"
                .to_string()
        }
    };
    let toml = format!(
        r#"# cqs embedding model configuration
# Copy this to your project's cqs.toml [embedding] section

[embedding]
model = "custom"
repo = "{repo}"
onnx_path = "model.onnx"
tokenizer_path = "tokenizer.json"
{dim_line}
# query_prefix = ""
# doc_prefix = ""
"#
    );
    std::fs::write(&toml_path, &toml)?;

    // Restrict model.toml permissions on Unix (contains model config)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&toml_path, std::fs::Permissions::from_mode(0o600))
        {
            tracing::debug!(path = %toml_path.display(), error = %e, "Failed to set file permissions");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_dim_override_takes_priority() {
        let dir = tempfile::TempDir::new().unwrap();
        // Write a config.json with a different dim
        std::fs::write(dir.path().join("config.json"), r#"{"hidden_size": 768}"#).unwrap();

        // Override should win over auto-detect
        let result = resolve_dim(Some(1024), dir.path());
        assert_eq!(result, Some(1024));
    }

    #[test]
    fn resolve_dim_auto_detects_from_config_json() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"hidden_size": 768, "model_type": "bert"}"#,
        )
        .unwrap();

        let result = resolve_dim(None, dir.path());
        assert_eq!(result, Some(768));
    }

    #[test]
    fn resolve_dim_none_when_no_config() {
        let dir = tempfile::TempDir::new().unwrap();
        // No config.json exists
        let result = resolve_dim(None, dir.path());
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_dim_none_when_config_missing_hidden_size() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"model_type": "bert"}"#).unwrap();

        let result = resolve_dim(None, dir.path());
        assert_eq!(result, None);
    }

    #[test]
    fn write_model_toml_includes_dim_when_known() {
        let dir = tempfile::TempDir::new().unwrap();
        write_model_toml(dir.path(), "org/model", Some(1024)).unwrap();

        let content = std::fs::read_to_string(dir.path().join("model.toml")).unwrap();
        assert!(content.contains("dim = 1024"), "should contain dim = 1024");
        assert!(content.contains("org/model"), "should contain repo name");
    }

    #[test]
    fn validate_repo_id_accepts_well_formed_ids() {
        assert!(validate_repo_id("intfloat/e5-base-v2").is_ok());
        assert!(validate_repo_id("BAAI/bge-large-en-v1.5").is_ok());
        assert!(validate_repo_id("org/model_name").is_ok());
        assert!(validate_repo_id("a/b").is_ok());
    }

    #[test]
    fn validate_repo_id_rejects_toml_injection_chars() {
        // Each of these would corrupt
        // model.toml via the `format!("repo = \"{repo}\"")` template if
        // accepted.
        assert!(validate_repo_id("evil/model\rinjected = 1").is_err()); // CR
        assert!(validate_repo_id("evil/model]\n[other]").is_err()); // ]
        assert!(validate_repo_id("evil/model[x]").is_err()); // [
        assert!(validate_repo_id("evil/model = 1").is_err()); // =
        assert!(validate_repo_id("evil/model#comment").is_err()); // #
        assert!(validate_repo_id("evil/model\"quote").is_err()); // " (still rejected)
        assert!(validate_repo_id("evil/model\\back").is_err()); // \ (still rejected)
        assert!(validate_repo_id("evil/model\nline").is_err()); // \n (still rejected)
        assert!(validate_repo_id("evil/model with space").is_err()); // space
        assert!(validate_repo_id("evil/model\ttab").is_err()); // tab
    }

    #[test]
    fn validate_repo_id_rejects_missing_slash() {
        assert!(validate_repo_id("nomodelpart").is_err());
        assert!(validate_repo_id("").is_err());
    }

    #[test]
    fn validate_repo_id_rejects_leading_dash() {
        // Avoid arg-confusion in the optimum subprocess.
        assert!(validate_repo_id("-evil/model").is_err());
    }

    /// `validate_repo_id` rejects `..` path-traversal at the top so a hostile
    /// `org/../../etc/passwd` can't escape `optimum`'s CWD. A refactor that
    /// relied on the char-set whitelist alone (`.` and `/` are both allowed
    /// individually) would silently break this, so pin it with a test.
    #[test]
    fn validate_repo_id_rejects_parent_directory_refs() {
        assert!(
            validate_repo_id("org/../../etc/passwd").is_err(),
            "classic path-traversal must be rejected"
        );
        assert!(
            validate_repo_id("..").is_err(),
            "bare `..` must be rejected"
        );
        assert!(
            validate_repo_id("foo/..").is_err(),
            "`..` as a segment must be rejected"
        );
        assert!(
            validate_repo_id("a..b/c").is_err(),
            "`..` as a substring (even mid-segment) must be rejected"
        );
        assert!(
            validate_repo_id("../escape/model").is_err(),
            "`..` at start must be rejected"
        );
    }

    #[test]
    fn write_model_toml_comments_dim_when_unknown() {
        let dir = tempfile::TempDir::new().unwrap();
        write_model_toml(dir.path(), "org/model", None).unwrap();

        let content = std::fs::read_to_string(dir.path().join("model.toml")).unwrap();
        assert!(
            content.contains("# dim = ???"),
            "should contain commented dim placeholder"
        );
    }
}
