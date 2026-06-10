//! `cqs status` — watch-mode freshness + daemon operational stats.
//!
//! Connects to the running `cqs watch --serve` daemon and reports the
//! latest [`WatchSnapshot`]. Designed for agent loops that want to gate
//! work on freshness — eval runners, ceremony commands, in-IDE pre-query
//! checks. The CLI sits next to `cqs ping`: same socket, same envelope,
//! different command name.
//!
//! Two report flags compose over the same snapshot query:
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
///
/// `slot` (the global `--slot` flag) scopes the report to one entry of
/// the per-slot vec: the `state:` line reflects that slot's freshness,
/// and `--wait` polls until THAT slot is fresh — so an eval harness can
/// gate on the exact slot it's about to measure while the daemon keeps
/// propagating to it in the background.
pub(crate) fn cmd_status(
    json: bool,
    watch_fresh: bool,
    watch: bool,
    wait: bool,
    wait_secs: u64,
    slot: Option<&str>,
) -> Result<()> {
    let _span = tracing::info_span!(
        "cmd_status",
        json,
        watch_fresh,
        watch,
        wait,
        wait_secs,
        slot
    )
    .entered();

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

        // Slot-scoped report: filter the per-slot vec and gate on the
        // named slot's freshness instead of the global state machine.
        // Handled before the global paths because the wait semantics
        // differ (client-side poll on the slot entry).
        if let Some(slot_name) = slot {
            return cmd_status_slot_scoped(&cqs_dir, slot_name, json, watch, wait, budget_secs);
        }

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
        let _ = (json, watch, wait, wait_secs, slot);
        let _ = find_project_root;
        eprintln!("cqs: status is unix-only (daemon socket uses Unix domain sockets)");
        std::process::exit(1);
    }
}

/// Locate the named slot in a snapshot's per-slot vec (active slot
/// first, sibling slots after). Pure helper so the lookup contract is
/// unit-testable without a daemon.
#[cfg(any(unix, test))]
fn slot_entry<'a>(
    snap: &'a cqs::watch_status::WatchSnapshot,
    name: &str,
) -> Option<&'a cqs::watch_status::SlotWatchStatus> {
    snap.ops
        .as_ref()
        .and_then(|ops| ops.slots.iter().find(|s| s.name == name))
}

/// Slot-scoped `cqs status --slot X (--watch-fresh|--watch)`.
///
/// Single-shot: report the named slot's entry; exit 1 when the daemon
/// doesn't track it (not in the per-slot vec — wrong name, or the
/// daemon predates slot propagation). `--wait`: client-side poll until
/// the slot's state is `fresh` or the budget expires (the daemon's
/// `wait_fresh` notifier is global-state only, so slot waits poll the
/// snapshot — same cadence the global fallback poller uses).
#[cfg(unix)]
fn cmd_status_slot_scoped(
    cqs_dir: &std::path::Path,
    slot_name: &str,
    json: bool,
    show_ops: bool,
    wait: bool,
    budget_secs: u64,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_status_slot_scoped", slot = slot_name, wait).entered();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget_secs);
    loop {
        let snap = match cqs::daemon_translate::daemon_status(cqs_dir) {
            Ok(s) => s,
            Err(err) => emit_no_daemon(&err.as_message(), json),
        };
        let Some(entry) = slot_entry(&snap, slot_name) else {
            let msg = format!(
                "slot {slot_name:?} is not tracked by the daemon (known slots: {})",
                snap.ops
                    .as_ref()
                    .map(|o| o
                        .slots
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_else(|| "<none>".to_string())
            );
            emit_no_daemon(&msg, json);
        };
        let fresh = entry.state == cqs::watch_status::FreshnessState::Fresh;
        if !wait || fresh {
            // Single-shot (exit 0 with the state in the output, same
            // contract as the global `--watch-fresh` single-shot), or
            // wait satisfied.
            if json {
                crate::cli::json_envelope::emit_json(entry)?;
            } else {
                print_slot_text(entry, show_ops);
            }
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            if json {
                let payload = serde_json::json!({
                    "slot": entry,
                    "wait_secs": budget_secs,
                });
                crate::cli::json_envelope::emit_json_error_with_data(
                    crate::cli::json_envelope::error_codes::TIMEOUT,
                    &format!(
                        "slot {slot_name:?} still {} after {budget_secs}s",
                        entry.state
                    ),
                    Some(payload),
                )?;
            } else {
                print_slot_text(entry, show_ops);
                eprintln!("cqs: slot {slot_name:?} still stale after {budget_secs}s wait");
            }
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Text rendering for one slot entry. Mirrors the global `state:` line
/// shape so existing `grep -q '^state: fresh'` gates work unchanged
/// with `--slot`.
#[cfg(unix)]
fn print_slot_text(entry: &cqs::watch_status::SlotWatchStatus, show_ops: bool) {
    println!("state: {}", entry.state.as_str());
    println!(
        "slot={} queue_depth={} last_synced_at={}",
        entry.name,
        entry.queue_depth,
        entry
            .last_synced_at
            .map(|t| t.to_string())
            .unwrap_or_else(|| "none".to_string()),
    );
    if show_ops {
        match entry.last_reindex.as_ref() {
            Some(lr) => println!(
                "last_reindex_at={} last_reindex_ms={} last_reindex_files={}",
                lr.at_unix_secs, lr.duration_ms, lr.files
            ),
            None => println!("last_reindex=none"),
        }
        match entry.last_error.as_ref() {
            Some(err) => println!(
                "last_error_at={} last_error={}",
                err.at_unix_secs, err.message
            ),
            None => println!("last_error=none"),
        }
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
        // block ships on every daemon publish since the ops block shipped, so `--watch`
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

/// Render the `--watch` operational block. Grep-friendly
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
            "slot={} state={} queue_depth={} last_synced_at={} last_error={}",
            slot.name,
            slot.state.as_str(),
            slot.queue_depth,
            slot.last_synced_at
                .map(|t| t.to_string())
                .unwrap_or_else(|| "none".to_string()),
            slot.last_error
                .as_ref()
                .map(|e| e.message.as_str())
                .unwrap_or("none"),
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

    /// `--slot X` lookup: finds active and sibling entries by name,
    /// returns None for unknown slots or snapshots without an ops block
    /// (old daemon binary).
    #[test]
    fn slot_entry_finds_by_name_across_active_and_siblings() {
        use cqs::watch_status::{SlotWatchStatus, WatchOpsStats};

        let mut snap = WatchSnapshot::unknown();
        assert!(
            super::slot_entry(&snap, "default").is_none(),
            "no ops block → no slot entries"
        );

        let mk = |name: &str, state: FreshnessState| SlotWatchStatus {
            name: name.to_string(),
            state,
            last_synced_at: None,
            last_reindex: None,
            queue_depth: 0,
            last_error: None,
        };
        snap.ops = Some(WatchOpsStats {
            slots: vec![
                mk("default", FreshnessState::Fresh),
                mk("exp-a", FreshnessState::Stale),
            ],
            ..Default::default()
        });

        assert_eq!(
            super::slot_entry(&snap, "default").map(|e| e.state),
            Some(FreshnessState::Fresh)
        );
        assert_eq!(
            super::slot_entry(&snap, "exp-a").map(|e| e.state),
            Some(FreshnessState::Stale)
        );
        assert!(super::slot_entry(&snap, "nope").is_none());
    }
}
