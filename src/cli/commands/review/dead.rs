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
// Args + core (surface-agnostic, MCP-ready)
// ---------------------------------------------------------------------------

/// Input for [`dead_core`]. Derives `Deserialize` (MCP param surface) with
/// doc-commented fields; `min_confidence` deserializes from the same
/// `low`/`medium`/`high` strings the CLI / wire accept.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct DeadArgs {
    /// Include public-API functions in the main `dead` list (otherwise they
    /// land in `possibly_dead_pub`, which agents usually skip).
    #[serde(default)]
    pub include_pub: bool,
    /// Minimum confidence to report (`low` | `medium` | `high`). Entries below
    /// this level are filtered out of both lists.
    #[serde(
        default = "default_dead_confidence",
        deserialize_with = "de_confidence"
    )]
    pub min_confidence: DeadConfidence,
}

fn default_dead_confidence() -> DeadConfidence {
    DeadConfidence::Low
}

/// Deserialize a [`DeadConfidence`] from its stable `low`/`medium`/`high`
/// string. Kept local to the adapter layer so the lib enum stays
/// `Serialize`-only (no eval-reachable source touched).
fn de_confidence<'de, D>(de: D) -> std::result::Result<DeadConfidence, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let s = String::deserialize(de)?;
    match s.to_ascii_lowercase().as_str() {
        "low" => Ok(DeadConfidence::Low),
        "medium" => Ok(DeadConfidence::Medium),
        "high" => Ok(DeadConfidence::High),
        other => Err(serde::de::Error::custom(format!(
            "invalid dead confidence '{other}' (expected low|medium|high)"
        ))),
    }
}

/// Surface-agnostic core for `cqs dead`. Finds zero-caller functions, filters
/// by `min_confidence`, and returns the typed [`DeadOutput`]. Both the CLI
/// (`cmd_dead`) and the daemon (`dispatch_dead`) drive this so the dead-code
/// schema has exactly one definition site.
pub(crate) fn dead_core(
    store: &cqs::Store<cqs::store::ReadOnly>,
    root: &Path,
    args: &DeadArgs,
) -> Result<DeadOutput> {
    let _span = tracing::info_span!("dead_core", include_pub = args.include_pub).entered();
    let (confident, possibly_pub) = store
        .find_dead_code(args.include_pub)
        .context("Failed to detect dead code")?;

    let confident: Vec<_> = confident
        .into_iter()
        .filter(|d| d.confidence >= args.min_confidence)
        .collect();
    let possibly_pub: Vec<_> = possibly_pub
        .into_iter()
        .filter(|d| d.confidence >= args.min_confidence)
        .collect();

    Ok(build_dead_output(&confident, &possibly_pub, root))
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
        confidence: d.confidence.as_str().to_string(),
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

    let args = DeadArgs {
        include_pub,
        min_confidence: min_level,
    };
    let output = dead_core(&ctx.store, &ctx.root, &args)?;

    if json {
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        display_dead_text(&output, ctx.cli.quiet);
    }

    Ok(())
}

/// Render the typed [`DeadOutput`] as human-readable text. Reads the same
/// struct the JSON path emits so the two renderings can't drift.
fn display_dead_text(output: &DeadOutput, quiet: bool) {
    if output.dead.is_empty() && output.possibly_dead_pub.is_empty() {
        println!("No dead code found.");
        return;
    }

    if !output.dead.is_empty() {
        if !quiet {
            println!("Dead code ({} functions):", output.dead.len());
            println!();
        }
        for d in &output.dead {
            println!(
                "  {} {}:{}  [{}] ({})",
                d.name, d.file, d.line_start, d.chunk_type, d.confidence,
            );
            if !quiet {
                println!("    {}", d.signature.lines().next().unwrap_or(""));
            }
        }
    }

    if !output.possibly_dead_pub.is_empty() {
        if !output.dead.is_empty() {
            println!();
        }
        println!(
            "Possibly dead (public API, {} functions):",
            output.possibly_dead_pub.len()
        );
        if !quiet {
            println!("  (Use --include-pub to include these in the main list)");
        }
        println!();
        for d in &output.possibly_dead_pub {
            println!(
                "  {} {}:{}  [{}] ({})",
                d.name, d.file, d.line_start, d.chunk_type, d.confidence,
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

    /// `DeadArgs` deserializes from a wire/MCP-shaped object, mapping the
    /// `min_confidence` string through the local `de_confidence` helper without
    /// the lib enum deriving `Deserialize`.
    #[test]
    fn dead_args_deserialize_confidence_string() {
        let args: DeadArgs =
            serde_json::from_value(serde_json::json!({"min_confidence": "high"})).unwrap();
        assert_eq!(args.min_confidence, DeadConfidence::High);
        assert!(!args.include_pub, "include_pub defaults to false");

        // Empty object → defaults (include_pub=false, min_confidence=Low).
        let def: DeadArgs = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(def.min_confidence, DeadConfidence::Low);

        // Unknown confidence string is a hard error (no silent default).
        assert!(
            serde_json::from_value::<DeadArgs>(serde_json::json!({"min_confidence": "bogus"}))
                .is_err()
        );
    }

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
