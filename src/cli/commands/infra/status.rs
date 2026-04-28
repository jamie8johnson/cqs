//! `cqs status` — watch-mode freshness query. (#1182 — Layer 3)
//!
//! Connects to the running `cqs watch --serve` daemon and reports the
//! latest [`WatchSnapshot`]. Designed for agent loops that want to gate
//! work on freshness — eval runners, ceremony commands, in-IDE pre-query
//! checks. The CLI sits next to `cqs ping`: same socket, same envelope,
//! different command name.
//!
//! `--watch-fresh` is the canonical flag (today the only mode). Future
//! sibling flags (`--last-error`, `--clients`) can land without breaking
//! agents already pinning `--watch-fresh`.
//!
//! `--wait` polls client-side until the snapshot reports `state == fresh`
//! or the `--wait-secs` budget expires. Polling on the client keeps the
//! daemon thread free — a server-side wait would pin a connection for the
//! whole budget, eating slots from the cap.
//!
//! [`WatchSnapshot`]: cqs::watch_status::WatchSnapshot

use anyhow::Result;

use crate::cli::find_project_root;

/// `cqs status --watch-fresh [--json] [--wait [--wait-secs N]]`.
///
/// Behaviour matrix:
///
/// | flags                      | exit | output                                           |
/// |----------------------------|------|--------------------------------------------------|
/// | (no flag)                  | 1    | error — must pass `--watch-fresh`                |
/// | `--watch-fresh`            | 0    | one-line text summary                            |
/// | `--watch-fresh --json`     | 0    | full WatchSnapshot JSON                          |
/// | `--wait` without `--watch-fresh` | 1 | error — `--wait` requires `--watch-fresh`     |
/// | `--watch-fresh --wait`     | 0/1  | text summary; exit 1 if budget expired           |
/// | `--watch-fresh --wait --json` | 0/1 | snapshot JSON; exit 1 if budget expired       |
///
/// No daemon → exit 1, friendly stderr message (parity with `cqs ping`).
pub(crate) fn cmd_status(json: bool, watch_fresh: bool, wait: bool, wait_secs: u64) -> Result<()> {
    let _span = tracing::info_span!("cmd_status", json, watch_fresh, wait, wait_secs).entered();

    if !watch_fresh {
        // Reserve room for future sibling modes by gating the entire
        // command on at least one explicit "what to report" flag.
        let msg = "cqs status: pass --watch-fresh to query daemon freshness";
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget_secs);
        // 250 ms keeps the polling latency well under a typical user's
        // patience for "is it done yet?" without hammering the daemon.
        let poll_interval = std::time::Duration::from_millis(250);

        loop {
            match cqs::daemon_translate::daemon_status(&cqs_dir) {
                Ok(snap) => {
                    if !wait || snap.is_fresh() {
                        emit_snapshot(&snap, json)?;
                        // Exit 1 if `--wait` ran out without reaching fresh.
                        if wait && !snap.is_fresh() {
                            std::process::exit(1);
                        }
                        return Ok(());
                    }
                    if std::time::Instant::now() >= deadline {
                        emit_snapshot(&snap, json)?;
                        // Budget expired before fresh — surface as exit 1
                        // so scripts can distinguish "fresh" from "timed
                        // out still stale".
                        std::process::exit(1);
                    }
                    std::thread::sleep(poll_interval);
                }
                Err(msg) => {
                    if json {
                        crate::cli::json_envelope::emit_json_error(
                            crate::cli::json_envelope::error_codes::IO_ERROR,
                            &msg,
                        )?;
                    } else {
                        eprintln!("cqs: {msg}");
                    }
                    std::process::exit(1);
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = (json, wait, wait_secs);
        let _ = find_project_root;
        eprintln!("cqs: status is unix-only (daemon socket uses Unix domain sockets)");
        std::process::exit(1);
    }
}

#[cfg(unix)]
fn emit_snapshot(snap: &cqs::watch_status::WatchSnapshot, json: bool) -> Result<()> {
    if json {
        crate::cli::json_envelope::emit_json(snap)?;
    } else {
        print_text(snap);
    }
    Ok(())
}

#[cfg(unix)]
fn print_text(snap: &cqs::watch_status::WatchSnapshot) {
    // Single-line summary so a script can do
    //   `cqs status --watch-fresh | grep -q '^state: fresh'`
    // without parsing JSON. Counters that matter live on the second line.
    println!("state: {}", snap.state.as_str());
    println!(
        "modified_files={} pending_notes={} rebuild_in_flight={} dropped_this_cycle={} idle_secs={}",
        snap.modified_files,
        snap.pending_notes,
        snap.rebuild_in_flight,
        snap.dropped_this_cycle,
        snap.idle_secs,
    );
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
