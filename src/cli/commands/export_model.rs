//! Export a HuggingFace model to ONNX format for use with cqs.

use std::path::Path;

/// Find a working Python interpreter.
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

pub(crate) fn cmd_export_model(
    repo: &str,
    output: &Path,
    dim_override: Option<u64>,
) -> anyhow::Result<()> {
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
            &output.to_string_lossy(),
        ])
        .output()?;

    if !export.status.success() {
        let stderr = String::from_utf8_lossy(&export.stderr);
        anyhow::bail!("ONNX export failed:\n{}", stderr);
    }

    // EX-32: Resolve embedding dimension and write model.toml
    let resolved_dim = resolve_dim(dim_override, &output);
    write_model_toml(&output, repo, resolved_dim)?;

    println!("Exported to {}", output.display());
    if resolved_dim.is_some() {
        println!("Edit model.toml to set prefixes, then copy to your cqs.toml");
    } else {
        println!("Edit model.toml to set dim and prefixes, then copy to your cqs.toml");
    }
    tracing::info!("Model exported to {}", output.display());
    Ok(())
}

/// Resolve embedding dimension: --dim override > config.json auto-detect > None.
fn resolve_dim(dim_override: Option<u64>, output_dir: &Path) -> Option<u64> {
    let _span = tracing::info_span!("resolve_dim").entered();
    if let Some(d) = dim_override {
        tracing::info!(dim = d, "Using --dim override");
        return Some(d);
    }
    let detected = std::fs::read_to_string(output_dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|j| j["hidden_size"].as_u64());
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
    resolved_dim: Option<u64>,
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

    // SEC-19: Restrict model.toml permissions on Unix (contains model config)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&toml_path, std::fs::Permissions::from_mode(0o600));
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
