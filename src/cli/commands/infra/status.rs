//! `cqs status` — watch-mode freshness + daemon operational stats.
//!
//! Connects to the running `cqs watch --serve` daemon and reports the
//! latest [`WatchSnapshot`]. Designed for agent loops that want to gate
//! work on freshness — eval runners, ceremony commands, in-IDE pre-query
//! checks. The CLI sits next to `cqs ping`: same socket, same envelope,
//! different command name.
//!
//! Two report flags compose over the same snapshot query (#1715):
//!
//! - `--watch-fresh` — the freshness state machine (canonical since #1182).
//! - `--watch` — the daemon's operational stats block (in-flight clients,
//!   queue depth, dropped events, last-reindex latency, reconcile state,
//!   last error, per-slot freshness). Replaces journalctl grepping.
//!
//! `--wait` polls client-side until the snapshot reports `state == fresh`
//! or the `--wait-secs` budget expires. Polling on the client keeps the
//! daemon thread free — a server-side wait would pin a connection for the
//! whole budget, eating slots from the cap.
//!
//! [`WatchSnapshot`]: cqs::watch_status::WatchSnapshot

use anyhow::Result;

use crate::cli::find_project_root;

/// `cqs status (--watch-fresh|--watch) [--json] [--wait [--wait-secs N]]`.
///
/// Behaviour matrix:
///
/// | flags                      | exit | output                                           |
/// |----------------------------|------|--------------------------------------------------|
/// | (no flag)                  | 1    | error — must pass `--watch-fresh` or `--watch`   |
/// | `--watch-fresh`            | 0    | one-line text summary                            |
/// | `--watch`                  | 0    | text summary + operational stats block           |
/// | `--watch[-fresh] --json`   | 0    | full WatchSnapshot JSON (includes `ops`)         |
/// | `--wait` without a report flag | 1 | error — `--wait` requires `--watch-fresh`/`--watch` |
/// | `--watch[-fresh] --wait`   | 0/1  | text summary; exit 1 if budget expired           |
/// | `--watch[-fresh] --wait --json` | 0/1 | snapshot JSON; exit 1 if budget expired      |
///
/// No daemon → exit 1, friendly stderr message (parity with `cqs ping`).
/// Both flags share the same `daemon_status` query and the same no-daemon
/// error shape — `--watch` is additive output, not a second command.
pub(crate) fn cmd_status(
    json: bool,
    watch_fresh: bool,
    watch: bool,
    wait: bool,
    wait_secs: u64,
) -> Result<()> {
    let _span =
        tracing::info_span!("cmd_status", json, watch_fresh, watch, wait, wait_secs).entered();

    if !watch_fresh && !watch {
        // Gate the entire command on at least one explicit "what to report"
        // flag, and nudge the user toward the canonical combo
        // (`--watch-fresh --wait`) — agents and operators who hit the bare
        // `cqs status` probably want to wait for fresh, not poll once.
        let msg = "cqs status: hint: try --watch-fresh --wait (or --watch for daemon stats)";
        if json {
            crate::cli::json_envelope::emit_json_error(
                crate::cli::json_envelope::error_codes::INVALID_INPUT,
                msg,
            )?;
        } else {
            eprintln!("{msg}");
        }
        std::process::exit(1);
    }

    #[cfg(unix)]
    {
        let root = find_project_root();
        let cqs_dir = cqs::resolve_index_dir(&root);

        // Cap `--wait-secs` at 600 (10 min) so a runaway agent loop can't
        // pin the daemon socket forever.
        let budget_secs = wait_secs.min(600);

        // Without --wait, the user wants a single snapshot read — short-circuit
        // around `wait_for_fresh` so we don't pay a Stale → poll → Stale loop.
        if !wait {
            return match cqs::daemon_translate::daemon_status(&cqs_dir) {
                Ok(snap) => {
                    emit_snapshot(&snap, json, watch)?;
                    Ok(())
                }
                Err(err) => emit_no_daemon(&err.as_message(), json),
            };
        }

        // Shares the polling helper with `cqs eval --require-fresh`. The
        // status CLI translates outcomes to its `process::exit` paths;
        // eval translates to anyhow errors.
        match cqs::daemon_translate::wait_for_fresh(&cqs_dir, budget_secs) {
            cqs::daemon_translate::FreshnessWait::Fresh(snap) => {
                emit_snapshot(&snap, json, watch)?;
                Ok(())
            }
            cqs::daemon_translate::FreshnessWait::Timeout(snap) => {
                if json {
                    // Error envelope so JSON consumers see
                    // `error.code="timeout"` alongside the non-zero exit
                    // code. Embedding the snapshot in `data` keeps the
                    // counter information for callers that surface them.
                    let payload = serde_json::json!({
                        "snapshot": snap,
                        "wait_secs": budget_secs,
                    });
                    crate::cli::json_envelope::emit_json_error_with_data(
                        crate::cli::json_envelope::error_codes::TIMEOUT,
                        &format!("watch index still stale after {budget_secs}s"),
                        Some(payload),
                    )?;
                } else {
                    print_text(&snap, watch);
                    eprintln!("cqs: watch index still stale after {budget_secs}s wait");
                }
                // Budget expired before fresh — surface as exit 1
                // so scripts can distinguish "fresh" from "timed
                // out still stale".
                std::process::exit(1);
            }
            cqs::daemon_translate::FreshnessWait::NoDaemon(msg) => emit_no_daemon(&msg, json),
            // Transport / BadResponse fold into the same exit-1 path as
            // NoDaemon — the operator-side detail (which class fired) is
            // in the message verbatim.
            cqs::daemon_translate::FreshnessWait::Transport(msg)
            | cqs::daemon_translate::FreshnessWait::BadResponse(msg) => emit_no_daemon(&msg, json),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = (json, watch, wait, wait_secs);
        let _ = find_project_root;
        eprintln!("cqs: status is unix-only (daemon socket uses Unix domain sockets)");
        std::process::exit(1);
    }
}

#[cfg(unix)]
fn emit_snapshot(
    snap: &cqs::watch_status::WatchSnapshot,
    json: bool,
    show_ops: bool,
) -> Result<()> {
    if json {
        // The JSON shape is the full WatchSnapshot either way — the `ops`
        // block ships on every daemon publish since #1715, so `--watch`
        // and `--watch-fresh` consumers parse one schema.
        crate::cli::json_envelope::emit_json(snap)?;
    } else {
        print_text(snap, show_ops);
    }
    Ok(())
}

/// Surface a "no daemon" result as an error envelope (JSON) or stderr line
/// (text) and process-exit 1. Pulled out so the no-wait short-circuit and
/// the wait path render the same shape on transport failure.
#[cfg(unix)]
fn emit_no_daemon(msg: &str, json: bool) -> ! {
    if json {
        let _ = crate::cli::json_envelope::emit_json_error(
            crate::cli::json_envelope::error_codes::IO_ERROR,
            msg,
        );
    } else {
        eprintln!("cqs: {msg}");
    }
    std::process::exit(1);
}

#[cfg(unix)]
fn print_text(snap: &cqs::watch_status::WatchSnapshot, show_ops: bool) {
    // Single-line summary so a script can do
    //   `cqs status --watch-fresh | grep -q '^state: fresh'`
    // without parsing JSON. Counters that matter live on the second line.
    println!("state: {}", snap.state.as_str());
    println!(
        "modified_files={} pending_notes={} rebuild_in_flight={} dropped_this_cycle={} last_event_unix_secs={}",
        snap.modified_files,
        snap.pending_notes,
        snap.rebuild_in_flight,
        snap.dropped_this_cycle,
        snap.last_event_unix_secs,
    );
    if show_ops {
        print_ops_text(snap);
    }
}

/// Render the `--watch` operational block (#1715). Grep-friendly
/// `key=value` lines, same convention as the counters line above.
#[cfg(unix)]
fn print_ops_text(snap: &cqs::watch_status::WatchSnapshot) {
    let Some(ops) = snap.ops.as_ref() else {
        // Old daemon binary that predates the ops block — say so rather
        // than printing fake zeros.
        println!("ops: unavailable (daemon predates --watch; restart it on the current binary)");
        return;
    };
    match ops.last_reindex.as_ref() {
        Some(lr) => println!(
            "clients_in_flight={} reconcile_pending={} last_reindex_at={} last_reindex_ms={} last_reindex_files={}",
            ops.in_flight_clients, ops.reconcile_pending, lr.at_unix_secs, lr.duration_ms, lr.files,
        ),
        None => println!(
            "clients_in_flight={} reconcile_pending={} last_reindex=none",
            ops.in_flight_clients, ops.reconcile_pending,
        ),
    }
    match ops.last_error.as_ref() {
        Some(err) => println!(
            "last_error_at={} last_error={}",
            err.at_unix_secs, err.message
        ),
        None => println!("last_error=none"),
    }
    for slot in &ops.slots {
        println!(
            "slot={} state={} last_synced_at={}",
            slot.name,
            slot.state.as_str(),
            slot.last_synced_at
                .map(|t| t.to_string())
                .unwrap_or_else(|| "none".to_string()),
        );
    }
}

#[cfg(test)]
mod tests {
    use cqs::watch_status::{FreshnessState, WatchSnapshot};

    fn snap_with(state: FreshnessState) -> WatchSnapshot {
        let mut s = WatchSnapshot::unknown();
        s.state = state;
        s
    }

    #[test]
    fn fresh_snapshot_is_fresh() {
        let s = snap_with(FreshnessState::Fresh);
        assert!(s.is_fresh());
    }

    #[test]
    fn stale_snapshot_is_not_fresh() {
        let s = snap_with(FreshnessState::Stale);
        assert!(!s.is_fresh());
    }

    #[test]
    fn unknown_snapshot_is_not_fresh() {
        let s = snap_with(FreshnessState::Unknown);
        assert!(!s.is_fresh());
    }
}
