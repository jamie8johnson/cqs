//! Export a HuggingFace model to ONNX format for use with cqs.

use std::path::Path;

/// Find a working Python interpreter.
///
/// Tries `python3`, `python`, `py` in order. Validates with `--version`.
fn find_python() -> anyhow::Result<String> {
    for name in &["python3", "python", "py"] {
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

pub(crate) fn cmd_export_model(repo: &str, output: &Path) -> anyhow::Result<()> {
    let _span = tracing::info_span!("export_model", repo).entered();

    // PB-30: Canonicalize output path
    let output = dunce::canonicalize(output).unwrap_or_else(|_| output.to_path_buf());

    // SEC-18: Validate repo format to prevent TOML injection
    if !repo.contains('/') || repo.contains('"') || repo.contains('\n') || repo.contains('\\') {
        anyhow::bail!(
            "Invalid repo ID format. Expected: org/model-name (e.g. intfloat/e5-base-v2)"
        );
    }

    println!("Exporting {} to ONNX...", repo);

    // PB-29/EH-32: Find a working Python interpreter first
    let python = find_python()?;

    // OB-26: Check Python deps, capture stderr for diagnostics
    let check = std::process::Command::new(&python)
        .args(["-c", "import optimum; import sentence_transformers"])
        .output()?;
    if !check.status.success() {
        let stderr = String::from_utf8_lossy(&check.stderr);
        anyhow::bail!(
            "Missing Python dependencies. Install with:\n  \
             pip install optimum sentence-transformers\n\n\
             Python stderr:\n{}",
            stderr.trim()
        );
    }

    // Export via optimum
    let export = std::process::Command::new(&python)
        .args([
            "-m",
            "optimum.exporters.onnx",
            "--model",
            repo,
            "--task",
            "feature-extraction",
            "--opset",
            "11",
            &output.display().to_string(),
        ])
        .output()?;

    if !export.status.success() {
        let stderr = String::from_utf8_lossy(&export.stderr);
        anyhow::bail!("ONNX export failed:\n{}", stderr);
    }

    // Write model.toml template
    let toml_path = output.join("model.toml");
    let toml = format!(
        r#"# cqs embedding model configuration
# Copy this to your project's cqs.toml [embedding] section

[embedding]
model = "custom"
repo = "{repo}"
onnx_path = "model.onnx"
tokenizer_path = "tokenizer.json"
# dim = ???  # Check {repo} config.json for hidden_size
# query_prefix = ""
# doc_prefix = ""
"#
    );
    std::fs::write(&toml_path, toml)?;

    // SEC-19: Restrict model.toml permissions on Unix (contains model config)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&toml_path, std::fs::Permissions::from_mode(0o600));
    }

    println!("Exported to {}", output.display());
    println!("Edit model.toml to set dim and prefixes, then copy to your cqs.toml");
    tracing::info!("Model exported to {}", output.display());
    Ok(())
}
