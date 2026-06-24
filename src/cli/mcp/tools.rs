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
//!   relayed `arguments` into, so the schema and the wire contract cannot drift.
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
fn schema<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| {
        // schemars schema serialization is infallible in practice; fall back to
        // a minimal object so a hypothetical failure can't poison tools/list.
        serde_json::json!({"type": "object", "properties": {}})
    })
}

// Core struct aliases — identical import paths to
// `cli::batch::json_args`, so the schema source and the daemon deserialize
// target are provably the same type.
use crate::cli::args::ReadArgs;
use crate::cli::commands::blame::BlameArgs as BlameCore;
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
            input_schema: schema::<QueryArgs>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_gather",
            command: "gather",
            description:
                "Assemble context: a seed semantic search expanded along the call graph (BFS). \
                 Returns the seed hits plus their callers/callees up to a depth.",
            input_schema: schema::<GatherCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_scout",
            command: "scout",
            description:
                "Investigation brief for a task: search + callers + tests + staleness + notes in \
                 one call. The first step before implementing.",
            input_schema: schema::<ScoutCore>,
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
            input_schema: schema::<CallersCoreArgs>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_callees",
            command: "callees",
            description: "What does this function call? Its direct callees from the call graph.",
            input_schema: schema::<CalleesCore>,
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
            input_schema: schema::<ImpactCoreArgs>,
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
            input_schema: schema::<DeadCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_ci",
            command: "ci",
            description:
                "Bundled pre-merge review for the current diff: review + dead-code gate, with a \
                 pass/fail verdict.",
            input_schema: schema::<CiCore>,
            annotations: ToolAnnotations::READ,
        },
        ToolDef {
            name: "cqs_review",
            command: "review",
            description:
                "Risk-ranked review of the current diff: which changed functions have the most \
                 callers / least test coverage.",
            input_schema: schema::<ReviewCore>,
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
                 next tick — it does NOT block on the build. Poll completion with cqs_wait_fresh \
                 (or cqs_status). Non-destructive: rebuilds from the source tree (the full-rebuild \
                 `--force` variant is withheld).",
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
pub fn list() -> Value {
    let tools: Vec<Value> = tool_table()
        .iter()
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
        .collect();
    serde_json::json!({ "tools": tools })
}

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
    let _span = tracing::info_span!("mcp_tools_call").entered();

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
    let tool = match find_tool(name) {
        Some(t) => t,
        None => {
            return CallOutcome::ProtocolError(
                lifecycle::METHOD_NOT_FOUND,
                format!("unknown tool: {name}"),
            )
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
        return CallOutcome::ProtocolError(
            lifecycle::INVALID_PARAMS,
            format!("invalid arguments for {name}: {e}"),
        );
    }

    // 4. relay as the Lane 1 JSON-args frame, then classify the envelope.
    relay_and_classify(cqs_dir, tool.command, &arguments)
}

/// Deserialize-check `arguments` against the command's Phase-0 core. Returns
/// `Ok(())` if the object is shape-valid. Mirrors the type set in
/// `build_batch_cmd` so the pre-check accepts exactly what the daemon will.
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
        "test-map" => check::<TestMapCoreArgs>(arguments),
        "trace" => check::<TraceCoreArgs>(arguments),
        "blame" => check::<BlameCore>(arguments),
        "diff" => check::<DiffCore>(arguments),
        "drift" => check::<DriftCore>(arguments),
        "dead" => check::<DeadCore>(arguments),
        "ci" => check::<CiCore>(arguments),
        "review" => check::<ReviewCore>(arguments),
        "plan" => check::<PlanCore>(arguments),
        "read" => check::<ReadArgs>(arguments),
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
/// transport. (The `cqs mcp` subcommand is itself unix-gated at the CLI layer.)
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
        Some(SlimEnvelope::Data { payload, meta }) => success_result(payload, meta),
        // Not a recognized slim envelope — pass the raw output through as
        // structuredContent so a non-standard handler shape still reaches the
        // client rather than being dropped.
        None => success_result(output, None),
    }
}

/// Build a successful `CallToolResult` from the dispatch `data` payload and the
/// optional envelope `_meta` (Blocker #5, D4c).
fn success_result(payload: &Value, meta: Option<&Value>) -> Value {
    // D4c: empty-with-candidates → isError:true so the model retries with a
    // candidate. Genuinely-empty / no-candidate / dead-verdict → empty-but-ok.
    if is_empty_with_candidates(payload) {
        let text = serde_json::to_string(payload)
            .unwrap_or_else(|_| "no exact match; candidates available".to_string());
        return serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("no exact match — retry with one of the candidates: {text}")
            }],
            "structuredContent": payload,
            "isError": true
        });
    }

    // The text mirror is the JSON-stringified data, for clients that don't read
    // structuredContent. Per-result signals (rank_signals/trust_level) live
    // inside `payload`, so structuredContent carries them automatically
    // (Blocker #5) — only the envelope-level _meta is hoisted separately.
    let text = serde_json::to_string(payload).unwrap_or_else(|_| "<unserializable>".to_string());
    let mut result = serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": payload,
        "isError": false
    });
    if let Some(m) = meta {
        if !m.is_null() {
            // Hoist the envelope _meta (stale_origins / worktree_overlay /
            // worktree_stale) — NOT the per-result signals (those ride in
            // structuredContent). See Blocker #5.
            if let Some(obj) = result.as_object_mut() {
                obj.insert("_meta".to_string(), m.clone());
            }
        }
    }
    result
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

    #[test]
    fn mcp_name_round_trips_to_command() {
        assert_eq!(mcp_name_to_command("cqs_test_map"), "test-map");
        assert_eq!(mcp_name_to_command("cqs_search"), "search");
        assert_eq!(mcp_name_to_command("cqs_callers"), "callers");
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
}
