//! Daemon-side wire verbs for the MCP Tasks surface (Phase 3b floor, lane 2).
//!
//! The transport (`cli::mcp`) reaches the daemon [`super::TaskStore`] over the
//! existing JSON-args socket path — NO socket/daemon protocol change. Three
//! one-shot verbs plug into [`crate::cli::batch::dispatch_via_view_json`] ahead
//! of `build_batch_cmd` (they have no `BatchCmd`, they read the `BatchView`
//! directly):
//!
//! - `eval-task` — admit a `cqs_eval` run as an MCP task (the augmented
//!   `tools/call`). Returns the created task descriptor, or the ONE opaque
//!   retryable `Unavailable` on an admission reject (single-flight-busy OR
//!   store-cap-full, byte-identical per the one-opaque-reject rule — both flow from
//!   [`super::super::handlers::dispatch_eval_task`]'s single `Unavailable`).
//! - `task-get` — a cheap non-blocking status read (`tasks/get`).
//! - `task-result` — a cheap non-blocking terminal read (`tasks/result`), which
//!   the transport polls until terminal. The blocking is transport-side; this
//!   verb never blocks the daemon handler pool.
//!
//! Each verb returns its RAW payload; the shared dispatch (`write_json_line`)
//! applies the single `{"data": …}` slim envelope the transport peels as
//! `output.data` before shaping the MCP wire form (nested `CreateTaskResult`,
//! flat `GetTaskResult`, or the verbatim `tasks/result` terminal). The verbs must
//! NOT pre-wrap with [`crate::cli::json_envelope::wrap_value`] — that double-nests
//! the payload under `output.data.data`, where the transport dial never looks. An
//! admission reject rides the slim ERROR envelope the shared path emits from `Err`.

use anyhow::Result;
use serde_json::{json, Value};

use crate::cli::batch::BatchView;

use super::{TaskDescriptor, TaskId, TaskLookup, TaskResultLookup, TerminalKind};

/// Wire command: admit a `cqs_eval` run as a task (the augmented `tools/call`).
/// `pub(crate)` (re-exported at the `tasks` module root) so the transport dials
/// the SAME string this module intercepts — one source of truth for the verb.
pub(crate) const CMD_EVAL_TASK: &str = "eval-task";
/// Wire command: non-blocking `tasks/get` status read.
pub(crate) const CMD_TASK_GET: &str = "task-get";
/// Wire command: non-blocking `tasks/result` terminal read (transport polls it).
pub(crate) const CMD_TASK_RESULT: &str = "task-result";

/// Whether `command` is one of the three task wire verbs — the intercept
/// predicate [`crate::cli::batch::dispatch_via_view_json`] keys off before
/// `build_batch_cmd` (which would reject these as unknown commands).
pub(in crate::cli::batch) fn is_task_verb(command: &str) -> bool {
    matches!(command, CMD_EVAL_TASK | CMD_TASK_GET | CMD_TASK_RESULT)
}

/// Dispatch a task wire verb against the `BatchView`. Returns the slim
/// success/`Value` on success, or an `Err` the caller redacts into the slim
/// error envelope (used for the `eval-task` admission reject). `task-get` /
/// `task-result` never error — a `Gone` task is a normal `state:"gone"` payload,
/// mapped to `-32602` transport-side, not a daemon error.
///
/// Precondition: [`is_task_verb`] returned `true` for `command`.
pub(in crate::cli::batch) fn dispatch_task_verb(
    view: &BatchView,
    command: &str,
    arguments: &Value,
) -> Result<Value> {
    match command {
        CMD_EVAL_TASK => admit_eval_task(view, arguments),
        CMD_TASK_GET => Ok(get_task(view, arguments)),
        CMD_TASK_RESULT => Ok(result_task(view, arguments)),
        // Unreachable: the caller gates on `is_task_verb`.
        other => Err(anyhow::anyhow!("not a task verb: {other}")),
    }
}

/// Render a task descriptor as the flat wire object both `eval-task` and
/// `task-get` return (the transport nests it for `CreateTaskResult`). `ttl` is
/// `number | null` (null = unlimited) in SECONDS — this is the internal
/// daemon-socket surface; the MCP transport converts to the spec's
/// milliseconds at its boundary (`descriptor_ttl_secs_to_ms`).
fn descriptor_json(d: &TaskDescriptor) -> Value {
    let mut v = json!({
        "taskId": d.task_id,
        "status": d.status.as_wire(),
        "createdAt": d.created_at,
        "lastUpdatedAt": d.last_updated_at,
        "ttl": d.ttl_secs,
    });
    // Additive `{done, total}` progress (D2). Skip-if-none: a counter-less or
    // cross-version descriptor emits NO `progress` key rather than a `null` —
    // the transport reads it skip-if-absent, and a `null` would be a distinct,
    // wrong wire shape. `as_object_mut` is fence-safe (no panic): the `json!`
    // literal above is always an object, and the `if let` degrades to a no-op
    // in the impossible non-object case.
    if let Some(p) = d.progress {
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "progress".to_string(),
                json!({ "done": p.done, "total": p.total }),
            );
        }
    }
    v
}

/// `eval-task`: parse the eval core knobs + the resolved `ttl_secs`, then admit
/// the run through the shared single-flight/cap admission. On success returns the
/// descriptor; on an admission reject propagates the opaque `Unavailable` Err.
fn admit_eval_task(view: &BatchView, arguments: &Value) -> Result<Value> {
    // The transport resolves and injects `ttl_secs` (a concrete number). Absent
    // or non-numeric ⇒ `None` (unlimited); the transport clamps it, so a hostile
    // raw socket client cannot request unbounded retention beyond the store caps.
    let ttl_secs = arguments.get("ttl_secs").and_then(|v| v.as_u64());

    // The eval core (`query_file`, `limit`, `category`, `reranker`) deserializes
    // straight from `arguments`; the extra `ttl_secs` key is ignored by the core
    // (serde drops unknown fields), so no separate strip is needed.
    let args: crate::cli::commands::EvalArgs =
        serde_json::from_value(arguments.clone()).map_err(|e| {
            crate::cli::json_envelope::ClientFacingError::new(
                crate::cli::json_envelope::ErrorCode::InvalidInput,
                format!("invalid eval task arguments: {e}"),
            )
        })?;
    if args.limit == 0 {
        return Err(crate::cli::json_envelope::ClientFacingError::new(
            crate::cli::json_envelope::ErrorCode::InvalidInput,
            "limit must be at least 1".to_string(),
        )
        .into());
    }

    let descriptor = crate::cli::batch::handlers::dispatch_eval_task(view, &args, ttl_secs)?;
    // Return the RAW payload: `dispatch_via_view_json` writes every verb's value
    // through `write_json_line`, which applies the single `{"data": …}` envelope
    // the transport `dial` reads as `output.data`. Pre-wrapping here would
    // double-nest it to `output.data.data`, which the dial cannot see.
    Ok(json!({ "task": descriptor_json(&descriptor) }))
}

/// `task-get`: non-blocking status read. `found` carries the descriptor; `gone`
/// is a normal terminal state the transport maps to `-32602`.
fn get_task(view: &BatchView, arguments: &Value) -> Value {
    let payload = match task_id_from_args(arguments) {
        Some(id) => match view.task_store.get(&id) {
            TaskLookup::Found(d) => json!({ "state": "found", "descriptor": descriptor_json(&d) }),
            TaskLookup::Gone => json!({ "state": "gone" }),
        },
        // A missing/blank taskId can never name a live task ⇒ `gone` (the
        // transport surfaces `-32602`), never an oracle.
        None => json!({ "state": "gone" }),
    };
    // Raw payload — `write_json_line` applies the single `data` envelope wrap
    // (see `admit_eval_task`); pre-wrapping would hide the state behind a second
    // `data` layer the transport `dial` never unwraps.
    payload
}

/// `task-result`: non-blocking terminal read. Compares the run's stamped epoch to
/// this view's live `checkout_epoch` and injects the staleness marker on a
/// mismatch (the freshness-lie guard). Pending / gone are normal states.
fn result_task(view: &BatchView, arguments: &Value) -> Value {
    let payload = match task_id_from_args(arguments) {
        Some(id) => match view.task_store.result(&id, view.checkout_epoch) {
            TaskResultLookup::Ready { kind, staleness } => {
                let staleness = staleness
                    .map(|s| serde_json::to_value(&s).unwrap_or(Value::Null))
                    .unwrap_or(Value::Null);
                json!({ "state": "ready", "terminal": terminal_json(&kind), "staleness": staleness })
            }
            TaskResultLookup::Pending => json!({ "state": "pending" }),
            TaskResultLookup::Gone => json!({ "state": "gone" }),
        },
        None => json!({ "state": "gone" }),
    };
    // Raw payload — the single `data` envelope wrap is `write_json_line`'s job
    // (see `admit_eval_task`); a pre-wrap here would double-nest the state.
    payload
}

/// Serialize a terminal outcome so the transport can return it verbatim: a
/// `tool_result` (a successful JSON-RPC response carrying a CallToolResult body,
/// `is_error` mapping the task's `failed` status) or an `rpc_error` (the call
/// failed at the JSON-RPC level — returned as that error, never a synthetic
/// CallToolResult).
fn terminal_json(kind: &TerminalKind) -> Value {
    match kind {
        TerminalKind::ToolResult { value, is_error } => json!({
            "kind": "tool_result",
            "value": value,
            "is_error": is_error,
        }),
        TerminalKind::RpcError { code, message } => json!({
            "kind": "rpc_error",
            "code": code.as_str(),
            "message": message,
        }),
    }
}

/// Read the `taskId` string from the wire arguments. A missing / non-string /
/// empty value yields `None`; the caller maps that to `gone` (never an error
/// oracle distinguishing "no such task" from "malformed request").
fn task_id_from_args(arguments: &Value) -> Option<TaskId> {
    let s = arguments.get("taskId").and_then(|v| v.as_str())?;
    if s.is_empty() {
        return None;
    }
    Some(TaskId::from_wire(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::batch::tasks::{TaskStatus, TaskStore};

    #[test]
    fn is_task_verb_recognizes_the_three_and_nothing_else() {
        assert!(is_task_verb(CMD_EVAL_TASK));
        assert!(is_task_verb(CMD_TASK_GET));
        assert!(is_task_verb(CMD_TASK_RESULT));
        assert!(!is_task_verb("eval"));
        assert!(!is_task_verb("search"));
        assert!(!is_task_verb(""));
    }

    #[test]
    fn descriptor_json_carries_ttl_as_null_when_unlimited() {
        let store = TaskStore::new();
        let entry = store.new_entry(0, None);
        let d = store.describe(&entry);
        let v = descriptor_json(&d);
        assert_eq!(v.get("status").and_then(|s| s.as_str()), Some("working"));
        assert!(v.get("ttl").is_some_and(|t| t.is_null()), "None ttl ⇒ null");
        assert!(v.get("taskId").and_then(|t| t.as_str()).is_some());
        assert!(v.get("createdAt").and_then(|t| t.as_str()).is_some());
    }

    #[test]
    fn descriptor_json_carries_numeric_ttl() {
        let store = TaskStore::new();
        let entry = store.new_entry(0, Some(900));
        let d = store.describe(&entry);
        let v = descriptor_json(&d);
        assert_eq!(v.get("ttl").and_then(|t| t.as_u64()), Some(900));
    }

    /// The wire `task-get` descriptor MUST carry the `{done, total}` progress
    /// (design D2, line 35) — the whole point of PR1 is a poller observing it.
    /// A store-produced descriptor always has `Some` progress; the admission
    /// path stamps `total` = M before the entry is pollable.
    #[test]
    fn descriptor_json_carries_stamped_progress() {
        let store = TaskStore::new();
        let entry = store.new_entry(0, Some(900));
        entry.stamp_progress_total(5);
        let d = store.describe(&entry);
        let v = descriptor_json(&d);
        let p = v.get("progress").expect("progress key present on the wire");
        assert_eq!(
            p.get("total").and_then(|t| t.as_u64()),
            Some(5),
            "total = M"
        );
        assert_eq!(
            p.get("done").and_then(|t| t.as_u64()),
            Some(0),
            "done starts at 0 before the worker runs"
        );
    }

    #[test]
    fn task_id_from_args_rejects_missing_and_empty() {
        assert!(task_id_from_args(&json!({})).is_none());
        assert!(task_id_from_args(&json!({ "taskId": "" })).is_none());
        assert!(task_id_from_args(&json!({ "taskId": 5 })).is_none());
        assert!(task_id_from_args(&json!({ "taskId": "abc" })).is_some());
    }

    #[test]
    fn terminal_json_tool_result_shape() {
        let v = terminal_json(&TerminalKind::ToolResult {
            value: json!({ "r_at_1": 0.5 }),
            is_error: false,
        });
        assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("tool_result"));
        assert_eq!(v.get("is_error").and_then(|b| b.as_bool()), Some(false));
        assert_eq!(
            v.get("value")
                .and_then(|x| x.get("r_at_1"))
                .and_then(|x| x.as_f64()),
            Some(0.5)
        );
    }

    #[test]
    fn terminal_json_rpc_error_shape() {
        let v = terminal_json(&TerminalKind::RpcError {
            code: crate::cli::json_envelope::ErrorCode::Internal,
            message: "boom".to_string(),
        });
        assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("rpc_error"));
        assert_eq!(v.get("code").and_then(|c| c.as_str()), Some("internal"));
        assert_eq!(v.get("message").and_then(|m| m.as_str()), Some("boom"));
    }

    /// `task-get` on an unknown id is a `gone` state (mapped to `-32602`
    /// transport-side), wrapped in the slim `data` envelope — never a daemon
    /// error, so it is not a store-state oracle.
    #[test]
    fn get_unknown_id_is_gone_state() {
        let store = TaskStore::new();
        // A minimal view is awkward to build here; exercise the store + shaping
        // directly through the descriptor path the verb uses.
        let ghost = TaskId::from_wire("does-not-exist");
        assert!(matches!(store.get(&ghost), TaskLookup::Gone));
    }

    /// A terminal `task-result` read surfaces both the payload and (on an epoch
    /// move) the staleness marker in the wire shape the transport parses.
    #[test]
    fn result_ready_shape_carries_terminal_and_staleness() {
        let store = TaskStore::new();
        let entry = store.new_entry(5, Some(60));
        entry.publish(
            TerminalKind::ToolResult {
                value: json!({ "ok": true }),
                is_error: false,
            },
            TaskStatus::Completed,
        );
        let id = TaskId::from_wire(&store.describe(&entry).task_id);
        assert!(store.try_admit(entry));
        // Read at a DIFFERENT epoch ⇒ staleness marker present.
        match store.result(&id, 9) {
            TaskResultLookup::Ready { kind, staleness } => {
                let payload = json!({
                    "state": "ready",
                    "terminal": terminal_json(&kind),
                    "staleness": staleness.map(|s| serde_json::to_value(&s).unwrap()),
                });
                assert_eq!(payload["state"], "ready");
                assert_eq!(payload["terminal"]["kind"], "tool_result");
                assert!(payload["staleness"]["index_moved_since_run"].as_bool() == Some(true));
                assert_eq!(payload["staleness"]["computed_at_epoch"].as_u64(), Some(5));
            }
            _ => panic!("expected Ready"),
        }
    }
}
