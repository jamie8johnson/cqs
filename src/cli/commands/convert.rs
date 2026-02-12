//! CLI handler for `cqs convert`.

use std::path::PathBuf;

use anyhow::Result;

use super::super::Cli;

pub fn cmd_convert(
    _cli: &Cli,
    path: &str,
    output: Option<&str>,
    overwrite: bool,
    dry_run: bool,
    clean_tags: Option<&str>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_convert").entered();

    let source = PathBuf::from(path);
    if !source.exists() {
        anyhow::bail!("Path not found: {}", path);
    }

    // Default output dir: same directory as input (or input dir itself)
    let output_dir = match output {
        Some(dir) => PathBuf::from(dir),
        None => {
            if source.is_dir() {
                source.clone()
            } else {
                source
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            }
        }
    };

    let tags: Vec<String> = clean_tags
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
        .unwrap_or_default();

    let opts = cqs::convert::ConvertOptions {
        output_dir,
        overwrite,
        dry_run,
        clean_tags: tags,
    };

    let results = cqs::convert::convert_path(&source, &opts)?;

    if results.is_empty() {
        println!("No supported documents found.");
        return Ok(());
    }

    if dry_run {
        println!(
            "Dry run — {} document(s) would be converted:\n",
            results.len()
        );
    } else {
        println!("Converted {} document(s):\n", results.len());
    }

    for r in &results {
        println!(
            "  {} → {}",
            r.source.display(),
            r.output.file_name().unwrap_or_default().to_string_lossy()
        );
        println!(
            "    Format: {} | Title: {} | Sections: {}",
            r.format, r.title, r.sections
        );
    }

    Ok(())
}
