//! Dead code detection command
//!
//! Core struct is [`DeadOutput`]; build with [`build_dead_output`].
//! CLI uses text output for human display, batch serializes with `serde_json::to_value()`.

use std::path::Path;

use anyhow::{Context as _, Result};
use cqs::store::{DeadConfidence, DeadFunction};

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub(crate) struct DeadFunctionEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub chunk_type: String,
    pub signature: String,
    pub language: String,
    pub confidence: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DeadOutput {
    pub dead: Vec<DeadFunctionEntry>,
    pub possibly_dead_pub: Vec<DeadFunctionEntry>,
    pub count: usize,
    pub possibly_pub_count: usize,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build the typed dead-code report shared between CLI and batch.
pub(crate) fn build_dead_output(
    confident: &[DeadFunction],
    possibly_pub: &[DeadFunction],
    root: &Path,
) -> DeadOutput {
    let _span = tracing::info_span!(
        "build_dead_output",
        confident = confident.len(),
        possibly = possibly_pub.len()
    )
    .entered();

    let format = |d: &DeadFunction| DeadFunctionEntry {
        name: d.chunk.name.clone(),
        file: cqs::rel_display(&d.chunk.file, root).to_string(),
        line_start: d.chunk.line_start,
        line_end: d.chunk.line_end,
        chunk_type: d.chunk.chunk_type.to_string(),
        signature: d.chunk.signature.clone(),
        language: d.chunk.language.to_string(),
        confidence: confidence_label(d.confidence).to_string(),
    };

    DeadOutput {
        count: confident.len(),
        possibly_pub_count: possibly_pub.len(),
        dead: confident.iter().map(&format).collect(),
        possibly_dead_pub: possibly_pub.iter().map(&format).collect(),
    }
}

// ---------------------------------------------------------------------------
// CLI command
// ---------------------------------------------------------------------------

/// Find functions/methods with no callers in the indexed codebase
pub(crate) fn cmd_dead(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    json: bool,
    include_pub: bool,
    min_level: DeadConfidence,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_dead").entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let (confident, possibly_pub) = store
        .find_dead_code(include_pub)
        .context("Failed to detect dead code")?;

    // Filter by minimum confidence
    let confident: Vec<_> = confident
        .into_iter()
        .filter(|d| d.confidence >= min_level)
        .collect();
    let possibly_pub: Vec<_> = possibly_pub
        .into_iter()
        .filter(|d| d.confidence >= min_level)
        .collect();

    if json {
        let output = build_dead_output(&confident, &possibly_pub, root);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        display_dead_text(&confident, &possibly_pub, root, ctx.cli.quiet);
    }

    Ok(())
}

/// Human-readable confidence label
fn confidence_label(c: DeadConfidence) -> &'static str {
    c.as_str()
}

fn display_dead_text(
    confident: &[DeadFunction],
    possibly_pub: &[DeadFunction],
    root: &Path,
    quiet: bool,
) {
    if confident.is_empty() && possibly_pub.is_empty() {
        println!("No dead code found.");
        return;
    }

    if !confident.is_empty() {
        if !quiet {
            println!("Dead code ({} functions):", confident.len());
            println!();
        }
        for dead in confident {
            let rel = cqs::rel_display(&dead.chunk.file, root);
            println!(
                "  {} {}:{}  [{}] ({})",
                dead.chunk.name,
                rel,
                dead.chunk.line_start,
                dead.chunk.chunk_type,
                confidence_label(dead.confidence),
            );
            if !quiet {
                println!("    {}", dead.chunk.signature.lines().next().unwrap_or(""));
            }
        }
    }

    if !possibly_pub.is_empty() {
        if !confident.is_empty() {
            println!();
        }
        println!(
            "Possibly dead (public API, {} functions):",
            possibly_pub.len()
        );
        if !quiet {
            println!("  (Use --include-pub to include these in the main list)");
        }
        println!();
        for dead in possibly_pub {
            let rel = cqs::rel_display(&dead.chunk.file, root);
            println!(
                "  {} {}:{}  [{}] ({})",
                dead.chunk.name,
                rel,
                dead.chunk.line_start,
                dead.chunk.chunk_type,
                confidence_label(dead.confidence),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_output_empty() {
        let output = DeadOutput {
            dead: vec![],
            possibly_dead_pub: vec![],
            count: 0,
            possibly_pub_count: 0,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["count"], 0);
        assert_eq!(json["possibly_pub_count"], 0);
        assert!(json["dead"].as_array().unwrap().is_empty());
        assert!(json["possibly_dead_pub"].as_array().unwrap().is_empty());
    }

    #[test]
    fn dead_output_serialization() {
        let output = DeadOutput {
            dead: vec![DeadFunctionEntry {
                name: "unused_fn".into(),
                file: "src/lib.rs".into(),
                line_start: 10,
                line_end: 20,
                chunk_type: "function".into(),
                signature: "fn unused_fn()".into(),
                language: "rust".into(),
                confidence: "high".into(),
            }],
            possibly_dead_pub: vec![],
            count: 1,
            possibly_pub_count: 0,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["count"], 1);
        assert_eq!(json["dead"][0]["name"], "unused_fn");
        assert_eq!(json["dead"][0]["file"], "src/lib.rs");
        assert_eq!(json["dead"][0]["line_start"], 10);
        assert_eq!(json["dead"][0]["line_end"], 20);
        assert_eq!(json["dead"][0]["chunk_type"], "function");
        assert_eq!(json["dead"][0]["language"], "rust");
        assert_eq!(json["dead"][0]["confidence"], "high");
    }
}
