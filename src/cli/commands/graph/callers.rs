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

/// Error message when `--edge-kind` is combined with `--cross-project`. The
/// cross-project path loads a `CallGraph` that discards edge kinds (kinds are
/// not threaded through the multi-index merge), so applying the filter would
/// silently return the unfiltered superset. Honest refusal until kinds are
/// threaded through `CallGraph` (tracked as a follow-up). Shared verbatim by
/// CLI and daemon so the parity test can pin the exact string.
pub(crate) const EDGE_KIND_CROSS_PROJECT_ERR: &str =
    "edge-kind filtering is not supported with --cross-project";

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
pub(crate) fn callers_core(
    store: &Store<ReadOnly>,
    args: &CallersArgs,
) -> Result<CallersCoreOutput> {
    let _span =
        tracing::info_span!("callers_core", name = %args.name, limit = args.limit).entered();
    // Standardised cap. The store query returns every caller; we truncate
    // before rendering so the user can paginate via repeated calls.
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    // `Type::method` receiver-type disambiguation. Resolved read-side
    // from `chunks.parent_type_name`, before kind detection (which classifies
    // the *bare* name, never the qualified form).
    if let Some((qual_type, method)) = split_type_qualifier(&args.name) {
        return callers_qualified(store, qual_type, method, args.edge_kind, limit);
    }

    // Polymorphic-routing kind detection. Dispatch the kind-mismatch
    // fallback before the call-graph query.
    let (chunks, fallback) = super::detect_fallback(store, &args.name);
    if let Some(fk) = fallback {
        let text = notes_text::callers(fk);
        return Ok(CallersCoreOutput::Fallback(KindFallbackOutput::new(
            &args.name, &chunks, fk, "callers", &text,
        )));
    }

    let mut callers = store
        .get_callers_full(&args.name)
        .context("Failed to load callers")?;
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
    Ok(CallersCoreOutput::Callers(output))
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
    Ok(defs
        .into_iter()
        .map(|(ty, count)| DefCandidate {
            qualified: match ty {
                Some(t) => format!("{t}::{name}"),
                None => name.to_string(),
            },
            count,
        })
        .collect())
}

/// Resolve the queried `Type::method`'s callers with receiver-type
/// attribution. `other_owner_types` (every *other* type that defines
/// a same-named method) drives the exclusion of callers that target a
/// different type's method; everything else is included, with unproven
/// receivers flagged `ambiguous`.
fn callers_qualified(
    store: &Store<ReadOnly>,
    qual_type: &str,
    method: &str,
    edge_kind: Option<CallEdgeKind>,
    limit: usize,
) -> Result<CallersCoreOutput> {
    let other_owner_types = other_owner_types(store, method, qual_type)?;
    let mut attributed = store
        .get_callers_attributed(method, qual_type, &other_owner_types)
        .context("Failed to load attributed callers")?;
    if let Some(want) = edge_kind {
        attributed.retain(|a| a.caller.edge_kind == want);
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
    }))
}

/// The set of enclosing types (other than `qual_type`) that define a method
/// named `method`. Callers parented to one of these target *that* type's
/// method, so the `Type::method` resolution excludes them.
fn other_owner_types(
    store: &Store<ReadOnly>,
    method: &str,
    qual_type: &str,
) -> Result<std::collections::HashSet<String>> {
    let defs = store
        .count_method_defs_by_type(method)
        .context("Failed to enumerate method definitions")?;
    Ok(defs
        .into_iter()
        .filter_map(|(ty, _)| ty)
        .filter(|t| t != qual_type)
        .collect())
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
pub(crate) fn callees_core(
    store: &Store<ReadOnly>,
    args: &CalleesArgs,
) -> Result<CalleesCoreOutput> {
    let _span =
        tracing::info_span!("callees_core", name = %args.name, limit = args.limit).entered();
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    // `Type::method` scoping: callees of the method as defined under
    // the queried type, scoped by the def's origin file(s).
    if let Some((qual_type, method)) = split_type_qualifier(&args.name) {
        return callees_qualified(store, qual_type, method, args.edge_kind, limit);
    }

    let (chunks, fallback) = super::detect_fallback(store, &args.name);
    if let Some(fk) = fallback {
        let text = notes_text::callees(fk);
        return Ok(CalleesCoreOutput::Fallback(KindFallbackOutput::new(
            &args.name, &chunks, fk, "callees", &text,
        )));
    }

    let mut callees = store
        .get_callees_full(&args.name, None)
        .context("Failed to load callees")?;
    if let Some(want) = args.edge_kind {
        callees.retain(|c| c.edge_kind == want);
    }
    let total = callees.len();
    callees.truncate(limit);
    let mut output = build_callees(&args.name, &callees, total);
    output.candidates = multi_def_candidates(store, &args.name)?;
    Ok(CalleesCoreOutput::Callees(output))
}

/// Resolve callees of `Type::method` by scoping `get_callees_full` to
/// the origin file(s) where the type's method is defined. Callees carry no
/// receiver-type ambiguity (they are the methods *this* method calls), so no
/// attribution marker is emitted. A union over multiple defining files (rare)
/// is deduplicated by `(name, line)`.
fn callees_qualified(
    store: &Store<ReadOnly>,
    qual_type: &str,
    method: &str,
    edge_kind: Option<CallEdgeKind>,
    limit: usize,
) -> Result<CalleesCoreOutput> {
    let origins = store
        .get_type_method_origins(qual_type, method)
        .context("Failed to resolve type-method origins")?;
    let name = format!("{qual_type}::{method}");
    let mut callees: Vec<CalleeInfo> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for origin in &origins {
        let part = store
            .get_callees_full(method, Some(origin))
            .context("Failed to load callees")?;
        for c in part {
            if seen.insert((c.name.clone(), c.line)) {
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
    let total = callers.len();
    callers.truncate(limit);
    let entries: Vec<CallerEntry> = callers
        .iter()
        .map(|c| CallerEntry {
            name: c.caller.name.clone(),
            file: normalize_path(&c.caller.file).to_string(),
            line_start: c.caller.line,
            project: c.project.clone(),
            // Cross-project edges come from the in-memory CallGraph (no
            // edge_kind tracking) — always the default `call`, rendered omitted.
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
    let total = callees.len();
    callees.truncate(limit);
    let calls: Vec<CalleeEntry> = callees
        .iter()
        .map(|c| CalleeEntry {
            name: c.name.clone(),
            line_start: c.line,
            project: c.project.clone(),
            // Cross-project callees come from the in-memory CallGraph (no
            // edge_kind) — default `call`, rendered omitted.
            edge_kind: String::new(),
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

    if cross_project && edge_kind.is_some() {
        anyhow::bail!("{}", EDGE_KIND_CROSS_PROJECT_ERR);
    }

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
                println!(
                    "  {} ({}:{}) [{}]",
                    c.name.cyan(),
                    c.file,
                    c.line_start,
                    c.project.dimmed()
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
                println!("No callers found for '{}'", name);
                print_candidates(&output.candidates);
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
                print_candidates(&output.candidates);
            }
        }
    }
    Ok(())
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
    for c in candidates {
        let plural = if c.count == 1 { "def" } else { "defs" };
        println!("  {} ({} {})", c.qualified.cyan(), c.count, plural);
    }
}

/// Render the caller total line, surfacing a clipped window: when the
/// `--limit` cap dropped callers, the line reads `Showing N of M caller(s)
/// (raise --limit to see more)` rather than a bare `Total: N` that hides the
/// truncation. When nothing was dropped it stays `Total: M caller(s)`.
fn print_caller_total(shown: usize, total: usize) {
    if total > shown {
        println!(
            "Showing {} of {} caller(s) (raise --limit to see more)",
            shown, total
        );
    } else {
        println!("Total: {} caller(s)", total);
    }
}

/// Mirror of [`print_caller_total`] for the callees surface.
fn print_callee_total(shown: usize, total: usize) {
    if total > shown {
        println!(
            "Showing {} of {} call(s) (raise --limit to see more)",
            shown, total
        );
    } else {
        println!("Total: {} call(s)", total);
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

    if cross_project && edge_kind.is_some() {
        anyhow::bail!("{}", EDGE_KIND_CROSS_PROJECT_ERR);
    }

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
                    println!("  {} [{}]", c.name, c.project.dimmed());
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
            } else {
                println!("Functions called by '{}':", name.cyan());
                println!();
                if output.calls.is_empty() {
                    println!("  (no function calls found)");
                } else {
                    for callee in &output.calls {
                        if callee.edge_kind.is_empty() {
                            println!("  {}", callee.name);
                        } else {
                            println!("  {} [{}]", callee.name, callee.edge_kind.dimmed());
                        }
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
}
