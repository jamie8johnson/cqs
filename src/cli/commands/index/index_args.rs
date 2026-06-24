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
    /// Target slot name (per-project `.cqs/slots/<name>/`). Advisory on the
    /// fire-and-forget daemon path: the daemon queues a reconcile of the slot it
    /// already serves (the active slot it was opened against), so a `slot` that
    /// differs from the served slot does not redirect the rebuild. Present in the
    /// schema for honesty and forward use; omit it to reindex the active slot.
    pub slot: Option<String>,
}
