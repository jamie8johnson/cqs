//! Dead code detection command
//!
//! Core struct is [`DeadOutput`]; build with [`build_dead_output`].
//! CLI uses text output for human display, batch serializes with `serde_json::to_value()`.
//!
//! ## Verdicts (§2)
//!
//! Each dead entry self-classifies into a [`DeadVerdict`] (skip-when-default:
//! `unclassified` is omitted). The classification is ordered, first-match-wins:
//! `test-only` → `low-confidence-live` → `known-gap` → `dead`. The `dead`
//! verdict is the actionable residue; `--verdict dead` is the consumable list.

use std::path::Path;

use anyhow::{Context as _, Result};
use cqs::store::{DeadConfidence, DeadFunction};

// ---------------------------------------------------------------------------
// Verdicts (§2)
// ---------------------------------------------------------------------------

/// Self-classification of a dead-code entry. Ordered most-excusable to
/// least: a `test-only` fixture is almost never worth deleting, a `dead`
/// entry is the actionable residue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeadVerdict {
    /// Default: no classification ran / none matched above `dead`. Rendered as
    /// the absent (skip-when-default) state on JSON entries.
    Unclassified,
    /// Origin under `tests/` or enclosing `#[cfg(test)]` module — a test
    /// fixture, not product code.
    TestOnly,
    /// Has callers, but every edge reaching it is a heuristic kind
    /// (`macro_heuristic` / `fn_pointer`) — liveness rests on heuristics that
    /// may be false positives. Consumes §1's `edge_kind` column.
    LowConfidenceLive,
    /// Language/extension in a known static call-graph gap (`.js` served
    /// assets wired from HTML, Python runtime-invoked dunders).
    KnownGap,
    /// None of the above — the genuinely-dead residue.
    Dead,
}

impl DeadVerdict {
    /// Stable string for JSON / `--verdict` filter / text grouping.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            DeadVerdict::Unclassified => "unclassified",
            DeadVerdict::TestOnly => "test-only",
            DeadVerdict::LowConfidenceLive => "low-confidence-live",
            DeadVerdict::KnownGap => "known-gap",
            DeadVerdict::Dead => "dead",
        }
    }

    /// Parse a `--verdict` filter string; unknown values are a hard error.
    pub(crate) fn parse(s: &str) -> std::result::Result<DeadVerdict, String> {
        match s.to_ascii_lowercase().as_str() {
            "unclassified" => Ok(DeadVerdict::Unclassified),
            "test-only" => Ok(DeadVerdict::TestOnly),
            "low-confidence-live" => Ok(DeadVerdict::LowConfidenceLive),
            "known-gap" => Ok(DeadVerdict::KnownGap),
            "dead" => Ok(DeadVerdict::Dead),
            other => Err(format!(
                "invalid verdict '{other}' (expected unclassified|test-only|\
                 low-confidence-live|known-gap|dead)"
            )),
        }
    }
}

/// Static known-gap table: (language/extension predicate, reason). Each row
/// states a documented call-graph gap where "no callers" is a known
/// false-positive, not genuine death. The classifier keys on the chunk's
/// origin extension and language.
///
/// Rows:
/// - `.js` served assets: event handlers are wired from HTML (`onclick="..."`,
///   `addEventListener`) which the JS call-graph walker doesn't resolve to an
///   edge, so served-asset handlers look uncalled.
/// - Python dunder protocol methods (`__aenter__`, `__exit__`, `__iter__`, …):
///   invoked by the runtime (context-manager / iterator / async protocols),
///   never by a syntactic call, so they show zero callers.
fn known_gap_reason(entry: &DeadFunction) -> Option<&'static str> {
    let origin = entry.chunk.file.to_string_lossy();
    let lang = entry.chunk.language.to_string();
    let name = entry.chunk.name.as_str();

    // .js served assets: HTML-wired event handlers.
    if origin.ends_with(".js") || origin.ends_with(".mjs") {
        return Some("js served asset — event handlers wired from HTML, not a syntactic call");
    }

    // Python runtime-invoked dunder protocol methods.
    if lang == "python" && name.starts_with("__") && name.ends_with("__") {
        return Some("python dunder — invoked by the runtime protocol, not a syntactic call");
    }

    None
}

/// Whether a dead entry is test-only: origin under a `tests/` path segment, or
/// the chunk content sits inside a `#[cfg(test)]` module. The content scan is a
/// substring check — false-positive-friendly in the safe direction (a comment
/// mentioning `#[cfg(test)]` keeps the function classified test-only, which only
/// moves it OUT of the actionable `dead` list).
fn is_test_only(entry: &DeadFunction) -> bool {
    let origin = entry.chunk.file.to_string_lossy();
    // Origin-prefix: a `tests/` path segment (also `/tests/`, `\tests\`).
    if origin.starts_with("tests/") || origin.contains("/tests/") || origin.contains("\\tests\\") {
        return true;
    }
    // Enclosing #[cfg(test)] module: the chunk content carries the attribute
    // when the chunk is the module, or the function lives directly under it.
    // The store can't answer the module-chain question without a parse, so this
    // degrades to a content substring scan (documented limitation).
    entry.chunk.content.contains("#[cfg(test)]")
}

/// Classify a dead entry into its verdict. Ordered, first-match-wins:
/// test-only → low-confidence-live → known-gap → dead. `low_conf_names`
/// carries the §1-derived set of heuristic-only-caller function names.
fn classify_verdict(
    entry: &DeadFunction,
    low_conf_names: &std::collections::HashSet<String>,
) -> (DeadVerdict, &'static str) {
    if is_test_only(entry) {
        return (
            DeadVerdict::TestOnly,
            "origin under tests/ or #[cfg(test)] module",
        );
    }
    if low_conf_names.contains(&entry.chunk.name) {
        return (
            DeadVerdict::LowConfidenceLive,
            "all callers reach via heuristic edges (macro/fn-pointer)",
        );
    }
    if let Some(reason) = known_gap_reason(entry) {
        return (DeadVerdict::KnownGap, reason);
    }
    (
        DeadVerdict::Dead,
        "no callers; none of the excusing tiers matched",
    )
}

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
    /// Verdict label (skip-when-default: `unclassified` omitted).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub verdict: String,
    /// Human-readable reason for the verdict (skip-when-default).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub verdict_reason: String,
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
    /// Restrict output to one verdict class (`test-only`, `low-confidence-live`,
    /// `known-gap`, `dead`, `unclassified`). `None` ⇒ all verdicts.
    /// `--verdict dead` is the actionable residue.
    #[serde(default, deserialize_with = "de_opt_verdict")]
    pub verdict: Option<DeadVerdict>,
}

fn default_dead_confidence() -> DeadConfidence {
    DeadConfidence::Low
}

/// Deserialize an optional [`DeadVerdict`] filter from its stable string.
fn de_opt_verdict<'de, D>(de: D) -> std::result::Result<Option<DeadVerdict>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let opt = Option::<String>::deserialize(de)?;
    match opt {
        None => Ok(None),
        Some(s) => Ok(Some(
            DeadVerdict::parse(&s).map_err(serde::de::Error::custom)?,
        )),
    }
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

    // §1-derived low-confidence-live population: function names whose only
    // call-graph edges are heuristic kinds. Used to classify the verdict; the
    // `find_dead_code` candidates never overlap this set (they have zero
    // callers, these have heuristic-only callers), so it only relabels via the
    // ordered classifier.
    let low_conf_names = store
        .find_low_confidence_live_names()
        .context("Failed to query low-confidence-live names")?;

    let mut output = build_dead_output(&confident, &possibly_pub, root, &low_conf_names);

    // Apply the `--verdict` filter to both lists, then recount.
    if let Some(want) = args.verdict {
        let want = want.as_str();
        output.dead.retain(|e| e.verdict == want);
        output.possibly_dead_pub.retain(|e| e.verdict == want);
        output.count = output.dead.len();
        output.possibly_pub_count = output.possibly_dead_pub.len();
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build the typed dead-code report shared between CLI and batch. Each entry
/// is classified into a [`DeadVerdict`] via the ordered `classify_verdict`;
/// `low_conf_names` is the §1-derived heuristic-only-caller set.
pub(crate) fn build_dead_output(
    confident: &[DeadFunction],
    possibly_pub: &[DeadFunction],
    root: &Path,
    low_conf_names: &std::collections::HashSet<String>,
) -> DeadOutput {
    let _span = tracing::info_span!(
        "build_dead_output",
        confident = confident.len(),
        possibly = possibly_pub.len()
    )
    .entered();

    let format = |d: &DeadFunction| {
        let (verdict, reason) = classify_verdict(d, low_conf_names);
        DeadFunctionEntry {
            name: d.chunk.name.clone(),
            file: cqs::rel_display(&d.chunk.file, root).to_string(),
            line_start: d.chunk.line_start,
            line_end: d.chunk.line_end,
            chunk_type: d.chunk.chunk_type.to_string(),
            signature: d.chunk.signature.clone(),
            language: d.chunk.language.to_string(),
            confidence: d.confidence.as_str().to_string(),
            // `unclassified` is the skip-when-default state; every other verdict
            // is emitted.
            verdict: if verdict == DeadVerdict::Unclassified {
                String::new()
            } else {
                verdict.as_str().to_string()
            },
            verdict_reason: if verdict == DeadVerdict::Unclassified {
                String::new()
            } else {
                reason.to_string()
            },
        }
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
    verdict: Option<DeadVerdict>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_dead").entered();

    let args = DeadArgs {
        include_pub,
        min_confidence: min_level,
        verdict,
    };
    let output = dead_core(&ctx.store, &ctx.root, &args)?;

    if json {
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        display_dead_text(&output, ctx.cli.quiet);
    }

    Ok(())
}

/// Render the typed [`DeadOutput`] as human-readable text, grouped by verdict.
/// Reads the same struct the JSON path emits so the two renderings can't drift.
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
        // Group by verdict so the actionable `dead` residue is visible apart
        // from the excused tiers. Order: dead first (actionable), then the
        // excusing verdicts.
        for group in &["dead", "known-gap", "low-confidence-live", "test-only", ""] {
            let members: Vec<_> = output.dead.iter().filter(|d| d.verdict == *group).collect();
            if members.is_empty() {
                continue;
            }
            if !quiet {
                let label = if group.is_empty() {
                    "unclassified"
                } else {
                    group
                };
                println!("  [{label}]");
            }
            for d in members {
                println!(
                    "  {} {}:{}  [{}] ({})",
                    d.name, d.file, d.line_start, d.chunk_type, d.confidence,
                );
                if !quiet {
                    println!("    {}", d.signature.lines().next().unwrap_or(""));
                }
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
                verdict: "dead".into(),
                verdict_reason: "no callers".into(),
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

    // ----- Verdict classification (§2) -----

    /// Build a minimal `DeadFunction` for classifier tests.
    fn dead_fn(
        name: &str,
        origin: &str,
        language: cqs::parser::Language,
        content: &str,
    ) -> DeadFunction {
        use cqs::store::ChunkSummary;
        DeadFunction {
            chunk: ChunkSummary {
                id: format!("{origin}:1:{name}"),
                file: std::path::PathBuf::from(origin),
                language,
                chunk_type: cqs::parser::ChunkType::Function,
                name: name.to_string(),
                signature: format!("fn {name}()"),
                content: content.to_string(),
                doc: None,
                line_start: 1,
                line_end: 3,
                content_hash: "h".into(),
                window_idx: None,
                parent_id: None,
                parent_type_name: None,
                parser_version: 0,
                vendored: false,
            },
            confidence: DeadConfidence::High,
        }
    }

    #[test]
    fn verdict_test_only_by_origin_prefix() {
        let f = dead_fn(
            "make_x",
            "tests/helpers.rs",
            cqs::parser::Language::Rust,
            "fn make_x() {}",
        );
        let (v, _) = classify_verdict(&f, &std::collections::HashSet::new());
        assert_eq!(v, DeadVerdict::TestOnly);
    }

    #[test]
    fn verdict_test_only_by_cfg_test_content() {
        let f = dead_fn(
            "helper",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "#[cfg(test)]\nmod t { fn helper() {} }",
        );
        let (v, _) = classify_verdict(&f, &std::collections::HashSet::new());
        assert_eq!(v, DeadVerdict::TestOnly);
    }

    #[test]
    fn verdict_low_confidence_live_consumes_edge_kind_set() {
        let f = dead_fn(
            "cb",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn cb() {}",
        );
        let mut names = std::collections::HashSet::new();
        names.insert("cb".to_string());
        let (v, _) = classify_verdict(&f, &names);
        assert_eq!(v, DeadVerdict::LowConfidenceLive);
    }

    #[test]
    fn verdict_known_gap_js_asset() {
        let f = dead_fn(
            "onClick",
            "src/serve/assets/app.js",
            cqs::parser::Language::JavaScript,
            "function onClick() {}",
        );
        let (v, _) = classify_verdict(&f, &std::collections::HashSet::new());
        assert_eq!(v, DeadVerdict::KnownGap);
    }

    #[test]
    fn verdict_known_gap_python_dunder() {
        let f = dead_fn(
            "__aenter__",
            "src/ctx.py",
            cqs::parser::Language::Python,
            "def __aenter__(self): ...",
        );
        let (v, _) = classify_verdict(&f, &std::collections::HashSet::new());
        assert_eq!(v, DeadVerdict::KnownGap);
    }

    #[test]
    fn verdict_dead_residue() {
        let f = dead_fn(
            "genuinely_dead",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn genuinely_dead() {}",
        );
        let (v, _) = classify_verdict(&f, &std::collections::HashSet::new());
        assert_eq!(v, DeadVerdict::Dead);
    }

    /// Ordering: test-only wins over a name that's also in the low-conf set.
    #[test]
    fn verdict_ordering_test_only_beats_low_conf() {
        let f = dead_fn(
            "make_x",
            "tests/h.rs",
            cqs::parser::Language::Rust,
            "fn make_x() {}",
        );
        let mut names = std::collections::HashSet::new();
        names.insert("make_x".to_string());
        let (v, _) = classify_verdict(&f, &names);
        assert_eq!(v, DeadVerdict::TestOnly);
    }

    #[test]
    fn verdict_parse_rejects_unknown() {
        assert!(DeadVerdict::parse("bogus").is_err());
        assert_eq!(DeadVerdict::parse("dead").unwrap(), DeadVerdict::Dead);
    }

    /// `--verdict` deserializes from the wire and rejects unknown values.
    #[test]
    fn dead_args_deserialize_verdict() {
        let args: DeadArgs =
            serde_json::from_value(serde_json::json!({"verdict": "dead"})).unwrap();
        assert_eq!(args.verdict, Some(DeadVerdict::Dead));
        let none: DeadArgs = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(none.verdict, None);
        assert!(
            serde_json::from_value::<DeadArgs>(serde_json::json!({"verdict": "nope"})).is_err()
        );
    }

    /// The `dead` verdict is emitted (not skip-when-default); only
    /// `unclassified` is omitted.
    #[test]
    fn dead_verdict_serialized_on_entry() {
        let low = std::collections::HashSet::new();
        let f = dead_fn("x", "src/lib.rs", cqs::parser::Language::Rust, "fn x() {}");
        let out = build_dead_output(&[f], &[], std::path::Path::new("."), &low);
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["dead"][0]["verdict"], "dead");
        assert!(json["dead"][0].get("verdict_reason").is_some());
    }
}
