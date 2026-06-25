//! `tools/list` generation and `tools/call` dispatch.
//!
//! ## The tool surface (D1, D4b)
//!
//! The exposed tools are exactly the daemon's JSON-args-capable commands (the
//! 20 with a Phase-0 `schemars::JsonSchema` core, per
//! `cli::batch::json_args::build_batch_cmd`) MINUS the withheld set
//! (`context`, `explain` — their relay carries UNSCANNED doc/signature content,
//! RT-RELAY doc/signature gap; D4b). `explain` is not JSON-args-capable in the
//! first place, so the withhold materially removes only `context`. Each tool:
//! - Carries the `cqs_`-prefixed, underscore, noun-first name (D1):
//!   `cqs_search`, `cqs_callers`, `cqs_notes_add` (none in P1), … — the v0.10.0
//!   precedent. The lone hyphenated command `test-map` becomes `cqs_test_map`.
//! - Advertises an `inputSchema` generated from its Phase-0 core via
//!   `schemars::schema_for!` — the SAME struct the daemon deserializes the
//!   relayed `arguments` into. For an overlay-capable command the three
//!   worktree-overlay tri-state keys (`overlay` / `no_overlay` / `overlay_root`)
//!   are read off the raw wire object by the daemon's `overlay_from_args` rather
//!   than off the core struct, so they are injected into the advertised schema
//!   explicitly (`schema_with_overlay`) — keeping schema and wire contract from
//!   drifting on those keys too.
//! - Is annotated `readOnly` (every P1 tool is a read).
//!
//! ## tools/call (Blocker #1, #5; D4c)
//!
//! `arguments` is deserialized into the core struct (a shape pre-check; the
//! authoritative deserialize happens daemon-side), then relayed as the Lane 1
//! JSON-args frame. The daemon's two-layer envelope is classified:
//! - Socket-layer transport/parse failure → a JSON-RPC protocol error.
//! - A handler error riding under `status:"ok"` (the slim error envelope:
//!   `output = {error:{...}}` — error-present, NO `data` key, optional `_meta`,
//!   the shape `classify_slim_envelope` matches as `Error`)
//!   → `CallToolResult{isError:true}` with the redacted message — NOT a
//!   protocol error (Blocker #1).
//! - A success (`output = {data:..., (opt)_meta:...}`) → `CallToolResult` with
//!   `structuredContent` = `data`, a `content[text]` mirror, and the envelope
//!   `_meta` hoisted to `CallToolResult._meta`. Per-result signals
//!   (`rank_signals`/`trust_level`) live inside `data` and ride through
//!   `structuredContent` automatically — the envelope `_meta` carries only
//!   `stale_origins`/`worktree_overlay`/`worktree_stale` (Blocker #5).
//! - A success whose `data` is empty BUT carries `candidates` → `isError:true`
//!   so the model retries with a candidate; a genuinely-empty / no-candidate /
//!   `dead`-verdict result is empty-but-ok (D4c).

use std::path::Path;

use serde_json::Value;

use super::lifecycle;

/// The four MCP tool-annotation hints (MCP 2025-11-25). Advisory metadata for
/// clients that honor them — NOT a security boundary (the daemon's path/overlay
/// gates and the §2 opt-in flag are the real boundary). cqs overrides the
/// spec's destructive-by-default per command semantics rather than accepting it.
pub struct ToolAnnotations {
    /// The tool only reads state. Every Phase-1 tool is a pure read.
    pub read_only: bool,
    /// Re-running with the same args has the same effect as running once.
    pub idempotent: bool,
    /// The tool may destroy state (data loss). The lone destructive flag in the
    /// exposed Phase-2a set is `cqs_notes_remove`.
    pub destructive: bool,
    /// The tool reaches the open world (network). All cqs tools are local-only.
    pub open_world: bool,
}

impl ToolAnnotations {
    /// The read-quartet every Phase-1 tool carries: read-only, idempotent,
    /// non-destructive, local-only. Same values the bridge hardcoded in P1,
    /// now expressed per-row.
    const READ: ToolAnnotations = ToolAnnotations {
        read_only: true,
        idempotent: true,
        destructive: false,
        open_world: false,
    };

    /// Render to the `tools/list` annotations object.
    fn to_json(&self) -> Value {
        serde_json::json!({
            "readOnlyHint": self.read_only,
            "idempotentHint": self.idempotent,
            "destructiveHint": self.destructive,
            "openWorldHint": self.open_world,
        })
    }
}

/// A statically-declared MCP tool. `input_schema` is a fn pointer that
/// generates the JSON Schema on demand (schemars `schema_for!` is not `const`).
pub struct ToolDef {
    /// The `cqs_`-prefixed MCP tool name (D1).
    pub name: &'static str,
    /// The bare daemon command name this tool relays to (e.g. `test-map`).
    pub command: &'static str,
    /// One-line description surfaced in `tools/list`.
    pub description: &'static str,
    /// Generates the `inputSchema` (JSON Schema 2020-12) from the command's
    /// Phase-0 core struct.
    pub input_schema: fn() -> Value,
    /// Per-tool annotation hints (§3). Read tools carry [`ToolAnnotations::READ`];
    /// the Phase-2a mutators carry per-command hints.
    pub annotations: ToolAnnotations,
}

/// Render the schema for a core `T` as a `serde_json::Value`. schemars 1.x
/// `schema_for!` produces a 2020-12 schema; `serde(default)` cores yield no
/// `required` array (every param optional on the wire).
///
/// A core with zero schema-visible fields (an empty struct, or one whose only
/// field is `#[schemars(skip)]`) renders WITHOUT a `properties` key. Every tool
/// row is required to carry a `properties` object (the well-formedness +
/// overlay-honesty guards assert it), so a missing `properties` is normalized
/// to an empty `{}` here — the one chokepoint all core schemas flow through, so
/// any future zero-arg tool inherits the guarantee.
fn schema<T: schemars::JsonSchema>() -> Value {
    let mut s = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| {
        // schemars schema serialization is infallible in practice; fall back to
        // a minimal object so a hypothetical failure can't poison tools/list.
        serde_json::json!({"type": "object", "properties": {}})
    });
    if let Some(obj) = s.as_object_mut() {
        obj.entry("properties")
            .or_insert_with(|| Value::Object(Default::default()));
    }
    s
}

/// The worktree-overlay tri-state keys the daemon's `overlay_from_args` reads
/// off the raw wire object (NOT off the core struct), for an overlay-capable
/// command. Kept here as the single source the schema injection and the
/// schema-honesty test both reference, so adding/removing a key updates both.
const OVERLAY_KEYS: [&str; 3] = ["overlay", "no_overlay", "overlay_root"];

/// The advertised JSON-Schema property for one [`OVERLAY_KEYS`] entry. Keyed by
/// the const so the injected property set and the key list cannot drift.
fn overlay_property_schema(key: &str) -> Value {
    match key {
        "overlay" => serde_json::json!({
            "type": "boolean",
            "description": "Force the worktree overlay ON (reflect this checkout's edits)."
        }),
        "no_overlay" => serde_json::json!({
            "type": "boolean",
            "description": "Force the worktree overlay OFF (use the parent index as-is)."
        }),
        "overlay_root" => serde_json::json!({
            "type": "string",
            "description": "Overlay-root path override (routed through the daemon's \
                            overlay-root validation gate)."
        }),
        // Unreachable: `OVERLAY_KEYS` is the closed set this is called over.
        _ => serde_json::json!({ "description": "worktree-overlay control key" }),
    }
}

/// Like [`schema`], but for an overlay-capable command: inject the three
/// worktree-overlay tri-state properties ([`OVERLAY_KEYS`]) into the schema's
/// `properties` object so the advertised `inputSchema` declares every key the
/// daemon will read off the wire. Without this the daemon accepts `overlay`/
/// `no_overlay`/`overlay_root` (via `overlay_from_args`) while the schema hides
/// them — a schema-vs-wire drift. The keys are wire-optional (no `required`
/// entry), so a caller that omits them gets the default behavior.
fn schema_with_overlay<T: schemars::JsonSchema>() -> Value {
    let mut s = schema::<T>();
    if let Some(obj) = s.as_object_mut() {
        let props = obj
            .entry("properties")
            .or_insert_with(|| Value::Object(Default::default()));
        if let Some(props) = props.as_object_mut() {
            for key in OVERLAY_KEYS {
                props.insert(key.to_string(), overlay_property_schema(key));
            }
        }
    }
    s
}

// Core struct aliases — identical import paths to
// `cli::batch::json_args`, so the schema source and the daemon deserialize
// target are provably the same type.
use crate::cli::args::ReadArgs;
use crate::cli::args::StaleArgs as StaleCore;
use crate::cli::commands::blame::BlameArgs as BlameCore;
use crate::cli::commands::diff::DiffArgs as DiffCore;
use crate::cli::commands::drift::DriftArgs as DriftCore;
use crate::cli::commands::search::gather::GatherArgs as GatherCore;
use crate::cli::commands::search::onboard::OnboardArgs as OnboardCore;
use crate::cli::commands::search::query::QueryArgs;
use crate::cli::commands::search::related::RelatedArgs as RelatedCore;
use crate::cli::commands::search::scout::ScoutArgs as ScoutCore;
use crate::cli::commands::search::similar::SimilarArgs as SimilarCore;
use crate::cli::commands::search::where_cmd::WhereArgs as WhereCore;
use crate::cli::commands::{
    CalleesArgs as CalleesCore, CallersCoreArgs, CiArgs as CiCore, DeadArgs as DeadCore,
    DepsCoreArgs, HealthArgs as HealthCore, ImpactCoreArgs, PlanArgs as PlanCore,
    ReviewArgs as ReviewCore, StatsArgs as StatsCore, TestMapCoreArgs, TraceCoreArgs,
};

/// The composed tool table for `tools/list` — the Phase-1 read tools, plus the
/// Phase-2a notes mutators when `CQS_MCP_ENABLE_MUTATIONS=1`
/// ([`crate::cli::mcp::mutations_enabled`]). When the flag is unset the result
/// is byte-identical to Phase 1 (zero delta). The registry-parity guard in
/// `mod.rs` pins this set against `build_batch_cmd`'s arms.
pub fn tool_table() -> Vec<&'static ToolDef> {
    let mut tools: Vec<&'static ToolDef> = read_tools().iter().collect();
    if crate::cli::mcp::mutations_enabled() {
        tools.extend(mutation_tools().iter());
    }
    tools
}

/// The Phase-1 read tools — single source of truth for the unconditional part
/// of `tools/list`. Every row is a JSON-args-capable command with a Phase-0
/// core, EXCEPT the withheld `context`/`explain` (D4b). Each carries the
/// read-quartet annotation.
fn read_tools() -> &'static [ToolDef] {
    &[
        ToolDef {
            name: "cqs_search",
            command: "search",
            description:
                "Semantic code search (hybrid RRF). Find functions/methods by concept, not just \
                 name — e.g. 'retry with exponential backoff' finds retry logic regardless of \
                 naming. Use name_only for fast 'where is X defined?' lookups.",
            input_schema: schema_with_overlay::<QueryArgs>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_gather",
            command: "gather",
            description:
                "Assemble context: a seed semantic search expanded along the call graph (BFS). \
                 Returns the seed hits plus their callers/callees up to a depth.",
            input_schema: schema_with_overlay::<GatherCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_scout",
            command: "scout",
            description:
                "Investigation brief for a task: search + callers + tests + staleness + notes in \
                 one call. The first step before implementing.",
            input_schema: schema_with_overlay::<ScoutCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_onboard",
            command: "onboard",
            description:
                "Guided tour of a concept: entry point → call chain → types → tests. For exploring \
                 unfamiliar code.",
            input_schema: schema::<OnboardCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_similar",
            command: "similar",
            description:
                "Find functions structurally/semantically similar to a named function (nearest \
                 neighbors by embedding).",
            input_schema: schema::<SimilarCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_callers",
            command: "callers",
            description:
                "Who calls this function? Direct callers from the call graph, each tagged with an \
                 edge_kind (call / serde_callback / macro_heuristic / fn_pointer / doc_reference).",
            input_schema: schema_with_overlay::<CallersCoreArgs>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_callees",
            command: "callees",
            description: "What does this function call? Its direct callees from the call graph.",
            input_schema: schema_with_overlay::<CalleesCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_deps",
            command: "deps",
            description: "Type/symbol dependencies of a function (or, with reverse, its reverse \
                 dependencies).",
            input_schema: schema::<DepsCoreArgs>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_impact",
            command: "impact",
            description:
                "Blast radius of changing a function: callers, transitively-affected functions, \
                 and the tests that cover them. The pre-edit safety check.",
            input_schema: schema_with_overlay::<ImpactCoreArgs>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_test_map",
            command: "test-map",
            description: "Which tests exercise this function (directly or transitively)?",
            input_schema: schema::<TestMapCoreArgs>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_trace",
            command: "trace",
            description:
                "Shortest call path(s) between a source and a target function, if one exists.",
            input_schema: schema::<TraceCoreArgs>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_blame",
            command: "blame",
            description:
                "Semantic git blame for a function: who changed it, when, and why (commit \
                 messages), plus optional caller context.",
            input_schema: schema::<BlameCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_diff",
            command: "diff",
            description:
                "Semantic diff between two indexed references (or a reference and the project): \
                 added / removed / modified functions.",
            input_schema: schema::<DiffCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_drift",
            command: "drift",
            description:
                "Functions whose implementation has drifted from a reference index, ranked by \
                 semantic distance.",
            input_schema: schema::<DriftCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_dead",
            command: "dead",
            description: "Find dead code (zero callers), each carrying a verdict (test-only / \
                 low-confidence-live / known-gap / dead) — only `dead` is a confident absence \
                 claim.",
            input_schema: schema_with_overlay::<DeadCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_ci",
            command: "ci",
            description:
                "Bundled pre-merge review for the current diff: review + dead-code gate, with a \
                 pass/fail verdict.",
            input_schema: schema_with_overlay::<CiCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_review",
            command: "review",
            description:
                "Risk-ranked review of the current diff: which changed functions have the most \
                 callers / least test coverage.",
            input_schema: schema_with_overlay::<ReviewCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_plan",
            command: "plan",
            description:
                "Task-planning brief for a described change: scout + gather + impact + suggested \
                 file placement.",
            input_schema: schema::<PlanCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_read",
            command: "read",
            description:
                "Read a project file with cqs notes injected (or, with focus, just one function \
                 plus its type dependencies).",
            input_schema: schema::<ReadArgs>,
            annotations: ToolAnnotations::READ,
        },
        // Simple read tools (MCP Phase 2). Flat cores, no overlay tri-state.
        ToolDef {
            name: "cqs_where",
            command: "where",
            description:
                "Suggest where to add new code described in natural language: ranked file \
                 placements with insertion line, nearby function, and the local patterns \
                 (imports, error handling, naming, visibility) of each candidate.",
            input_schema: schema::<WhereCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_related",
            command: "related",
            description:
                "Functions related to a named one by co-occurrence: shared callers, shared \
                 callees, and shared custom types, each ranked by overlap count.",
            input_schema: schema::<RelatedCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_stale",
            command: "stale",
            description:
                "Index freshness: which indexed files have changed (stale) or been deleted \
                 (missing) since the last index, plus the total indexed count. Use count_only \
                 for just the counts.",
            input_schema: schema::<StaleCore>,
            annotations: ToolAnnotations::READ,
        },
        // Zero-arg read tools (MCP Phase 1). Both take no input — their cores
        // advertise an empty `properties` object — and both build their core via
        // `*Args::default()` on every surface, so there is no knob to expose.
        ToolDef {
            name: "cqs_stats",
            command: "stats",
            description:
                "Index statistics: chunk/file/note counts, call-graph and type-graph totals, \
                 per-language and per-type breakdowns, model, schema version, and freshness \
                 (stale/missing files). The one-call snapshot of what's indexed.",
            input_schema: schema::<StatsCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_health",
            command: "health",
            description:
                "Codebase quality snapshot: dead-code count, staleness, hotspots (most-called \
                 functions), and untested functions. The pre-work health check.",
            input_schema: schema::<HealthCore>,
            annotations: ToolAnnotations::READ,
        },
    ]
}

/// The gated mutation tools (§3) — the Phase-2a notes channel plus the Phase-2b
/// fire-and-forget `cqs_index`. Appended to `tool_table` ONLY when
/// `CQS_MCP_ENABLE_MUTATIONS=1`. Each carries per-command annotations:
/// `add` is additive (non-destructive, non-idempotent), `update` is idempotent
/// but mutating, `remove` is the lone `destructiveHint:true` in the set, and
/// `index` is a queued reindex (idempotent, non-destructive — rebuilds from the
/// source tree). The `command` column maps to the `json_args::build_batch_cmd`
/// arms (`notes-add`/`notes-update`/`notes-remove`/`index`) — also gated
/// daemon-side.
///
/// The DESTRUCTIVE set (`gc`/`slot remove`/`index --force`/`model swap`/
/// `cache clear`) is NOT here and has no flag that re-enables it: boundary by
/// absence, the same mechanism that withholds `context`. Note that `cqs_index`
/// IS exposed but `index --force` is NOT — the scoped `IndexArgs` core has no
/// `force` field, so the destructive full-rebuild variant is unreachable over
/// the wire.
fn mutation_tools() -> &'static [ToolDef] {
    use crate::cli::commands::index::IndexArgs as IndexCore;
    use crate::cli::commands::notes::{NotesAddArgs, NotesRemoveArgs, NotesUpdateArgs};
    &[
        ToolDef {
            name: "cqs_notes_add",
            command: "notes-add",
            description:
                "Add a project note (a sentiment-tagged observation that biases code-search \
                 ranking and is injected into `read`). Appends to docs/notes.toml; the watch \
                 loop reindexes. Additive — calling twice writes two notes.",
            input_schema: schema::<NotesAddArgs>,
            annotations: ToolAnnotations {
                read_only: false,
                idempotent: false,
                destructive: false,
                open_world: false,
            },
        },
        ToolDef {
            name: "cqs_notes_update",
            command: "notes-update",
            description:
                "Update an existing note matched by its exact text: replace its text, sentiment, \
                 mentions, or kind. Rewrites docs/notes.toml; the watch loop reindexes.",
            input_schema: schema::<NotesUpdateArgs>,
            annotations: ToolAnnotations {
                read_only: false,
                // Re-applying the same update converges to the same rewrite.
                idempotent: true,
                destructive: false,
                open_world: false,
            },
        },
        ToolDef {
            name: "cqs_notes_remove",
            command: "notes-remove",
            description:
                "Remove a note matched by its exact text from docs/notes.toml (no soft-delete); \
                 the watch loop reindexes. Removing an absent note is a no-op error.",
            input_schema: schema::<NotesRemoveArgs>,
            annotations: ToolAnnotations {
                read_only: false,
                idempotent: true,
                // The note text is gone — the lone destructive flag in the set.
                destructive: true,
                open_world: false,
            },
        },
        ToolDef {
            name: "cqs_index",
            command: "index",
            description:
                "Queue a reindex of the active slot (fire-and-forget): returns immediately with \
                 {queued:true, reindex_deferred:true}; the watch loop performs the rebuild on its \
                 next tick — it does NOT block on the build. To check freshness, re-run a read \
                 tool (e.g. cqs_search) and inspect its `_meta.stale_origins`: an empty list \
                 means the index has caught up. Non-destructive: rebuilds from the source tree \
                 (the full-rebuild `--force` variant is withheld).",
            input_schema: schema::<IndexCore>,
            annotations: ToolAnnotations {
                read_only: false,
                // A queued reindex converges to the same fresh index — repeated
                // calls coalesce into one watch-loop walk.
                idempotent: true,
                // Rebuilds from source-of-truth; no data loss.
                destructive: false,
                open_world: false,
            },
        },
    ]
}

/// Map an MCP tool name (`cqs_test_map`) back to the bare daemon command
/// (`test-map`). The mapping is the inverse of the table's `name`/`command`
/// columns; this helper exists for the registry-parity guard, which compares
/// command names. Falls back to a `cqs_`-stripped, underscore→hyphen form for
/// an unknown name so the guard fails loudly rather than silently matching.
#[cfg(test)]
pub fn mcp_name_to_command(mcp_name: &str) -> String {
    if let Some(def) = tool_table().into_iter().find(|t| t.name == mcp_name) {
        return def.command.to_string();
    }
    mcp_name
        .strip_prefix("cqs_")
        .unwrap_or(mcp_name)
        .replace('_', "-")
}

/// Look up a tool by its MCP name. Respects the mutation flag — a notes-mutation
/// tool is not findable (and so not callable) when the flag is off.
fn find_tool(name: &str) -> Option<&'static ToolDef> {
    tool_table().into_iter().find(|t| t.name == name)
}

/// Build the `tools/list` result: `{tools: [{name, description, inputSchema,
/// annotations}, ...]}`.
///
/// The rendered `tools` array is memoized per flag-state in a `LazyLock`
/// ([`READ_TOOLS_JSON`] / [`READ_PLUS_MUTATION_TOOLS_JSON`]) — `tools/list` is
/// hot, and re-running every tool's `schema_for!` per call (the only non-trivial
/// cost here) is pure waste because the schemas are static. Each snapshot is
/// post-overlay-injection (it renders the `ToolDef.input_schema` fn pointers,
/// which already produce the overlay-augmented schemas for overlay-capable
/// tools). The flag chooses the snapshot but never re-renders.
pub fn list() -> Value {
    let tools = if crate::cli::mcp::mutations_enabled() {
        &*READ_PLUS_MUTATION_TOOLS_JSON
    } else {
        &*READ_TOOLS_JSON
    };
    serde_json::json!({ "tools": tools })
}

/// Render a tool slice into the `tools/list` array shape. Called once per
/// snapshot inside the `LazyLock` initializers below.
fn render_tools(defs: &[ToolDef]) -> Vec<Value> {
    defs.iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": (t.input_schema)(),
                // Per-tool annotations (§3) — hints, not enforcement (the
                // daemon's path/overlay gates + the §2 opt-in flag are the real
                // boundary). Read tools carry the read-quartet; mutators differ.
                "annotations": t.annotations.to_json()
            })
        })
        .collect()
}

/// Memoized `tools/list` array for the flag-OFF (read-only) surface.
static READ_TOOLS_JSON: std::sync::LazyLock<Vec<Value>> =
    std::sync::LazyLock::new(|| render_tools(read_tools()));

/// Memoized `tools/list` array for the flag-ON surface (read tools + the gated
/// mutators), in `tool_table` order.
static READ_PLUS_MUTATION_TOOLS_JSON: std::sync::LazyLock<Vec<Value>> =
    std::sync::LazyLock::new(|| {
        let mut v = render_tools(read_tools());
        v.extend(render_tools(mutation_tools()));
        v
    });

/// The outcome of a `tools/call`: either a `CallToolResult` value (the JSON-RPC
/// `result`, which may itself carry `isError:true` for a handler error) or a
/// JSON-RPC protocol error `(code, message)`.
pub enum CallOutcome {
    Result(Value),
    ProtocolError(i32, String),
}

/// Handle a `tools/call` request. `cqs_dir` is the resolved `.cqs` directory
/// whose daemon socket the bridge relays to.
pub fn call(cqs_dir: &Path, params: Option<Value>) -> CallOutcome {
    // The tool name is recorded onto the span once resolved (declared Empty up
    // front so the field exists from span creation), and every exit path emits a
    // completion event so a call's outcome is observable from the trace alone.
    let span = tracing::info_span!("mcp_tools_call", tool = tracing::field::Empty);
    let _entered = span.enter();

    let params = match params {
        Some(p) => p,
        None => {
            return CallOutcome::ProtocolError(
                lifecycle::INVALID_PARAMS,
                "tools/call requires params".to_string(),
            )
        }
    };

    // 1. name → tool.
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return CallOutcome::ProtocolError(
                lifecycle::INVALID_PARAMS,
                "tools/call params missing string `name`".to_string(),
            )
        }
    };
    span.record("tool", name);
    let tool = match find_tool(name) {
        Some(t) => t,
        None => {
            tracing::warn!(tool = name, "MCP tools/call: unknown tool");
            return CallOutcome::ProtocolError(
                lifecycle::METHOD_NOT_FOUND,
                format!("unknown tool: {name}"),
            );
        }
    };

    // 2. arguments (default to an empty object; serde(default) cores accept it).
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(Value::Object(o)) => Value::Object(o.clone()),
        Some(_) => {
            return CallOutcome::ProtocolError(
                lifecycle::INVALID_PARAMS,
                "tools/call `arguments` must be an object".to_string(),
            )
        }
    };

    // 3. shape pre-check: deserialize into the core. This catches a bad-typed
    //    field as a JSON-RPC -32602 BEFORE a socket round-trip, with a clearer
    //    message than the daemon's redacted echo. The daemon re-deserializes
    //    authoritatively (it is the validation boundary, not this check).
    if let Err(e) = validate_arguments(tool.command, &arguments) {
        tracing::warn!(tool = name, error = %e, "MCP tools/call: argument pre-check rejected");
        return CallOutcome::ProtocolError(
            lifecycle::INVALID_PARAMS,
            format!("invalid arguments for {name}: {e}"),
        );
    }

    // 4. relay as the Lane 1 JSON-args frame, then classify the envelope. Emit
    //    one completion event carrying the tool name: info on a successful
    //    CallToolResult, warn on a handler error (isError:true) or a protocol
    //    error — so a call's outcome is visible without re-deriving it.
    let outcome = relay_and_classify(cqs_dir, tool.command, &arguments);
    match &outcome {
        CallOutcome::Result(result) => {
            let is_error = result.get("isError").and_then(|v| v.as_bool()) == Some(true);
            if is_error {
                tracing::warn!(
                    tool = name,
                    "MCP tools/call complete: handler error (isError)"
                );
            } else {
                tracing::info!(tool = name, "MCP tools/call complete: ok");
            }
        }
        CallOutcome::ProtocolError(code, _) => {
            tracing::warn!(tool = name, code, "MCP tools/call complete: protocol error");
        }
    }
    outcome
}

/// The `max_depth` range the daemon enforces for `test-map` / `trace`: their
/// argv `--depth` is a clap-bounded `u16` (1..=50), and the JSON-args adapter
/// (`test_map_args_from_core` / `trace_args_from_core`) re-applies the SAME
/// `u16::try_from(..).filter(|d| (1..=50).contains(d))` gate before dispatch. A
/// precheck that only deserialized the `usize` core would pass a value the
/// daemon then range-rejects — so the precheck must apply the same bound to
/// reject it pre-relay (with a clearer message than the daemon's redacted echo).
fn validate_max_depth(arguments: &Value) -> Result<(), String> {
    if let Some(v) = arguments.get("max_depth") {
        // Only validate a value that is actually present and integral. A
        // non-integer or out-of-range value is caught by the shape check first
        // for the type, but the range is the daemon's, so re-check it here.
        let n = v
            .as_u64()
            .ok_or_else(|| "max_depth must be a non-negative integer".to_string())?;
        let in_range = u16::try_from(n).is_ok_and(|d| (1..=50).contains(&d));
        if !in_range {
            return Err(format!("max_depth must be in 1..=50, got {n}"));
        }
    }
    Ok(())
}

/// Deserialize-check `arguments` against the command's Phase-0 core, then apply
/// the daemon's non-serde range gates (e.g. the `test-map` / `trace`
/// `max_depth` 1..=50 bound enforced in the JSON-args adapter). Returns `Ok(())`
/// when the object is both shape-valid AND within the bounds the daemon will
/// enforce — so a value the daemon would range-reject is rejected pre-relay. The
/// daemon re-deserializes and re-validates authoritatively (it is the boundary,
/// not this check); this pre-check exists to surface a clearer message earlier.
fn validate_arguments(command: &str, arguments: &Value) -> Result<(), String> {
    fn check<T: serde::de::DeserializeOwned>(arguments: &Value) -> Result<(), String> {
        serde_json::from_value::<T>(arguments.clone())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    match command {
        "search" => check::<QueryArgs>(arguments),
        "gather" => check::<GatherCore>(arguments),
        "scout" => check::<ScoutCore>(arguments),
        "onboard" => check::<OnboardCore>(arguments),
        "similar" => check::<SimilarCore>(arguments),
        "callers" => check::<CallersCoreArgs>(arguments),
        "callees" => check::<CalleesCore>(arguments),
        "deps" => check::<DepsCoreArgs>(arguments),
        "impact" => check::<ImpactCoreArgs>(arguments),
        // `test-map` / `trace` carry a `max_depth` the daemon range-gates (1..=50)
        // in the JSON-args adapter — re-apply that bound after the shape check.
        "test-map" => {
            check::<TestMapCoreArgs>(arguments).and_then(|()| validate_max_depth(arguments))
        }
        "trace" => check::<TraceCoreArgs>(arguments).and_then(|()| validate_max_depth(arguments)),
        "blame" => check::<BlameCore>(arguments),
        "diff" => check::<DiffCore>(arguments),
        "drift" => check::<DriftCore>(arguments),
        "dead" => check::<DeadCore>(arguments),
        "ci" => check::<CiCore>(arguments),
        "review" => check::<ReviewCore>(arguments),
        "plan" => check::<PlanCore>(arguments),
        "read" => check::<ReadArgs>(arguments),
        // Phase-2 simple read tools — flat cores, shape check only.
        "where" => check::<WhereCore>(arguments),
        "related" => check::<RelatedCore>(arguments),
        "stale" => check::<StaleCore>(arguments),
        // Zero-arg read tools (Phase 1). The core has no advertised properties,
        // so the shape check just rejects a non-object / wrong-typed `arguments`.
        "stats" => check::<StatsCore>(arguments),
        "health" => check::<HealthCore>(arguments),
        // Phase-2a gated notes mutators. Only reachable when the mutation tools
        // are in the table (flag on), since `find_tool` gates on the same flag —
        // but the pre-check arm is unconditional so a flag-on call shape-checks.
        "notes-add" => check::<crate::cli::commands::notes::NotesAddArgs>(arguments),
        "notes-update" => check::<crate::cli::commands::notes::NotesUpdateArgs>(arguments),
        "notes-remove" => check::<crate::cli::commands::notes::NotesRemoveArgs>(arguments),
        // Phase-2b gated fire-and-forget reindex. The scoped core (no `force`)
        // is the shape pre-check; the daemon re-deserializes authoritatively.
        "index" => check::<crate::cli::commands::index::IndexArgs>(arguments),
        // Unreachable: every table command is covered above. A new tool with no
        // arm fails the pre-check loudly rather than silently skipping it.
        other => Err(format!("no argument schema for command {other}")),
    }
}

/// Relay the JSON-args frame to the daemon and map the response envelope into a
/// `CallToolResult` (Blocker #1, #5; D4c).
#[cfg(unix)]
fn relay_and_classify(cqs_dir: &Path, command: &str, arguments: &Value) -> CallOutcome {
    use cqs::daemon_translate::{daemon_json_args_request, DaemonRpcError};

    let envelope = match daemon_json_args_request(cqs_dir, command, arguments) {
        Ok(env) => env,
        Err(DaemonRpcError::SocketMissing(msg)) => {
            // D4a: bridge-only, no in-process fallback. A missing daemon is a
            // transport failure → JSON-RPC internal error with operator advice.
            return CallOutcome::ProtocolError(
                lifecycle::INTERNAL_ERROR,
                format!("cqs daemon not running: {msg}. Start `cqs watch --serve`."),
            );
        }
        Err(DaemonRpcError::Transport(msg)) => {
            return CallOutcome::ProtocolError(
                lifecycle::INTERNAL_ERROR,
                format!("cqs daemon transport failure: {msg}"),
            );
        }
        Err(DaemonRpcError::DaemonError(msg)) => {
            // Socket-layer `status:"error"` — a bad relay / NUL / missing
            // command. Our request was malformed at the transport layer.
            return CallOutcome::ProtocolError(
                lifecycle::INVALID_PARAMS,
                format!("cqs daemon rejected the request: {msg}"),
            );
        }
        Err(DaemonRpcError::BadResponse(msg)) => {
            return CallOutcome::ProtocolError(
                lifecycle::INTERNAL_ERROR,
                format!("cqs daemon returned a malformed response: {msg}"),
            );
        }
        Err(DaemonRpcError::ResponseTooLarge(msg)) => {
            // The result was valid but exceeded the relay read cap. Surface the
            // limit verbatim so the agent can raise the cap or narrow the query
            // instead of reading it as a malformed-response failure.
            return CallOutcome::ProtocolError(
                lifecycle::INTERNAL_ERROR,
                format!("cqs daemon {msg}"),
            );
        }
    };

    // The success envelope is `{"status":"ok","output":<dispatch>}`. Peel the
    // socket layer to reach the dispatch slim envelope.
    let output = match envelope.get("output") {
        Some(o) => o,
        None => {
            return CallOutcome::ProtocolError(
                lifecycle::INTERNAL_ERROR,
                "daemon response missing `output`".to_string(),
            )
        }
    };

    CallOutcome::Result(classify_output(output))
}

/// Non-unix stub: the daemon socket is unix-only, so the bridge has no
/// transport. On a non-unix target `serve_stdio` fails fast before the stdin
/// loop, so this path is not reached in practice; it exists to keep the module
/// compiling on non-unix targets.
#[cfg(not(unix))]
fn relay_and_classify(_cqs_dir: &Path, _command: &str, _arguments: &Value) -> CallOutcome {
    CallOutcome::ProtocolError(
        lifecycle::INTERNAL_ERROR,
        "the cqs MCP bridge requires a unix daemon socket".to_string(),
    )
}

/// Classify the dispatch-layer `output` into a `CallToolResult` (Blocker #1,
/// #5; D4c). `output` is the slim envelope: `{data, (opt)_meta}` on success or
/// `{error:{code,message}, (opt)_meta}` on a handler error riding under
/// `status:"ok"`.
fn classify_output(output: &Value) -> Value {
    use cqs::daemon_translate::{classify_slim_envelope, SlimEnvelope};

    match classify_slim_envelope(output) {
        // Handler error under status:"ok" (Blocker #1) → isError:true.
        Some(SlimEnvelope::Error { code, message }) => {
            tracing::warn!(code = %code, "MCP tool handler error mapped to isError:true");
            serde_json::json!({
                "content": [{ "type": "text", "text": message }],
                "isError": true,
                "_meta": { "error": { "code": code } }
            })
        }
        // Success → structuredContent + content[text] mirror + _meta hoist.
        Some(SlimEnvelope::Data { payload, meta }) => {
            success_result(payload.clone(), meta.cloned())
        }
        // Not a recognized slim envelope. If the unrecognized shape carries an
        // `error` key it is almost certainly a daemon failure the slim matcher
        // didn't recognize (e.g. an extra sibling key, or both data+error) —
        // surface it as isError:true rather than masking a failure as success.
        // A genuinely error-free non-standard shape passes through as success.
        None if output.get("error").is_some() => {
            tracing::warn!(
                "MCP tool unrecognized envelope carrying an `error` key mapped to isError:true"
            );
            let mut result = success_result(output.clone(), None);
            if let Some(obj) = result.as_object_mut() {
                obj.insert("isError".to_string(), Value::Bool(true));
            }
            result
        }
        None => success_result(output.clone(), None),
    }
}

/// Max byte length of the inlined `content[text]` mirror. Above this the text is
/// a short summary; the full payload always remains in `structuredContent`. The
/// mirror exists only for clients that don't read `structuredContent`, so it is
/// not worth doubling a large payload's bytes on the per-call hot path.
const MAX_TEXT_MIRROR_BYTES: usize = 4 * 1024;

/// JSON-serialize `value` for the `content[text]` mirror, size-gated: the full
/// text when it fits under [`MAX_TEXT_MIRROR_BYTES`], a short summary otherwise
/// (the full data is always in `structuredContent`, so the mirror can be terse).
fn text_mirror(value: &Value) -> String {
    let full = serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string());
    if full.len() <= MAX_TEXT_MIRROR_BYTES {
        full
    } else {
        format!(
            "[{} bytes elided — read structuredContent for the full payload]",
            full.len()
        )
    }
}

/// Build a successful `CallToolResult` from the dispatch `data` payload (moved
/// into `structuredContent`) and the optional envelope `_meta` (Blocker #5,
/// D4c). The `content[text]` mirror is derived from the same value and
/// size-gated ([`text_mirror`]) so a large payload is not embedded twice on the
/// hot path.
fn success_result(payload: Value, meta: Option<Value>) -> Value {
    // D4c: empty-with-candidates → isError:true so the model retries with a
    // candidate. Genuinely-empty / no-candidate / dead-verdict → empty-but-ok.
    let is_candidates = is_empty_with_candidates(&payload);
    // Build the text mirror FROM the value before moving it into the result.
    let text = if is_candidates {
        format!(
            "no exact match — retry with one of the candidates: {}",
            text_mirror(&payload)
        )
    } else {
        text_mirror(&payload)
    };

    let mut obj = serde_json::Map::new();
    obj.insert(
        "content".to_string(),
        serde_json::json!([{ "type": "text", "text": text }]),
    );
    // MOVE the payload into structuredContent — no clone. Per-result signals
    // (rank_signals/trust_level) live inside it, so they ride through
    // automatically (Blocker #5); only the envelope-level _meta is hoisted.
    obj.insert("structuredContent".to_string(), payload);
    obj.insert("isError".to_string(), Value::Bool(is_candidates));
    if let Some(m) = meta {
        if !m.is_null() {
            // Hoist the envelope _meta (stale_origins / worktree_overlay /
            // worktree_stale). See Blocker #5.
            obj.insert("_meta".to_string(), m);
        }
    }
    Value::Object(obj)
}

/// D4c shape probe: a result that is "empty BUT carries candidates" — an
/// object whose primary collection is empty while a `candidates` array is
/// non-empty (the not-found-with-suggestions shape callers/impact/etc. emit so
/// the model can retry). A genuinely-empty result (no candidates, or a `dead`
/// verdict) is NOT flagged.
fn is_empty_with_candidates(payload: &Value) -> bool {
    let obj = match payload.as_object() {
        Some(o) => o,
        None => return false,
    };
    // Candidates must be present and non-empty.
    let has_candidates = obj
        .get("candidates")
        .and_then(|c| c.as_array())
        .is_some_and(|a| !a.is_empty());
    if !has_candidates {
        return false;
    }
    // And the result's primary payload must be empty: every other array/object
    // field is empty (so this is a "no hit, here are near-misses" answer, not a
    // hit that happens to also list candidates).
    let primary_empty = obj.iter().all(|(k, v)| {
        if k == "candidates" || k == "_meta" {
            return true;
        }
        match v {
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.is_empty(),
            Value::Null => true,
            // Scalars (counts, names, flags) don't count as "primary content".
            _ => true,
        }
    });
    primary_empty
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the mutation flag OFF (Phase-1 surface), every exposed tool is a
    /// read — `readOnlyHint:true`. Serial-gated in the shared `mcp_mutations_env`
    /// group because `list()` reads the process-global flag; an ungated parallel
    /// run can observe a flag a serial test set, flipping a mutator into view.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn list_emits_well_formed_tools() {
        // SAFETY: serial group + restore via the prior value below.
        let prior = std::env::var(crate::cli::mcp::MUTATIONS_ENV).ok();
        std::env::remove_var(crate::cli::mcp::MUTATIONS_ENV);

        let v = list();
        let tools = v
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("tools array");
        assert!(!tools.is_empty());
        for t in tools {
            assert!(t
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap()
                .starts_with("cqs_"));
            assert!(t.get("inputSchema").is_some());
            assert_eq!(
                t.get("annotations")
                    .and_then(|a| a.get("readOnlyHint"))
                    .and_then(|v| v.as_bool()),
                Some(true)
            );
        }

        match prior {
            Some(p) => std::env::set_var(crate::cli::mcp::MUTATIONS_ENV, p),
            None => std::env::remove_var(crate::cli::mcp::MUTATIONS_ENV),
        }
    }

    /// Blocker #1: a handler error riding under status:"ok" (slim envelope with
    /// an `error` key and no `data`) maps to isError:true, NOT a false success.
    #[test]
    fn handler_error_maps_to_is_error() {
        let output = serde_json::json!({
            "error": { "code": "not_found", "message": "function 'nope' not found" }
        });
        let result = classify_output(&output);
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            result
                .get("_meta")
                .and_then(|m| m.get("error"))
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_str()),
            Some("not_found")
        );
        // The redacted message reaches the client via content[text].
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .unwrap();
        assert!(text.contains("not found"));
    }

    /// A success envelope maps to isError:false with structuredContent == data.
    #[test]
    fn data_maps_to_structured_content() {
        let output = serde_json::json!({
            "data": { "callers": [{ "name": "foo" }] },
            "_meta": { "stale_origins": ["src/a.rs"] }
        });
        let result = classify_output(&output);
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            result.get("structuredContent"),
            Some(&serde_json::json!({ "callers": [{ "name": "foo" }] }))
        );
        // Blocker #5: the envelope _meta is hoisted.
        assert_eq!(
            result.get("_meta").and_then(|m| m.get("stale_origins")),
            Some(&serde_json::json!(["src/a.rs"]))
        );
    }

    /// D4c: empty-with-candidates → isError:true (model should retry).
    #[test]
    fn empty_with_candidates_is_error() {
        let output = serde_json::json!({
            "data": { "callers": [], "candidates": ["foo_bar", "foo_baz"] }
        });
        let result = classify_output(&output);
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(true));
        // structuredContent still carries the candidates for the retry.
        assert!(result.get("structuredContent").is_some());
    }

    /// D4c: a genuinely-empty result (no candidates) is empty-but-ok.
    #[test]
    fn genuinely_empty_is_ok() {
        let output = serde_json::json!({ "data": { "callers": [] } });
        let result = classify_output(&output);
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(false));
    }

    /// A dead-verdict result with results present is NOT flagged as
    /// empty-with-candidates even if it lists no candidates.
    #[test]
    fn dead_verdict_not_flagged() {
        let output = serde_json::json!({
            "data": { "dead_functions": [{ "name": "x", "verdict": "dead" }] }
        });
        let result = classify_output(&output);
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(false));
    }

    /// Masking guard (robustness): an UNRECOGNIZED (non-slim) envelope that
    /// nonetheless carries an `error` key must map to isError:true, not be masked
    /// as a false success. The slim matcher returns `None` here (an extra sibling
    /// key beyond data/error/_meta makes it non-slim), so the error-key check in
    /// `classify_output` is what catches the daemon failure.
    #[test]
    fn unrecognized_envelope_with_error_key_is_error() {
        // Extra sibling key `status` makes this non-slim, but it still carries an
        // `error` — a daemon failure that must NOT read as success.
        let output = serde_json::json!({
            "error": { "code": "boom", "message": "something failed" },
            "status": "weird"
        });
        let result = classify_output(&output);
        assert_eq!(
            result.get("isError").and_then(|v| v.as_bool()),
            Some(true),
            "an unrecognized envelope carrying `error` must be isError:true: {result}"
        );
        // The full unrecognized output still reaches the client via
        // structuredContent (nothing dropped).
        assert!(result.get("structuredContent").is_some());
    }

    /// Contrast: an unrecognized (non-slim) envelope with NO `error` key passes
    /// through as a success — a non-standard but error-free handler shape still
    /// reaches the client.
    #[test]
    fn unrecognized_envelope_without_error_is_ok() {
        let output = serde_json::json!({ "weird": { "shape": true }, "extra": 1 });
        let result = classify_output(&output);
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(false));
        assert!(result.get("structuredContent").is_some());
    }

    /// The large-payload text mirror is size-gated: a payload over the cap keeps
    /// the full data in structuredContent but elides the inlined text mirror.
    #[test]
    fn large_payload_text_mirror_is_size_gated() {
        let big: String = "x".repeat(MAX_TEXT_MIRROR_BYTES + 100);
        let output = serde_json::json!({ "data": { "blob": big } });
        let result = classify_output(&output);
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(false));
        // structuredContent carries the full payload.
        let structured = result.get("structuredContent").expect("structuredContent");
        assert!(structured.get("blob").is_some());
        // The text mirror is the elision summary, not the full blob.
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .unwrap();
        assert!(
            text.contains("elided") && text.len() < MAX_TEXT_MIRROR_BYTES,
            "oversized payload must produce an elided text mirror, got {} bytes",
            text.len()
        );
    }

    #[test]
    fn mcp_name_round_trips_to_command() {
        assert_eq!(mcp_name_to_command("cqs_test_map"), "test-map");
        assert_eq!(mcp_name_to_command("cqs_search"), "search");
        assert_eq!(mcp_name_to_command("cqs_callers"), "callers");
    }

    /// Collect `cqs_<ident>` tokens from `text`, in order.
    fn cqs_tokens(text: &str) -> Vec<String> {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(rel) = text[i..].find("cqs_") {
            let start = i + rel;
            let mut end = start + "cqs_".len();
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            out.push(text[start..end].to_string());
            i = end;
        }
        out
    }

    /// Description honesty (docs-lie): every `cqs_*` tool token mentioned in ANY
    /// tool description must name a tool that actually exists in `tool_table()` —
    /// a description must not steer the agent at a tool that resolves to
    /// METHOD_NOT_FOUND. Runs with the mutation flag ON so the mutator-tool
    /// descriptions (and the full name set) are both in scope. Serial-gated on
    /// the shared env group.
    #[test]
    #[serial_test::serial(mcp_mutations_env)]
    fn descriptions_reference_only_real_tools() {
        let prior = std::env::var(crate::cli::mcp::MUTATIONS_ENV).ok();
        std::env::set_var(crate::cli::mcp::MUTATIONS_ENV, "1");

        let table = tool_table();
        let known: std::collections::BTreeSet<&str> = table.iter().map(|t| t.name).collect();
        for t in &table {
            for token in cqs_tokens(t.description) {
                assert!(
                    known.contains(token.as_str()),
                    "tool `{}` description references `{token}`, which is not a registered tool \
                     (would resolve to METHOD_NOT_FOUND); known tools: {:?}",
                    t.name,
                    known
                );
            }
        }

        match prior {
            Some(p) => std::env::set_var(crate::cli::mcp::MUTATIONS_ENV, p),
            None => std::env::remove_var(crate::cli::mcp::MUTATIONS_ENV),
        }
    }

    /// The MCP tool names whose daemon command consumes the worktree-overlay
    /// tri-state via `overlay_from_args` (search/gather/scout/callers/callees/
    /// impact/dead/ci/review). The list mirrors the `overlay_from_args(arguments)`
    /// call sites in `json_args::build_batch_cmd`.
    const OVERLAY_CAPABLE_TOOLS: [&str; 9] = [
        "cqs_search",
        "cqs_gather",
        "cqs_scout",
        "cqs_callers",
        "cqs_callees",
        "cqs_impact",
        "cqs_dead",
        "cqs_ci",
        "cqs_review",
    ];

    /// Schema honesty (schema≠wire): every overlay key the daemon's
    /// `overlay_from_args` reads off the wire must be a declared property of the
    /// overlay-capable tool's advertised `inputSchema` — otherwise the daemon
    /// accepts a knob the schema hides. Conversely, a NON-overlay tool must NOT
    /// advertise the overlay keys (the daemon ignores them for it, so offering
    /// them would mislead). Pins the schema injection to the daemon's consumption.
    #[test]
    fn overlay_keys_declared_iff_command_consumes_them() {
        let overlay: std::collections::BTreeSet<&str> = OVERLAY_CAPABLE_TOOLS.into_iter().collect();
        for def in read_tools() {
            let schema = (def.input_schema)();
            let props = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .unwrap_or_else(|| panic!("`{}` schema must carry properties", def.name));
            if overlay.contains(def.name) {
                for key in OVERLAY_KEYS {
                    assert!(
                        props.contains_key(key),
                        "overlay-capable `{}` inputSchema must declare overlay key `{key}`; \
                         got properties: {:?}",
                        def.name,
                        props.keys().collect::<Vec<_>>()
                    );
                }
            } else {
                for key in OVERLAY_KEYS {
                    assert!(
                        !props.contains_key(key),
                        "non-overlay `{}` inputSchema must NOT declare overlay key `{key}` \
                         (the daemon ignores it for this command)",
                        def.name
                    );
                }
            }
        }
    }

    /// Schema honesty: the `cqs_trace` / `cqs_test_map` `inputSchema` must NOT
    /// advertise `max_nodes`. The field is env-resolved in the handler and does
    /// NOT round-trip on the JSON-args adapter, so advertising it would offer a
    /// knob the daemon silently ignores. `#[schemars(skip)]` drops it from the
    /// schema while serde still tolerates it on the wire (inert).
    #[test]
    fn trace_and_test_map_schema_omits_max_nodes() {
        for tool in ["cqs_trace", "cqs_test_map"] {
            let def = find_tool(tool).unwrap_or_else(|| panic!("`{tool}` must be in the table"));
            let schema = (def.input_schema)();
            let props = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .unwrap_or_else(|| panic!("`{tool}` schema must carry properties"));
            assert!(
                !props.contains_key("max_nodes"),
                "`{tool}` inputSchema must NOT advertise the inert `max_nodes` field; \
                 got properties: {:?}",
                props.keys().collect::<Vec<_>>()
            );
            // A real, advertised field is still present — proves we didn't
            // accidentally empty the schema.
            assert!(
                props.contains_key("max_depth"),
                "`{tool}` inputSchema must still advertise `max_depth`"
            );
        }
    }

    /// An unknown tool name is a -32601 protocol error, not a panic.
    #[test]
    fn unknown_tool_is_protocol_error() {
        let outcome = call(
            Path::new("/nonexistent/.cqs"),
            Some(serde_json::json!({ "name": "cqs_bogus", "arguments": {} })),
        );
        match outcome {
            CallOutcome::ProtocolError(code, _) => assert_eq!(code, lifecycle::METHOD_NOT_FOUND),
            CallOutcome::Result(_) => panic!("expected protocol error for unknown tool"),
        }
    }

    /// Malformed arguments (wrong-typed field) are a -32602 protocol error
    /// caught by the pre-check, before any socket round-trip.
    #[test]
    fn malformed_arguments_is_invalid_params() {
        let outcome = call(
            Path::new("/nonexistent/.cqs"),
            // callers.name is a String; a number fails the pre-check.
            Some(serde_json::json!({ "name": "cqs_callers", "arguments": { "name": 42 } })),
        );
        match outcome {
            CallOutcome::ProtocolError(code, _) => assert_eq!(code, lifecycle::INVALID_PARAMS),
            CallOutcome::Result(_) => panic!("expected invalid-params for malformed arguments"),
        }
    }

    /// Precheck-vs-daemon parity: a `max_depth` the daemon range-rejects
    /// (1..=50) is rejected by the pre-check too, before any socket round-trip —
    /// the pre-check accepts exactly what the daemon will. An in-range value
    /// passes the pre-check (and only then relays).
    #[test]
    fn out_of_range_max_depth_is_invalid_params() {
        for tool in ["cqs_trace", "cqs_test_map"] {
            // 999 is shape-valid (a usize) but outside the daemon's 1..=50 gate.
            let args = if tool == "cqs_trace" {
                serde_json::json!({ "source": "a", "target": "b", "max_depth": 999 })
            } else {
                serde_json::json!({ "name": "a", "max_depth": 999 })
            };
            let outcome = call(
                Path::new("/nonexistent/.cqs"),
                Some(serde_json::json!({ "name": tool, "arguments": args })),
            );
            match outcome {
                CallOutcome::ProtocolError(code, msg) => {
                    assert_eq!(
                        code,
                        lifecycle::INVALID_PARAMS,
                        "out-of-range max_depth must be -32602 for {tool}: {msg}"
                    );
                    assert!(
                        msg.contains("1..=50"),
                        "the rejection must name the daemon's range for {tool}: {msg}"
                    );
                }
                CallOutcome::Result(_) => {
                    panic!("expected invalid-params for out-of-range max_depth on {tool}")
                }
            }
        }
        // A zero is also out of range (the lower bound is 1).
        assert!(validate_max_depth(&serde_json::json!({ "max_depth": 0 })).is_err());
        // An in-range value passes the pre-check's range gate.
        assert!(validate_max_depth(&serde_json::json!({ "max_depth": 5 })).is_ok());
        // Absent max_depth is fine (the daemon defaults it).
        assert!(validate_max_depth(&serde_json::json!({ "name": "a" })).is_ok());
    }
}
