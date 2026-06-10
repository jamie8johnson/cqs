//! CLI handler for `cqs convert`.

use std::path::PathBuf;

use anyhow::Result;

/// One converted document in `cqs convert --json`.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ConvertEntry {
    pub source: String,
    pub output: String,
    pub format: String,
    pub title: String,
    pub sections: usize,
}

/// `cqs convert --json` payload. `skipped` is reserved (empty) — the converter
/// surfaces skips as warnings, not a structured list, so the field is a
/// forward-compat schema reservation rather than a populated array. convert is
/// document-conversion orchestration with no daemon path; this typed output is
/// the schema for its inline JSON.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ConvertOutput {
    pub converted: Vec<ConvertEntry>,
    pub skipped: Vec<serde_json::Value>,
    pub took_ms: u64,
    pub dry_run: bool,
}

pub fn cmd_convert(
    path: &str,
    output: Option<&str>,
    overwrite: bool,
    dry_run: bool,
    clean_tags: Option<&str>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_convert").entered();
    // Track wall-clock for the JSON envelope; doubles as a sanity metric on
    // long PDF/CHM batches.
    let start = std::time::Instant::now();

    let source = PathBuf::from(path);
    if !source.exists() {
        anyhow::bail!("Path not found: {}", path);
    }

    // Default output dir: same directory as input (or input dir itself)
    // Canonicalize to normalize symlinks and warn if outside source tree.
    let output_dir = match output {
        Some(dir) => {
            let raw = PathBuf::from(dir);
            let canonical = dunce::canonicalize(&raw).unwrap_or(raw);
            if let Ok(source_parent) = dunce::canonicalize(source.parent().unwrap_or(&source)) {
                if !canonical.starts_with(&source_parent) {
                    tracing::warn!(
                        output = %canonical.display(),
                        source = %source_parent.display(),
                        "Output directory is outside source tree"
                    );
                }
            }
            canonical
        }
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

    if json {
        // Structured summary for JSON-driven agents. `skipped` stays empty —
        // see ConvertOutput docs (schema reservation, not a lie).
        let converted: Vec<ConvertEntry> = results
            .iter()
            .map(|r| ConvertEntry {
                source: r.source.display().to_string(),
                output: r.output.display().to_string(),
                format: r.format.to_string(),
                title: r.title.clone(),
                sections: r.sections,
            })
            .collect();
        crate::cli::json_envelope::emit_json(&ConvertOutput {
            converted,
            skipped: Vec::new(),
            took_ms: start.elapsed().as_millis() as u64,
            dry_run,
        })?;
        return Ok(());
    }

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
