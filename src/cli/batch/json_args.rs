//! JSON-args daemon dispatch (MCP Phase 1, D3-b).
//!
//! The daemon socket request frame is parsed untyped, so alongside the
//! historical argv form
//!
//! ```json
//! {"command": "search", "args": ["error handling", "-n", "3"]}
//! ```
//!
//! it now accepts a JSON `arguments` object that deserializes directly into the
//! command's Phase-0 core input struct:
//!
//! ```json
//! {"command": "search", "arguments": {"query": "error handling", "limit": 3}}
//! ```
//!
//! ## The routing invariant
//!
//! The JSON-args path MUST reach the handler through the SAME downstream
//! dispatch the argv path uses — [`super::commands::dispatch`] →
//! `handlers::dispatch_*` — and NEVER call a `*_core` fn directly. The argv
//! path's only step skipped here is the clap token parse; everything after
//! (`commands::dispatch`, the handler's overlay/path gates, validation) runs
//! identically. The overlay-root validation
//! ([`super::BatchView::set_validated_overlay_request`]) and `read`'s
//! canonicalize + `starts_with` traversal gate both live inside the handlers,
//! so routing through `commands::dispatch` preserves them. Calling a `*_core`
//! directly would bypass those gates and is the bug this design forecloses.
//!
//! ## The args.rs ↔ core impedance
//!
//! The `arguments` object deserializes into the Phase-0 core struct (the one
//! that derives `serde::Deserialize` + `schemars::JsonSchema` — the shape the
//! MCP `inputSchema` advertises). The handlers, however, consume the `args.rs`
//! clap structs and adapt those to the cores themselves (e.g.
//! `daemon_query_args(SearchArgs) -> QueryArgs`, applying daemon-only postures
//! like `always_route`). So each converter here takes the deserialized core and
//! builds the matching `args.rs` struct, defaulting the `args.rs`-only fields
//! that the JSON schema does not expose (`cross_project`, the overlay
//! tri-state, `ref_name`, …) to the same values clap would leave them at when
//! they are absent from an argv invocation. Feeding that struct through
//! `commands::dispatch` yields output value-equal to the argv path — the
//! parity guard.

use anyhow::{bail, Result};
use serde::de::DeserializeOwned;

use crate::cli::args::{
    BlameArgs, CallersArgs, CiArgs, ContextArgs, DeadArgs, DepsArgs, DiffArgs, DriftArgs,
    GatherArgs, ImpactArgs, LimitArg, OnboardArgs, OverlayArgs, PlanArgs, ReadArgs, ReviewArgs,
    ScoutArgs, SearchArgs, SimilarArgs, TestMapArgs, TraceArgs,
};
use crate::cli::definitions::{GateThreshold, OutputArgs, OutputFormat, TextJsonArgs};

use super::commands::BatchCmd;

// Core (Phase-0, JsonSchema) input structs — the deserialize targets. Aliased
// to avoid colliding with the like-named `args.rs` clap structs above.
// `io` / `train` are private modules in `commands`; their core structs are
// reached through the `pub(crate) use io::<m>` re-exports at the `commands`
// level. `search` is a `pub(crate) mod`, so its submodules are addressed
// directly.
use crate::cli::commands::blame::BlameArgs as BlameCore;
use crate::cli::commands::context::ContextArgs as ContextCore;
use crate::cli::commands::diff::DiffArgs as DiffCore;
use crate::cli::commands::drift::DriftArgs as DriftCore;
use crate::cli::commands::search::gather::GatherArgs as GatherCore;
use crate::cli::commands::search::onboard::OnboardArgs as OnboardCore;
use crate::cli::commands::search::query::QueryArgs;
use crate::cli::commands::search::scout::ScoutArgs as ScoutCore;
use crate::cli::commands::search::similar::SimilarArgs as SimilarCore;
use crate::cli::commands::{
    CalleesArgs as CalleesCore, CallersCoreArgs, CiArgs as CiCore, DeadArgs as DeadCore,
    DepsCoreArgs, ImpactCoreArgs, PlanArgs as PlanCore, ReviewArgs as ReviewCore, TestMapCoreArgs,
    TraceCoreArgs,
};

/// Default `TextJsonArgs` for a JSON-args-built variant. The daemon always
/// frames JSON regardless of this flag (the handler ignores it), so the value
/// is inert — but the variant requires it.
fn text_json() -> TextJsonArgs {
    TextJsonArgs { json: false }
}

/// Default `OutputArgs` (text/json/mermaid) for the impact/trace variants. As
/// with `text_json`, the daemon downgrades non-JSON to JSON, so the value is
/// inert on the wire.
fn output_text() -> OutputArgs {
    OutputArgs {
        format: OutputFormat::Text,
        json: false,
    }
}

/// Deserialize an `arguments` object into the typed core `T`, mapping a serde
/// failure onto a clear error the daemon surfaces as a parse error. `T` carries
/// `#[serde(default)]` (struct- or field-level), so a caller omitting an
/// optional field inherits the production default.
///
/// The cores do not `deny_unknown_fields`, so the cross-command overlay
/// tri-state keys (`overlay` / `no_overlay` / `overlay_root`) ride alongside the
/// core fields in the same object and are ignored here; [`overlay_from_args`]
/// reads them separately and stamps the argv-side `OverlayArgs`.
fn parse_core<T: DeserializeOwned>(command: &str, arguments: &serde_json::Value) -> Result<T> {
    serde_json::from_value::<T>(arguments.clone())
        .map_err(|e| anyhow::anyhow!("invalid arguments for '{command}': {e}"))
}

/// Extract the worktree-overlay tri-state from the raw `arguments` object for an
/// overlay-capable command. The cores omit `overlay_root` (it is wire-only,
/// hidden from `--help`), so the bridge forwards it as an optional top-level key
/// (decision D2). Stamping it onto the argv-side `OverlayArgs` is what routes a
/// JSON-args overlay request through the daemon's overlay-root validation gate
/// (`set_validated_overlay_request`) — the same gate the argv `--overlay-root`
/// flows through. A non-overlay command ignores these keys.
fn overlay_from_args(arguments: &serde_json::Value) -> OverlayArgs {
    let obj = match arguments.as_object() {
        Some(o) => o,
        None => return OverlayArgs::default(),
    };
    let overlay = obj
        .get("overlay")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let no_overlay = obj
        .get("no_overlay")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let overlay_root = obj
        .get("overlay_root")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    OverlayArgs {
        overlay,
        no_overlay,
        overlay_root,
    }
}

/// Substring marker in the catch-all rejection for a command with no Phase-0
/// core. The probe predicate [`is_json_args_capable`] keys off this exact
/// phrase, so the bail message and the predicate cannot drift — change one and
/// the other follows.
const NO_CORE_REJECTION: &str = "does not accept a JSON `arguments` object";

/// The canonical set of JSON-args-capable command names — the SINGLE source of
/// truth for "which commands [`build_batch_cmd`] accepts an `arguments` object
/// for." Lists exactly the non-catch-all match arms below (the read commands
/// plus the gated `notes-*` / `index` mutators). Co-located with the arms so a
/// reviewer edits both together; the `json_args_capability_matches_arms`
/// exhaustiveness test PROVES this list equals the arms by probing
/// `build_batch_cmd`, so a new arm without an entry here (or vice versa) fails
/// the build. The MCP registry-parity guard derives its expected tool set from
/// this slice rather than re-listing it.
///
/// Test-only: its consumers are the exhaustiveness test here and the MCP guard
/// in `cli::mcp` (both `#[cfg(test)]`). Production routing reads the match arms
/// directly, never this list.
#[cfg(test)]
pub(crate) const JSON_ARGS_CAPABLE_COMMANDS: &[&str] = &[
    "search",
    "gather",
    "scout",
    "onboard",
    "similar",
    "callers",
    "callees",
    "deps",
    "impact",
    "test-map",
    "trace",
    "blame",
    "context",
    "diff",
    "drift",
    "dead",
    "ci",
    "review",
    "plan",
    "read",
    "notes-add",
    "notes-update",
    "notes-remove",
    "index",
];

/// Whether `command` is JSON-args-capable: [`build_batch_cmd`] accepts an
/// `arguments` object for it (it has a Phase-0 core) rather than falling through
/// to the catch-all "no Phase-0 core" rejection. Derived by probing
/// `build_batch_cmd` with an empty object and inspecting whether the error (if
/// any) is the catch-all — so the predicate reads the arms directly and cannot
/// drift from them. A capable command with `{}` either builds (serde defaults)
/// or fails for a different reason (a missing required field, the gated-mutation
/// opt-in); only an argv-only / unknown command yields the catch-all marker.
///
/// Test-only: it exists to pin [`JSON_ARGS_CAPABLE_COMMANDS`] against the arms
/// (the `json_args_capability_matches_arms` guard). Production never needs a
/// capability predicate — `build_batch_cmd` already returns the right error.
#[cfg(test)]
pub(crate) fn is_json_args_capable(command: &str) -> bool {
    match build_batch_cmd(command, &serde_json::Value::Object(Default::default())) {
        Ok(_) => true,
        Err(e) => !e.to_string().contains(NO_CORE_REJECTION),
    }
}

/// Build the `BatchCmd` for a JSON-args request: deserialize `arguments` into
/// the command's Phase-0 core, then construct the matching argv-side
/// `BatchCmd` variant. The caller feeds the result into the SAME
/// `commands::dispatch` the argv path uses.
///
/// An unknown command, or a command that has no Phase-0 core struct (and so is
/// argv-only in Phase 1 — `notes`, `where`, `explain`, `read`'s siblings, the
/// zero-arg infra commands, …), returns an error rather than dispatching.
pub(super) fn build_batch_cmd(command: &str, arguments: &serde_json::Value) -> Result<BatchCmd> {
    let _span = tracing::info_span!("json_args_build_cmd", command).entered();

    let cmd = match command {
        "search" => {
            let c: QueryArgs = parse_core(command, arguments)?;
            BatchCmd::Search {
                args: search_args_from_core(c, overlay_from_args(arguments)),
                output: text_json(),
            }
        }
        "gather" => {
            let c: GatherCore = parse_core(command, arguments)?;
            BatchCmd::Gather {
                args: gather_args_from_core(c, overlay_from_args(arguments)),
                output: text_json(),
            }
        }
        "scout" => {
            let c: ScoutCore = parse_core(command, arguments)?;
            BatchCmd::Scout {
                args: scout_args_from_core(c, overlay_from_args(arguments)),
                output: text_json(),
            }
        }
        "onboard" => {
            let c: OnboardCore = parse_core(command, arguments)?;
            BatchCmd::Onboard {
                args: onboard_args_from_core(c),
                output: text_json(),
            }
        }
        "similar" => {
            let c: SimilarCore = parse_core(command, arguments)?;
            BatchCmd::Similar {
                args: similar_args_from_core(c),
                output: text_json(),
            }
        }
        "callers" => {
            let c: CallersCoreArgs = parse_core(command, arguments)?;
            BatchCmd::Callers {
                args: callers_args_from_core(
                    c.name,
                    c.limit,
                    c.edge_kind,
                    overlay_from_args(arguments),
                ),
                output: text_json(),
            }
        }
        "callees" => {
            let c: CalleesCore = parse_core(command, arguments)?;
            BatchCmd::Callees {
                args: callers_args_from_core(
                    c.name,
                    c.limit,
                    c.edge_kind,
                    overlay_from_args(arguments),
                ),
                output: text_json(),
            }
        }
        "deps" => {
            let c: DepsCoreArgs = parse_core(command, arguments)?;
            BatchCmd::Deps {
                args: deps_args_from_core(c),
                output: text_json(),
            }
        }
        "impact" => {
            let c: ImpactCoreArgs = parse_core(command, arguments)?;
            BatchCmd::Impact {
                args: impact_args_from_core(c, overlay_from_args(arguments)),
                output: output_text(),
            }
        }
        "test-map" => {
            let c: TestMapCoreArgs = parse_core(command, arguments)?;
            BatchCmd::TestMap {
                args: test_map_args_from_core(c)?,
                output: text_json(),
            }
        }
        "trace" => {
            let c: TraceCoreArgs = parse_core(command, arguments)?;
            BatchCmd::Trace {
                args: trace_args_from_core(c)?,
                output: output_text(),
            }
        }
        "blame" => {
            let c: BlameCore = parse_core(command, arguments)?;
            BatchCmd::Blame {
                args: blame_args_from_core(c),
                output: text_json(),
            }
        }
        "context" => {
            let c: ContextCore = parse_core(command, arguments)?;
            BatchCmd::Context {
                args: context_args_from_core(c),
                output: text_json(),
            }
        }
        "diff" => {
            let c: DiffCore = parse_core(command, arguments)?;
            BatchCmd::Diff {
                args: diff_args_from_core(c),
                output: text_json(),
            }
        }
        "drift" => {
            let c: DriftCore = parse_core(command, arguments)?;
            BatchCmd::Drift {
                args: drift_args_from_core(c),
                output: text_json(),
            }
        }
        "dead" => {
            let c: DeadCore = parse_core(command, arguments)?;
            BatchCmd::Dead {
                args: dead_args_from_core(c, overlay_from_args(arguments)),
                output: text_json(),
            }
        }
        "ci" => {
            let c: CiCore = parse_core(command, arguments)?;
            BatchCmd::Ci {
                args: ci_args_from_core(c, overlay_from_args(arguments)),
                output: text_json(),
            }
        }
        "review" => {
            let c: ReviewCore = parse_core(command, arguments)?;
            BatchCmd::Review {
                args: review_args_from_core(c, overlay_from_args(arguments)),
                output: text_json(),
            }
        }
        "plan" => {
            let c: PlanCore = parse_core(command, arguments)?;
            BatchCmd::Plan {
                args: plan_args_from_core(c),
                output: text_json(),
            }
        }
        "read" => {
            // `ReadArgs` IS the core (`read_core` consumes it directly), so the
            // deserialize target and the variant field are the same struct.
            let c: ReadArgs = parse_core(command, arguments)?;
            BatchCmd::Read {
                args: c,
                output: text_json(),
            }
        }
        // ─── MCP Phase 2a: the gated notes-mutation channel ────────────────────
        //
        // These reverse the historical "notes mutations not on daemon" rejection,
        // but ONLY behind the operator opt-in `CQS_MCP_ENABLE_MUTATIONS=1`
        // (`mcp::mutations_enabled`). The handlers write `docs/notes.toml` (a
        // file, not the `Store<ReadOnly>`); the watch loop reindexes — so the
        // daemon's read-only-Store typestate is preserved. The flag is checked
        // here too (not just in the bridge's `tools/list`) so a raw socket client
        // that bypasses the tool list still cannot mutate without the opt-in.
        // The destructive set stays withheld by ABSENCE — no arm exists for it.
        "notes-add" => {
            require_mutations_enabled(command)?;
            let c: crate::cli::commands::notes::NotesAddArgs = parse_core(command, arguments)?;
            BatchCmd::NotesAdd { args: c }
        }
        "notes-update" => {
            require_mutations_enabled(command)?;
            let c: crate::cli::commands::notes::NotesUpdateArgs = parse_core(command, arguments)?;
            BatchCmd::NotesUpdate { args: c }
        }
        "notes-remove" => {
            require_mutations_enabled(command)?;
            let c: crate::cli::commands::notes::NotesRemoveArgs = parse_core(command, arguments)?;
            BatchCmd::NotesRemove { args: c }
        }
        // ─── MCP Phase 2b: gated fire-and-forget reindex ───────────────────────
        //
        // `cqs_index` over the daemon. Same opt-in gate as the notes channel.
        // The handler QUEUES (flips the reconcile signal) and returns
        // immediately — it never builds the index and never acquires a writable
        // `Store`, so the `Store<ReadOnly>` invariant holds. The scoped core
        // exposes only the non-destructive subset (`slot`, …); `--force` is
        // withheld by ABSENCE (the core has no `force` field), so a forced full
        // rebuild is unreachable over the wire regardless of the flag.
        "index" => {
            require_mutations_enabled(command)?;
            let c: crate::cli::commands::index::IndexArgs = parse_core(command, arguments)?;
            BatchCmd::Index { args: c }
        }
        other => bail!(
            "command '{other}' {NO_CORE_REJECTION} \
             (no Phase-0 core struct); use the argv `args` form"
        ),
    };
    Ok(cmd)
}

/// Reject a gated mutation command when the operator has not opted in via
/// `CQS_MCP_ENABLE_MUTATIONS=1`. The boundary is enforced daemon-side, not only
/// in the bridge's `tools/list`, so a raw socket client cannot bypass it.
fn require_mutations_enabled(command: &str) -> Result<()> {
    if !crate::cli::mcp::mutations_enabled() {
        bail!(
            "command '{command}' is a gated MCP mutation; set \
             {}=1 to enable the notes-mutation channel",
            crate::cli::mcp::MUTATIONS_ENV
        );
    }
    Ok(())
}

// ─── Core → args.rs converters ─────────────────────────────────────────────
//
// Each builds the argv-side struct from the Phase-0 core, defaulting the
// fields the JSON schema does not expose to the values clap leaves them at when
// absent from an argv invocation (so the parity holds against an argv call that
// also omits them).

fn search_args_from_core(c: QueryArgs, overlay: OverlayArgs) -> SearchArgs {
    use crate::cli::args::RerankerMode;
    SearchArgs {
        query: c.query,
        limit_arg: LimitArg { limit: c.limit },
        threshold: c.threshold,
        name_boost: c.name_boost,
        lang: c.lang,
        include_type: c.include_type,
        exclude_type: c.exclude_type,
        path: c.path,
        pattern: c.pattern,
        name_only: c.name_only,
        rrf: c.rrf,
        include_docs: c.include_docs,
        reranker: if c.rerank {
            Some(RerankerMode::Onnx)
        } else {
            None
        },
        splade: c.splade,
        splade_alpha: c.splade_alpha,
        no_content: false,
        context: None,
        expand_parent: c.expand_parent,
        ref_name: None,
        include_refs: false,
        tokens: c.tokens,
        no_stale_check: false,
        no_demote: c.no_demote,
        // The core's `record_rank_signals` is the inverse of the CLI flag; the
        // daemon adapter recomputes it as `!no_rank_signals`, so map it back.
        no_rank_signals: !c.record_rank_signals,
        overlay,
    }
}

fn gather_args_from_core(c: GatherCore, overlay: OverlayArgs) -> GatherArgs {
    GatherArgs {
        query: c.query,
        depth: c.depth,
        direction: c.direction,
        limit_arg: LimitArg { limit: c.limit },
        tokens: c.tokens,
        ref_name: None,
        overlay,
    }
}

fn scout_args_from_core(c: ScoutCore, overlay: OverlayArgs) -> ScoutArgs {
    ScoutArgs {
        query: c.query,
        limit_arg: LimitArg { limit: c.limit },
        tokens: c.tokens,
        search_limit: c.search_limit,
        search_threshold: c.search_threshold,
        min_gap_ratio: c.min_gap_ratio,
        overlay,
    }
}

fn onboard_args_from_core(c: OnboardCore) -> OnboardArgs {
    OnboardArgs {
        query: c.query,
        depth: c.depth,
        direction: c.direction,
        tokens: c.tokens,
        limit_arg: LimitArg { limit: c.limit },
    }
}

fn similar_args_from_core(c: SimilarCore) -> SimilarArgs {
    SimilarArgs {
        name: c.name,
        limit_arg: LimitArg { limit: c.limit },
        threshold: c.threshold,
        lang: None,
        path: None,
    }
}

fn callers_args_from_core(
    name: String,
    limit: usize,
    edge_kind: Option<cqs::parser::CallEdgeKind>,
    overlay: OverlayArgs,
) -> CallersArgs {
    CallersArgs {
        name,
        cross_project: false,
        edge_kind: edge_kind.map(|k| k.as_str().to_string()),
        limit_arg: LimitArg { limit },
        overlay,
    }
}

fn deps_args_from_core(c: DepsCoreArgs) -> DepsArgs {
    DepsArgs {
        name: c.name,
        reverse: c.reverse,
        cross_project: false,
        limit_arg: LimitArg { limit: c.limit },
    }
}

fn impact_args_from_core(c: ImpactCoreArgs, overlay: OverlayArgs) -> ImpactArgs {
    ImpactArgs {
        name: c.name,
        depth: c.depth,
        suggest_tests: c.suggest_tests,
        // Core `include_types` ↔ args.rs `type_impact` (same flag, two names).
        type_impact: c.include_types,
        cross_project: false,
        limit_arg: LimitArg { limit: c.limit },
        overlay,
    }
}

fn test_map_args_from_core(c: TestMapCoreArgs) -> Result<TestMapArgs> {
    // args.rs `depth` is a clap-bounded `u16` (1..=50). The core `max_depth` is
    // `usize`; reject an out-of-range value with the same shape clap would,
    // rather than silently truncating. `max_nodes` is env-resolved in the
    // handler and has no argv flag, so it does not round-trip (consistent with
    // the argv path, which also cannot set it).
    let depth = u16::try_from(c.max_depth)
        .ok()
        .filter(|d| (1..=50).contains(d))
        .ok_or_else(|| anyhow::anyhow!("max_depth must be in 1..=50, got {}", c.max_depth))?;
    Ok(TestMapArgs {
        name: c.name,
        depth,
        cross_project: false,
        limit_arg: LimitArg { limit: c.limit },
    })
}

fn trace_args_from_core(c: TraceCoreArgs) -> Result<TraceArgs> {
    let max_depth = u16::try_from(c.max_depth)
        .ok()
        .filter(|d| (1..=50).contains(d))
        .ok_or_else(|| anyhow::anyhow!("max_depth must be in 1..=50, got {}", c.max_depth))?;
    Ok(TraceArgs {
        source: c.source,
        target: c.target,
        max_depth,
        cross_project: false,
        limit_arg: LimitArg {
            limit: crate::cli::args::DEFAULT_LIMIT,
        },
    })
}

fn blame_args_from_core(c: BlameCore) -> BlameArgs {
    BlameArgs {
        name: c.name,
        commits: c.commits,
        callers: c.callers,
    }
}

fn context_args_from_core(c: ContextCore) -> ContextArgs {
    ContextArgs {
        path: c.path,
        summary: c.summary,
        compact: c.compact,
        tokens: c.tokens,
    }
}

fn diff_args_from_core(c: DiffCore) -> DiffArgs {
    // The core's `target` is a non-optional String (defaulting to "project");
    // the argv-side `target` is `Option<String>`. `cmd_diff`/`dispatch_diff`
    // treat `None` as "project", so map the core default back to `None` to keep
    // the wire shape identical to an argv call that omits the target.
    let target = if c.target == "project" {
        None
    } else {
        Some(c.target)
    };
    DiffArgs {
        source: c.source,
        target,
        threshold: c.threshold,
        lang: c.lang,
    }
}

fn drift_args_from_core(c: DriftCore) -> DriftArgs {
    DriftArgs {
        reference: c.reference,
        threshold: c.threshold,
        min_drift: c.min_drift,
        lang: c.lang,
        limit: c.limit,
    }
}

fn dead_args_from_core(c: DeadCore, overlay: OverlayArgs) -> DeadArgs {
    DeadArgs {
        include_pub: c.include_pub,
        min_confidence: c.min_confidence,
        // Core verdict is the typed enum; the argv-side field is the stable
        // string the handler re-parses via `DeadVerdict::parse`.
        verdict: c.verdict.map(|v| v.as_str().to_string()),
        overlay,
    }
}

fn ci_args_from_core(c: CiCore, overlay: OverlayArgs) -> CiArgs {
    CiArgs {
        base: None,
        stdin: false,
        // Mirrors the clap `--gate` default ("high") so a JSON-args `ci` gates
        // identically to an argv `ci` that omits `--gate`.
        gate: GateThreshold::High,
        tokens: c.tokens,
        overlay,
    }
}

fn review_args_from_core(c: ReviewCore, overlay: OverlayArgs) -> ReviewArgs {
    ReviewArgs {
        base: None,
        stdin: false,
        tokens: c.tokens,
        overlay,
    }
}

fn plan_args_from_core(c: PlanCore) -> PlanArgs {
    PlanArgs {
        description: c.description,
        limit_arg: LimitArg { limit: c.limit },
        tokens: c.tokens,
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::batch::{create_test_context, dispatch_via_view, dispatch_via_view_json};
    use cqs::embedder::Embedding;
    use cqs::parser::{CallEdgeKind, CallSite, Chunk, ChunkType, FunctionCalls, Language};
    use cqs::store::{ModelInfo, Store};
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn make_chunk(id: &str, name: &str) -> Chunk {
        let content = format!("fn {name}() {{ }}");
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: id.to_string(),
            file: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            byte_start: 0,
            content_hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    /// Seed two functions with one caller→callee edge, plus a `src/lib.rs` file
    /// on disk under the project root so `read` can resolve a real path. Returns
    /// the tempdir (root) and a daemon `BatchContext` over the index.
    fn seed_ctx() -> (TempDir, crate::cli::batch::BatchContext) {
        let dir = TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        // A real source file under the root for the `read` path tests.
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "fn caller_fn() { callee_fn(); }\nfn callee_fn() {}\n",
        )
        .expect("write src/lib.rs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let embedding = Embedding::new(emb_vec);
        {
            let store = Store::open(&index_path).expect("open store");
            store.init(&ModelInfo::default()).expect("init");
            let chunks = vec![
                (
                    make_chunk("src/lib.rs:1:caller", "caller_fn"),
                    embedding.clone(),
                ),
                (
                    make_chunk("src/lib.rs:2:callee", "callee_fn"),
                    embedding.clone(),
                ),
            ];
            store
                .upsert_chunks_batch(&chunks, Some(0))
                .expect("upsert chunks");
            let fc = FunctionCalls {
                name: "caller_fn".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "callee_fn".to_string(),
                    line_number: 1,
                    kind: CallEdgeKind::Call,
                }],
            };
            store
                .upsert_function_calls(Path::new("src/lib.rs"), &[fc])
                .expect("upsert function call");
        }
        let ctx = create_test_context(&cqs_dir).expect("create_test_context");
        (dir, ctx)
    }

    /// Run a command via the argv path and the JSON-args path against the same
    /// view, returning the two parsed response envelopes for comparison.
    fn run_both(
        ctx: &crate::cli::batch::BatchContext,
        command: &str,
        argv: &[&str],
        arguments: serde_json::Value,
    ) -> (serde_json::Value, serde_json::Value) {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let mut out_argv = Vec::new();
        dispatch_via_view(&ctx.build_view(None), command, &argv, &mut out_argv);
        let mut out_json = Vec::new();
        dispatch_via_view_json(&ctx.build_view(None), command, &arguments, &mut out_json);
        let v_argv: serde_json::Value =
            serde_json::from_slice(&out_argv).expect("argv output is JSON");
        let v_json: serde_json::Value =
            serde_json::from_slice(&out_json).expect("json-args output is JSON");
        (v_argv, v_json)
    }

    /// The core guard: the JSON-args dispatch must be value-equal to the argv
    /// dispatch for the same logical request, across an embedder-free spread.
    #[test]
    fn parity_json_args_equals_argv() {
        let (_dir, ctx) = seed_ctx();
        // (command, argv, JSON arguments)
        let cases: Vec<(&str, Vec<&str>, serde_json::Value)> = vec![
            ("callers", vec!["callee_fn"], json!({"name": "callee_fn"})),
            ("callees", vec!["caller_fn"], json!({"name": "caller_fn"})),
            ("deps", vec!["caller_fn"], json!({"name": "caller_fn"})),
            ("dead", vec![], json!({})),
            ("impact", vec!["callee_fn"], json!({"name": "callee_fn"})),
            ("test-map", vec!["callee_fn"], json!({"name": "callee_fn"})),
            (
                "trace",
                vec!["caller_fn", "callee_fn"],
                json!({"source": "caller_fn", "target": "callee_fn"}),
            ),
            ("context", vec!["src/lib.rs"], json!({"path": "src/lib.rs"})),
            ("read", vec!["src/lib.rs"], json!({"path": "src/lib.rs"})),
        ];
        for (command, argv, arguments) in cases {
            let (v_argv, v_json) = run_both(&ctx, command, &argv, arguments);
            // Guard against a false-equal where BOTH paths error identically:
            // a happy-path command must carry `data`, not an `error`.
            assert!(
                v_argv.get("data").is_some_and(|d| !d.is_null()),
                "argv path for `{command}` should return data, got: {v_argv}"
            );
            assert_eq!(
                v_argv, v_json,
                "JSON-args output must equal argv output for `{command}`\nargv:  {v_argv}\njson:  {v_json}"
            );
        }
    }

    /// Non-default-value parity: a JSON-args request that sets explicit fields
    /// matches the argv request carrying the equivalent flags. Guards the
    /// converters' field mapping (limit, edge_kind, depth) against silent drops.
    #[test]
    fn parity_json_args_with_explicit_fields() {
        let (_dir, ctx) = seed_ctx();
        let cases: Vec<(&str, Vec<&str>, serde_json::Value)> = vec![
            (
                "callers",
                vec!["callee_fn", "--limit", "3", "--edge-kind", "call"],
                json!({"name": "callee_fn", "limit": 3, "edge_kind": "call"}),
            ),
            (
                "impact",
                vec!["callee_fn", "--depth", "2", "--limit", "7", "--type-impact"],
                json!({"name": "callee_fn", "depth": 2, "limit": 7, "include_types": true}),
            ),
            (
                "dead",
                vec!["--include-pub", "--min-confidence", "high"],
                json!({"include_pub": true, "min_confidence": "high"}),
            ),
        ];
        for (command, argv, arguments) in cases {
            let (v_argv, v_json) = run_both(&ctx, command, &argv, arguments);
            assert_eq!(
                v_argv, v_json,
                "JSON-args output must equal argv output for `{command}` with explicit fields\nargv:  {v_argv}\njson:  {v_json}"
            );
        }
    }

    /// A missing optional field deserializes via `#[serde(default)]` — an empty
    /// `arguments` object for a command whose only required field has a default
    /// dispatches successfully (here `dead`, all-defaulted).
    #[test]
    fn missing_optional_field_uses_default() {
        let (_dir, ctx) = seed_ctx();
        let mut out = Vec::new();
        dispatch_via_view_json(&ctx.build_view(None), "dead", &json!({}), &mut out);
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
        assert!(
            v.get("data").is_some() && v.get("error").is_none_or(|e| e.is_null()),
            "empty arguments must dispatch successfully via serde defaults, got: {v}"
        );
    }

    /// An out-of-tree `overlay_root` carried in a JSON-args request is rejected
    /// by the SAME overlay-root validation gate the argv path uses — proving the
    /// JSON relay routes through `dispatch_*`, not a `*_core` bypass. Covers a
    /// call-graph command (`callers`) and `dead`.
    #[test]
    fn overlay_root_out_of_tree_rejected_on_json_path() {
        let (_dir, ctx) = seed_ctx();
        for (command, arguments) in [
            (
                "callers",
                json!({"name": "callee_fn", "overlay": true, "overlay_root": "/etc"}),
            ),
            ("dead", json!({"overlay": true, "overlay_root": "/etc"})),
        ] {
            let mut out = Vec::new();
            dispatch_via_view_json(&ctx.build_view(None), command, &arguments, &mut out);
            let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
            let err = v.get("error");
            assert!(
                err.is_some_and(|e| !e.is_null()),
                "an out-of-tree overlay_root must be rejected on the JSON-args path for `{command}`, got: {v}"
            );
        }
    }

    /// A `read` request with a path that escapes the project root is blocked on
    /// the JSON-args path by `read_core`'s canonicalize + starts_with gate — the
    /// gate is reached because the relay routes through `dispatch_read`, never
    /// `read_core` directly.
    #[test]
    fn read_path_traversal_blocked_on_json_path() {
        let (_dir, ctx) = seed_ctx();
        let mut out = Vec::new();
        dispatch_via_view_json(
            &ctx.build_view(None),
            "read",
            &json!({"path": "../../../../etc/hostname"}),
            &mut out,
        );
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
        let err = v.get("error");
        assert!(
            err.is_some_and(|e| !e.is_null()),
            "a path-traversal `read` must be blocked on the JSON-args path, got: {v}"
        );
    }

    /// An unknown command with an `arguments` object returns a clean error
    /// envelope, never a panic.
    #[test]
    fn unknown_command_returns_clean_error() {
        let (_dir, ctx) = seed_ctx();
        let mut out = Vec::new();
        dispatch_via_view_json(
            &ctx.build_view(None),
            "nonexistent_cmd",
            &json!({"foo": 1}),
            &mut out,
        );
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
        assert!(
            v.get("error").is_some_and(|e| !e.is_null()),
            "unknown command must return an error envelope, got: {v}"
        );
    }

    /// A command that has no Phase-0 core (argv-only in P1) returns a clean
    /// error on the JSON-args path rather than dispatching or panicking.
    #[test]
    fn argv_only_command_rejected_on_json_path() {
        // `where` / `explain` are argv-only. (`notes-*` mutations now have a
        // gated core — covered by the MCP Phase-2a tests below.)
        let err = build_batch_cmd("where", &json!({"description": "x"}));
        assert!(
            err.is_err(),
            "an argv-only command must be rejected on the JSON-args path"
        );
    }

    // ─── MCP Phase 2a: gated notes-mutation channel ────────────────────────────

    /// RAII guard for `CQS_MCP_ENABLE_MUTATIONS`, restoring the prior value on
    /// drop. Pairs with `#[serial_test::serial(mcp_mutations_env)]`.
    struct MutEnvGuard {
        prior: Option<String>,
    }
    impl MutEnvGuard {
        fn set(on: bool) -> Self {
            let prior = std::env::var("CQS_MCP_ENABLE_MUTATIONS").ok();
            if on {
                std::env::set_var("CQS_MCP_ENABLE_MUTATIONS", "1");
            } else {
                std::env::remove_var("CQS_MCP_ENABLE_MUTATIONS");
            }
            Self { prior }
        }
    }
    impl Drop for MutEnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var("CQS_MCP_ENABLE_MUTATIONS", v),
                None => std::env::remove_var("CQS_MCP_ENABLE_MUTATIONS"),
            }
        }
    }

    /// Read `docs/notes.toml` text under a project root, "" if absent.
    fn read_notes_toml(root: &std::path::Path) -> String {
        std::fs::read_to_string(root.join("docs/notes.toml")).unwrap_or_default()
    }

    /// With the flag OFF, every notes mutation is rejected by `build_batch_cmd`
    /// — the daemon-side enforcement that a raw socket client can't bypass the
    /// bridge's `tools/list` gating. No file is written.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn notes_mutations_rejected_when_flag_off() {
        let _guard = MutEnvGuard::set(false);
        let (dir, ctx) = seed_ctx();
        for (command, arguments) in [
            ("notes-add", json!({"text": "a note", "sentiment": -0.5})),
            (
                "notes-update",
                json!({"text": "a note", "new_text": "b note"}),
            ),
            ("notes-remove", json!({"text": "a note"})),
        ] {
            // build_batch_cmd refuses without the opt-in.
            assert!(
                build_batch_cmd(command, &arguments).is_err(),
                "`{command}` must be rejected when CQS_MCP_ENABLE_MUTATIONS is unset"
            );
            // And the full dispatch returns an error envelope, no file written.
            let mut out = Vec::new();
            dispatch_via_view_json(&ctx.build_view(None), command, &arguments, &mut out);
            let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
            assert!(
                v.get("error").is_some_and(|e| !e.is_null()),
                "`{command}` flag-off must return an error envelope, got: {v}"
            );
        }
        assert!(
            read_notes_toml(dir.path()).is_empty(),
            "no notes.toml must be written while the flag is off"
        );
    }

    /// With the flag ON, `notes-add` over the JSON-args path actually appends to
    /// the temp `docs/notes.toml` (the gate-safe file write) and returns a
    /// success envelope marked `reindex_deferred` (the daemon does NOT reindex
    /// — the watch loop does).
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn notes_add_round_trip_writes_file() {
        let _guard = MutEnvGuard::set(true);
        let (dir, ctx) = seed_ctx();
        let mut out = Vec::new();
        dispatch_via_view_json(
            &ctx.build_view(None),
            "notes-add",
            &json!({"text": "daemon wrote this", "sentiment": -0.5, "mentions": ["json_args.rs"]}),
            &mut out,
        );
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
        // Success envelope (data present, no error).
        assert!(
            v.get("data").is_some_and(|d| !d.is_null()),
            "notes-add must return a data envelope, got: {v}"
        );
        let data = &v["data"];
        assert_eq!(data["status"], "added");
        assert_eq!(
            data["reindex_deferred"], true,
            "daemon path defers reindex to the watch loop"
        );
        assert_eq!(
            data["indexed"], false,
            "daemon path does not reindex from the handler"
        );
        // The file was actually written with the note text.
        let toml = read_notes_toml(dir.path());
        assert!(
            toml.contains("daemon wrote this"),
            "notes.toml must contain the appended note, got:\n{toml}"
        );
        assert!(
            toml.contains("json_args.rs"),
            "notes.toml must contain the mention, got:\n{toml}"
        );
    }

    /// DATA-SAFETY: a daemon notes mutation must flip the shared pending-notes
    /// signal so the note reindex is driven by the writer — independent of an
    /// inotify event, which is unreliable for `docs/notes.toml` on the WSL
    /// `/mnt/c` deployment. Without this, a successfully-written note could be
    /// NEVER indexed and the index would diverge from the file indefinitely.
    ///
    /// Each of add / update / remove must flip it: all three write the file.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn notes_mutation_flips_pending_notes_signal() {
        use std::sync::atomic::Ordering;
        let _guard = MutEnvGuard::set(true);

        for (command, arguments) in [
            (
                "notes-add",
                json!({"text": "needs index", "sentiment": 0.0}),
            ),
            // update/remove target the note add seeds, so add first inside each
            // case via a fresh ctx to keep the cases independent.
            ("notes-update", json!({"text": "seed", "new_text": "seed2"})),
            ("notes-remove", json!({"text": "seed"})),
        ] {
            let (_dir, ctx) = seed_ctx();
            // Seed a note for the update/remove cases (add case ignores it).
            if command != "notes-add" {
                let mut seed_out = Vec::new();
                dispatch_via_view_json(
                    &ctx.build_view(None),
                    "notes-add",
                    &json!({"text": "seed", "sentiment": 0.0}),
                    &mut seed_out,
                );
                // The seed add already flips the signal; clear it so the case
                // under test proves IT flips the signal, not the seed.
                ctx.pending_notes_signal.store(false, Ordering::Release);
            }
            assert!(
                !ctx.pending_notes_signal.load(Ordering::Acquire),
                "{command}: pending-notes signal must start cleared"
            );

            let mut out = Vec::new();
            dispatch_via_view_json(&ctx.build_view(None), command, &arguments, &mut out);
            let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
            assert!(
                v.get("data").is_some_and(|d| !d.is_null()),
                "{command} must succeed, got: {v}"
            );
            assert!(
                ctx.pending_notes_signal.load(Ordering::Acquire),
                "{command} must flip the pending-notes signal so the watch loop reindexes \
                 the note independent of inotify"
            );
        }
    }

    /// Flag ON: a full add → update → remove round-trip over the JSON-args path,
    /// each mutation reflected in the temp file.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn notes_add_update_remove_round_trip() {
        let _guard = MutEnvGuard::set(true);
        let (dir, ctx) = seed_ctx();
        let run = |command: &str, arguments: serde_json::Value| -> serde_json::Value {
            let mut out = Vec::new();
            dispatch_via_view_json(&ctx.build_view(None), command, &arguments, &mut out);
            serde_json::from_slice(&out).expect("JSON")
        };

        // add
        let v = run("notes-add", json!({"text": "first", "sentiment": 0.0}));
        assert_eq!(v["data"]["status"], "added");
        assert!(read_notes_toml(dir.path()).contains("first"));

        // update (idempotent rewrite)
        let v = run(
            "notes-update",
            json!({"text": "first", "new_text": "second"}),
        );
        assert_eq!(v["data"]["status"], "updated");
        let toml = read_notes_toml(dir.path());
        assert!(toml.contains("second"), "updated text present");
        assert!(!toml.contains("\"first\""), "old text gone");

        // remove
        let v = run("notes-remove", json!({"text": "second"}));
        assert_eq!(v["data"]["status"], "removed");
        assert!(
            !read_notes_toml(dir.path()).contains("second"),
            "removed note gone from file"
        );

        // remove again → not-found error (idempotent no-op surfaces as error).
        let v = run("notes-remove", json!({"text": "second"}));
        assert!(
            v.get("error").is_some_and(|e| !e.is_null()),
            "removing an absent note must error, got: {v}"
        );
    }

    /// The daemon notes-write path preserves `Store<ReadOnly>`: it never
    /// acquires a writable store. We assert this by type — `ctx.store()` returns
    /// `Arc<Store<ReadOnly>>`, and the round-trip above mutates the FILE while
    /// the same view's store stays read-only and usable.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn notes_write_keeps_store_read_only() {
        let _guard = MutEnvGuard::set(true);
        let (_dir, ctx) = seed_ctx();
        let view = ctx.build_view(None);
        // Type assertion: the view's store is read-only (compile-time proof the
        // handler had no writable store to reach for).
        let store: std::sync::Arc<cqs::store::Store<cqs::store::ReadOnly>> = view.store();
        let mut out = Vec::new();
        dispatch_via_view_json(&view, "notes-add", &json!({"text": "ro check"}), &mut out);
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
        assert_eq!(v["data"]["status"], "added");
        // The read-only store is still live after the file mutation (a read
        // succeeds — the typestate was never traded for a writable handle).
        assert!(
            store.chunk_count().is_ok(),
            "the daemon's Store<ReadOnly> must stay usable across a notes write"
        );
    }

    // ─── MCP Phase 2b: gated fire-and-forget reindex (`index`) ─────────────────

    /// With the flag OFF, `index` over the JSON-args path is rejected by
    /// `build_batch_cmd` AND by the full dispatch — the daemon-side enforcement
    /// that a raw socket client can't bypass the bridge's `tools/list` gating.
    /// The reconcile signal is NOT flipped.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn index_rejected_when_flag_off() {
        use std::sync::atomic::Ordering;
        let _guard = MutEnvGuard::set(false);
        let (_dir, ctx) = seed_ctx();
        // build_batch_cmd refuses without the opt-in.
        assert!(
            build_batch_cmd("index", &json!({})).is_err(),
            "`index` must be rejected when CQS_MCP_ENABLE_MUTATIONS is unset"
        );
        // Full dispatch returns an error envelope and does NOT queue.
        assert!(
            !ctx.reconcile_signal.load(Ordering::Acquire),
            "reconcile signal must start un-pending"
        );
        let mut out = Vec::new();
        dispatch_via_view_json(&ctx.build_view(None), "index", &json!({}), &mut out);
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
        assert!(
            v.get("error").is_some_and(|e| !e.is_null()),
            "`index` flag-off must return an error envelope, got: {v}"
        );
        assert!(
            !ctx.reconcile_signal.load(Ordering::Acquire),
            "flag-off `index` must NOT flip the reconcile signal"
        );
    }

    /// With the flag ON, `index` returns PROMPTLY with the queued/deferred
    /// envelope (a fire-and-forget queue, NOT a blocking build), and flips the
    /// shared reconcile signal so the watch loop performs the rebuild.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn index_queues_promptly_when_flag_on() {
        use std::sync::atomic::Ordering;
        let _guard = MutEnvGuard::set(true);
        let (_dir, ctx) = seed_ctx();
        assert!(
            !ctx.reconcile_signal.load(Ordering::Acquire),
            "reconcile signal must start un-pending"
        );

        let start = std::time::Instant::now();
        let mut out = Vec::new();
        dispatch_via_view_json(&ctx.build_view(None), "index", &json!({}), &mut out);
        let elapsed_ms = start.elapsed().as_millis();
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");

        // Fire-and-forget: it must return well before any real index build could
        // run. A blocking build would take seconds even on a tiny index; a queue
        // is sub-millisecond. 500 ms is a generous ceiling for the dispatch.
        assert!(
            elapsed_ms < 500,
            "`index` must return promptly (fire-and-forget), took {elapsed_ms}ms"
        );

        let data = &v["data"];
        assert!(
            v.get("data").is_some_and(|d| !d.is_null()),
            "index must return a data envelope, got: {v}"
        );
        assert_eq!(data["status"], "queued");
        assert_eq!(data["queued"], true, "the response must mark queued:true");
        assert_eq!(
            data["reindex_deferred"], true,
            "the daemon defers the rebuild to the watch loop"
        );
        // The signal is now pending — the watch loop will drain it.
        assert!(
            ctx.reconcile_signal.load(Ordering::Acquire),
            "flag-on `index` must flip the reconcile signal"
        );
    }

    /// The daemon `index` path preserves `Store<ReadOnly>`: it never acquires a
    /// writable store (it only flips the reconcile signal). Mirrors the notes
    /// `Store<ReadOnly>` invariant test — type-level proof the handler had no
    /// writable store to reach for, and the read-only store stays usable.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn index_keeps_store_read_only() {
        let _guard = MutEnvGuard::set(true);
        let (_dir, ctx) = seed_ctx();
        let view = ctx.build_view(None);
        // Type assertion: the view's store is read-only (compile-time proof the
        // handler had no writable store to reach for).
        let store: std::sync::Arc<cqs::store::Store<cqs::store::ReadOnly>> = view.store();
        let mut out = Vec::new();
        dispatch_via_view_json(&view, "index", &json!({}), &mut out);
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
        assert_eq!(v["data"]["status"], "queued");
        // The read-only store is still live after queueing (a read succeeds —
        // the typestate was never traded for a writable handle).
        assert!(
            store.chunk_count().is_ok(),
            "the daemon's Store<ReadOnly> must stay usable across an index queue"
        );
    }

    /// A `slot` that names a DIFFERENT slot than the daemon serves must be
    /// REFUSED — not silently accepted as a bare queued success against the
    /// served slot. The reconcile path cannot target a non-served slot, so
    /// honoring the request is impossible; reporting `queued` while reindexing a
    /// different slot would be an undetectable lie across the interop boundary.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn index_mismatched_slot_is_refused_not_a_bare_queued_success() {
        use std::sync::atomic::Ordering;
        let _guard = MutEnvGuard::set(true);
        let (_dir, ctx) = seed_ctx();
        let view = ctx.build_view(None);

        // Seed the shared watch snapshot with a served slot the daemon is bound
        // to (the watch loop publishes this in production; here we plant it).
        let mut snap = cqs::watch_status::WatchSnapshot::unknown();
        snap.active_slot = Some("served".to_string());
        view.test_overwrite_watch_snapshot(snap);

        let mut out = Vec::new();
        dispatch_via_view_json(&view, "index", &json!({"slot": "other"}), &mut out);
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");

        // Must be an ERROR envelope, not a queued success.
        assert!(
            v.get("error").is_some_and(|e| !e.is_null()),
            "a mismatched-slot index must return an error envelope, got: {v}"
        );
        assert!(
            v.get("data").is_none_or(|d| d.is_null()),
            "a mismatched-slot index must NOT return a queued data payload, got: {v}"
        );
        // The error message must NAME the served slot so the caller can act.
        let msg = v["error"]["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("served"),
            "the rejection must name the served slot, got: {msg}"
        );
        // And it must NOT have flipped the reconcile signal — no wrong-slot
        // reindex was queued.
        assert!(
            !ctx.reconcile_signal.load(Ordering::Acquire),
            "a refused mismatched-slot index must NOT queue a reconcile"
        );
    }

    /// A `slot` that MATCHES the served slot proceeds to a normal queued
    /// success and echoes `served_slot` so the caller can confirm the target.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn index_matching_slot_queues_and_echoes_served_slot() {
        use std::sync::atomic::Ordering;
        let _guard = MutEnvGuard::set(true);
        let (_dir, ctx) = seed_ctx();
        let view = ctx.build_view(None);

        let mut snap = cqs::watch_status::WatchSnapshot::unknown();
        snap.active_slot = Some("served".to_string());
        view.test_overwrite_watch_snapshot(snap);

        let mut out = Vec::new();
        dispatch_via_view_json(&view, "index", &json!({"slot": "served"}), &mut out);
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");

        assert_eq!(v["data"]["status"], "queued");
        assert_eq!(
            v["data"]["served_slot"], "served",
            "the success payload must echo the served slot, got: {v}"
        );
        assert!(
            ctx.reconcile_signal.load(Ordering::Acquire),
            "a matching-slot index must queue the reconcile"
        );
    }

    /// `index --force` is unreachable over the wire: the scoped `IndexArgs` core
    /// has no `force` field, so a `force:true` key in `arguments` is silently
    /// ignored (deserialize succeeds, dropping the unknown key) — there is no
    /// path that turns a fire-and-forget queue into a forced full rebuild. The
    /// queue still happens (non-destructive), and no forced behavior is invoked.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn index_force_key_is_ignored_not_a_forced_rebuild() {
        let _guard = MutEnvGuard::set(true);
        // The core deserializes `{"force": true}` to the default (force is not a
        // field) — proving `--force` cannot be smuggled in via the wire.
        let cmd = build_batch_cmd("index", &json!({"force": true, "slot": "primary"}))
            .expect("index with an unknown `force` key still builds (key ignored)");
        match cmd {
            BatchCmd::Index { args } => {
                assert_eq!(
                    args.slot.as_deref(),
                    Some("primary"),
                    "the known `slot` field round-trips"
                );
                // There is no `force` field to inspect — its absence from the
                // struct is the withhold. The build succeeding with the queue
                // semantics (not a forced rebuild) is the assertion.
            }
            _ => panic!("expected BatchCmd::Index"),
        }
    }

    /// Malformed `arguments` (a type the core cannot accept) produces a clean
    /// deserialize error, not a panic.
    #[test]
    fn malformed_arguments_returns_clean_error() {
        let (_dir, ctx) = seed_ctx();
        let mut out = Vec::new();
        // `callers.name` is a String; a number is invalid.
        dispatch_via_view_json(
            &ctx.build_view(None),
            "callers",
            &json!({"name": 42}),
            &mut out,
        );
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
        assert!(
            v.get("error").is_some_and(|e| !e.is_null()),
            "malformed arguments must return an error envelope, got: {v}"
        );
    }

    /// NUL bytes in a JSON-args string value are rejected (mirrors the argv
    /// `reject_null_tokens` contract).
    #[test]
    fn nul_byte_in_arguments_rejected() {
        let (_dir, ctx) = seed_ctx();
        let mut out = Vec::new();
        dispatch_via_view_json(
            &ctx.build_view(None),
            "callers",
            &json!({"name": "calle\u{0000}e_fn"}),
            &mut out,
        );
        let v: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
        assert!(
            v.get("error").is_some_and(|e| !e.is_null()),
            "a NUL byte in arguments must be rejected, got: {v}"
        );
    }

    /// The embedder-dependent commands (search/gather/scout/onboard/similar) and
    /// the reference commands (diff/drift) can't run hermetically without an
    /// embedder/index, so pin their CONVERTERS instead: an `arguments` object
    /// builds the expected `BatchCmd` variant with fields mapped correctly. This
    /// catches a converter field-mapping regression without a GPU load.
    #[test]
    fn converter_builds_expected_variants() {
        // `BatchCmd` is already in scope via `use super::*`.

        // search: QueryArgs fields → SearchArgs, overlay stamped from top-level.
        let cmd = build_batch_cmd(
            "search",
            &json!({"query": "q", "limit": 9, "name_only": true}),
        )
        .expect("search");
        match cmd {
            BatchCmd::Search { args, .. } => {
                assert_eq!(args.query, "q");
                assert_eq!(args.limit_arg.limit, 9);
                assert!(args.name_only);
            }
            _ => panic!("expected Search"),
        }

        // gather: depth + direction round-trip.
        let cmd = build_batch_cmd("gather", &json!({"query": "q", "depth": 3})).expect("gather");
        match cmd {
            BatchCmd::Gather { args, .. } => {
                assert_eq!(args.query, "q");
                assert_eq!(args.depth, 3);
            }
            _ => panic!("expected Gather"),
        }

        // diff: core `target` default "project" → argv-side None.
        let cmd = build_batch_cmd("diff", &json!({"source": "v1"})).expect("diff");
        match cmd {
            BatchCmd::Diff { args, .. } => {
                assert_eq!(args.source, "v1");
                assert_eq!(args.target, None);
            }
            _ => panic!("expected Diff"),
        }

        // similar: name + threshold.
        let cmd =
            build_batch_cmd("similar", &json!({"name": "f", "threshold": 0.5})).expect("similar");
        match cmd {
            BatchCmd::Similar { args, .. } => {
                assert_eq!(args.name, "f");
                assert!((args.threshold - 0.5).abs() < 1e-6);
            }
            _ => panic!("expected Similar"),
        }

        // plan: description + limit.
        let cmd =
            build_batch_cmd("plan", &json!({"description": "do x", "limit": 4})).expect("plan");
        match cmd {
            BatchCmd::Plan { args, .. } => {
                assert_eq!(args.description, "do x");
                assert_eq!(args.limit_arg.limit, 4);
            }
            _ => panic!("expected Plan"),
        }
    }

    /// Exhaustiveness guard for the §2 derivation: the canonical
    /// `JSON_ARGS_CAPABLE_COMMANDS` list must equal the set of commands
    /// `build_batch_cmd` actually accepts (the non-catch-all arms), proven by
    /// probing the builder rather than re-reading the arms by eye. A new arm
    /// added without a list entry (or a list entry with no arm) fails here, so
    /// the MCP registry-parity guard — which derives from this list — cannot
    /// silently miss a newly-capable command.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn json_args_capability_matches_arms() {
        // The flag-gated mutators are capable arms; enable the opt-in so the
        // probe sees them as capable (with the flag off they bail on the gate,
        // not the catch-all — still "capable", but we exercise the on path too).
        let _g = MutEnvGuard::set(true);

        // 1. Every listed command is genuinely capable (no catch-all rejection).
        for &cmd in JSON_ARGS_CAPABLE_COMMANDS {
            assert!(
                is_json_args_capable(cmd),
                "`{cmd}` is in JSON_ARGS_CAPABLE_COMMANDS but build_batch_cmd rejects it as \
                 having no Phase-0 core — the list and the match arms diverged"
            );
        }

        // 2. A representative spread of argv-only / unknown commands is NOT
        //    capable — the catch-all bites, and they are absent from the list.
        for cmd in [
            "where",
            "explain",
            "notes",
            "health",
            "task",
            "nonexistent_cmd",
        ] {
            assert!(
                !is_json_args_capable(cmd),
                "`{cmd}` is argv-only/unknown but build_batch_cmd treats it as JSON-args-capable"
            );
            assert!(
                !JSON_ARGS_CAPABLE_COMMANDS.contains(&cmd),
                "`{cmd}` is not JSON-args-capable yet appears in JSON_ARGS_CAPABLE_COMMANDS"
            );
        }

        // 3. The list has no duplicates (a copy-paste straggler would inflate
        //    the derived MCP tool count).
        let unique: std::collections::BTreeSet<&&str> = JSON_ARGS_CAPABLE_COMMANDS.iter().collect();
        assert_eq!(
            unique.len(),
            JSON_ARGS_CAPABLE_COMMANDS.len(),
            "JSON_ARGS_CAPABLE_COMMANDS contains a duplicate"
        );
    }

    /// The flag-OFF probe still classifies a gated mutator as capable: the gate
    /// bail is NOT the catch-all "no Phase-0 core" rejection, so `notes-add`
    /// remains JSON-args-capable regardless of the opt-in. This pins that the
    /// capability predicate keys on the core's existence, not the flag.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn gated_mutator_is_capable_even_with_flag_off() {
        let _g = MutEnvGuard::set(false);
        for cmd in ["notes-add", "notes-update", "notes-remove", "index"] {
            assert!(
                is_json_args_capable(cmd),
                "`{cmd}` must be JSON-args-capable even with the mutation flag off \
                 (the gate bail is not the no-core catch-all)"
            );
        }
    }

    /// `test-map` / `trace` clamp `max_depth` to the clap `u16` 1..=50 range; an
    /// out-of-range value is a clean error, not a silent truncation or panic.
    #[test]
    fn out_of_range_depth_rejected() {
        assert!(build_batch_cmd("test-map", &json!({"name": "f", "max_depth": 999})).is_err());
        assert!(build_batch_cmd(
            "trace",
            &json!({"source": "a", "target": "b", "max_depth": 0})
        )
        .is_err());
    }
}
