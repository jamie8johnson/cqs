//! The Phase-0 JsonSchema core for `cqs index` (MCP Phase 2b, fire-and-forget).
//!
//! A scoped slice of the clap-side [`crate::cli::args::IndexArgs`] that exposes
//! ONLY the non-destructive subset to an MCP client. It derives
//! `serde::Deserialize` + `schemars::JsonSchema`, so it doubles as the
//! `cqs_index` `inputSchema` source and the daemon deserialize target — the
//! same struct-is-the-contract discipline the read cores and the Phase-2a notes
//! cores follow.
//!
//! ## What this core deliberately OMITS (boundary by absence)
//!
//! The destructive / model-swapping fields of the clap `IndexArgs` are NOT
//! present here, so they cannot be set over the wire even when the mutation flag
//! is on:
//! - `force` — the destructive full-rebuild variant. Withheld per the design
//!   (`index --force` is the destructive twin of a fire-and-forget add); a
//!   forced full rebuild from a steered MCP client is exactly the blast-radius
//!   the §2 boundary withholds.
//! - the `--improve-docs` / `--apply` / `--improve-all` / `llm-summaries`
//!   family — these rewrite source files / spend API budget, out of charter for
//!   a queued reindex.
//!
//! ## The fire-and-forget semantics
//!
//! The daemon dispatch for this core does NOT run an index build. It QUEUES a
//! reconcile (flips the shared `SharedReconcileSignal`, the same primitive
//! `reconcile` uses) and returns immediately; the watch loop performs the
//! actual reindex on its next tick. So this core never reaches a writable
//! `Store` — the daemon's `Store<ReadOnly>` invariant holds. The client polls
//! completion via the already-exposed read tools `cqs_wait_fresh` / `cqs_status`.

/// Scoped, non-destructive input for the `cqs_index` MCP tool (Phase 2b).
///
/// `#[serde(default)]` on the struct so an MCP caller can send `{}` (queue a
/// reindex of the active slot) or supply just `slot`. Every field is optional on
/// the wire — a queued reindex needs no required input.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub(crate) struct IndexArgs {
    /// Target slot name (per-project `.cqs/slots/<name>/`). On the
    /// fire-and-forget daemon path the reconcile targets the slot the daemon was
    /// opened against; the reconcile path cannot redirect to another slot. A
    /// `slot` that matches the served slot (or is omitted) queues that slot's
    /// reindex. A `slot` that names a DIFFERENT slot is REFUSED with an error
    /// naming the served slot — the daemon will not silently reindex the wrong
    /// slot while reporting success. Omit it to reindex the served slot.
    pub slot: Option<String>,
}

/// Daemon-surface JSON for the fire-and-forget `cqs_index` queue response. THE
/// `outputSchema` source for the tool — a typed struct so the queue-signal
/// fields are declared rather than emitted from a bare `json!`. Serializes
/// byte-identically to the prior literal (`status` / `queued` /
/// `reindex_deferred` / `was_pending` / `served_slot`).
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub(crate) struct IndexQueuedOutput {
    /// Always `"queued"` — the reconcile was enqueued, not performed inline.
    pub status: &'static str,
    /// Always `true` — the shared reconcile signal was flipped.
    pub queued: bool,
    /// Always `true` — the rebuild is deferred to the watch loop's next tick.
    pub reindex_deferred: bool,
    /// Whether an earlier reconcile request was still un-drained (the watch loop
    /// coalesces repeated requests into one walk).
    pub was_pending: bool,
    /// The slot the queued reconcile targets. `null` only on the no-slot
    /// ramp-up path (reindexes the active slot, which needs no confirmation);
    /// a slot-qualified request confirms its target before reaching here.
    // always-emit: `null` is load-bearing — it marks the no-slot ramp-up queue
    // versus a confirmed slot echo, so the caller can verify no substitution.
    pub served_slot: Option<String>,
}
