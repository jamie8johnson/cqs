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
#[derive(Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
// kebab-case on the schema to match the stable strings the `--verdict` filter /
// `de_opt_verdict` deserializer accept (`test-only`, `low-confidence-live`,
// `known-gap`). schemars reads serde attributes even without a serde derive.
#[serde(rename_all = "kebab-case")]
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
    /// Returns true when this entry sits in the rule's known gap. Takes the whole
    /// [`DeadFunction`] so a rule can consult the chunk's structural metadata
    /// (`chunk_type`, `parent_type_name`), not only its name/origin/language —
    /// the external-trait-method gap needs `chunk_type` to tell a real trait-impl
    /// method from a free function that merely shares a framework method name.
    matches: fn(entry: &DeadFunction) -> bool,
    /// Reason surfaced as `verdict_reason` on a `known-gap` entry.
    reason: &'static str,
}

/// Whether `entry`'s origin is a served front-end asset — an `.js`/`.mjs` file
/// under a served-assets directory whose handlers are wired from HTML
/// (`onclick="..."`, `addEventListener`) rather than a syntactic call. SCOPED to
/// the served path: the gap is "HTML-wired served assets", not "any .js
/// anywhere". A build script like `scripts/build.mjs` with zero callers is
/// genuinely dead and must NOT be excused. The prefix table is a fixed
/// compile-time `const` allowlist (not runtime-configurable); a new
/// served-assets root is added by editing `SERVED_ASSET_PREFIXES`.
fn is_served_js_asset(entry: &DeadFunction) -> bool {
    /// Served-assets directory prefixes. Origins are normalized to forward
    /// slashes before classification, so only `/`-form prefixes are listed.
    const SERVED_ASSET_PREFIXES: &[&str] = &["src/serve/assets/"];
    let origin = entry.chunk.file.to_string_lossy();
    let is_js = origin.ends_with(".js") || origin.ends_with(".mjs");
    is_js
        && SERVED_ASSET_PREFIXES
            .iter()
            .any(|prefix| origin.starts_with(prefix))
}

/// Whether `entry` is a Python runtime-invoked dunder protocol method
/// (`__aenter__`, `__exit__`, `__iter__`, …): invoked by the runtime
/// (context-manager / iterator / async protocols), never by a syntactic call.
fn is_python_dunder(entry: &DeadFunction) -> bool {
    let lang = entry.chunk.language.to_string();
    let name = entry.chunk.name.as_str();
    lang == "python" && name.starts_with("__") && name.ends_with("__")
}

/// Whether `entry` is a Rust METHOD that implements an EXTERNAL framework trait
/// invoked by dynamic dispatch — the framework calls it through a trait object /
/// generic bound, never by a syntactic call the static graph can see. The
/// implemented-trait name is not in chunk metadata (only the parent Type), so
/// this keys on a tightly-scoped EXPLICIT method-name allowlist, the direct Rust
/// analog of [`is_python_dunder`] / [`is_served_js_asset`]. It is NOT a broad
/// "any trait method" heuristic — only the listed names, only for Rust, AND only
/// for chunks the parser tagged `ChunkType::Method` are excused; a local `fmt`
/// or `do_thing`, a non-Rust method sharing a name, or a FREE FUNCTION that
/// merely shares a framework method name (a `ChunkType::Function` named
/// `visit_seq` that lives in no `impl Trait for Type`) stays in the actionable
/// `dead` residue. The `ChunkType::Method` gate is the robust structural signal
/// that the name is a real trait-impl method rather than an adversarial
/// free-function namesake. (`parent_type_name` is NOT consulted: the dead-code
/// Phase-2 hydration path does not populate it, so it is always absent in the
/// real sweep — `chunk_type` is the only structural signal available there.)
///
/// Allowlist members:
/// - the serde `Visitor` family (`Deserialize` drives these via the
///   `Deserializer` callbacks — the deserializer picks which `visit_*` to call
///   at runtime, so no caller is ever syntactically present), plus `expecting`,
/// - `hnsw_filter`, the `hnsw_rs::FilterT` trait method the ANN search invokes
///   through a `&dyn FilterT` predicate.
fn is_external_trait_method(entry: &DeadFunction) -> bool {
    /// Framework-dispatched Rust trait methods. Held as an explicit set (not a
    /// name-shape rule) so the excuse never widens past methods known to be
    /// invoked by a runtime/framework rather than a syntactic call. The serde
    /// names are the full `Visitor` trait surface (serde 1.0).
    const EXTERNAL_TRAIT_METHODS: &[&str] = &[
        // serde `Visitor` family.
        "expecting",
        "visit_bool",
        "visit_i8",
        "visit_i16",
        "visit_i32",
        "visit_i64",
        "visit_i128",
        "visit_u8",
        "visit_u16",
        "visit_u32",
        "visit_u64",
        "visit_u128",
        "visit_f32",
        "visit_f64",
        "visit_char",
        "visit_str",
        "visit_borrowed_str",
        "visit_string",
        "visit_bytes",
        "visit_borrowed_bytes",
        "visit_byte_buf",
        "visit_none",
        "visit_some",
        "visit_unit",
        "visit_newtype_struct",
        "visit_seq",
        "visit_map",
        "visit_enum",
        // hnsw_rs `FilterT`.
        "hnsw_filter",
    ];
    let lang = entry.chunk.language.to_string();
    let name = entry.chunk.name.as_str();
    // Robust structural gate: only a real trait-impl method (`ChunkType::Method`)
    // is excused. A free function (`ChunkType::Function`) sharing the name is not
    // framework-dispatched and stays `dead`.
    entry.chunk.chunk_type == cqs::parser::ChunkType::Method
        && lang == "rust"
        && EXTERNAL_TRAIT_METHODS.contains(&name)
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
    KnownGapRule {
        matches: is_external_trait_method,
        reason: "external trait method — invoked by the framework via dynamic dispatch, not a syntactic call",
    },
];

/// Classify a dead entry against the static known-gap table. Returns the reason
/// of the first matching rule, or `None` if no documented gap applies.
fn known_gap_reason(entry: &DeadFunction) -> Option<&'static str> {
    KNOWN_GAP_RULES
        .iter()
        .find(|rule| (rule.matches)(entry))
        .map(|rule| rule.reason)
}

/// Whether `content` carries a real `#[cfg(test)]` ATTRIBUTE (not the same text
/// appearing inside a comment or string literal). Position-anchored, not a raw
/// substring: a Rust attribute sits at the start of a line modulo leading
/// whitespace, so a line qualifies only when its first non-whitespace run is
/// `#[cfg(test)]`. A comment-spoofed `// #[cfg(test)]` has `//` as the
/// first non-whitespace and is rejected, so an attacker cannot demote a
/// genuinely-dead function to `test-only` by planting the attribute text in a
/// comment. (Still a heuristic — a `cfg(test)`-gated attribute split across
/// lines, or one inside a multiline string, is out of scope; the structural
/// `ChunkType::Test` tag is the authoritative test signal and is checked first.)
fn contains_cfg_test_attr(content: &str) -> bool {
    content
        .lines()
        .any(|line| line.trim_start().starts_with("#[cfg(test)]"))
}

/// Whether a dead entry is test-only: the chunk is tagged `ChunkType::Test`, its
/// origin is under a `tests/` path segment, or the chunk content carries a real
/// `#[cfg(test)]` attribute at a line start. The content check is
/// position-anchored ([`contains_cfg_test_attr`]) so a comment mentioning
/// `#[cfg(test)]` does NOT keep the function classified test-only — an adversary
/// cannot hide a genuinely-dead function from the actionable `dead` list by
/// planting the attribute text in a comment.
fn is_test_only(entry: &DeadFunction) -> bool {
    // Chunk-type tag: the parser already classifies a `#[test]` function as
    // `ChunkType::Test`. A bare `#[test]` fn's byte range carries only the
    // `#[test]` attribute — the enclosing `#[cfg(test)]` lives on the `mod
    // tests`, outside this chunk — so the content scan below cannot see it. The
    // tag is the authoritative signal; rely on it first.
    if entry.chunk.chunk_type == cqs::parser::ChunkType::Test {
        return true;
    }
    let origin = entry.chunk.file.to_string_lossy();
    // Origin-prefix: a `tests/` path segment (also `/tests/`, `\tests\`).
    if origin.starts_with("tests/") || origin.contains("/tests/") || origin.contains("\\tests\\") {
        return true;
    }
    // Enclosing #[cfg(test)] module: the chunk content carries the attribute
    // when the chunk is the module, or the function lives directly under it.
    // The store can't answer the module-chain question without a parse, so this
    // degrades to a position-anchored attribute scan (a comment-spoofed
    // `// #[cfg(test)]` is rejected; documented limitation otherwise).
    contains_cfg_test_attr(&entry.chunk.content)
}

/// Classify a dead entry into its verdict. Ordered, first-match-wins:
/// test-only → low-confidence-live → known-gap → dead. `low_conf` maps a
/// callee name to its §1-derived heuristic-caller breakdown (present only for
/// functions reached solely by heuristic edges). `overlay_candidate` maps a
/// callee name to its candidate-edge breakdown recomputed over the merged
/// (parent+overlay) candidate graph — consulted for Direction-B additions, whose
/// parent-truth `low_conf` is stale.
fn classify_verdict(
    entry: &DeadFunction,
    low_conf: &std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo>,
    overlay_candidate: &std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo>,
) -> (DeadVerdict, String) {
    if is_test_only(entry) {
        // Claim only what the test-only check actually knows: a `ChunkType::Test`
        // tag (a `#[test]` function), a `tests/` path segment, or a real
        // `#[cfg(test)]` attribute at a line start in the chunk content (a
        // comment-spoofed mention is rejected — position-anchored, not substring).
        return (
            DeadVerdict::TestOnly,
            "test chunk, origin under tests/ path, or a line-start '#[cfg(test)]' attribute"
                .to_string(),
        );
    }
    // Pick the liveness-evidence map for this entry. A Direction-B overlay
    // addition (`overlay_dead`) was computed dead over the authoritative merged
    // (parent+overlay) caller graph in this worktree, so its parent-graph-derived
    // `low_conf` HEURISTIC breakdown is stale — consulting it would relabel a
    // genuinely-worktree-dead function `low-confidence-live` whenever its bare
    // name collides with a parent heuristic/candidate name, hiding it from
    // `--verdict dead`. For these we consult only the CANDIDATE map, which is
    // recomputed over the merged candidate graph (mask-then-union of parent +
    // overlay `candidate_edges`), so a candidate-only addition — dead over the
    // merged real caller graph but still referenced by a worktree candidate edge
    // — correctly relabels `low-confidence-live`. A parent entry keeps the full
    // parent `low_conf` breakdown (heuristic + candidate). The remaining tiers
    // (known-gap, dead) are name/path/content-derived and stay valid for both.
    let evidence = if entry.overlay_dead {
        overlay_candidate
    } else {
        low_conf
    };
    if let Some(info) = evidence.get(&entry.chunk.name) {
        // Name the exact provenance and counts rather than asserting "all
        // callers are heuristic" generically. Two populations may contribute:
        // heuristic `function_calls` edges and `candidate_edges` (Lane 2)
        // references. A candidate-ONLY callee has `total == 0` and
        // `candidate_total > 0`; render only the populations that are present so
        // the reason never claims "0 heuristic edge(s)".
        let mut parts: Vec<String> = Vec::new();
        if info.total > 0 {
            let kinds = info
                .kind_counts
                .iter()
                .map(|(kind, n)| format!("{kind}×{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("{} heuristic edge(s) [{}]", info.total, kinds));
        }
        if info.candidate_total > 0 {
            let kinds = info
                .candidate_counts
                .iter()
                .map(|(kind, n)| format!("{kind}×{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!(
                "{} candidate edge(s) [{}]",
                info.candidate_total, kinds
            ));
        }
        let detail = if parts.is_empty() {
            // Defensive: the name is in the map only when one population is
            // nonzero, so this is unreachable in practice.
            "heuristic/candidate evidence".to_string()
        } else {
            parts.join("; ")
        };
        return (
            DeadVerdict::LowConfidenceLive,
            format!("no trusted caller; reached only by {detail}"),
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
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
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

    // Worktree-overlay merge over the parent dead populations. Delegates to the
    // shared lib merge (`cqs::store::apply_dead_overlay`) so `cqs dead` and
    // `cqs ci` cannot drift.
    let participated = if let Some(ov) = overlay {
        cqs::store::apply_dead_overlay(
            store,
            ov,
            &mut confident,
            &mut possibly_pub,
            args.include_pub,
            args.min_confidence,
        )?
    } else {
        false
    };

    // Overlay-merged candidate map (mask-then-union of parent + overlay
    // `candidate_edges`), keyed by callee name. The verdict classifier consults
    // it for Direction-B additions, whose parent-truth `low_conf` is stale: a
    // candidate-only addition relabels `low-confidence-live` instead of `dead`.
    // Empty when there is no overlay (parent entries never consult it).
    let overlay_candidate = match overlay {
        Some(ov) => cqs::store::build_overlay_candidate_map(store, ov)
            .context("Failed to build overlay candidate map")?,
        None => std::collections::HashMap::new(),
    };

    let mut output = build_dead_output(
        &confident,
        &possibly_pub,
        root,
        &low_conf,
        &overlay_candidate,
    );

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

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build the typed dead-code report shared between CLI and batch. Each entry
/// is classified into a [`DeadVerdict`] via the ordered `classify_verdict`;
/// `low_conf` is the §1-derived heuristic-caller breakdown keyed by callee name.
/// `overlay_candidate` is the overlay-merged candidate breakdown (empty without
/// an overlay) the classifier consults for Direction-B additions.
pub(crate) fn build_dead_output(
    confident: &[DeadFunction],
    possibly_pub: &[DeadFunction],
    root: &Path,
    low_conf: &std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo>,
    overlay_candidate: &std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo>,
) -> DeadOutput {
    let _span = tracing::info_span!(
        "build_dead_output",
        confident = confident.len(),
        possibly = possibly_pub.len()
    )
    .entered();

    let format = |d: &DeadFunction| {
        let (verdict, reason) = classify_verdict(d, low_conf, overlay_candidate);
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

    /// Empty overlay-merged candidate map. Parent-form classifier tests (no
    /// overlay) pass this so the `overlay_dead` consult never fires.
    fn no_overlay_cand() -> std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo> {
        std::collections::HashMap::new()
    }

    /// Parent-form classify wrapper: no overlay candidate map. The bulk of the
    /// verdict tests classify a parent entry (`overlay_dead = false`), which
    /// never consults the overlay map, so this keeps them at two args.
    fn classify_parent(
        entry: &DeadFunction,
        low_conf: &std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo>,
    ) -> (DeadVerdict, String) {
        classify_verdict(entry, low_conf, &no_overlay_cand())
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
                candidate_total: 0,
                candidate_counts: vec![],
            },
        );
        m
    }

    /// Low-confidence map with one CANDIDATE-ONLY callee `name`: zero heuristic
    /// `function_calls` edges, one `candidate_edges` (Lane 2) reference of
    /// `kind`. Models the candidate-edge campaign Lane-3 flip.
    fn candidate_only_low_conf(
        name: &str,
        kind: &str,
    ) -> std::collections::HashMap<String, cqs::store::LowConfidenceLiveInfo> {
        let mut m = std::collections::HashMap::new();
        m.insert(
            name.to_string(),
            cqs::store::LowConfidenceLiveInfo {
                total: 0,
                kind_counts: vec![],
                candidate_total: 1,
                candidate_counts: vec![(kind.to_string(), 1)],
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
            overlay_dead: false,
        }
    }

    /// Like [`dead_fn`] but tags the chunk `ChunkType::Method` — a real
    /// trait-impl method. Used by the external-trait-method gap tests, whose
    /// known-gap excuse is now structurally gated on `ChunkType::Method` (a free
    /// function sharing a framework method name stays `dead`).
    fn dead_method(
        name: &str,
        origin: &str,
        language: cqs::parser::Language,
        content: &str,
    ) -> DeadFunction {
        let mut f = dead_fn(name, origin, language, content);
        f.chunk.chunk_type = cqs::parser::ChunkType::Method;
        f
    }

    #[test]
    fn verdict_test_only_by_origin_prefix() {
        let f = dead_fn(
            "make_x",
            "tests/helpers.rs",
            cqs::parser::Language::Rust,
            "fn make_x() {}",
        );
        let (v, _) = classify_parent(&f, &no_low_conf());
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
        let (v, _) = classify_parent(&f, &no_low_conf());
        assert_eq!(v, DeadVerdict::TestOnly);
    }

    /// Adversarial-content regression: an attacker can plant the literal
    /// `#[cfg(test)]` in a COMMENT inside a genuinely-dead function to try to
    /// demote it to `test-only` (hiding it from `--verdict dead`). The content
    /// scan is
    /// position-anchored ([`contains_cfg_test_attr`]) — only a real attribute at a
    /// line start counts — so a `// #[cfg(test)]` comment is rejected and the
    /// function classifies `dead`. Calibration: the SAME text as a real
    /// line-start attribute (`verdict_test_only_by_cfg_test_content`) classifies
    /// `test-only`, so line position is exactly what flips the verdict. Also
    /// guards an indented attribute (a `#[cfg(test)]` inside a `mod` block) — a
    /// real attribute modulo leading whitespace stays `test-only`.
    #[test]
    fn verdict_cfg_test_in_comment_only_stays_dead() {
        let commented = dead_fn(
            "looks_dead",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn looks_dead() {\n    // #[cfg(test)] this is only a comment\n}",
        );
        let (v, _) = classify_parent(&commented, &no_low_conf());
        assert_eq!(
            v,
            DeadVerdict::Dead,
            "a #[cfg(test)] mention only in a comment must NOT demote a dead fn to test-only"
        );

        // A real attribute, indented (e.g. inside a `mod` block), still counts —
        // position-anchoring trims leading whitespace, only the comment marker is
        // what disqualifies the spoof.
        let real_indented = dead_fn(
            "real_helper",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "mod outer {\n    #[cfg(test)]\n    mod t { fn real_helper() {} }\n}",
        );
        let (v_real, _) = classify_parent(&real_indented, &no_low_conf());
        assert_eq!(
            v_real,
            DeadVerdict::TestOnly,
            "a real indented #[cfg(test)] attribute must still classify test-only"
        );
    }

    /// A `#[test]` function classifies `test-only` even when its content lacks
    /// `#[cfg(test)]` and its origin is NOT under a `tests/` path. The parser
    /// tags it `ChunkType::Test`; a bare `#[test]` fn's byte range carries only
    /// `#[test]` (the enclosing `#[cfg(test)]` is on the `mod tests`, outside the
    /// chunk), so the content substring scan alone misses it — the chunk-type tag
    /// is what catches it. RED before the fix: with neither the `tests/` prefix
    /// nor `#[cfg(test)]` in content, the old `is_test_only` returned false and
    /// the entry mis-classified `dead`. Calibration: the SAME chunk built as
    /// `ChunkType::Function` (via `dead_fn`) classifies `dead`, so the tag is what
    /// flips the verdict.
    #[test]
    fn verdict_test_only_by_chunk_type_tag() {
        let mut f = dead_fn(
            "it_works",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            // Only `#[test]` is inside the chunk — no `#[cfg(test)]`, no tests/
            // path. The content scan and origin-prefix check both miss it.
            "#[test]\nfn it_works() { assert!(true); }",
        );
        f.chunk.chunk_type = cqs::parser::ChunkType::Test;
        let (v, reason) = classify_parent(&f, &no_low_conf());
        assert_eq!(
            v,
            DeadVerdict::TestOnly,
            "a #[test] fn (ChunkType::Test) must classify test-only even without \
             #[cfg(test)] in content or a tests/ origin"
        );
        assert!(
            reason.contains("test chunk"),
            "reason should name the test-chunk tag: {reason}"
        );

        // Calibration: the identical chunk tagged Function (the `dead_fn`
        // default) has no other test-only signal, so it is `dead`. The tag is
        // exactly what flips the verdict.
        let f_fn = dead_fn(
            "it_works",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "#[test]\nfn it_works() { assert!(true); }",
        );
        let (v_fn, _) = classify_parent(&f_fn, &no_low_conf());
        assert_eq!(
            v_fn,
            DeadVerdict::Dead,
            "without the ChunkType::Test tag, the same chunk would be dead"
        );
    }

    #[test]
    fn verdict_low_confidence_live_consumes_edge_kind_set() {
        let f = dead_fn(
            "cb",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn cb() {}",
        );
        let (v, reason) = classify_parent(&f, &low_conf_with("cb"));
        assert_eq!(v, DeadVerdict::LowConfidenceLive);
        // Reason names the heuristic kinds/counts, not a generic claim.
        assert!(
            reason.contains("macro_heuristic") && reason.contains("heuristic edge"),
            "reason should name heuristic kinds: {reason}"
        );
    }

    /// A candidate-ONLY callee (zero heuristic `function_calls` edges, present
    /// only in `candidate_edges`) classifies `low-confidence-live`, and the
    /// reason names the candidate kind/count — NOT a generic "0 heuristic
    /// edge(s)" claim. Calibration: an empty `low_conf` map would classify the
    /// same entry `dead` (see `verdict_dead_residue`), so the consult is what
    /// flips the verdict.
    #[test]
    fn verdict_candidate_only_callee_is_low_confidence_live() {
        let f = dead_fn(
            "maybe_fn",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn maybe_fn() {}",
        );
        let (v, reason) = classify_parent(
            &f,
            &candidate_only_low_conf("maybe_fn", "bare_arg_unresolved"),
        );
        assert_eq!(
            v,
            DeadVerdict::LowConfidenceLive,
            "candidate-only callee must be low-confidence-live, not dead"
        );
        assert!(
            reason.contains("candidate edge") && reason.contains("bare_arg_unresolved"),
            "reason must name the candidate kind/count, not a generic heuristic claim: {reason}"
        );
        assert!(
            !reason.contains("heuristic edge"),
            "candidate-only reason must NOT claim heuristic edges (it has zero): {reason}"
        );

        // Calibration: the SAME entry with an empty map is `dead`.
        let (v_dead, _) = classify_parent(&f, &no_low_conf());
        assert_eq!(
            v_dead,
            DeadVerdict::Dead,
            "without the candidate consult, the same entry would be dead"
        );
    }

    /// Seam guard (overlay × verdict-classification): a Direction-B overlay
    /// addition (`overlay_dead = true`) skips the parent-truth `low_conf`
    /// HEURISTIC map — that map is parent-graph-derived and stale under the
    /// overlay, so a genuinely-worktree-dead function whose name collides with a
    /// parent heuristic name must NOT be relabeled `low-confidence-live` (which
    /// would hide it from `--verdict dead`). The overlay_dead leg here passes the
    /// stale map as `low_conf` and an EMPTY overlay candidate map, so with no
    /// merged-candidate evidence it classifies `dead`. Calibration: the same map
    /// applied to the same name with `overlay_dead = false` (a parent entry, via
    /// `classify_parent`) yields `low-confidence-live`. So the flag, not the map,
    /// is what flips the verdict. (The companion candidate-recompute relabel — an
    /// overlay_dead entry WITH merged-candidate evidence → low-confidence-live —
    /// is pinned by `verdict_overlay_dead_candidate_recompute_relabels`.)
    #[test]
    fn verdict_overlay_dead_bypasses_parent_low_conf_relabel() {
        let mut f = dead_fn(
            "foo",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn foo() {}",
        );
        // Calibration: the parent-entry form (overlay_dead = false) collides
        // with the parent candidate map → low-confidence-live.
        let (v_parent, _) =
            classify_parent(&f, &candidate_only_low_conf("foo", "bare_arg_unresolved"));
        assert_eq!(
            v_parent,
            DeadVerdict::LowConfidenceLive,
            "calibration: a PARENT entry whose name is in the candidate map is low-confidence-live"
        );

        // Mark it a Direction-B overlay addition. With the stale parent map as
        // `low_conf` and an EMPTY overlay candidate map, it must classify `dead`:
        // the parent heuristic map is skipped and there is no merged-candidate
        // evidence.
        f.overlay_dead = true;
        let (v, reason) = classify_verdict(
            &f,
            &candidate_only_low_conf("foo", "bare_arg_unresolved"),
            &no_overlay_cand(),
        );
        assert_eq!(
            v,
            DeadVerdict::Dead,
            "an overlay-dead addition must classify `dead`, NOT be relabeled by the stale \
             parent low_conf map (or it is filtered out of `--verdict dead`)"
        );
        assert!(
            reason.contains("no callers"),
            "overlay-dead reason must be the plain dead reason, not a low-conf relabel: {reason}"
        );
    }

    /// The candidate-recompute fix (this PR): a Direction-B overlay addition
    /// (`overlay_dead = true`) that IS named in the overlay-merged candidate map
    /// — a function dead over the merged real caller graph but still referenced
    /// by a worktree candidate edge — relabels `low-confidence-live`, not `dead`.
    /// RED before the fix: the classifier forced `dead` for every `overlay_dead`
    /// entry (the blanket `!overlay_dead` skip), ignoring any candidate evidence.
    /// Calibration: the SAME entry with an EMPTY overlay candidate map is `dead`
    /// (see `verdict_overlay_dead_bypasses_parent_low_conf_relabel`), so the
    /// merged-candidate consult is exactly what flips the verdict.
    #[test]
    fn verdict_overlay_dead_candidate_recompute_relabels() {
        let mut f = dead_fn(
            "maybe_overlay_fn",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn maybe_overlay_fn() {}",
        );
        f.overlay_dead = true;

        // The overlay-merged candidate map names this entry (a candidate-only
        // reference the confident extractor declined to resolve). The parent
        // `low_conf` map is empty — the relabel must come from the overlay map.
        let (v, reason) = classify_verdict(
            &f,
            &no_low_conf(),
            &candidate_only_low_conf("maybe_overlay_fn", "bare_arg_unresolved"),
        );
        assert_eq!(
            v,
            DeadVerdict::LowConfidenceLive,
            "an overlay-dead entry named in the overlay-merged candidate map must \
             relabel low-confidence-live, not dead"
        );
        assert!(
            reason.contains("candidate edge") && reason.contains("bare_arg_unresolved"),
            "reason must name the merged candidate kind/count: {reason}"
        );
        assert!(
            !reason.contains("heuristic edge"),
            "candidate-only relabel must NOT claim heuristic edges: {reason}"
        );

        // Calibration: an EMPTY overlay candidate map leaves the same entry dead.
        let (v_dead, _) = classify_verdict(&f, &no_low_conf(), &no_overlay_cand());
        assert_eq!(
            v_dead,
            DeadVerdict::Dead,
            "without the merged-candidate consult, the same overlay-dead entry is dead"
        );
    }

    /// Tier ordering holds for a Direction-B addition: a GENUINELY test-only
    /// origin (under `tests/`) still classifies `test-only`, and a known-gap
    /// origin (with no candidate evidence) still classifies `known-gap`.
    /// test-only is checked BEFORE the candidate consult, so it wins even when
    /// the entry is named in the overlay-merged candidate map. known-gap is
    /// checked AFTER the candidate consult (the documented order is
    /// `test-only → low-confidence-live → known-gap → dead`), so it survives only
    /// when there is no candidate evidence — the candidate consult, like the
    /// parent `low_conf` consult, intentionally outranks known-gap.
    #[test]
    fn verdict_overlay_dead_still_honors_test_only_and_known_gap() {
        // test-only: origin under tests/ → TestOnly even when overlay_dead and
        // named in the overlay candidate map (test-only is checked first).
        let mut t = dead_fn(
            "helper",
            "tests/support.rs",
            cqs::parser::Language::Rust,
            "fn helper() {}",
        );
        t.overlay_dead = true;
        let (vt, _) = classify_verdict(
            &t,
            &no_low_conf(),
            &candidate_only_low_conf("helper", "bare_arg_unresolved"),
        );
        assert_eq!(
            vt,
            DeadVerdict::TestOnly,
            "an overlay-dead test-helper must stay test-only (test-only beats the candidate consult)"
        );

        // known-gap: a served-asset JS handler with NO candidate evidence →
        // KnownGap even when overlay_dead. (With candidate evidence the
        // documented order relabels it low-confidence-live first — same as a
        // parent known-gap entry that also has a heuristic edge.)
        let mut k = dead_fn(
            "onClick",
            "src/serve/assets/app.js",
            cqs::parser::Language::JavaScript,
            "function onClick() {}",
        );
        k.overlay_dead = true;
        let (vk, _) = classify_verdict(&k, &no_low_conf(), &no_overlay_cand());
        assert_eq!(
            vk,
            DeadVerdict::KnownGap,
            "an overlay-dead served-asset handler with no candidate evidence must stay known-gap"
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
        let (v, _) = classify_parent(&f, &no_low_conf());
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
        let (v, _) = classify_parent(&f, &no_low_conf());
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
        let (v, _) = classify_parent(&f, &low_conf_with("doc_mentioned"));
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
        let (v, _) = classify_parent(&f, &no_low_conf());
        assert_eq!(v, DeadVerdict::KnownGap);
    }

    /// A serde `Visitor` trait METHOD (`visit_seq`) implemented for an external
    /// trait is invoked by the deserializer via dynamic dispatch — the static
    /// call graph sees no syntactic caller, so it must classify `known-gap`, not
    /// `dead`. Mirrors the real false-positive at `src/hnsw/persist.rs`. The
    /// excuse is gated on `ChunkType::Method`, so the chunk is built as a method
    /// (`dead_method`) — its free-function namesake stays `dead`, see
    /// `verdict_external_trait_method_free_fn_namesake_stays_dead`.
    #[test]
    fn verdict_known_gap_visit_seq() {
        let f = dead_method(
            "visit_seq",
            "src/hnsw/persist.rs",
            cqs::parser::Language::Rust,
            "fn visit_seq<A>(self, mut seq: A) -> Result<usize, A::Error> { Ok(0) }",
        );
        let (v, reason) = classify_parent(&f, &no_low_conf());
        assert_eq!(
            v,
            DeadVerdict::KnownGap,
            "a serde Visitor method is framework-dispatched, not syntactically called"
        );
        assert!(
            reason.contains("external trait method"),
            "reason should name the external-trait-method gap: {reason}"
        );
    }

    /// The `hnsw_rs::FilterT::hnsw_filter` trait method is invoked by the ANN
    /// search through a `&dyn FilterT` predicate — no syntactic caller — so it
    /// classifies `known-gap`. Mirrors the real false-positive at
    /// `src/hnsw/search.rs`. Built as a `ChunkType::Method` (the excuse is
    /// method-gated).
    #[test]
    fn verdict_known_gap_hnsw_filter() {
        let f = dead_method(
            "hnsw_filter",
            "src/hnsw/search.rs",
            cqs::parser::Language::Rust,
            "fn hnsw_filter(&self, id: &DataId) -> bool { true }",
        );
        let (v, reason) = classify_parent(&f, &no_low_conf());
        assert_eq!(
            v,
            DeadVerdict::KnownGap,
            "the FilterT::hnsw_filter method is framework-dispatched, not syntactically called"
        );
        assert!(
            reason.contains("external trait method"),
            "reason should name the external-trait-method gap: {reason}"
        );
    }

    /// Adversarial-content regression: an attacker can name a genuinely-dead
    /// FREE FUNCTION after a framework method (`visit_seq`) to steal the
    /// `known-gap` excuse and
    /// hide it from `--verdict dead`. The gap is now structurally gated on
    /// `ChunkType::Method`, so a `ChunkType::Function` named `visit_seq` — living
    /// in no `impl Trait for Type` — stays `dead`, while a real trait-impl method
    /// of the same name still classifies `known-gap`. Both halves in one test so
    /// the `chunk_type` gate is shown to be exactly what flips the verdict.
    #[test]
    fn verdict_external_trait_method_free_fn_namesake_stays_dead() {
        // Free function named after a serde Visitor method, NOT in any impl.
        let free_fn = dead_fn(
            "visit_seq",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn visit_seq() { /* genuinely dead free function */ }",
        );
        let (v_free, _) = classify_parent(&free_fn, &no_low_conf());
        assert_eq!(
            v_free,
            DeadVerdict::Dead,
            "a ChunkType::Function named visit_seq is not a trait-impl method — must stay dead"
        );

        // The identical name as a real trait-impl method still earns known-gap —
        // the ChunkType::Method tag is exactly what flips the verdict.
        let method = dead_method(
            "visit_seq",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn visit_seq<A>(self, mut seq: A) -> Result<usize, A::Error> { Ok(0) }",
        );
        let (v_method, reason) = classify_parent(&method, &no_low_conf());
        assert_eq!(
            v_method,
            DeadVerdict::KnownGap,
            "a real trait-impl method (ChunkType::Method) named visit_seq stays known-gap"
        );
        assert!(
            reason.contains("external trait method"),
            "reason should name the external-trait-method gap: {reason}"
        );
    }

    /// Over-excusing guard: the external-trait-method gap is a Rust-scoped
    /// EXPLICIT allowlist, NOT a name-class heuristic. A non-allowlisted Rust
    /// method name (a local `fmt` or an arbitrary `do_thing`) stays `dead`, and
    /// an allowlisted NAME in a non-Rust language (a Python fn named `visit_seq`)
    /// also stays `dead` — the rule keys on `lang == rust`, `ChunkType::Method`,
    /// AND membership.
    #[test]
    fn verdict_external_trait_method_does_not_over_excuse() {
        // A non-allowlisted Rust method name → still dead.
        let local_fmt = dead_fn(
            "fmt",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn fmt(&self) {}",
        );
        let (v_fmt, _) = classify_parent(&local_fmt, &no_low_conf());
        assert_eq!(
            v_fmt,
            DeadVerdict::Dead,
            "a non-allowlisted Rust method name must stay dead (not a name-class heuristic)"
        );

        let do_thing = dead_fn(
            "do_thing",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn do_thing() {}",
        );
        let (v_do, _) = classify_parent(&do_thing, &no_low_conf());
        assert_eq!(
            v_do,
            DeadVerdict::Dead,
            "an arbitrary Rust function name must stay dead"
        );

        // An allowlisted NAME in a non-Rust language → still dead (the rule is
        // Rust-scoped; a Python fn named visit_seq is not framework-dispatched).
        let py_visit = dead_fn(
            "visit_seq",
            "src/visitor.py",
            cqs::parser::Language::Python,
            "def visit_seq(self): ...",
        );
        let (v_py, _) = classify_parent(&py_visit, &no_low_conf());
        assert_eq!(
            v_py,
            DeadVerdict::Dead,
            "an allowlisted name in a non-Rust language must stay dead (Rust-scoped allowlist)"
        );
    }

    #[test]
    fn verdict_dead_residue() {
        let f = dead_fn(
            "genuinely_dead",
            "src/lib.rs",
            cqs::parser::Language::Rust,
            "fn genuinely_dead() {}",
        );
        let (v, _) = classify_parent(&f, &no_low_conf());
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
        let (v, _) = classify_parent(&f, &low_conf_with("make_x"));
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
        let out = build_dead_output(
            &[f],
            &[],
            std::path::Path::new("."),
            &low,
            &no_overlay_cand(),
        );
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["dead"][0]["verdict"], "dead");
        assert!(json["dead"][0].get("verdict_reason").is_some());
    }
}
