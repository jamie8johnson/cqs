//! Export a HuggingFace model to ONNX format for use with cqs.

use std::path::Path;

pub(crate) fn cmd_export_model(repo: &str, output: &Path) -> anyhow::Result<()> {
    let _span = tracing::info_span!("export_model", repo).entered();

    println!("Exporting {} to ONNX...", repo);

    // Check Python deps
    let check = std::process::Command::new("python3")
        .args(["-c", "import optimum; import sentence_transformers"])
        .output()?;
    if !check.status.success() {
        anyhow::bail!(
            "Missing Python dependencies. Install with:\n  \
             pip install optimum sentence-transformers"
        );
    }

    // Export via optimum
    let export = std::process::Command::new("python3")
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
    let toml = format!(
        r#"# cqs embedding model configuration
# Copy this to your project's cqs.toml [embedding] section

[embedding]
model = "custom"
repo = "{repo}"
onnx_path = "model.onnx"
tokenizer = "tokenizer.json"
# dim = ???  # Check {repo} config.json for hidden_size
# query_prefix = ""
# doc_prefix = ""
"#
    );
    std::fs::write(output.join("model.toml"), toml)?;

    println!("Exported to {}", output.display());
    println!("Edit model.toml to set dim and prefixes, then copy to your cqs.toml");
    tracing::info!("Model exported to {}", output.display());
    Ok(())
}
