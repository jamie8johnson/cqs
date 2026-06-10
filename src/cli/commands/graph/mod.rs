//! Graph commands — call graph analysis, impact, tracing, type dependencies

mod callers;
mod deps;
pub(crate) mod explain;
mod impact;
mod impact_diff;
pub(crate) mod notes_text;
mod test_map;
pub(crate) mod trace;

pub(crate) use callers::{
    callees_core, callers_core, cmd_callees, cmd_callers, CalleesArgs,
    CallersArgs as CallersCoreArgs,
};
pub(crate) use deps::{cmd_deps, deps_core, DepsArgs as DepsCoreArgs};
pub(crate) use explain::cmd_explain;
pub(crate) use impact::{cmd_impact, impact_core, ImpactArgs as ImpactCoreArgs};
pub(crate) use impact_diff::cmd_impact_diff;
pub(crate) use test_map::{
    build_test_map, build_test_map_output, cmd_test_map, test_map_core, test_map_max_nodes,
    TestMapArgs as TestMapCoreArgs,
};
pub(crate) use trace::{cmd_trace, trace_core, trace_max_nodes, TraceArgs as TraceCoreArgs};

use cqs::kind::{detect_kind_for_store, Kind};
use cqs::store::{ChunkSummary, ReadOnly, Store, StoreError};
use notes_text::FallbackKind;

/// Classify a name against the indexed corpus and decide whether a graph
/// command should run its kind-mismatch fallback. Returns the matching
/// chunks (so the caller reuses them for the fallback `definitions`)
/// alongside the resolved [`FallbackKind`], or `None` when the name routes
/// through the command's normal flow.
///
/// This is the single classification site the six cores share, replacing
/// the inlined `lookup_by_name` + `KindHit::from` + `classify_hits`
/// incantation each command (and the daemon's `try_kind_fallback`) used to
/// carry independently.
///
/// Classification goes through [`detect_kind_for_store`] — the lib's
/// routing classifier, which reads only `chunk_type` per hit (no
/// `ChunkSummary` clones). The fallback rendering still needs the full
/// summaries (`content`, `signature`, `line_end`, …) that [`KindHit`]
/// drops, so this fetches them via `lookup_by_name`; the two reads hit the
/// same indexed `WHERE name = ?` row set.
pub(crate) fn detect_fallback(
    store: &Store<ReadOnly>,
    name: &str,
) -> Result<(Vec<ChunkSummary>, Option<FallbackKind>), StoreError> {
    let (kind, _hits) = detect_kind_for_store(store, name)?;
    let chunks = store.lookup_by_name(name)?;
    Ok((chunks, fallback_kind(kind)))
}

/// Map a routing-level [`Kind`] to the [`FallbackKind`] that drives a
/// graph command's kind-mismatch fallback. Returns `None` for the kinds
/// that route through the command's normal flow (Function / Multiple /
/// Other / NotFound) and never produce a fallback.
///
/// Centralizing the mapping keeps every command's core agreeing on which
/// kinds fall back; the per-command core decides what to do with the
/// `Some`/`None` (deps, for instance, also runs the normal flow for
/// `Type`).
pub(crate) fn fallback_kind(kind: Kind) -> Option<FallbackKind> {
    match kind {
        Kind::Const => Some(FallbackKind::Const),
        Kind::Type => Some(FallbackKind::Type),
        Kind::Module => Some(FallbackKind::Module),
        Kind::Ambiguous => Some(FallbackKind::Ambiguous),
        // Function: the happy path. Multiple: resolves deterministically.
        // Other: freeform chunk types the routing matrix doesn't rule on.
        // NotFound: surfaces an empty / not-found result downstream.
        Kind::Function | Kind::Multiple | Kind::Other | Kind::NotFound => None,
    }
}

/// Typed kind-mismatch fallback payload shared by every graph command's
/// core. Serializes to the exact `{kind, fallback_from, name,
/// definitions, note}` object the CLI and daemon have always emitted —
/// the single JSON schema source for every fallback.
///
/// `definitions` stays `Vec<serde_json::Value>` (the Phase-0 capped /
/// content-truncated values from [`chunks_to_definitions`]) so the wire
/// shape, including the skip-when-false `truncated` per-entry field, is
/// byte-identical to the pre-unification literal-built objects.
#[derive(Debug, serde::Serialize)]
pub(crate) struct KindFallbackOutput {
    /// Routing-level kind label: `const` / `type` / `module` / `ambiguous`.
    pub kind: &'static str,
    /// The command this fallback fired from (`callers`, `impact`, …).
    pub fallback_from: &'static str,
    /// The queried name.
    pub name: String,
    /// Capped, content-truncated definition sites for the name.
    pub definitions: Vec<serde_json::Value>,
    /// Agent-facing redirect explaining why the command doesn't apply.
    pub note: &'static str,
}

impl KindFallbackOutput {
    /// Build a fallback payload from the queried name, its matching
    /// chunks, the firing command, and the resolved fallback text.
    pub(crate) fn new(
        name: &str,
        chunks: &[cqs::store::ChunkSummary],
        fallback_kind: FallbackKind,
        fallback_from: &'static str,
        text: &notes_text::FallbackText,
    ) -> Self {
        KindFallbackOutput {
            kind: fallback_kind.label(),
            fallback_from,
            name: name.to_string(),
            definitions: chunks_to_definitions(chunks),
            note: text.note,
        }
    }
}

/// Plain-text rendering of a [`KindFallbackOutput`] shared by the CLI
/// adapters. Prints the kind-specific lead line, the definition sites,
/// and the trailing redirect — byte-stable with the pre-unification
/// per-command text blocks.
///
/// `defs_heading` differentiates trace ("Source definitions:") from the
/// other commands ("Definitions:").
pub(crate) fn render_kind_fallback_text(
    lead: &str,
    chunks: &[cqs::store::ChunkSummary],
    redirect: &str,
    defs_heading: &str,
) {
    println!("{lead}");
    println!();
    println!("{defs_heading}");
    for c in chunks {
        println!(
            "  {}:{}-{} ({} {})",
            cqs::normalize_path(&c.file),
            c.line_start,
            c.line_end,
            c.language,
            c.chunk_type
        );
        if !c.signature.is_empty() {
            println!("    {}", c.signature);
        }
    }
    println!();
    println!("{redirect}");
}

/// Maximum number of `definitions[]` entries returned in a kind-mismatch
/// fallback response. Mirrors the standard graph-command result cap.
///
/// Shared by every CLI graph command (callers, callees, deps, impact,
/// test-map, trace) and the daemon dispatch handler so a hot name like
/// `Result` / `Error` matching hundreds of chunks can never balloon a
/// fallback response into multi-MB JSON.
pub(crate) const KIND_FALLBACK_MAX_DEFINITIONS: usize = 100;

/// Per-entry `content` byte cap inside a kind-mismatch fallback
/// `definitions[]` entry. Truncated content is suffixed with
/// `"... (truncated)"` and the entry gains a `truncated: true` field
/// so consumers can distinguish capped chunks from full ones.
pub(crate) const KIND_FALLBACK_MAX_CONTENT_BYTES: usize = 2048;

/// Shared chunk-to-definition transformation for every CLI-direct kind
/// fallback (callers, callees, deps, impact, test-map, trace) and the
/// daemon dispatch path. Each entry carries
/// file/line_start/line_end/language/chunk_type/signature/content — the
/// same shape every kind emits — and truncates content per
/// [`KIND_FALLBACK_MAX_CONTENT_BYTES`].
pub(crate) fn chunk_to_definition_value(c: &cqs::store::ChunkSummary) -> serde_json::Value {
    let _span = tracing::trace_span!("chunk_to_definition_value").entered();
    let (content, truncated) = if c.content.len() > KIND_FALLBACK_MAX_CONTENT_BYTES {
        // Truncate at a UTF-8 char boundary at or below the byte cap.
        // `floor_char_boundary` would be cleaner but isn't stable yet.
        let mut end = KIND_FALLBACK_MAX_CONTENT_BYTES;
        while !c.content.is_char_boundary(end) {
            end -= 1;
        }
        (format!("{}... (truncated)", &c.content[..end]), true)
    } else {
        (c.content.clone(), false)
    };
    let mut entry = serde_json::Map::new();
    entry.insert(
        "file".to_string(),
        serde_json::json!(cqs::normalize_path(&c.file)),
    );
    entry.insert("line_start".to_string(), serde_json::json!(c.line_start));
    entry.insert("line_end".to_string(), serde_json::json!(c.line_end));
    entry.insert(
        "language".to_string(),
        serde_json::json!(c.language.to_string()),
    );
    entry.insert(
        "chunk_type".to_string(),
        serde_json::json!(c.chunk_type.to_string()),
    );
    entry.insert("signature".to_string(), serde_json::json!(c.signature));
    entry.insert("content".to_string(), serde_json::json!(content));
    if truncated {
        entry.insert("truncated".to_string(), serde_json::json!(true));
    }
    serde_json::Value::Object(entry)
}

/// Build a capped `definitions[]` list from chunks for kind-mismatch
/// fallbacks. Takes at most [`KIND_FALLBACK_MAX_DEFINITIONS`] entries and
/// truncates each chunk's content via [`chunk_to_definition_value`]. Used
/// by every CLI graph command's kind fallback so the count + content caps
/// hold uniformly across all surfaces.
pub(crate) fn chunks_to_definitions(chunks: &[cqs::store::ChunkSummary]) -> Vec<serde_json::Value> {
    chunks
        .iter()
        .take(KIND_FALLBACK_MAX_DEFINITIONS)
        .map(chunk_to_definition_value)
        .collect()
}
