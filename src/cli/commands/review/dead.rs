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
    /// No trusted caller (`call` / `serde_callback`), but ≥1 heuristic edge
    /// (`macro_heuristic` / `fn_pointer`) reaches it — liveness rests on
    /// heuristics that may be false positives. Doc-reference edges are inert
    /// (a prose mention does not disqualify). Consumes §1's `edge_kind` column.
    LowConfidenceLive,
    /// Language/extension in a known static call-graph gap (served-asset `.js`
    /// wired from HTML, Python runtime-invoked dunders).
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

/// One row of the static known-gap table: a predicate over the dead entry and
/// the reason string to attach when it matches. Each row states a documented
/// call-graph gap where "no callers" is a known false-positive, not genuine
/// death.
struct KnownGapRule {
    /// Returns true when this entry sits in the rule's known gap.
    matches: fn(origin: &str, lang: &str, name: &str) -> bool,
    /// Reason surfaced as `verdict_reason` on a `known-gap` entry.
    reason: &'static str,
}

/// Whether `origin` is a served front-end asset — an `.js`/`.mjs` file under a
/// served-assets directory whose handlers are wired from HTML (`onclick="..."`,
/// `addEventListener`) rather than a syntactic call. SCOPED to the served path:
/// the gap is "HTML-wired served assets", not "any .js anywhere". A build
/// script like `scripts/build.mjs` with zero callers is genuinely dead and must
/// NOT be excused. The prefix table is extensible so other corpora can add
/// their served-assets roots.
fn is_served_js_asset(origin: &str, _lang: &str, _name: &str) -> bool {
    /// Served-assets directory prefixes. Origins are normalized to forward
    /// slashes before classification, so only `/`-form prefixes are listed.
    const SERVED_ASSET_PREFIXES: &[&str] = &["src/serve/assets/"];
    let is_js = origin.ends_with(".js") || origin.ends_with(".mjs");
    is_js
        && SERVED_ASSET_PREFIXES
            .iter()
            .any(|prefix| origin.starts_with(prefix))
}

/// Whether `name` is a Python runtime-invoked dunder protocol method
/// (`__aenter__`, `__exit__`, `__iter__`, …): invoked by the runtime
/// (context-manager / iterator / async protocols), never by a syntactic call.
fn is_python_dunder(_origin: &str, lang: &str, name: &str) -> bool {
    lang == "python" && name.starts_with("__") && name.ends_with("__")
}

/// The known-gap table. First matching row wins.
const KNOWN_GAP_RULES: &[KnownGapRule] = &[
    KnownGapRule {
        matches: is_served_js_asset,
        reason: "served js asset — event handlers wired from HTML, not a syntactic call",
    },
    KnownGapRule {
        matches: is_python_dunder,
        reason: "python dunder — invoked by the runtime protocol, not a syntactic call",
    },
];

/// Classify a dead entry against the static known-gap table. Returns the reason
/// of the first matching rule, or `None` if no documented gap applies.
fn known_gap_reason(entry: &DeadFunction) -> Option<&'static str> {
    let origin = entry.chunk.file.to_string_lossy();
    let lang = entry.chunk.language.to_string();
    let name = entry.chunk.name.as_str();

    KNOWN_GAP_RULES
        .iter()
        .find(|rule| (rule.matches)(&origin, &lang, name))
        .map(|rule| rule.reason)
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
/// test-only → low-confidence-live → known-gap → dead. `low_conf` maps a
/// callee name to its §1-derived heuristic-caller breakdown (present only for
/// functions reached solely by heuristic edges).
fn classify_verdict(
    entry: &DeadFunction,
    low_conf: &std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo>,
) -> (DeadVerdict, String) {
    if is_test_only(entry) {
        // Softened to claim only what the substring scan actually knows: a
        // `tests/` path segment or the literal `#[cfg(test)]` appearing in the
        // chunk content (which the comment scan cannot distinguish from a real
        // attribute — documented limitation).
        return (
            DeadVerdict::TestOnly,
            "origin under tests/ path or content contains '#[cfg(test)]'".to_string(),
        );
    }
    if let Some(info) = low_conf.get(&entry.chunk.name) {
        // Name the heuristic kinds and counts rather than asserting "all
        // callers are heuristic" generically.
        let kinds = info
            .kind_counts
            .iter()
            .map(|(kind, n)| format!("{kind}×{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        return (
            DeadVerdict::LowConfidenceLive,
            format!(
                "no trusted caller; reached only by {} heuristic edge(s) [{}]",
                info.total, kinds
            ),
        );
    }
    if let Some(reason) = known_gap_reason(entry) {
        return (DeadVerdict::KnownGap, reason.to_string());
    }
    (
        DeadVerdict::Dead,
        "no callers; none of the excusing tiers matched".to_string(),
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
///
/// Plain entry point: no overlay. The full logic lives in [`dead_overlay`];
/// this delegates with `None` (participation discarded), so CLI / tests are
/// byte-unchanged.
pub(crate) fn dead_core(
    store: &cqs::Store<cqs::store::ReadOnly>,
    root: &Path,
    args: &DeadArgs,
) -> Result<DeadOutput> {
    Ok(dead_overlay(store, root, args, None)?.0)
}

/// Overlay-aware core for `cqs dead` (#1858 Part B). Identical to [`dead_core`]
/// when `overlay` is `None`. When `Some`, the dead set is recomputed over the
/// MERGED caller graph (parent real-caller edges minus delta-touched
/// caller-origins, plus the worktree's edges), in two directions:
///
/// - **parent-dead → live (removal):** a parent-dead function the worktree now
///   really-calls (the overlay store holds a real-caller edge to it) is dropped
///   from the dead set — it is live in this checkout.
/// - **parent-live → dead (addition):** a parent-live function whose every
///   real-caller edge sits in a delta-touched origin, and which the worktree no
///   longer calls, becomes dead. Candidates are exactly the callees of the delta
///   files ([`Store::distinct_callees_from_origins`]); for each, `merge_callers`
///   over (parent, overlay) is checked for zero real-caller edges. A newly-dead
///   addition is reported at `Medium` confidence — the file-activity recompute
///   `score_confidence` does for the parent set is not re-run under the overlay,
///   so `Medium` is the honest floor.
///
/// Both directions filter on [`CallEdgeKind::is_real_caller`] (a `doc_reference`
/// is inert), matching `fetch_uncalled_functions`'s own real-caller contract.
///
/// `dead`'s answer is fully determined by the merged caller graph (it has no
/// transitive/test/type sections), so the daemon adapter emits the honest
/// `_meta.overlay_graph = "full"` marker gated on the returned participation
/// bool. Participation is true iff a direction changed the dead set; an active
/// overlay whose delta is irrelevant returns the parent dead set untouched and
/// reports `false`.
///
/// Scope note: a worktree-ADDED function (its def is only in the overlay store,
/// not the parent) that nothing calls is not surfaced — `find_dead_code` runs
/// over the parent store, so an overlay-only def is invisible to the candidate
/// scan. Consistent with dead-code's prefer-under-reporting bias; the
/// already-present-function flips this PR targets are fully covered.
pub(crate) fn dead_overlay(
    store: &cqs::Store<cqs::store::ReadOnly>,
    root: &Path,
    args: &DeadArgs,
    overlay: Option<&cqs::worktree_overlay::WorktreeOverlay>,
) -> Result<(DeadOutput, bool)> {
    let _span = tracing::info_span!(
        "dead_overlay",
        include_pub = args.include_pub,
        overlay = overlay.is_some()
    )
    .entered();
    let (confident, possibly_pub) = store
        .find_dead_code(args.include_pub)
        .context("Failed to detect dead code")?;

    // §2 additive overlay (`cqs dead` ONLY): the heuristic-only-callee
    // population. `find_dead_code` now holds the strict zero-edge contract
    // (restoring `health`/`ci`/`suggest` to never-report-heuristic-live-as-dead),
    // so these names are NOT among its candidates. We query them separately and
    // UNION them into this report, where the classifier relabels them
    // `low-confidence-live`. The two populations are disjoint by construction
    // (zero-edge vs has-a-heuristic-edge), so the union never double-counts.
    let (low_confident, low_possibly_pub) = store
        .find_low_confidence_live_functions(args.include_pub)
        .context("Failed to detect low-confidence-live functions")?;

    let mut confident: Vec<_> = confident
        .into_iter()
        .chain(low_confident)
        .filter(|d| d.confidence >= args.min_confidence)
        .collect();
    let mut possibly_pub: Vec<_> = possibly_pub
        .into_iter()
        .chain(low_possibly_pub)
        .filter(|d| d.confidence >= args.min_confidence)
        .collect();

    // §1-derived heuristic-caller breakdown (kind + count), keyed by callee
    // name. Drives the `low-confidence-live` verdict reason string for the
    // unioned entries above.
    let low_conf = store
        .find_low_confidence_live_names()
        .context("Failed to query low-confidence-live names")?;

    // Worktree-overlay merge over the parent dead populations.
    let participated = if let Some(ov) = overlay {
        apply_dead_overlay(store, ov, &mut confident, &mut possibly_pub, args)?
    } else {
        false
    };

    let mut output = build_dead_output(&confident, &possibly_pub, root, &low_conf);

    // Apply the `--verdict` filter to both lists, then recount.
    if let Some(want) = args.verdict {
        let want = want.as_str();
        output.dead.retain(|e| e.verdict == want);
        output.possibly_dead_pub.retain(|e| e.verdict == want);
        output.count = output.dead.len();
        output.possibly_pub_count = output.possibly_dead_pub.len();
    }

    Ok((output, participated))
}

/// Whether `name` has ≥1 real-caller edge (excludes `doc_reference`) in `store`.
/// A best-effort store read: a query error logs and degrades to `false` (treats
/// the function as having no real caller), which is the safe direction for the
/// overlay merge — it never resurrects a function on a failed read.
fn has_real_caller<M>(store: &cqs::Store<M>, name: &str) -> bool {
    match store.get_callers_full(name) {
        Ok(callers) => callers.iter().any(|c| c.edge_kind.is_real_caller()),
        Err(e) => {
            tracing::warn!(error = %e, name, "overlay get_callers_full failed; treating as no real caller");
            false
        }
    }
}

/// Apply the worktree-overlay merge to the parent dead populations, in place.
/// Returns whether either direction changed the set (the participation signal
/// the daemon gates `_meta.overlay_graph = "full"` on).
fn apply_dead_overlay(
    store: &cqs::Store<cqs::store::ReadOnly>,
    ov: &cqs::worktree_overlay::WorktreeOverlay,
    confident: &mut Vec<DeadFunction>,
    possibly_pub: &mut Vec<DeadFunction>,
    args: &DeadArgs,
) -> Result<bool> {
    let mut participated = false;

    // ── Direction A: parent-dead → live ──────────────────────────────────────
    // Drop any parent-dead entry the worktree now really-calls. (A worktree file
    // added a real call edge to a previously-uncalled function.)
    let before_dead = confident.len();
    let before_pub = possibly_pub.len();
    confident.retain(|d| !has_real_caller(&ov.store, &d.chunk.name));
    possibly_pub.retain(|d| !has_real_caller(&ov.store, &d.chunk.name));
    if confident.len() != before_dead || possibly_pub.len() != before_pub {
        participated = true;
    }

    // ── Direction B: parent-live → dead ──────────────────────────────────────
    // Candidates = functions the delta files used to call (parent-side). For each
    // not already dead, recompute the merged caller set and add it if it now has
    // zero real callers.
    let masked: Vec<String> = ov
        .masked_origins
        .iter()
        .map(|p| cqs::normalize_path(p).to_string())
        .collect();
    let candidates = store
        .distinct_callees_from_origins(&masked)
        .context("Failed to resolve delta-file callees for dead overlay")?;

    // Names already present in the dead set (either direction) must not be
    // re-added.
    let already: std::collections::HashSet<&str> = confident
        .iter()
        .chain(possibly_pub.iter())
        .map(|d| d.chunk.name.as_str())
        .collect();

    let mut additions: Vec<DeadFunction> = Vec::new();
    for name in &candidates {
        if already.contains(name.as_str()) {
            continue;
        }
        // The parent definition of this name must be a callable, top-level,
        // non-test chunk to be a dead candidate at all (mirrors
        // `fetch_uncalled_functions`'s Tier-1 filters). When the def lives in a
        // delta file the worktree chunk carries the current line range, so prefer
        // the overlay def; fall back to the parent def.
        let def = match resolve_dead_candidate_def(store, ov, name) {
            Some(c) => c,
            None => continue,
        };

        // Merged caller set: parent callers minus delta-origin call-sites, plus
        // the overlay's callers. Dead iff zero REAL callers survive.
        let parent_callers = store
            .get_callers_full(name)
            .context("Failed to load parent callers for dead overlay candidate")?;
        let overlay_callers = ov
            .store
            .get_callers_full(name)
            .context("Failed to load overlay callers for dead overlay candidate")?;
        let merged = ov.merge_callers(parent_callers, overlay_callers);
        let has_real = merged.iter().any(|c| c.edge_kind.is_real_caller());
        if has_real {
            continue;
        }

        // Newly dead. `Medium` is the honest floor — the file-activity recompute
        // `score_confidence` runs for the parent set is not re-run here.
        let dead_fn = DeadFunction {
            chunk: def,
            confidence: cqs::store::DeadConfidence::Medium,
        };
        if dead_fn.confidence < args.min_confidence {
            continue;
        }
        additions.push(dead_fn);
    }
    if !additions.is_empty() {
        participated = true;
        for dead_fn in additions {
            // Route public defs to the possibly-dead-pub list (unless
            // --include-pub), matching `score_confidence`'s visibility split.
            let is_pub = dead_fn.chunk.signature.starts_with("pub ")
                || dead_fn.chunk.signature.starts_with("pub(");
            if is_pub && !args.include_pub {
                possibly_pub.push(dead_fn);
            } else {
                confident.push(dead_fn);
            }
        }
    }

    Ok(participated)
}

/// Resolve the definition chunk for a Direction-B dead candidate, applying the
/// Tier-1 admissibility filters `fetch_uncalled_functions` applies so an
/// overlay-added entry can never be a shape the parent path would have excluded:
/// the def must be a callable, NON-`Property`, NON-doc-path (`is_dead_doc_path`),
/// top-level (`parent_id` None) chunk. The non-test exclusion is added on top —
/// `fetch_uncalled_functions` does not test-filter at the SQL layer (it filters
/// later in `filter_candidates`), so excluding tests here is the safe
/// (under-report) direction. Prefers the overlay store's def (current worktree
/// line range) when the name is defined there, else the parent's. Returns `None`
/// for a name with no admissible def (an external/std call, a test, a nested
/// chunk, a property, a doc-block fn).
fn resolve_dead_candidate_def(
    store: &cqs::Store<cqs::store::ReadOnly>,
    ov: &cqs::worktree_overlay::WorktreeOverlay,
    name: &str,
) -> Option<cqs::store::ChunkSummary> {
    let admissible = |c: &cqs::store::ChunkSummary| -> bool {
        c.chunk_type.is_callable()
            && c.chunk_type != cqs::parser::ChunkType::Property
            && c.parent_id.is_none()
            && !cqs::store::is_dead_doc_path(&c.file.to_string_lossy())
            && !cqs::is_test_chunk(&c.name, &c.file.to_string_lossy())
    };
    // Prefer the overlay def (worktree line range) when present + admissible.
    if let Ok(chunks) = ov.store.get_chunks_by_name(name) {
        if let Some(c) = chunks.into_iter().find(|c| admissible(c)) {
            return Some(c);
        }
    }
    match store.get_chunks_by_name(name) {
        Ok(chunks) => chunks.into_iter().find(|c| admissible(c)),
        Err(e) => {
            tracing::warn!(error = %e, name, "dead overlay candidate def lookup failed");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build the typed dead-code report shared between CLI and batch. Each entry
/// is classified into a [`DeadVerdict`] via the ordered `classify_verdict`;
/// `low_conf` is the §1-derived heuristic-caller breakdown keyed by callee name.
pub(crate) fn build_dead_output(
    confident: &[DeadFunction],
    possibly_pub: &[DeadFunction],
    root: &Path,
    low_conf: &std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo>,
) -> DeadOutput {
    let _span = tracing::info_span!(
        "build_dead_output",
        confident = confident.len(),
        possibly = possibly_pub.len()
    )
    .entered();

    let format = |d: &DeadFunction| {
        let (verdict, reason) = classify_verdict(d, low_conf);
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
                reason
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

    /// Empty low-confidence map for classifier tests with no heuristic callers.
    fn no_low_conf() -> std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo> {
        std::collections::HashMap::new()
    }

    /// Low-confidence map with one heuristic-only callee `name`.
    fn low_conf_with(
        name: &str,
    ) -> std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo> {
        let mut m = std::collections::HashMap::new();
        m.insert(
            name.to_string(),
            cqs::store::LowConfidenceLiveInfo {
                total: 1,
                kind_counts: vec![("macro_heuristic".to_string(), 1)],
            },
        );
        m
    }

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
        let (v, _) = classify_verdict(&f, &no_low_conf());
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
        let (v, _) = classify_verdict(&f, &no_low_conf());
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
        let (v, reason) = classify_verdict(&f, &low_conf_with("cb"));
        assert_eq!(v, DeadVerdict::LowConfidenceLive);
        // Reason names the heuristic kinds/counts, not a generic claim.
        assert!(
            reason.contains("macro_heuristic") && reason.contains("heuristic edge"),
            "reason should name heuristic kinds: {reason}"
        );
    }

    #[test]
    fn verdict_known_gap_js_asset() {
        let f = dead_fn(
            "onClick",
            "src/serve/assets/app.js",
            cqs::parser::Language::JavaScript,
            "function onClick() {}",
        );
        let (v, _) = classify_verdict(&f, &no_low_conf());
        assert_eq!(v, DeadVerdict::KnownGap);
    }

    /// A non-served build script (`scripts/build.mjs`) with zero callers is
    /// genuinely dead — the known-gap excuse is scoped to served-assets paths,
    /// so an `.mjs` outside `src/serve/assets/` classifies as `dead`, NOT
    /// `known-gap` — the served-asset excuse is scoped to served paths.
    #[test]
    fn verdict_non_served_mjs_is_dead_not_known_gap() {
        let f = dead_fn(
            "buildBundle",
            "scripts/build.mjs",
            cqs::parser::Language::JavaScript,
            "function buildBundle() {}",
        );
        let (v, _) = classify_verdict(&f, &no_low_conf());
        assert_eq!(
            v,
            DeadVerdict::Dead,
            "a build script outside served-assets must not get the known-gap excuse"
        );
    }

    /// A doc mention does NOT disqualify a function from
    /// `low-confidence-live`. The store-side query treats `doc_reference` edges
    /// as inert (neither trusted nor heuristic), so a function reached by a doc
    /// reference plus a macro edge still appears in `low_conf` and the classifier
    /// labels it `low-confidence-live`, not `dead`.
    #[test]
    fn verdict_doc_reference_does_not_block_low_confidence_live() {
        let f = dead_fn(
            "doc_mentioned",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn doc_mentioned() {}",
        );
        // The map carries it because the store query found a heuristic edge and
        // no trusted edge; the doc edge was ignored.
        let (v, _) = classify_verdict(&f, &low_conf_with("doc_mentioned"));
        assert_eq!(v, DeadVerdict::LowConfidenceLive);
    }

    #[test]
    fn verdict_known_gap_python_dunder() {
        let f = dead_fn(
            "__aenter__",
            "src/ctx.py",
            cqs::parser::Language::Python,
            "def __aenter__(self): ...",
        );
        let (v, _) = classify_verdict(&f, &no_low_conf());
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
        let (v, _) = classify_verdict(&f, &no_low_conf());
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
        let (v, _) = classify_verdict(&f, &low_conf_with("make_x"));
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
        let low = no_low_conf();
        let f = dead_fn("x", "src/lib.rs", cqs::parser::Language::Rust, "fn x() {}");
        let out = build_dead_output(&[f], &[], std::path::Path::new("."), &low);
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["dead"][0]["verdict"], "dead");
        assert!(json["dead"][0].get("verdict_reason").is_some());
    }
}
