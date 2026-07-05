//! Transport-side MCP Tasks surface (Phase 3b floor, lane 2).
//!
//! Shared by BOTH transports (`bridge`/stdio and `http`): task creation and the
//! `tasks/get` read are quick one-shot daemon dials that route through
//! [`super::bridge::route_with`] on either surface; only the `tasks/result`
//! block-until-terminal differs (stdio loops inline here — a single sequential
//! client — while HTTP owns an isolated blocking budget in `http.rs` so the
//! block never holds one of the outermost one-shot request permits, the block-budget isolation).
//!
//! ## Wire shapes (the spec's deliberate asymmetry — D6)
//!
//! - **`CreateTaskResult` NESTS the task**: `{ task: Task, _meta: {related-task} }`
//!   — the augmented `tools/call` reply.
//! - **`GetTaskResult` is FLAT**: `Task` fields at the top level of `result`,
//!   plus `_meta: {related-task}`. Do NOT build one from the other by wrapping.
//! - **`tasks/result` returns the underlying result VERBATIM**: a `CallToolResult`
//!   on success (even `isError:true` — a *successful* JSON-RPC response carrying a
//!   tool-level error, which maps the task's status to `failed`), or the JSON-RPC
//!   ERROR itself when the call failed at the JSON-RPC level — never wrapped in a
//!   synthetic `CallToolResult`. Plus `_meta` (related-task, and the freshness
//!   marker when the index moved under the run).
//!
//! Only `cqs_eval` is task-augmentable (`taskSupport:"optional"`); augmenting any
//! other tool is `-32601`. A task that is unknown / expired / GC'd is `-32602`
//! (never an HTTP 404 — 404 is session-expiry only).

use std::path::Path;

use serde_json::{json, Value};

use super::lifecycle::{self, JsonRpcError, JsonRpcResponse};

/// The MCP spec `_meta` key for the related-task marker (a MUST on
/// `tasks/result`, included on `CreateTaskResult`/`GetTaskResult` too — the
/// inclusive reading is safe and cheap).
const RELATED_TASK_META_KEY: &str = "io.modelcontextprotocol/related-task";

/// Freshness marker `_meta` key: injected into a `tasks/result` when the live
/// invalidation epoch has moved past the run's stamped epoch, so a consumer (the
/// recall-gate) refuses a verdict describing a substrate that no longer exists.
const TASK_FRESHNESS_META_KEY: &str = "io.cqs/task-freshness";

/// `pollInterval` (ms) advertised on task descriptors — a spec MAY (D2) so a
/// polling client paces its `tasks/get` calls sanely across a ~20-min eval. This
/// is the CLIENT cadence, distinct from the transport's own tight internal poll
/// while blocking on `tasks/result` ([`INTERNAL_POLL_INTERVAL`]).
const CLIENT_POLL_INTERVAL_MS: u64 = 5_000;

/// The default/max task retention the transport resolves an augmentation `ttl`
/// to. A client may request a SHORTER ttl but not a longer one — retention is
/// clamped to the daemon default so a hostile client cannot pin memory beyond
/// the store's own bound. Single source of truth is the daemon-side const.
const TTL_SECS_DEFAULT_AND_MAX: u64 = crate::cli::batch::tasks::DEFAULT_TASK_TTL_SECS;

/// The transport's OWN poll cadence while blocking on `tasks/result` — tight so
/// the block returns promptly after the task terminalizes. NOT the client-facing
/// [`CLIENT_POLL_INTERVAL_MS`].
pub(super) const INTERNAL_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(200);

/// Defensive anti-wedge ceiling on a single `tasks/result` block. The daemon's
/// publish-on-drop handle guarantees every worker terminalizes (so this is
/// unreachable in practice), and a full eval is ~20 min; this backstop is well
/// above that so it never truncates a real run. It is NOT the spec's block window
/// (the spec MUSTs block-until-terminal) — it only bounds a hypothetically-wedged
/// task instead of looping forever.
pub(super) const MAX_BLOCK_DURATION: std::time::Duration = std::time::Duration::from_secs(45 * 60);

/// Whether a `tools/call` params object carries a `task` augmentation. A present,
/// non-null `task` field triggers the create-task path; absent or `null` is a
/// normal `tools/call`.
pub(super) fn is_augmented(params: &Option<Value>) -> bool {
    params
        .as_ref()
        .and_then(|p| p.get("task"))
        .is_some_and(|t| !t.is_null())
}

// ── daemon dial ──────────────────────────────────────────────────────────────

/// The three outcomes of dialing a task wire verb.
enum Dialed {
    /// The daemon's slim-envelope `output.data` payload.
    Data(Value),
    /// A daemon HANDLER error riding under `status:"ok"` (the slim
    /// `{"error":{code,message}}`) — e.g. the opaque `unavailable` admission
    /// reject.
    HandlerError { code: String, message: String },
    /// A JSON-RPC PROTOCOL error (daemon absent / transport / malformed).
    Transport { code: i32, message: String },
}

/// Dial one task wire verb over the daemon socket and classify its envelope.
#[cfg(unix)]
fn dial(cqs_dir: &Path, command: &str, arguments: &Value) -> Dialed {
    use cqs::daemon_translate::{daemon_json_args_request, DaemonRpcError};

    let envelope = match daemon_json_args_request(cqs_dir, command, arguments) {
        Ok(env) => env,
        Err(DaemonRpcError::SocketMissing(msg)) => {
            return Dialed::Transport {
                code: lifecycle::INTERNAL_ERROR,
                message: format!("cqs daemon not running: {msg}. Start `cqs watch --serve`."),
            }
        }
        Err(DaemonRpcError::Busy(msg)) => {
            return Dialed::Transport {
                code: lifecycle::INTERNAL_ERROR,
                message: format!("cqs daemon busy (retry shortly): {msg}"),
            }
        }
        Err(
            DaemonRpcError::Transport(msg)
            | DaemonRpcError::Timeout(msg)
            | DaemonRpcError::BadResponse(msg)
            | DaemonRpcError::ResponseTooLarge(msg),
        ) => {
            return Dialed::Transport {
                code: lifecycle::INTERNAL_ERROR,
                message: format!("cqs daemon transport failure: {msg}"),
            }
        }
        Err(DaemonRpcError::DaemonError(msg)) => {
            return Dialed::Transport {
                code: lifecycle::INVALID_PARAMS,
                message: format!("cqs daemon rejected the request: {msg}"),
            }
        }
    };

    let output = match envelope.get("output") {
        Some(o) => o,
        None => {
            return Dialed::Transport {
                code: lifecycle::INTERNAL_ERROR,
                message: "daemon response missing `output`".to_string(),
            }
        }
    };
    // Peel the slim envelope: a handler error rides as `{"error":{...}}`, a
    // success as `{"data":...}`.
    if let Some(err) = output.get("error") {
        return Dialed::HandlerError {
            code: err
                .get("code")
                .and_then(|c| c.as_str())
                .unwrap_or("internal")
                .to_string(),
            message: err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("daemon error")
                .to_string(),
        };
    }
    match output.get("data") {
        Some(data) => Dialed::Data(data.clone()),
        None => Dialed::Transport {
            code: lifecycle::INTERNAL_ERROR,
            message: "daemon response missing `output.data`".to_string(),
        },
    }
}

/// Non-unix stub: the daemon socket is unix-only. `serve_stdio` fails fast
/// off-unix, so this is not reached in practice; it keeps the module compiling.
#[cfg(not(unix))]
fn dial(_cqs_dir: &Path, _command: &str, _arguments: &Value) -> Dialed {
    Dialed::Transport {
        code: lifecycle::INTERNAL_ERROR,
        message: "the cqs MCP task surface requires a unix daemon socket".to_string(),
    }
}

// ── shaping helpers ──────────────────────────────────────────────────────────

/// The related-task `_meta` object: `{ "io.modelcontextprotocol/related-task":
/// { taskId } }`.
fn related_task_meta(task_id: &str) -> Value {
    json!({ RELATED_TASK_META_KEY: { "taskId": task_id } })
}

/// Resolve an augmentation `task.ttl` to a concrete retention in SECONDS.
///
/// UNIT BOUNDARY: the spec's `TaskMetadata.ttl` is "requested duration in
/// **milliseconds** to retain task from creation" (2025-11-25 schema.ts); the
/// store's retention is seconds. This is the single inbound conversion point:
/// ms → secs with ceiling division (a 1..999ms request rounds up to 1s, never
/// down to an instant-GC 0). Absent / null / non-positive ⇒ the default; the
/// result is clamped to the max so retention stays bounded.
fn resolve_ttl(task: &Value) -> u64 {
    task.get("ttl")
        .and_then(|v| v.as_u64())
        .filter(|n| *n >= 1)
        .map(|ms| ms.div_ceil(1000).min(TTL_SECS_DEFAULT_AND_MAX))
        .unwrap_or(TTL_SECS_DEFAULT_AND_MAX)
}

/// Convert a daemon descriptor's `ttl` field (internal SECONDS, `null` for
/// unlimited) to the spec's wire unit (**milliseconds**) in place. The single
/// outbound conversion point, applied wherever a descriptor is shaped into a
/// spec `Task` (`CreateTaskResult` nested, `GetTaskResult` flat). `null`
/// (unlimited) passes through untouched; a saturating multiply keeps a
/// pathological stored value from wrapping.
fn descriptor_ttl_secs_to_ms(task: &mut Value) {
    if let Some(obj) = task.as_object_mut() {
        if let Some(ttl_secs) = obj.get("ttl").and_then(|v| v.as_u64()) {
            obj.insert("ttl".to_string(), json!(ttl_secs.saturating_mul(1000)));
        }
    }
}

/// Build a JSON-RPC error response carrying optional `data`.
fn rpc_error(
    id: Option<Value>,
    code: i32,
    message: String,
    data: Option<Value>,
) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message,
            data,
        }),
    }
}

/// The `-32602` a gone (unknown / expired / GC'd) task maps to. Never an HTTP
/// 404 — 404 is session-expiry only.
pub(super) fn task_gone(id: Option<Value>) -> JsonRpcResponse {
    rpc_error(
        id,
        lifecycle::INVALID_PARAMS,
        "unknown, expired, or garbage-collected taskId".to_string(),
        None,
    )
}

/// A retryable-busy JSON-RPC error (the `tasks/result` blocking budget / per-task
/// fan-out reject). Carries `data.retryable:true` so a client backs off and
/// polls `tasks/get` instead. HTTP-only: the stdio surface has no blocking budget
/// (one sequential client), so nothing on that surface rejects a block.
#[cfg(all(feature = "mcp-http", unix))]
pub(super) fn retryable_busy(id: Option<Value>, message: &str) -> JsonRpcResponse {
    rpc_error(
        id,
        lifecycle::INTERNAL_ERROR,
        message.to_string(),
        Some(json!({ "code": "unavailable", "retryable": true })),
    )
}

/// A generic internal JSON-RPC error (a poll-worker panic or other transport
/// fault while blocking). HTTP-only: the stdio block runs the poll inline and has
/// no `spawn_blocking` worker to fault.
#[cfg(all(feature = "mcp-http", unix))]
pub(super) fn internal_error(id: Option<Value>, message: &str) -> JsonRpcResponse {
    rpc_error(id, lifecycle::INTERNAL_ERROR, message.to_string(), None)
}

/// The anti-wedge backstop error a `tasks/result` block returns if it somehow
/// never terminalizes (unreachable given the daemon's publish-on-drop). Marked
/// retryable so a client falls back to `tasks/get` polling.
pub(super) fn block_backstop_error(id: Option<Value>) -> JsonRpcResponse {
    rpc_error(
        id,
        lifecycle::INTERNAL_ERROR,
        "tasks/result exceeded the block backstop; retry with tasks/get polling".to_string(),
        Some(json!({ "code": "timeout", "retryable": true })),
    )
}

/// Read the required `taskId` string from a request's params. Missing / blank ⇒
/// `-32602` (the same code a gone task gets — a client cannot distinguish
/// "malformed request" from "no such task", which is also the no-oracle posture).
pub(super) fn task_id_from_params(params: &Option<Value>) -> Result<String, ()> {
    params
        .as_ref()
        .and_then(|p| p.get("taskId"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or(())
}

// ── create task (augmented tools/call) ───────────────────────────────────────

/// Handle an augmented `tools/call` (a `task` field present): admit a `cqs_eval`
/// run as a task and return a `CreateTaskResult`, or a JSON-RPC error.
///
/// - Augmenting any tool other than `cqs_eval` → `-32601` (task support is
///   `optional` on `cqs_eval` alone; every other tool is default-forbidden).
/// - The admission reject (single-flight-busy OR store-cap-full) surfaces the ONE
///   opaque retryable `Unavailable`, byte-identical for both conditions
///   (the one-opaque-reject rule) — it flows from the daemon's single slim `unavailable` error.
pub(super) fn create_task(
    cqs_dir: &Path,
    id: Option<Value>,
    params: Option<Value>,
) -> JsonRpcResponse {
    let _span = tracing::info_span!("mcp_create_task").entered();

    let params = match &params {
        Some(p) => p,
        None => {
            return rpc_error(
                id,
                lifecycle::INVALID_PARAMS,
                "tools/call requires params".to_string(),
                None,
            )
        }
    };

    // Only `cqs_eval` is task-augmentable; every other tool (or an unknown name)
    // → -32601 (a spec SHOULD for the forbidden-tool case).
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name != "cqs_eval" {
        return rpc_error(
            id,
            lifecycle::METHOD_NOT_FOUND,
            format!("tool `{name}` does not support task augmentation (only cqs_eval does)"),
            None,
        );
    }

    // The `task` augmentation must be an object (`{ ttl? }`). The client's own
    // `_meta` (which may carry a `progressToken`) is preserved intact — never
    // stripped or rejected here; the (taskId ← progressToken) session bookkeeping
    // that consumes it hooks in later (P3a-2 progress-notification lane).
    let task_field = match params.get("task") {
        Some(t) if t.is_object() => t,
        _ => {
            return rpc_error(
                id,
                lifecycle::INVALID_PARAMS,
                "the `task` augmentation must be an object ({ ttl? })".to_string(),
                None,
            )
        }
    };
    let ttl_secs = resolve_ttl(task_field);

    // Arguments pre-check against the SAME eval core the daemon deserializes —
    // a clearer -32602 before the socket round-trip.
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(Value::Object(o)) => Value::Object(o.clone()),
        Some(_) => {
            return rpc_error(
                id,
                lifecycle::INVALID_PARAMS,
                "tools/call `arguments` must be an object".to_string(),
                None,
            )
        }
    };
    if let Err(e) = super::tools::validate_arguments("eval", &arguments) {
        return rpc_error(
            id,
            lifecycle::INVALID_PARAMS,
            format!("invalid arguments for cqs_eval: {e}"),
            None,
        );
    }

    // Inject the resolved ttl and dial the daemon `eval-task` admission verb.
    let mut dial_args = arguments;
    if let Some(obj) = dial_args.as_object_mut() {
        obj.insert("ttl_secs".to_string(), json!(ttl_secs));
    }
    match dial(cqs_dir, crate::cli::batch::tasks::CMD_EVAL_TASK, &dial_args) {
        Dialed::Data(data) => {
            tracing::info!("mcp create_task: admitted eval task");
            shape_create_result(id, &data)
        }
        Dialed::HandlerError { code, message } => {
            // The admission reject. `unavailable` is the opaque retryable
            // (byte-identical for single-flight-busy and store-cap-full); other
            // handler errors (a bad fixture) are caller mistakes and may differ.
            tracing::warn!(code = %code, "mcp create_task: admission rejected");
            let jsonrpc_code = taxonomy_to_jsonrpc(&code);
            let retryable = code == "unavailable" || code == "timeout";
            rpc_error(
                id,
                jsonrpc_code,
                message,
                Some(json!({ "code": code, "retryable": retryable })),
            )
        }
        Dialed::Transport { code, message } => rpc_error(id, code, message, None),
    }
}

/// Shape a daemon `eval-task` success (`{ "task": <descriptor> }`) into the
/// NESTED `CreateTaskResult`: `{ task: Task+pollInterval, _meta: {related-task} }`.
fn shape_create_result(id: Option<Value>, data: &Value) -> JsonRpcResponse {
    let descriptor = data.get("task").cloned().unwrap_or(Value::Null);
    let task_id = descriptor
        .get("taskId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut task = descriptor;
    descriptor_ttl_secs_to_ms(&mut task);
    if let Some(obj) = task.as_object_mut() {
        obj.insert("pollInterval".to_string(), json!(CLIENT_POLL_INTERVAL_MS));
    }
    lifecycle::success(
        id,
        json!({ "task": task, "_meta": related_task_meta(&task_id) }),
    )
}

/// Map a cqs taxonomy error code to a JSON-RPC integer code. Caller mistakes
/// (`invalid_input` / `parse_error` / `not_found`) map to `-32602`; everything
/// else (`internal` / `io_error` / `timeout` / `unavailable`) maps to the
/// catch-all `-32603`. The taxonomy string is preserved in `error.data.code`.
fn taxonomy_to_jsonrpc(code: &str) -> i32 {
    match code {
        "invalid_input" | "parse_error" | "not_found" => lifecycle::INVALID_PARAMS,
        _ => lifecycle::INTERNAL_ERROR,
    }
}

// ── tasks/get (non-blocking) ─────────────────────────────────────────────────

/// Handle `tasks/get`: a cheap non-blocking daemon dial returning the FLAT
/// `GetTaskResult` (Task fields at the top level, NOT nested) + `pollInterval` +
/// related-task `_meta`. A gone task → `-32602`.
pub(super) fn get_task(
    cqs_dir: &Path,
    id: Option<Value>,
    params: Option<Value>,
) -> JsonRpcResponse {
    let _span = tracing::info_span!("mcp_tasks_get").entered();
    let task_id = match task_id_from_params(&params) {
        Ok(t) => t,
        Err(()) => return task_gone(id),
    };
    match dial(
        cqs_dir,
        crate::cli::batch::tasks::CMD_TASK_GET,
        &json!({ "taskId": task_id }),
    ) {
        Dialed::Data(data) => shape_get_result(id, &task_id, &data),
        Dialed::HandlerError { message, .. } | Dialed::Transport { message, .. } => {
            rpc_error(id, lifecycle::INTERNAL_ERROR, message, None)
        }
    }
}

/// Shape a daemon `task-get` payload into the FLAT `GetTaskResult` (Task fields at
/// the top level of `result`, NOT nested) + `pollInterval` + related-task `_meta`.
/// A `gone` state → `-32602`. The descriptor is serialized additively (no
/// full-object equality assertion): a later lane extends it with progress fields.
fn shape_get_result(id: Option<Value>, task_id: &str, data: &Value) -> JsonRpcResponse {
    match data.get("state").and_then(|s| s.as_str()) {
        Some("found") => {
            let descriptor = data.get("descriptor").cloned().unwrap_or(Value::Null);
            let mut flat = descriptor;
            descriptor_ttl_secs_to_ms(&mut flat);
            if let Some(obj) = flat.as_object_mut() {
                obj.insert("pollInterval".to_string(), json!(CLIENT_POLL_INTERVAL_MS));
                obj.insert("_meta".to_string(), related_task_meta(task_id));
                lifecycle::success(id, Value::Object(obj.clone()))
            } else {
                task_gone(id)
            }
        }
        _ => task_gone(id),
    }
}

// ── tasks/result (block until terminal) ──────────────────────────────────────

/// One non-blocking `tasks/result` poll step: either the finished response
/// (terminal) or `Pending` (the caller sleeps and retries). Shared by the stdio
/// inline loop ([`block_for_result_stdio`]) and the HTTP budget-isolated loop
/// (`http.rs`).
pub(super) enum ResultStep {
    /// Terminal — the finished JSON-RPC response (a verbatim `CallToolResult`
    /// result, a passthrough JSON-RPC error, or `-32602` gone).
    Done(JsonRpcResponse),
    /// Not yet terminal — sleep [`INTERNAL_POLL_INTERVAL`] and poll again.
    Pending,
}

/// Dial the daemon `task-result` verb ONCE and classify it into a [`ResultStep`].
pub(super) fn poll_result_once(cqs_dir: &Path, id: &Option<Value>, task_id: &str) -> ResultStep {
    match dial(
        cqs_dir,
        crate::cli::batch::tasks::CMD_TASK_RESULT,
        &json!({ "taskId": task_id }),
    ) {
        Dialed::Data(data) => match data.get("state").and_then(|s| s.as_str()) {
            Some("ready") => ResultStep::Done(terminal_response(id.clone(), task_id, &data)),
            Some("pending") => ResultStep::Pending,
            // gone / unknown state ⇒ the task vanished (GC'd mid-poll) → -32602.
            _ => ResultStep::Done(task_gone(id.clone())),
        },
        // A daemon-absent / transport / handler error while polling is terminal
        // for the block (never loop forever on a dead daemon).
        Dialed::HandlerError { message, .. } | Dialed::Transport { message, .. } => {
            ResultStep::Done(rpc_error(
                id.clone(),
                lifecycle::INTERNAL_ERROR,
                message,
                None,
            ))
        }
    }
}

/// Shape a terminal `task-result` payload into the verbatim MCP response.
///
/// `data` is `{ state:"ready", terminal:{...}, staleness:{...}|null }`. A
/// `tool_result` terminal → a `CallToolResult` (the JSON-RPC RESULT, `isError`
/// mapping the tool-level error); an `rpc_error` terminal → the JSON-RPC ERROR
/// itself (never wrapped in a synthetic CallToolResult). Both carry the
/// related-task `_meta`; a moved index adds the freshness marker.
fn terminal_response(id: Option<Value>, task_id: &str, data: &Value) -> JsonRpcResponse {
    let terminal = data.get("terminal").cloned().unwrap_or(Value::Null);
    let staleness = data.get("staleness").filter(|s| !s.is_null()).cloned();

    // Belt-and-suspenders: a terminal payload missing its `terminal` object is
    // the torn-read window the daemon already treats as retry-not-error; poll
    // again rather than manufacture an empty result.
    let kind = terminal.get("kind").and_then(|k| k.as_str());
    match kind {
        Some("tool_result") => {
            let value = terminal.get("value").cloned().unwrap_or(Value::Null);
            let is_error = terminal
                .get("is_error")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);

            // Build the `_meta` (related-task MUST, plus the freshness marker on
            // an index move) and shape the CallToolResult exactly as a synchronous
            // cqs_eval tools/call would (via `classify_output`), so the result is
            // verbatim-equivalent to the non-task path.
            let mut meta = json!({ RELATED_TASK_META_KEY: { "taskId": task_id } });
            if let (Some(s), Some(obj)) = (staleness, meta.as_object_mut()) {
                obj.insert(TASK_FRESHNESS_META_KEY.to_string(), s);
            }
            let mut call_tool_result =
                super::tools::classify_output(&json!({ "data": value, "_meta": meta }));
            // A tool-level error overrides isError (the isError→failed mapping):
            // classify_output computes isError from the payload shape; a stored
            // is_error:true forces it true regardless.
            if is_error {
                if let Some(obj) = call_tool_result.as_object_mut() {
                    obj.insert("isError".to_string(), Value::Bool(true));
                }
            }
            lifecycle::success(id, call_tool_result)
        }
        Some("rpc_error") => {
            // The call failed at the JSON-RPC level: return THAT error, not a
            // CallToolResult. The taxonomy code + related-task ride in `data`.
            let code = terminal
                .get("code")
                .and_then(|c| c.as_str())
                .unwrap_or("internal");
            let message = terminal
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("eval task failed")
                .to_string();
            let mut err_data = json!({
                "code": code,
                RELATED_TASK_META_KEY: { "taskId": task_id },
            });
            if let Some(obj) = err_data.as_object_mut() {
                obj.insert(
                    "retryable".to_string(),
                    Value::Bool(code == "unavailable" || code == "timeout"),
                );
            }
            rpc_error(id, taxonomy_to_jsonrpc(code), message, Some(err_data))
        }
        // A `ready` state with no recognizable terminal shape: unreachable via
        // a healthy daemon (it returns `Pending` on an empty slot and
        // `terminal_json` only emits known kinds), so treat it as GONE
        // (-32602) — a crisp terminal error. Returning `Pending` here would
        // spin the block loop against a shape we will never learn to parse,
        // burning the full 20-min backstop for nothing.
        _ => task_gone(id),
    }
}

/// Handle `tasks/result` on the stdio surface: block until terminal by looping
/// [`poll_result_once`] with [`INTERNAL_POLL_INTERVAL`] sleeps. Correct for the
/// single sequential stdio client (there is no concurrency budget to starve). The
/// HTTP surface uses its own budget-isolated async loop (`http.rs`) so a block
/// never pins one of the outermost request permits (the block-budget isolation).
pub(super) fn block_for_result_stdio(
    cqs_dir: &Path,
    id: Option<Value>,
    params: Option<Value>,
) -> JsonRpcResponse {
    let _span = tracing::info_span!("mcp_tasks_result_stdio").entered();
    let task_id = match task_id_from_params(&params) {
        Ok(t) => t,
        Err(()) => return task_gone(id),
    };
    let start = std::time::Instant::now();
    loop {
        match poll_result_once(cqs_dir, &id, &task_id) {
            ResultStep::Done(resp) => return resp,
            ResultStep::Pending => {
                if start.elapsed() > MAX_BLOCK_DURATION {
                    tracing::warn!(
                        "mcp tasks/result: block backstop tripped (task never terminalized)"
                    );
                    return block_backstop_error(id);
                }
                std::thread::sleep(INTERNAL_POLL_INTERVAL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_augmented_detects_task_field() {
        assert!(is_augmented(&Some(
            json!({ "name": "cqs_eval", "task": {} })
        )));
        assert!(is_augmented(&Some(
            json!({ "name": "cqs_eval", "task": { "ttl": 60 } })
        )));
        assert!(!is_augmented(&Some(json!({ "name": "cqs_eval" }))));
        assert!(!is_augmented(&Some(
            json!({ "name": "cqs_eval", "task": null })
        )));
        assert!(!is_augmented(&None));
    }

    #[test]
    fn resolve_ttl_clamps_and_defaults() {
        assert_eq!(resolve_ttl(&json!({})), TTL_SECS_DEFAULT_AND_MAX);
        // Spec unit is MILLISECONDS: 60_000ms = 60s.
        assert_eq!(resolve_ttl(&json!({ "ttl": 60_000 })), 60);
        // A sub-second request rounds UP to 1s (never down to instant-GC 0).
        assert_eq!(resolve_ttl(&json!({ "ttl": 500 })), 1);
        assert_eq!(resolve_ttl(&json!({ "ttl": 60 })), 1);
        // Above the max clamps down.
        assert_eq!(
            resolve_ttl(&json!({ "ttl": TTL_SECS_DEFAULT_AND_MAX * 1000 + 1 })),
            TTL_SECS_DEFAULT_AND_MAX
        );
        // Zero / null / non-integer fall back to the default.
        assert_eq!(resolve_ttl(&json!({ "ttl": 0 })), TTL_SECS_DEFAULT_AND_MAX);
        assert_eq!(
            resolve_ttl(&json!({ "ttl": null })),
            TTL_SECS_DEFAULT_AND_MAX
        );
        assert_eq!(
            resolve_ttl(&json!({ "ttl": "x" })),
            TTL_SECS_DEFAULT_AND_MAX
        );
    }

    #[test]
    fn task_id_from_params_requires_nonempty_string() {
        assert_eq!(
            task_id_from_params(&Some(json!({ "taskId": "abc" }))),
            Ok("abc".to_string())
        );
        assert_eq!(task_id_from_params(&Some(json!({ "taskId": "" }))), Err(()));
        assert_eq!(task_id_from_params(&Some(json!({ "taskId": 5 }))), Err(()));
        assert_eq!(task_id_from_params(&Some(json!({}))), Err(()));
        assert_eq!(task_id_from_params(&None), Err(()));
    }

    #[test]
    fn task_gone_is_minus_32602() {
        let r = task_gone(Some(json!(1)));
        assert_eq!(
            r.error.as_ref().map(|e| e.code),
            Some(lifecycle::INVALID_PARAMS)
        );
        assert!(r.result.is_none());
    }

    /// `CreateTaskResult` NESTS the task under a `task` key (D6 asymmetry) and
    /// carries the related-task `_meta` + a `pollInterval`.
    #[test]
    fn create_result_nests_the_task() {
        let data = json!({
            "task": { "taskId": "tid", "status": "working", "createdAt": "t0", "lastUpdatedAt": "t0", "ttl": 900 }
        });
        let resp = shape_create_result(Some(json!(1)), &data);
        let result = resp.result.expect("result");
        // NESTED: task fields live under `result.task`, not at the top level.
        assert_eq!(
            result
                .get("task")
                .and_then(|t| t.get("taskId"))
                .and_then(|v| v.as_str()),
            Some("tid")
        );
        assert!(
            result.get("taskId").is_none(),
            "CreateTaskResult must NOT hoist task fields to the top level (it nests)"
        );
        assert_eq!(
            result
                .get("task")
                .and_then(|t| t.get("pollInterval"))
                .and_then(|v| v.as_u64()),
            Some(CLIENT_POLL_INTERVAL_MS)
        );
        // Wire `ttl` is MILLISECONDS (spec unit); the daemon descriptor's 900
        // is seconds and must be converted at this boundary.
        assert_eq!(
            result
                .get("task")
                .and_then(|t| t.get("ttl"))
                .and_then(|v| v.as_u64()),
            Some(900_000)
        );
        assert_eq!(
            result
                .get("_meta")
                .and_then(|m| m.get(RELATED_TASK_META_KEY))
                .and_then(|r| r.get("taskId"))
                .and_then(|v| v.as_str()),
            Some("tid")
        );
    }

    /// `GetTaskResult` is FLAT — Task fields at the top level of `result` (D6
    /// asymmetry) — with `pollInterval` + related-task `_meta`, and NOT nested.
    #[test]
    fn get_result_is_flat() {
        let data = json!({
            "state": "found",
            "descriptor": { "taskId": "tid", "status": "completed", "createdAt": "t0", "lastUpdatedAt": "t1", "ttl": 900 }
        });
        let resp = shape_get_result(Some(json!(2)), "tid", &data);
        let result = resp.result.expect("result");
        // FLAT: task fields at the TOP level, not under a `task` key.
        assert_eq!(result.get("taskId").and_then(|v| v.as_str()), Some("tid"));
        assert_eq!(
            result.get("status").and_then(|v| v.as_str()),
            Some("completed")
        );
        assert!(
            result.get("task").is_none(),
            "GetTaskResult must be FLAT (no `task` nesting)"
        );
        assert_eq!(
            result.get("pollInterval").and_then(|v| v.as_u64()),
            Some(CLIENT_POLL_INTERVAL_MS)
        );
        assert_eq!(
            result
                .get("_meta")
                .and_then(|m| m.get(RELATED_TASK_META_KEY))
                .and_then(|r| r.get("taskId"))
                .and_then(|v| v.as_str()),
            Some("tid")
        );
        // ttl present as a number (spec: number | null), in MILLISECONDS —
        // converted from the daemon descriptor's seconds at this boundary.
        assert_eq!(result.get("ttl").and_then(|v| v.as_u64()), Some(900_000));
    }

    /// A `gone` `task-get` state maps to `-32602`, never a flat descriptor.
    #[test]
    fn get_result_gone_is_minus_32602() {
        let resp = shape_get_result(Some(json!(3)), "tid", &json!({ "state": "gone" }));
        assert_eq!(
            resp.error.as_ref().map(|e| e.code),
            Some(lifecycle::INVALID_PARAMS)
        );
    }

    #[test]
    fn taxonomy_maps_caller_mistakes_to_invalid_params() {
        assert_eq!(
            taxonomy_to_jsonrpc("invalid_input"),
            lifecycle::INVALID_PARAMS
        );
        assert_eq!(taxonomy_to_jsonrpc("not_found"), lifecycle::INVALID_PARAMS);
        assert_eq!(taxonomy_to_jsonrpc("internal"), lifecycle::INTERNAL_ERROR);
        assert_eq!(
            taxonomy_to_jsonrpc("unavailable"),
            lifecycle::INTERNAL_ERROR
        );
    }

    /// A `tool_result` terminal is returned as a verbatim CallToolResult
    /// (JSON-RPC RESULT), with structuredContent carrying the eval payload and
    /// the related-task `_meta` present.
    #[test]
    fn terminal_tool_result_becomes_call_tool_result() {
        let data = json!({
            "state": "ready",
            "terminal": { "kind": "tool_result", "value": { "r_at_1": 0.5 }, "is_error": false },
            "staleness": null,
        });
        let resp = terminal_response(Some(json!(1)), "tid-1", &data);
        assert!(resp.error.is_none(), "tool_result is a successful response");
        let result = resp.result.expect("result present");
        assert_eq!(result.get("isError").and_then(|b| b.as_bool()), Some(false));
        assert_eq!(
            result
                .get("structuredContent")
                .and_then(|s| s.get("r_at_1"))
                .and_then(|v| v.as_f64()),
            Some(0.5)
        );
        let related = result
            .get("_meta")
            .and_then(|m| m.get(RELATED_TASK_META_KEY))
            .and_then(|r| r.get("taskId"))
            .and_then(|v| v.as_str());
        assert_eq!(related, Some("tid-1"));
    }

    /// A `tool_result` with `is_error:true` maps to a successful JSON-RPC
    /// response carrying `isError:true` (the isError→failed status mapping does
    /// NOT turn it into a JSON-RPC error).
    #[test]
    fn tool_result_is_error_stays_successful_response() {
        let data = json!({
            "state": "ready",
            "terminal": { "kind": "tool_result", "value": { "msg": "tool failed" }, "is_error": true },
            "staleness": null,
        });
        let resp = terminal_response(Some(json!(2)), "tid-2", &data);
        assert!(resp.error.is_none(), "still a successful JSON-RPC response");
        assert_eq!(
            resp.result
                .and_then(|r| r.get("isError").and_then(|b| b.as_bool())),
            Some(true)
        );
    }

    /// An `rpc_error` terminal is returned as the JSON-RPC ERROR itself, never a
    /// CallToolResult, with the taxonomy code + related-task in `error.data`.
    #[test]
    fn terminal_rpc_error_becomes_jsonrpc_error() {
        let data = json!({
            "state": "ready",
            "terminal": { "kind": "rpc_error", "code": "internal", "message": "eval blew up" },
            "staleness": null,
        });
        let resp = terminal_response(Some(json!(3)), "tid-3", &data);
        assert!(resp.result.is_none(), "rpc_error is not a CallToolResult");
        let err = resp.error.expect("error present");
        assert_eq!(err.code, lifecycle::INTERNAL_ERROR);
        assert_eq!(err.message, "eval blew up");
        let data = err.data.expect("error data");
        assert_eq!(data.get("code").and_then(|c| c.as_str()), Some("internal"));
        assert_eq!(
            data.get(RELATED_TASK_META_KEY)
                .and_then(|r| r.get("taskId"))
                .and_then(|v| v.as_str()),
            Some("tid-3")
        );
    }

    /// The freshness marker rides `_meta` when the index moved under the run.
    #[test]
    fn terminal_tool_result_injects_freshness_on_index_move() {
        let data = json!({
            "state": "ready",
            "terminal": { "kind": "tool_result", "value": { "ok": true }, "is_error": false },
            "staleness": { "computed_at_epoch": 5, "index_moved_since_run": true },
        });
        let resp = terminal_response(Some(json!(4)), "tid-4", &data);
        let meta = resp
            .result
            .and_then(|r| r.get("_meta").cloned())
            .expect("_meta present");
        let fresh = meta.get(TASK_FRESHNESS_META_KEY).expect("freshness marker");
        assert_eq!(
            fresh.get("index_moved_since_run").and_then(|b| b.as_bool()),
            Some(true)
        );
        assert_eq!(
            fresh.get("computed_at_epoch").and_then(|n| n.as_u64()),
            Some(5)
        );
    }
}
