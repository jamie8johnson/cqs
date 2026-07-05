//! The eval task worker (MCP Phase 3b floor, lane 1).
//!
//! Shaped after the `spawn_hnsw_rebuild` background-thread template (retained
//! `JoinHandle` semantics, a dropped-client-never-panics posture) with three
//! inversions the task tenant needs:
//!
//! 1. **Delivery is publish-into-`Arc<TaskEntry>`**, not an mpsc a handler
//!    `recv`s — so a `tasks/result` reader never blocks the daemon handler pool.
//! 2. **The single-flight guard rides INTO the worker** (moved by the admission
//!    handler) and releases on the worker's drop, so a task eval and a direct
//!    eval can never double-book the GPU.
//! 3. **A publish-on-drop handle** guarantees a worker that dies WITHOUT an
//!    explicit terminal publish — a `thread::Builder::spawn` failure after the
//!    entry was inserted, an early return, an unwind — self-heals the entry to
//!    `failed` instead of wedging it at `working` forever.
//!
//! The thread name carries a NON-SECRET monotonic job id, never the `taskId`:
//! the id is a capability handle and would otherwise leak into
//! `/proc/<pid>/task/<tid>/comm`, `ps -T`, and crash traces.
//!
//! Panic reality: the release profile is `panic = "abort"`, so a worker panic
//! aborts the whole process (systemd restarts it; the in-memory store and every
//! id are lost) rather than being contained — the same failure mode as the
//! synchronous eval dispatch. The safety argument is therefore the panic-token
//! fence over this file (it carries no bare panic source), NOT drop-on-unwind.
//! The publish-on-drop handle still covers the reachable non-panic exits: a
//! spawn failure, an early return, and unwind under the test profile.

use std::sync::Arc;

use crate::cli::batch::BatchView;
use crate::cli::commands::{eval_core, EvalCoreParams};

use super::{TaskEntry, TaskStatus, TerminalKind};

/// The worker's terminal-publication handle. Owned by the worker closure and
/// moved into it at spawn time, so its `Drop` runs whether the worker completes
/// normally, returns early, unwinds, OR never starts (the closure dropped by a
/// failed spawn). A normal publish flips `done`, making the drop a no-op; any
/// other exit path publishes `failed`.
pub(in crate::cli::batch) struct TerminalPublisher {
    entry: Arc<TaskEntry>,
    done: bool,
}

impl TerminalPublisher {
    fn new(entry: Arc<TaskEntry>) -> TerminalPublisher {
        TerminalPublisher { entry, done: false }
    }

    fn publish(&mut self, kind: TerminalKind, status: TaskStatus) {
        self.entry.publish(kind, status);
        self.done = true;
    }
}

impl Drop for TerminalPublisher {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // The worker exited without an explicit terminal publish (spawn-fail,
        // early return, unwind). Self-heal to `failed` so the entry is never a
        // permanent `working` zombie holding a cap slot. `publish` is write-once,
        // so this is a no-op if an explicit publish somehow already landed.
        self.entry.publish(
            TerminalKind::RpcError {
                code: crate::cli::json_envelope::ErrorCode::Internal,
                message: "eval task did not complete".to_string(),
            },
            TaskStatus::Failed,
        );
    }
}

/// Non-secret worker thread name — derived ONLY from the monotonic job counter,
/// never the `taskId`.
fn worker_thread_name(job_seq: u64) -> String {
    format!("cqs-task-eval-{job_seq}")
}

/// Spawn the eval task worker. `guard` is the single-flight token the admission
/// handler won; it is generic so this module never names the handler-side type,
/// and it is held (moved into the closure) purely so its `Drop` releases the
/// slot when the worker exits.
///
/// Ordering contract (the caller must honor it): the `TaskEntry` is already
/// inserted into the store when this is called, so a spawn failure here drops
/// the closure — and with it the `TerminalPublisher` — which marks the inserted
/// entry `failed`. The publisher is built OUTSIDE the closure and
/// moved in for exactly this reason: were it built inside, a failed spawn (the
/// closure never runs) would leave the entry a `working` zombie.
pub(in crate::cli::batch) fn spawn_eval_worker<G: Send + 'static>(
    view: BatchView,
    set: cqs::eval::schema::QuerySet,
    params: EvalCoreParams,
    entry: Arc<TaskEntry>,
    guard: G,
    job_seq: u64,
) {
    let publisher = TerminalPublisher::new(entry);
    // Span carries the non-secret job id ONLY — no `taskId` field.
    let span = tracing::info_span!("task_eval_worker", job_seq);
    let spawn_result = std::thread::Builder::new()
        .name(worker_thread_name(job_seq))
        .spawn(move || {
            let _enter = span.entered();
            // The guard rides in and releases the single-flight slot on drop.
            let _guard = guard;
            let mut publisher = publisher;
            match eval_core(&view, &set, &params) {
                Ok(output) => {
                    let value = serde_json::to_value(&output).unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "eval task: serialize EvalOutput failed");
                        serde_json::Value::Null
                    });
                    // A successful eval maps to `completed`. `eval_core` never
                    // emits a tool-level `isError` (its `Err` path becomes the
                    // RpcError arm below), so `is_error` is false here.
                    publisher.publish(
                        TerminalKind::ToolResult {
                            value,
                            is_error: false,
                        },
                        TaskStatus::Completed,
                    );
                }
                Err(e) => {
                    // Failed-task JSON-RPC-error passthrough: redact to the same
                    // opaque `(code, message)` the synchronous dispatch surfaces,
                    // so `tasks/result` returns that JSON-RPC error verbatim.
                    let (code, message) = crate::cli::json_envelope::redact_error(&e);
                    publisher.publish(TerminalKind::RpcError { code, message }, TaskStatus::Failed);
                }
            }
        });
    if let Err(e) = spawn_result {
        // Rare (resource exhaustion). The closure never ran, so it was dropped
        // here — and with it the moved-in publisher, which just marked the
        // already-inserted entry `failed`. Nothing further to do; log and move
        // on. The guard also released via the dropped closure.
        tracing::warn!(
            error = %e,
            job_seq,
            "eval task worker spawn failed; entry self-heals to failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::batch::tasks::{TaskId, TaskStore};

    /// The worker thread name is derived from the non-secret job counter and
    /// NEVER embeds the `taskId` — structurally impossible, since
    /// the function only takes the seq.
    #[test]
    fn thread_name_never_contains_task_id() {
        // A representative crypto-secure id (43 URL-safe base64 chars).
        let secret = TaskId::from_wire("aGVsbG8td29ybGQtc2VjcmV0LXRhc2staWQtZXhhbXBsZS0x");
        let name = worker_thread_name(42);
        assert_eq!(name, "cqs-task-eval-42");
        assert!(
            !name.contains(secret.as_str()),
            "thread name must never embed the taskId capability handle"
        );
    }

    /// A worker that drops its publisher WITHOUT an explicit publish self-heals
    /// the entry to `failed` — the spawn-failure / early-return / unwind path,
    /// so an inserted entry can never wedge at `working`.
    #[test]
    fn publish_on_drop_marks_failed() {
        let store = TaskStore::new();
        let entry = store.new_entry(0, Some(60));
        let publisher = TerminalPublisher::new(Arc::clone(&entry));
        drop(publisher);
        assert_eq!(
            entry.status(),
            TaskStatus::Failed,
            "an unpublished worker exit must leave the entry `failed`"
        );
    }

    /// An explicit publish flips `done`, so the drop is a no-op and does NOT
    /// overwrite a `completed` result with `failed`.
    #[test]
    fn explicit_publish_suppresses_drop_failed() {
        let store = TaskStore::new();
        let entry = store.new_entry(0, Some(60));
        {
            let mut publisher = TerminalPublisher::new(Arc::clone(&entry));
            publisher.publish(
                TerminalKind::ToolResult {
                    value: serde_json::json!({"ok": true}),
                    is_error: false,
                },
                TaskStatus::Completed,
            );
        } // publisher dropped here — must NOT overwrite to failed.
        assert_eq!(
            entry.status(),
            TaskStatus::Completed,
            "a completed result must survive the publisher drop"
        );
    }
}
