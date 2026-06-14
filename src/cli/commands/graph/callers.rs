//! Call graph commands for cqs
//!
//! Provides callers/callees analysis.
//!
//! ## Polymorphic routing
//!
//! `cqs callers <name>` and `cqs callees <name>` consult
//! `cqs::kind::classify_hits` against an exact-name lookup before the
//! call-graph query (see `docs/polymorphic-routing.md`): kind-mismatch
//! fallbacks return an object with `kind`, `fallback_from`, `name`,
//! `definitions`, and `note` fields.
//!
//! ## Output topology (function path)
//!
//! Both commands emit one object shape on the function path: callers returns
//! `{name, callers: [...], count}` and callees returns `{name, calls: [...],
//! count}` (cross-project adds a `project` field to each entry). Agents
//! discriminate the dispatch decision by key-probing, not by JSON type: a
//! `fallback_from` key means the kind-mismatch fallback fired; a `callers` /
//! `calls` key means the function path ran. Both paths are JSON objects.

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::normalize_path;
use cqs::parser::CallEdgeKind;
use cqs::store::{CalleeInfo, CallerInfo, ReadOnly, Store};

use super::notes_text;
use super::KindFallbackOutput;

// ─── Args (surface-agnostic, MCP-ready) ────────────────────────────────────

/// Input for [`callers_core`] / [`callees_core`]. Both commands take the
/// same shape, so they share one struct.
///
/// Cross-project resolution lives in the CLI / daemon adapters, not the
/// core: the cross-project path has its own (multi-index) semantics and no
/// kind-fallback. The core covers the single-project path both surfaces
/// share.
#[derive(Debug, serde::Deserialize)]
#[serde(default)]
pub(crate) struct CallersArgs {
    /// Function name to analyze.
    pub name: String,
    /// Max callers/callees returned (clamped 1..=100 inside the core).
    pub limit: usize,
    /// Restrict to edges of a single provenance kind (`call`, `serde_callback`,
    /// `macro_heuristic`, `fn_pointer`). `None` ⇒ all kinds. Consumes §1's
    /// `function_calls.edge_kind` column.
    #[serde(default, deserialize_with = "de_opt_edge_kind")]
    pub edge_kind: Option<CallEdgeKind>,
}

impl Default for CallersArgs {
    fn default() -> Self {
        Self {
            name: String::new(),
            // Mirrors clap `LimitArg` default.
            limit: crate::cli::args::DEFAULT_LIMIT,
            edge_kind: None,
        }
    }
}

/// Deserialize an optional [`CallEdgeKind`] from its stable string. Kept local
/// to the adapter so the lib enum stays `Serialize`-only.
fn de_opt_edge_kind<'de, D>(de: D) -> std::result::Result<Option<CallEdgeKind>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let opt = Option::<String>::deserialize(de)?;
    match opt {
        None => Ok(None),
        Some(s) => Ok(Some(parse_edge_kind(&s).map_err(serde::de::Error::custom)?)),
    }
}

/// Parse a `--edge-kind` string into a [`CallEdgeKind`], rejecting unknown
/// values (no silent default — an unknown kind is a user error worth
/// surfacing). Shared by the CLI flag parse and the wire deserializer.
pub(crate) fn parse_edge_kind(s: &str) -> std::result::Result<CallEdgeKind, String> {
    match s.to_ascii_lowercase().as_str() {
        "call" => Ok(CallEdgeKind::Call),
        "serde_callback" => Ok(CallEdgeKind::SerdeCallback),
        "macro_heuristic" => Ok(CallEdgeKind::MacroHeuristic),
        "fn_pointer" => Ok(CallEdgeKind::FnPointer),
        "doc_reference" => Ok(CallEdgeKind::DocReference),
        other => Err(format!(
            "invalid edge kind '{other}' (expected call|serde_callback|macro_heuristic|fn_pointer|doc_reference)"
        )),
    }
}

/// Alias so `callees` reads naturally at call sites. Same shape as
/// [`CallersArgs`].
pub(crate) type CalleesArgs = CallersArgs;

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct CallerEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    /// Originating project name on the cross-project path; omitted (single
    /// project) on the standard path.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project: String,
    /// Edge provenance, skip-when-default (absent ⇒ `call`). Empty string ⇒
    /// the default `call` kind; rendered omitted.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub edge_kind: String,
    /// Receiver-type attribution for a `Type::method` query. Empty on
    /// the bare-name path (no qualifier) and on proven self-calls; `"ambiguous"`
    /// when the caller's receiver could not be proven to be the queried Type
    /// (over-reported with a flag rather than dropped).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub attribution: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleeEntry {
    pub name: String,
    pub line_start: u32,
    /// Originating project name on the cross-project path; omitted (single
    /// project) on the standard path.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project: String,
    /// Edge provenance, skip-when-default (absent ⇒ `call`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub edge_kind: String,
}

/// Render a [`CallEdgeKind`] for the skip-when-default entry field: the default
/// `Call` kind maps to the empty string (omitted), any heuristic kind to its
/// stable label.
pub(crate) fn edge_kind_field(kind: CallEdgeKind) -> String {
    if kind == CallEdgeKind::Call {
        String::new()
    } else {
        kind.as_str().to_string()
    }
}

/// One `Type::method` disambiguation candidate: an enclosing type that
/// defines a same-named method, and how many definitions it carries. Emitted in
/// `CallersOutput::candidates` / `CalleesOutput::candidates` when a *bare* name
/// resolves to more than one definition, so the user learns the qualified forms
/// they can re-query with.
#[derive(Debug, serde::Serialize)]
pub(crate) struct DefCandidate {
    /// `Type::method`, or the bare method name for a free-function definition
    /// (no enclosing type).
    pub qualified: String,
    /// Number of definitions under this enclosing type.
    pub count: usize,
}

/// Function-path output for `cqs callers <name>`: `{name, callers, count,
/// total}`. The mirror of [`CalleesOutput`] — both commands share one object
/// topology so agents key-probe (`callers` vs `calls`) rather than
/// discriminating array-vs-object.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CallersOutput {
    pub name: String,
    pub callers: Vec<CallerEntry>,
    /// Number of entries in `callers` (the returned page after the `--limit`
    /// truncation).
    pub count: usize,
    /// Total callers before the `--limit` cap clipped the page. Equals `count`
    /// when nothing was dropped. Surfacing it keeps a capped window
    /// from reading as a complete list — a caller that sees `count < total`
    /// knows to paginate or raise `--limit`.
    pub total: usize,
    /// Disambiguation candidates when a *bare* name has more than one
    /// definition. Empty (omitted) for a single-def name or a
    /// `Type::method`-qualified query. Tells the user the `Type::method` forms
    /// available to narrow the query.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<DefCandidate>,
    /// On a `Type::method` query, the count of callers heuristically excluded
    /// because their enclosing type is a *different* type that also defines a
    /// same-named method. Skip-when-zero (omitted on the bare path and when no
    /// caller was excluded). The exclusion is a heuristic — a caller in type
    /// `Index` that calls `store.search()` is excluded though its receiver is
    /// really a `Store` — so surfacing the count keeps the narrowing visible.
    #[serde(skip_serializing_if = "is_zero")]
    pub excluded_other_owner: usize,
}

/// serde skip predicate for skip-when-zero `usize` fields.
fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleesOutput {
    pub name: String,
    pub calls: Vec<CalleeEntry>,
    /// Number of entries in `calls` (the returned page after `--limit`).
    pub count: usize,
    /// Total callees before the `--limit` cap. See [`CallersOutput::total`].
    pub total: usize,
    /// Disambiguation candidates for a bare multi-def name. See
    /// [`CallersOutput::candidates`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<DefCandidate>,
}

/// Single JSON-schema source for `cqs callers <name>`. Happy path is the
/// `{name, callers, count}` object (cross-project adds `project` per entry);
/// a kind mismatch yields the shared fallback object. Both variants are JSON
/// objects — agents key-probe (`callers` ⇒ function path, `fallback_from` ⇒
/// fallback) rather than discriminating by JSON type.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum CallersCoreOutput {
    /// Function path: `{name, callers, count}`.
    Callers(CallersOutput),
    /// Kind mismatch: `{kind, fallback_from, name, definitions, note}`.
    Fallback(KindFallbackOutput),
}

/// Single JSON-schema source for `cqs callees <name>`. Happy path is the
/// `{name, calls, count}` object; kind mismatch is the shared fallback.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum CalleesCoreOutput {
    /// Function path: `{name, calls, count}`.
    Callees(CalleesOutput),
    /// Kind mismatch: `{kind, fallback_from, name, definitions, note}`.
    Fallback(KindFallbackOutput),
}

// ─── Shared JSON builders ──────────────────────────────────────────────────

/// Build typed caller entries from caller info -- shared between CLI and batch.
pub(crate) fn build_caller_entries(callers: &[CallerInfo]) -> Vec<CallerEntry> {
    let _span = tracing::info_span!("build_caller_entries", count = callers.len()).entered();
    callers
        .iter()
        .map(|c| CallerEntry {
            name: c.name.clone(),
            file: normalize_path(&c.file).to_string(),
            line_start: c.line,
            project: String::new(),
            edge_kind: edge_kind_field(c.edge_kind),
            attribution: String::new(),
        })
        .collect()
}

/// Build typed callers output (`{name, callers, count, total}`) -- the
/// function-path shape both surfaces emit. Mirror of [`build_callees`].
/// `callers` is the already-truncated page; `total` is the pre-cap count so
/// a clipped window is visible rather than silent.
pub(crate) fn build_callers(name: &str, callers: &[CallerInfo], total: usize) -> CallersOutput {
    let _span = tracing::info_span!("build_callers", name, count = callers.len()).entered();
    let entries = build_caller_entries(callers);
    CallersOutput {
        name: name.to_string(),
        count: entries.len(),
        callers: entries,
        total,
        candidates: Vec::new(),
        excluded_other_owner: 0,
    }
}

/// Build typed callees output -- shared between CLI and batch. `callees` is
/// the truncated page; `total` is the pre-cap count.
pub(crate) fn build_callees(name: &str, callees: &[CalleeInfo], total: usize) -> CalleesOutput {
    let _span = tracing::info_span!("build_callees", name, count = callees.len()).entered();
    CalleesOutput {
        name: name.to_string(),
        calls: callees
            .iter()
            .map(|c| CalleeEntry {
                name: c.name.clone(),
                line_start: c.line,
                project: String::new(),
                edge_kind: edge_kind_field(c.edge_kind),
            })
            .collect(),
        count: callees.len(),
        total,
        candidates: Vec::new(),
    }
}

// ─── Cores ──────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs callers <name>` (single-project).
///
/// All logic lives here: cap normalization, kind classification, fallback
/// vs. function-path selection, and the SQL → typed-output translation.
/// Never prints, reads env, or branches on surface — the CLI and daemon
/// adapters render the returned [`CallersCoreOutput`].
///
/// The plain entry point serves the parent index unchanged (byte-identical to
/// the pre-overlay behaviour). The worktree-overlay merge lives in
/// [`callers_overlay`]; this delegates to it with no overlay (so participation
/// is always `false` and is discarded).
pub(crate) fn callers_core(
    store: &Store<ReadOnly>,
    args: &CallersArgs,
) -> Result<CallersCoreOutput> {
    Ok(callers_overlay(store, args, None)?.0)
}

/// Overlay-aware core for `cqs callers <name>` (single-project, #1858 Part B).
///
/// Identical to [`callers_core`] when `overlay` is `None` (the CLI / eval /
/// tests path — byte-unchanged). When `Some`, the bare-name function path
/// merges the worktree overlay's callers via
/// [`WorktreeOverlay::merge_callers`]: parent callers whose call-site origin is
/// in the delta are dropped, then the overlay's callers are unioned in. The
/// edge-kind filter and `--limit` cap apply to the merged set.
///
/// Returns `(output, overlay_participated)`. The bool reports whether the
/// overlay actually consulted the delta for THIS answer — true only when the
/// merge changed the caller set (a parent row's call-site origin was masked OR
/// ≥1 overlay row was unioned). It is `false` on every parent-truth path:
/// `overlay == None`, the `Type::method`-qualified early-return, the
/// kind-fallback early-return, and an active overlay that touched no origin
/// relevant to this query. The daemon adapter gates the
/// `_meta.overlay_graph = "full"` calibration marker on this bool so the marker
/// never over-claims: `overlay.is_some()` means "the worktree has some dirty
/// file", NOT "this answer reflects the delta".
///
/// The `Type::method`-qualified and kind-fallback paths stay on parent-truth in
/// Part B PR1 — the merge is wired only for the bare-name call-graph query (the
/// common surface). Extending the merge to the attributed `Type::method` path is
/// follow-up work.
pub(crate) fn callers_overlay(
    store: &Store<ReadOnly>,
    args: &CallersArgs,
    overlay: Option<&cqs::worktree_overlay::WorktreeOverlay>,
) -> Result<(CallersCoreOutput, bool)> {
    let _span =
        tracing::info_span!("callers_overlay", name = %args.name, limit = args.limit).entered();
    // Standardised cap. The store query returns every caller; we truncate
    // before rendering so the user can paginate via repeated calls.
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    // `Type::method` receiver-type disambiguation. Resolved read-side
    // from `chunks.parent_type_name`, before kind detection (which classifies
    // the *bare* name, never the qualified form). Parent-truth in PR1 — the
    // overlay never participates here, so this path reports `false`.
    if let Some((qual_type, method)) = split_type_qualifier(&args.name) {
        return Ok((
            callers_qualified(store, qual_type, method, args.edge_kind, limit)?,
            false,
        ));
    }

    // Polymorphic-routing kind detection. Dispatch the kind-mismatch
    // fallback before the call-graph query. Parent-truth in PR1 — the kind is
    // classified from the parent store, so a fallback verdict never reflects
    // the overlay: report `false`.
    let (chunks, fallback) = super::detect_fallback(store, &args.name);
    if let Some(fk) = fallback {
        let text = notes_text::callers(fk);
        return Ok((
            CallersCoreOutput::Fallback(KindFallbackOutput::new(
                &args.name, &chunks, fk, "callers", &text,
            )),
            false,
        ));
    }

    let mut callers = store
        .get_callers_full(&args.name)
        .context("Failed to load callers")?;
    // Worktree-overlay merge: drop parent callers from delta-touched origins,
    // union the overlay's callers (all from masked origins by construction).
    // `participated` records whether the merge actually changed the set: the
    // overlay added ≥1 caller, OR a parent caller's call-site origin was masked.
    // An active overlay whose delta touches no origin relevant to this query
    // leaves `participated == false` — the answer is pure parent-truth and must
    // not be stamped `"full"`.
    let mut participated = false;
    if let Some(ov) = overlay {
        let ov_callers = ov
            .store
            .get_callers_full(&args.name)
            .context("Failed to load overlay callers")?;
        participated =
            !ov_callers.is_empty() || callers.iter().any(|c| ov.masked_origins.contains(&c.file));
        callers = ov.merge_callers(callers, ov_callers);
    }
    // Edge-kind filter (§1): drop edges whose provenance kind doesn't match,
    // BEFORE the cap so `--limit` applies to the filtered set.
    if let Some(want) = args.edge_kind {
        callers.retain(|c| c.edge_kind == want);
    }
    // Total is the post-filter, pre-cap count so `total` reflects exactly the
    // set the user could page through with a larger `--limit`.
    let total = callers.len();
    callers.truncate(limit);
    let mut output = build_callers(&args.name, &callers, total);
    // A bare name with more than one definition advertises the `Type::method`
    // forms the user can re-query with.
    output.candidates = multi_def_candidates(store, &args.name)?;
    Ok((CallersCoreOutput::Callers(output), participated))
}

/// Split a `Type::method` qualifier into `(type, method)`, or `None` for a
/// bare name. Only the *last* `::` separates the receiver type from
/// the method, so a path-qualified `module::Type::method` keeps
/// `module::Type` as the receiver. Empty halves (`::method`, `Type::`) are
/// rejected — they're not a usable qualifier.
fn split_type_qualifier(name: &str) -> Option<(&str, &str)> {
    let (ty, method) = name.rsplit_once("::")?;
    if ty.is_empty() || method.is_empty() {
        return None;
    }
    Some((ty, method))
}

/// Build the `candidates` list for a bare multi-def name: the
/// `Type::method` qualified forms (and free-function bare form) the user can
/// narrow to. Returns empty when the name has a single definition (nothing to
/// disambiguate). Best-effort — a classification read error degrades to no
/// candidates rather than failing the whole command.
fn multi_def_candidates(store: &Store<ReadOnly>, name: &str) -> Result<Vec<DefCandidate>> {
    let defs = match store.count_method_defs_by_type(name) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, name, "candidate lookup failed; omitting candidates");
            return Ok(Vec::new());
        }
    };
    // One definition (or none) ⇒ no ambiguity to surface.
    if defs.len() <= 1 {
        return Ok(Vec::new());
    }
    Ok(defs_to_candidates(&defs, name))
}

/// Resolve the queried `Type::method`'s callers with receiver-type
/// attribution. `other_owner_types` (every *other* type that defines a
/// same-named method) drives the heuristic exclusion of callers that appear to
/// target a different type's method; everything else is included, with
/// unproven receivers flagged `ambiguous`. The count of excluded callers is
/// surfaced (`excluded_other_owner`) so the narrowing is visible.
///
/// Local-definition gate and the unknown-qualifier fallback compose carefully
/// (see the order below): the exact-qualified arm must run even when
/// `qual_type` has no local definition, because external / module qualifiers
/// (`std::fs::read_to_string`, `serde_json::to_value`) exist ONLY as
/// exact-qualified doc edges with no local type to attribute against.
///
/// 1. When `qual_type` has a local def → run the full attributed query
///    (bare + exact arms), with the other-owner exclusion set.
/// 2. When it does not → run the EXACT-ONLY query (no bare arm, so local
///    same-named call sites aren't mis-attributed under a fabricated type).
///    If that returns any rows, they are real doc edges naming this receiver —
///    surface them.
/// 3. Only when BOTH the local def is absent AND the exact arm is empty is it a
///    genuine typo (`Banana::search`) or a misused path: return empty with the
///    real `Type::method` candidates listed so the user sees the truth.
fn callers_qualified(
    store: &Store<ReadOnly>,
    qual_type: &str,
    method: &str,
    edge_kind: Option<CallEdgeKind>,
    limit: usize,
) -> Result<CallersCoreOutput> {
    // Single scan of the method's definitions feeds the local-def gate and the
    // other-owner-types exclusion set.
    let defs = store
        .count_method_defs_by_type(method)
        .context("Failed to enumerate method definitions")?;
    let qual_defines = defs.iter().any(|(ty, _)| ty.as_deref() == Some(qual_type));

    // The bare-method arm runs only when the qualifier has a local def; the
    // exact-qualified arm always runs (see `get_callers_attributed`).
    let other_owner_types: std::collections::HashSet<String> = if qual_defines {
        defs.iter()
            .filter_map(|(ty, _)| ty.clone())
            .filter(|t| t != qual_type)
            .collect()
    } else {
        std::collections::HashSet::new()
    };
    let (mut attributed, excluded) = store
        .get_callers_attributed(method, qual_type, &other_owner_types, qual_defines)
        .context("Failed to load attributed callers")?;
    if let Some(want) = edge_kind {
        attributed.retain(|a| a.caller.edge_kind == want);
    }

    // Genuine unknown qualifier: no local def AND no exact-qualified edges.
    // Surface the real owners as candidates instead of an empty result that
    // looks like "no callers".
    if !qual_defines && attributed.is_empty() {
        return Ok(CallersCoreOutput::Callers(CallersOutput {
            name: format!("{qual_type}::{method}"),
            callers: Vec::new(),
            count: 0,
            total: 0,
            candidates: defs_to_candidates(&defs, method),
            excluded_other_owner: 0,
        }));
    }

    let total = attributed.len();
    attributed.truncate(limit);
    let entries: Vec<CallerEntry> = attributed
        .iter()
        .map(|a| CallerEntry {
            name: a.caller.name.clone(),
            file: normalize_path(&a.caller.file).to_string(),
            line_start: a.caller.line,
            project: String::new(),
            edge_kind: edge_kind_field(a.caller.edge_kind),
            attribution: attribution_field(a.attribution),
        })
        .collect();
    Ok(CallersCoreOutput::Callers(CallersOutput {
        name: format!("{qual_type}::{method}"),
        count: entries.len(),
        callers: entries,
        total,
        candidates: Vec::new(),
        excluded_other_owner: excluded,
    }))
}

/// Project a method's definition rows (from [`Store::count_method_defs_by_type`])
/// into `Type::method` candidate forms. Unlike [`multi_def_candidates`], this
/// emits the list even for a single owner — the unknown-qualifier path uses it
/// to show the one real owner of a method the bogus qualifier didn't define.
fn defs_to_candidates(defs: &[(Option<String>, usize)], method: &str) -> Vec<DefCandidate> {
    defs.iter()
        .map(|(ty, count)| DefCandidate {
            qualified: match ty {
                Some(t) => format!("{t}::{method}"),
                None => method.to_string(),
            },
            count: *count,
        })
        .collect()
}

/// Map a [`cqs::store::CallerAttribution`] to the skip-when-empty wire field:
/// a proven self-call (`SelfType`) renders the empty string (omitted); an
/// unproven receiver (`Ambiguous`) emits `"ambiguous"`.
fn attribution_field(a: cqs::store::CallerAttribution) -> String {
    match a {
        cqs::store::CallerAttribution::SelfType => String::new(),
        cqs::store::CallerAttribution::Ambiguous => {
            cqs::store::CallerAttribution::Ambiguous.label().to_string()
        }
    }
}

/// Surface-agnostic core for `cqs callees <name>` (single-project).
///
/// The plain entry point serves the parent index unchanged. The worktree-
/// overlay merge lives in [`callees_overlay`]; this delegates with no overlay
/// (participation is always `false` and is discarded).
pub(crate) fn callees_core(
    store: &Store<ReadOnly>,
    args: &CalleesArgs,
) -> Result<CalleesCoreOutput> {
    Ok(callees_overlay(store, args, None)?.0)
}

/// Overlay-aware core for `cqs callees <name>` (single-project, #1858 Part B).
///
/// Identical to [`callees_core`] when `overlay` is `None`. When `Some`, the
/// bare-name function path resolves the asymmetric callee masking key (X's
/// DEFINITION file, NOT a per-row column — `function_calls` records callees by
/// name only, so there is nothing per-row to mask):
///
/// - If X's def-origin is in the delta (its body changed) the entire parent
///   callee set is suspect: drop ALL parent rows and serve the overlay's callee
///   rows for X.
/// - If X's def-origin is unchanged the parent callees are authoritative and the
///   overlay is NOT unioned — X was not edited, so the overlay holds no callee
///   rows for it. (Unioning here would inject unrelated overlay rows.)
///
/// The `x_def_masked` decision is [`WorktreeOverlay::callee_target_def_masked`]
/// over X's parent definition origins plus an overlay-store def check.
/// `Type::method` and kind-fallback paths stay on parent-truth in PR1.
///
/// Returns `(output, overlay_participated)`. For callees, participation is
/// exactly `x_def_masked`: the overlay path ran (parent rows dropped, overlay
/// rows served) only when X's definition lives in the delta. An active overlay
/// with X unedited returns the parent callees untouched — pure parent-truth,
/// `false`. The early-return parent-truth paths (`overlay == None`,
/// `Type::method`-qualified, kind-fallback) all report `false` too. The daemon
/// adapter gates the `_meta.overlay_graph = "full"` marker on this bool.
pub(crate) fn callees_overlay(
    store: &Store<ReadOnly>,
    args: &CalleesArgs,
    overlay: Option<&cqs::worktree_overlay::WorktreeOverlay>,
) -> Result<(CalleesCoreOutput, bool)> {
    let _span =
        tracing::info_span!("callees_overlay", name = %args.name, limit = args.limit).entered();
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    // `Type::method` scoping: callees of the method as defined under
    // the queried type, scoped by the def's origin file(s). Parent-truth in PR1.
    if let Some((qual_type, method)) = split_type_qualifier(&args.name) {
        return Ok((
            callees_qualified(store, qual_type, method, args.edge_kind, limit)?,
            false,
        ));
    }

    let (chunks, fallback) = super::detect_fallback(store, &args.name);
    if let Some(fk) = fallback {
        let text = notes_text::callees(fk);
        return Ok((
            CalleesCoreOutput::Fallback(KindFallbackOutput::new(
                &args.name, &chunks, fk, "callees", &text,
            )),
            false,
        ));
    }

    let mut callees = store
        .get_callees_full(&args.name, None, None)
        .context("Failed to load callees")?;
    // Worktree-overlay merge: the asymmetric callee key. X's callee set is
    // suspect iff X's DEFINITION lives in a delta-touched file. `x_def_masked`
    // IS the participation signal — when false the parent callees are returned
    // untouched (pure parent-truth) and the overlay is never unioned.
    let mut participated = false;
    if let Some(ov) = overlay {
        // X's parent definition origins (where X is defined in main).
        let parent_def_origins = store
            .get_chunks_by_name(&args.name)
            .context("Failed to resolve callee target definition origins")?
            .into_iter()
            .map(|c| c.file);
        let x_def_masked = ov.callee_target_def_masked(&args.name, parent_def_origins);
        participated = x_def_masked;
        let ov_callees = if x_def_masked {
            ov.store
                .get_callees_full(&args.name, None, None)
                .context("Failed to load overlay callees")?
        } else {
            // Body unchanged: the overlay scan is irrelevant; skip the query.
            Vec::new()
        };
        callees = ov.merge_callees(callees, ov_callees, x_def_masked);
    }
    if let Some(want) = args.edge_kind {
        callees.retain(|c| c.edge_kind == want);
    }
    let total = callees.len();
    callees.truncate(limit);
    let mut output = build_callees(&args.name, &callees, total);
    output.candidates = multi_def_candidates(store, &args.name)?;
    Ok((CalleesCoreOutput::Callees(output), participated))
}

/// Resolve callees of `Type::method` by scoping `get_callees_full` to
/// the definition site(s) — origin file AND def start line — where the type's
/// method is defined. Callees carry no receiver-type ambiguity (they are the
/// methods *this* method calls), so no attribution marker is emitted. A union
/// over multiple defining sites (rare) is deduplicated by `(origin, name,
/// line)` — origin is part of the key so two same-named callees at the same
/// line in different files don't collapse.
///
/// Line-level scoping (mirroring the callers side's `(origin, name,
/// line_start)` join) prevents two same-named methods sharing a file — a
/// `Store` and a `StoreBuilder` both defining `build` in `store.rs` — from
/// merging their callees under either `Type::method` query.
///
/// When the qualifier has no local definition of `method` (an external /
/// unknown qualifier like `std::fs::read_to_string`, or a typo like
/// `Banana::search`), the def-site list is empty and there are no callees to
/// show. Rather than a bare "no callees", surface the real owners as
/// disambiguation candidates (the callers side does the same), so the user
/// learns the qualified forms that actually resolve.
fn callees_qualified(
    store: &Store<ReadOnly>,
    qual_type: &str,
    method: &str,
    edge_kind: Option<CallEdgeKind>,
    limit: usize,
) -> Result<CalleesCoreOutput> {
    let def_sites = store
        .get_type_method_def_sites(qual_type, method)
        .context("Failed to resolve type-method def sites")?;
    let name = format!("{qual_type}::{method}");

    // No local def under this qualifier → no callees exist for it. Mirror the
    // callers side: if the method is defined under OTHER types, the qualifier
    // is wrong/unknown — surface those owners as candidates instead of an empty
    // result that reads as "this method calls nothing".
    if def_sites.is_empty() {
        let defs = store
            .count_method_defs_by_type(method)
            .context("Failed to enumerate method definitions")?;
        let candidates = defs_to_candidates(&defs, method);
        return Ok(CalleesCoreOutput::Callees(CalleesOutput {
            name,
            calls: Vec::new(),
            count: 0,
            total: 0,
            candidates,
        }));
    }

    let mut callees: Vec<CalleeInfo> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (origin, line) in &def_sites {
        let part = store
            .get_callees_full(method, Some(origin), Some(*line))
            .context("Failed to load callees")?;
        for c in part {
            if seen.insert((origin.clone(), c.name.clone(), c.line)) {
                callees.push(c);
            }
        }
    }
    if let Some(want) = edge_kind {
        callees.retain(|c| c.edge_kind == want);
    }
    let total = callees.len();
    callees.truncate(limit);
    Ok(CalleesCoreOutput::Callees(build_callees(
        &name, &callees, total,
    )))
}

// ─── Cross-project cores ─────────────────────────────────────────────────────
//
// The cross-project path has its own (multi-index) retrieval and no
// kind-fallback, so it gets its own core rather than a branch inside
// `callers_core`. Both surfaces (CLI `cmd_callers --cross-project`, daemon
// `dispatch_callers` cross branch) call these so the cap discipline and the
// `{name, callers|calls, count}` projection can't drift. Output topology is
// the same object shape as the single-project path; each entry gains a
// `project` field.

/// Surface-agnostic core for `cqs callers <name> --cross-project`.
///
/// Resolves callers across every project in `cross_ctx`, applies the shared
/// `1..=100` cap, and projects to the same `{name, callers, count}` object
/// the single-project core emits — entries carry their originating project.
pub(crate) fn callers_cross_core(
    cross_ctx: &mut cqs::cross_project::CrossProjectContext,
    args: &CallersArgs,
) -> Result<CallersOutput> {
    let _span =
        tracing::info_span!("callers_cross_core", name = %args.name, limit = args.limit).entered();
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);
    let mut callers = cross_ctx
        .get_callers_cross(&args.name)
        .context("Failed to load cross-project callers")?;
    // Edge-kind filter (§1, cross-project): provenance is now threaded through
    // the in-memory CallGraph, so the filter applies cross-project exactly as it
    // does single-project — BEFORE the cap so `--limit` pages the filtered set.
    if let Some(want) = args.edge_kind {
        callers.retain(|c| c.caller.edge_kind == want);
    }
    let total = callers.len();
    callers.truncate(limit);
    let entries: Vec<CallerEntry> = callers
        .iter()
        .map(|c| CallerEntry {
            name: c.caller.name.clone(),
            file: normalize_path(&c.caller.file).to_string(),
            line_start: c.caller.line,
            project: c.project.clone(),
            // Edge provenance threaded through the cross-project CallGraph,
            // skip-when-default (a `call` edge renders omitted).
            edge_kind: edge_kind_field(c.caller.edge_kind),
            // Cross-project has no Type::method resolution (no parent_type_name
            // in the merged CallGraph) — never attributed.
            attribution: String::new(),
        })
        .collect();
    Ok(CallersOutput {
        name: args.name.clone(),
        count: entries.len(),
        callers: entries,
        total,
        candidates: Vec::new(),
        excluded_other_owner: 0,
    })
}

/// Surface-agnostic core for `cqs callees <name> --cross-project`.
///
/// Mirror of [`callers_cross_core`]: projects to `{name, calls, count}` with
/// a `project` field on each entry.
pub(crate) fn callees_cross_core(
    cross_ctx: &mut cqs::cross_project::CrossProjectContext,
    args: &CalleesArgs,
) -> Result<CalleesOutput> {
    let _span =
        tracing::info_span!("callees_cross_core", name = %args.name, limit = args.limit).entered();
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);
    let mut callees = cross_ctx
        .get_callees_cross(&args.name)
        .context("Failed to load cross-project callees")?;
    // Edge-kind filter (§1, cross-project): see `callers_cross_core`.
    if let Some(want) = args.edge_kind {
        callees.retain(|c| c.edge_kind == want);
    }
    let total = callees.len();
    callees.truncate(limit);
    let calls: Vec<CalleeEntry> = callees
        .iter()
        .map(|c| CalleeEntry {
            name: c.name.clone(),
            line_start: c.line,
            project: c.project.clone(),
            // Edge provenance threaded through the cross-project CallGraph,
            // skip-when-default (a `call` edge renders omitted).
            edge_kind: edge_kind_field(c.edge_kind),
        })
        .collect();
    Ok(CalleesOutput {
        name: args.name.clone(),
        count: calls.len(),
        calls,
        total,
        candidates: Vec::new(),
    })
}

// ─── CLI commands (thin adapters over the cores) ───────────────────────────

/// Find functions that call the specified function
pub(crate) fn cmd_callers(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    limit: usize,
    cross_project: bool,
    edge_kind: Option<CallEdgeKind>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_callers", name, limit, cross_project).entered();
    let store = &ctx.store;
    let limit = limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let output = callers_cross_core(
            &mut cross_ctx,
            &CallersArgs {
                name: name.to_string(),
                limit,
                edge_kind,
            },
        )?;

        if json {
            crate::cli::json_envelope::emit_json(&output)?;
        } else if output.callers.is_empty() {
            println!("No callers found for '{}' (cross-project)", name);
        } else {
            println!("Functions that call '{}' (cross-project):", name);
            println!();
            for c in &output.callers {
                let kind_suffix = if c.edge_kind.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", c.edge_kind.dimmed())
                };
                println!(
                    "  {} ({}:{}) [{}]{}",
                    c.name.cyan(),
                    c.file,
                    c.line_start,
                    c.project.dimmed(),
                    kind_suffix
                );
            }
            println!();
            print_caller_total(output.count, output.total);
        }
        return Ok(());
    }

    // Standard single-project path — delegate to the shared core.
    match callers_core(
        store,
        &CallersArgs {
            name: name.to_string(),
            limit,
            edge_kind,
        },
    )? {
        CallersCoreOutput::Fallback(fb) => {
            if json {
                crate::cli::json_envelope::emit_json(&fb)?;
            } else {
                render_callers_fallback_text(name, store)?;
            }
        }
        CallersCoreOutput::Callers(output) => {
            if json {
                crate::cli::json_envelope::emit_json(&output)?;
            } else if output.callers.is_empty() {
                // An unknown qualifier (`Banana::search`) returns empty callers
                // WITH candidates — the real owners. Distinguish it from a
                // genuinely call-free name (empty callers, no candidates).
                if name.contains("::") && !output.candidates.is_empty() {
                    println!("'{}' is not a known Type::method — did you mean:", name);
                    print_candidate_lines(&output.candidates);
                } else {
                    println!("No callers found for '{}'", name);
                    print_candidates(&output.candidates);
                }
            } else {
                println!("Functions that call '{}':", name);
                println!();
                for caller in &output.callers {
                    let kind_suffix = if caller.edge_kind.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", caller.edge_kind.dimmed())
                    };
                    // An unproven receiver (`Type::method` over-report) is
                    // flagged inline so the user never reads it as certain.
                    let attr_suffix = if caller.attribution.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", caller.attribution.yellow())
                    };
                    println!(
                        "  {} ({}:{}){}{}",
                        caller.name.cyan(),
                        caller.file,
                        caller.line_start,
                        kind_suffix,
                        attr_suffix
                    );
                }
                println!();
                print_caller_total(output.count, output.total);
                print_excluded_other_owner(output.excluded_other_owner);
                print_candidates(&output.candidates);
            }
        }
    }
    Ok(())
}

/// Print the heuristic-exclusion notice for a `Type::method` query: N callers
/// were dropped because they sit in a *different* type that also defines the
/// method. No-op when nothing was excluded. Honors the "never silent
/// exclusion" promise — the narrowing is shown.
fn print_excluded_other_owner(excluded: usize) {
    if excluded == 0 {
        return;
    }
    let plural = if excluded == 1 { "caller" } else { "callers" };
    println!(
        "{}",
        format!("({excluded} {plural} excluded: in another type that also defines this method)")
            .dimmed()
    );
}

/// Print the `Type::method` disambiguation candidates for a bare multi-def
/// name. No-op when the list is empty (single-def name or a qualified
/// query). The hint tells the user the qualified forms they can narrow to.
fn print_candidates(candidates: &[DefCandidate]) {
    if candidates.is_empty() {
        return;
    }
    println!();
    println!(
        "{}",
        "This name has multiple definitions — narrow with a Type qualifier:".dimmed()
    );
    print_candidate_lines(candidates);
}

/// Render the candidate lines (`Type::method (N defs)`) shared by the
/// multi-def hint and the unknown-qualifier "did you mean" path.
fn print_candidate_lines(candidates: &[DefCandidate]) {
    for c in candidates {
        let plural = if c.count == 1 { "def" } else { "defs" };
        println!("  {} ({} {})", c.qualified.cyan(), c.count, plural);
    }
}

/// Render the caller total line, surfacing a clipped window: when the
/// `--limit` cap dropped callers, the line reads `Showing N of M caller(s)`
/// plus a paging hint, rather than a bare `Total: N` that hides the
/// truncation. When nothing was dropped it stays `Total: M caller(s)`.
fn print_caller_total(shown: usize, total: usize) {
    if total > shown {
        println!(
            "Showing {} of {} caller(s) ({})",
            shown,
            total,
            paging_hint(total)
        );
    } else {
        println!("Total: {} caller(s)", total);
    }
}

/// Mirror of [`print_caller_total`] for the callees surface.
fn print_callee_total(shown: usize, total: usize) {
    if total > shown {
        println!(
            "Showing {} of {} call(s) ({})",
            shown,
            total,
            paging_hint(total)
        );
    } else {
        println!("Total: {} call(s)", total);
    }
}

/// The paging hint for a clipped window. `--limit` can only reach
/// [`GRAPH_LIMIT_CAP`] entries, so once `total` exceeds the cap the bare
/// "raise --limit" advice dead-ends — there's no `--limit` value that pages
/// past it. In that case point at the working escape hatch instead: most of a
/// hot name's tail is low-trust `doc_reference` edges, so
/// `--edge-kind doc_reference` (or any other kind) narrows to a slice that fits
/// under the cap.
fn paging_hint(total: usize) -> &'static str {
    if total > crate::cli::GRAPH_LIMIT_CAP {
        "raise --limit, or narrow with --edge-kind (e.g. doc_reference) to page the rest"
    } else {
        "raise --limit to see more"
    }
}

/// Re-classify and render the plain-text callers fallback. The core
/// already decided a fallback fires; for text rendering the adapter needs
/// the chunks + kind to print the definition list, so it re-runs
/// `detect_fallback` (cheap indexed lookup). JSON callers render the typed
/// [`KindFallbackOutput`] directly and skip this.
fn render_callers_fallback_text(name: &str, store: &Store<ReadOnly>) -> Result<()> {
    let (chunks, fallback) = super::detect_fallback(store, name);
    if let Some(fk) = fallback {
        let text = notes_text::callers(fk);
        let lead = notes_text::callers_lead(fk, name);
        super::render_kind_fallback_text(&lead, &chunks, text.text_redirect, "Definitions:");
    }
    Ok(())
}

/// Find functions called by the specified function
pub(crate) fn cmd_callees(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    limit: usize,
    cross_project: bool,
    edge_kind: Option<CallEdgeKind>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_callees", name, limit, cross_project).entered();
    let store = &ctx.store;
    // See cmd_callers — same clamp range.
    let limit = limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let output = callees_cross_core(
            &mut cross_ctx,
            &CalleesArgs {
                name: name.to_string(),
                limit,
                edge_kind,
            },
        )?;

        if json {
            crate::cli::json_envelope::emit_json(&output)?;
        } else {
            println!("Functions called by '{}' (cross-project):", name.cyan());
            println!();
            if output.calls.is_empty() {
                println!("  (no function calls found)");
            } else {
                for c in &output.calls {
                    let kind_suffix = if c.edge_kind.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", c.edge_kind.dimmed())
                    };
                    println!("  {} [{}]{}", c.name, c.project.dimmed(), kind_suffix);
                }
            }
            println!();
            print_callee_total(output.count, output.total);
        }
        return Ok(());
    }

    // Standard single-project path — delegate to the shared core.
    match callees_core(
        store,
        &CalleesArgs {
            name: name.to_string(),
            limit,
            edge_kind,
        },
    )? {
        CalleesCoreOutput::Fallback(fb) => {
            if json {
                crate::cli::json_envelope::emit_json(&fb)?;
            } else {
                render_callees_fallback_text(name, store)?;
            }
        }
        CalleesCoreOutput::Callees(output) => {
            if json {
                crate::cli::json_envelope::emit_json(&output)?;
            } else if output.calls.is_empty() {
                // An unknown qualifier (`Banana::search`) returns empty calls
                // WITH candidates — the real owners. Mirror cmd_callers so the
                // user sees the did-you-mean list instead of a bare "no calls".
                if name.contains("::") && !output.candidates.is_empty() {
                    println!("'{}' is not a known Type::method — did you mean:", name);
                    print_candidate_lines(&output.candidates);
                } else {
                    println!("Functions called by '{}':", name.cyan());
                    println!();
                    println!("  (no function calls found)");
                    println!();
                    print_callee_total(output.count, output.total);
                    print_candidates(&output.candidates);
                }
            } else {
                println!("Functions called by '{}':", name.cyan());
                println!();
                for callee in &output.calls {
                    if callee.edge_kind.is_empty() {
                        println!("  {}", callee.name);
                    } else {
                        println!("  {} [{}]", callee.name, callee.edge_kind.dimmed());
                    }
                }
                println!();
                print_callee_total(output.count, output.total);
                print_candidates(&output.candidates);
            }
        }
    }
    Ok(())
}

/// Plain-text callees fallback renderer. See [`render_callers_fallback_text`].
fn render_callees_fallback_text(name: &str, store: &Store<ReadOnly>) -> Result<()> {
    let (chunks, fallback) = super::detect_fallback(store, name);
    if let Some(fk) = fallback {
        let text = notes_text::callees(fk);
        let lead = notes_text::callees_lead(fk, name);
        super::render_kind_fallback_text(&lead, &chunks, text.text_redirect, "Definitions:");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::chunks_to_definitions;
    use super::*;
    use cqs::store::ChunkSummary;

    /// A wire caller can supply just `name` and inherit the default limit.
    #[test]
    fn callers_args_deserialize_minimal() {
        let args: CallersArgs = serde_json::from_str(r#"{"name":"foo"}"#).unwrap();
        assert_eq!(args.name, "foo");
        assert_eq!(args.limit, crate::cli::args::DEFAULT_LIMIT);
    }

    #[test]
    fn test_caller_entry_field_names() {
        let entry = CallerEntry {
            name: "foo".into(),
            file: "src/lib.rs".into(),
            line_start: 42,
            project: String::new(),
            edge_kind: String::new(),
            attribution: String::new(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none());
        // Single-project entry omits the empty `project` field.
        assert!(json.get("project").is_none());
        // Default `call` edge omits edge_kind (skip-when-default).
        assert!(json.get("edge_kind").is_none());
        // Bare-name path omits attribution (skip-when-empty).
        assert!(json.get("attribution").is_none());
    }

    #[test]
    fn test_caller_entry_project_field_present_when_set() {
        let entry = CallerEntry {
            name: "foo".into(),
            file: "src/lib.rs".into(),
            line_start: 42,
            project: "openclaw".into(),
            edge_kind: String::new(),
            attribution: String::new(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["project"], "openclaw");
    }

    #[test]
    fn test_build_callers_empty() {
        let output = build_callers("foo", &[], 0);
        assert_eq!(output.count, 0);
        assert!(output.callers.is_empty());
        let json = serde_json::to_value(&output).unwrap();
        // Callers now shares the callees object topology: {name, callers, count, total}.
        assert_eq!(json["name"], "foo");
        assert!(json["callers"].as_array().unwrap().is_empty());
        assert_eq!(json["count"], 0);
        assert_eq!(json["total"], 0);
    }

    #[test]
    fn test_build_callers_object_shape() {
        let info = vec![CallerInfo {
            name: "caller_fn".into(),
            file: std::path::PathBuf::from("src/lib.rs"),
            line: 12,
            edge_kind: CallEdgeKind::Call,
        }];
        let output = build_callers("target", &info, 1);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "target");
        assert_eq!(json["count"], 1);
        assert_eq!(json["total"], 1);
        assert_eq!(json["callers"][0]["name"], "caller_fn");
        assert_eq!(json["callers"][0]["line_start"], 12);
        // Mirror of callees: agents key-probe `callers` vs `calls`.
        assert!(json.get("calls").is_none());
    }

    #[test]
    fn test_build_callees_empty() {
        let output = build_callees("foo", &[], 0);
        assert_eq!(output.count, 0);
        assert!(output.calls.is_empty());
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "foo");
    }

    /// A clipped page (page shorter than the pre-cap total) carries both the
    /// page `count` and the larger `total`, so a capped window is visible
    /// rather than silent.
    #[test]
    fn test_build_callers_total_reflects_clip() {
        let info = vec![CallerInfo {
            name: "shown".into(),
            file: std::path::PathBuf::from("src/lib.rs"),
            line: 1,
            edge_kind: CallEdgeKind::Call,
        }];
        // Page of 1, but 7 callers existed before the cap.
        let output = build_callers("target", &info, 7);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["count"], 1);
        assert_eq!(json["total"], 7);
    }

    #[test]
    fn test_callees_output_field_names() {
        let output = build_callees(
            "bar",
            &[CalleeInfo {
                name: "baz".into(),
                line: 10,
                edge_kind: CallEdgeKind::Call,
            }],
            1,
        );
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "bar");
        assert!(json.get("function").is_none());
        assert_eq!(json["calls"][0]["line_start"], 10);
        // Default-call callee omits edge_kind.
        assert!(json["calls"][0].get("edge_kind").is_none());
    }

    // Polymorphic-routing callers + callees kind-mismatch fallback shape.
    // Each test pins the JSON-builder contract so future schema tweaks are
    // deliberate, not accidental.

    fn make_chunk(
        chunk_type: cqs::parser::ChunkType,
        name: &str,
        file: &str,
        line: u32,
    ) -> ChunkSummary {
        ChunkSummary {
            id: format!("{}:{}:{}", file, line, "abcd1234"),
            file: std::path::PathBuf::from(file),
            language: cqs::parser::Language::Rust,
            chunk_type,
            name: name.to_string(),
            signature: format!("test sig for {}", name),
            content: format!("test content for {}", name),
            doc: None,
            line_start: line,
            line_end: line,
            content_hash: "abcd1234".to_string(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
            parser_version: 0,
            vendored: false,
        }
    }

    #[test]
    fn test_chunks_to_definitions_shape() {
        let chunk = make_chunk(cqs::parser::ChunkType::Constant, "X", "src/a.rs", 5);
        let defs = chunks_to_definitions(&[chunk]);
        assert_eq!(defs.len(), 1);
        let d = &defs[0];
        // The 7 fields every kind-mismatch fallback emits.
        for key in &[
            "file",
            "line_start",
            "line_end",
            "language",
            "chunk_type",
            "signature",
            "content",
        ] {
            assert!(d.get(*key).is_some(), "missing field: {key}");
        }
        assert_eq!(d["chunk_type"], "constant");
        assert_eq!(d["language"], "rust");
    }

    /// Pin the `{kind, fallback_from, name, definitions, note}` shape
    /// for the callers fallback. The test mirrors the impact module's
    /// `build_impact_kind_fallback_json_shape_invariants` — same shape,
    /// different `fallback_from` value.
    #[test]
    fn test_callers_fallback_payload_shape() {
        // Build the typed `KindFallbackOutput` the core emits and serialize
        // it (rather than a hand-rolled `json!` literal), so this pins the
        // production fallback shape.
        use super::super::notes_text::FallbackKind;
        let chunk = make_chunk(cqs::parser::ChunkType::Constant, "X", "src/a.rs", 5);
        let text = notes_text::callers(FallbackKind::Const);
        let out = KindFallbackOutput::new("X", &[chunk], FallbackKind::Const, "callers", &text);
        let payload = serde_json::to_value(&out).unwrap();
        assert_eq!(payload["kind"], "const");
        assert_eq!(payload["fallback_from"], "callers");
        assert_eq!(payload["name"], "X");
        assert_eq!(payload["definitions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_callees_fallback_payload_shape() {
        // Mirror of the callers payload via the typed builder, with
        // `fallback_from: "callees"`.
        use super::super::notes_text::FallbackKind;
        let chunk = make_chunk(cqs::parser::ChunkType::Class, "MyClass", "src/a.rs", 5);
        let text = notes_text::callees(FallbackKind::Type);
        let out =
            KindFallbackOutput::new("MyClass", &[chunk], FallbackKind::Type, "callees", &text);
        let payload = serde_json::to_value(&out).unwrap();
        assert_eq!(payload["fallback_from"], "callees");
        assert_eq!(payload["definitions"][0]["chunk_type"], "class");
    }

    // The callers/callees kind fallback routes through the shared
    // `chunks_to_definitions`, which caps the entry count and truncates
    // oversized content. Without these caps, `cqs callers Result --json`
    // on a hot name could emit unbounded multi-MB JSON.
    #[test]
    fn test_callers_fallback_caps_definitions_count() {
        use super::super::KIND_FALLBACK_MAX_DEFINITIONS;
        let chunks: Vec<ChunkSummary> = (0..(KIND_FALLBACK_MAX_DEFINITIONS + 50))
            .map(|i| {
                make_chunk(
                    cqs::parser::ChunkType::Constant,
                    &format!("X{i}"),
                    "src/lib.rs",
                    i as u32,
                )
            })
            .collect();
        let defs = chunks_to_definitions(&chunks);
        assert_eq!(defs.len(), KIND_FALLBACK_MAX_DEFINITIONS);
    }

    #[test]
    fn test_callers_fallback_truncates_oversized_content() {
        use super::super::KIND_FALLBACK_MAX_CONTENT_BYTES;
        let mut big = make_chunk(cqs::parser::ChunkType::Constant, "BIG", "src/lib.rs", 1);
        big.content = "x".repeat(KIND_FALLBACK_MAX_CONTENT_BYTES * 2);
        let defs = chunks_to_definitions(&[big]);
        let content = defs[0]["content"].as_str().unwrap();
        assert!(content.ends_with("... (truncated)"));
        assert_eq!(defs[0]["truncated"], true);
    }

    // ----- Edge provenance (§1) -----

    /// A default `call` caller omits `edge_kind`; a heuristic caller emits it.
    #[test]
    fn caller_entry_edge_kind_skip_when_default() {
        let info = vec![
            CallerInfo {
                name: "syntactic".into(),
                file: std::path::PathBuf::from("src/a.rs"),
                line: 1,
                edge_kind: CallEdgeKind::Call,
            },
            CallerInfo {
                name: "heuristic".into(),
                file: std::path::PathBuf::from("src/b.rs"),
                line: 2,
                edge_kind: CallEdgeKind::MacroHeuristic,
            },
        ];
        let output = build_callers("target", &info, 2);
        let json = serde_json::to_value(&output).unwrap();
        // Syntactic edge omits edge_kind.
        assert!(json["callers"][0].get("edge_kind").is_none());
        // Heuristic edge carries the label.
        assert_eq!(json["callers"][1]["edge_kind"], "macro_heuristic");
    }

    /// Callee edge_kind round-trips the same way.
    #[test]
    fn callee_entry_edge_kind_emitted_for_heuristic() {
        let callees = vec![CalleeInfo {
            name: "fp".into(),
            line: 4,
            edge_kind: CallEdgeKind::FnPointer,
        }];
        let output = build_callees("caller", &callees, 1);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["calls"][0]["edge_kind"], "fn_pointer");
    }

    /// `--edge-kind` parse rejects unknown kinds and round-trips known ones.
    #[test]
    fn parse_edge_kind_round_trip() {
        assert_eq!(
            parse_edge_kind("serde_callback").unwrap(),
            CallEdgeKind::SerdeCallback
        );
        assert_eq!(
            parse_edge_kind("fn_pointer").unwrap(),
            CallEdgeKind::FnPointer
        );
        assert!(parse_edge_kind("bogus").is_err());
    }

    /// The wire `CallersArgs` deserializes `edge_kind` and rejects bad values.
    #[test]
    fn callers_args_deserialize_edge_kind() {
        let args: CallersArgs =
            serde_json::from_value(serde_json::json!({"name":"f","edge_kind":"macro_heuristic"}))
                .unwrap();
        assert_eq!(args.edge_kind, Some(CallEdgeKind::MacroHeuristic));
        assert!(serde_json::from_value::<CallersArgs>(
            serde_json::json!({"name":"f","edge_kind":"nope"})
        )
        .is_err());
    }

    // ----- Type::method qualifier parsing -----

    #[test]
    fn split_type_qualifier_splits_on_last_colon_pair() {
        assert_eq!(
            split_type_qualifier("Store::search"),
            Some(("Store", "search"))
        );
        // Path-qualified: only the LAST `::` separates the method.
        assert_eq!(
            split_type_qualifier("module::Store::search"),
            Some(("module::Store", "search"))
        );
        // Bare name: no qualifier.
        assert_eq!(split_type_qualifier("search"), None);
        // Empty halves are not a usable qualifier.
        assert_eq!(split_type_qualifier("::search"), None);
        assert_eq!(split_type_qualifier("Store::"), None);
    }

    #[test]
    fn attribution_field_skips_self_emits_ambiguous() {
        use cqs::store::CallerAttribution;
        assert_eq!(attribution_field(CallerAttribution::SelfType), "");
        assert_eq!(attribution_field(CallerAttribution::Ambiguous), "ambiguous");
    }

    // ----- Paging hint -----

    /// A clipped window whose total fits under the cap advises `--limit`; a
    /// window whose total EXCEEDS the cap (where `--limit` dead-ends, since it
    /// clamps to `GRAPH_LIMIT_CAP`) advises the `--edge-kind` escape hatch
    /// instead. Pinned so the hint can't silently regress to the dead-end
    /// wording.
    #[test]
    fn paging_hint_switches_to_edge_kind_past_cap() {
        // Below the cap: a larger --limit can still reach everything.
        let under = paging_hint(crate::cli::GRAPH_LIMIT_CAP);
        assert_eq!(under, "raise --limit to see more");
        assert!(!under.contains("--edge-kind"));
        // Above the cap: --limit can't page past GRAPH_LIMIT_CAP, so the hint
        // points at --edge-kind filtering.
        let over = paging_hint(crate::cli::GRAPH_LIMIT_CAP + 1);
        assert!(
            over.contains("--edge-kind"),
            "past the cap, the hint must mention --edge-kind: {over:?}"
        );
        assert!(over.contains("doc_reference"));
    }
}
