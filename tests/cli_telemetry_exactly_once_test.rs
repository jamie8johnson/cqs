//! Exactly-once telemetry contract under relay degradation.
//!
//! Every invocation must book ONE invoke event and ONE complete event — the two
//! may sit on different surfaces (a daemon-booked invoke paired with a cli
//! complete on a fallback), but never zero and never two of either.
//!
//! Two seam-audit finds are pinned here end-to-end against a mock `UnixListener`
//! bound at the exact socket path the CLI computes:
//!
//! - **Double-book on a post-write fallback.** When the daemon has already
//!   booked its `surface:"daemon"` invoke (it read the request) and then the
//!   CLI falls back to inline execution — read-budget expiry, oversized
//!   response — the CLI must NOT book a second `surface:"cli"` invoke. The mock
//!   writes the daemon-side invoke row itself (as the real daemon does,
//!   pre-dispatch) so the file faithfully reproduces the two-writer scenario.
//! - **Zero-book on a busy reject.** A retryable reject (daemon at its
//!   concurrency cap) is refused before the request is read, so it books no
//!   daemon invoke; the CLI must fall back and book exactly one `surface:"cli"`
//!   invoke rather than surfacing a hard error with no invoke at all.
//!
//! The file is `#![cfg(unix)]`-gated: it depends on Unix domain sockets.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

mod common;

use common::cqs_v1 as cqs;
use tempfile::TempDir;

/// How the mock daemon behaves on an accepted connection.
#[derive(Clone, Copy)]
enum MockMode {
    /// Book a daemon invoke row, then never respond — the client's read budget
    /// expires and it falls back (post-write; daemon already booked).
    HangAfterBooking,
    /// Book a daemon invoke row, then reply with a payload past the response cap
    /// — the client caps out and falls back (post-write; daemon already booked).
    OversizedAfterBooking,
    /// Reply immediately with a RETRYABLE busy reject and book NOTHING — the
    /// client falls back and books its own single cli invoke.
    BusyReject,
    /// Book a daemon invoke row and reply normally — the daemon handled it, so
    /// the client prints output and books no cli invoke.
    NormalOk,
}

/// Mock daemon that reproduces the relay-degradation surfaces. Unlike the
/// happy-path mock in `daemon_forward_test.rs`, this one can inject the
/// daemon-side invoke row (mimicking `view::dispatch_via_view`) so the
/// two-writer double-book scenario is faithfully reproduced.
struct RelayMockDaemon {
    stop: Arc<AtomicBool>,
    sock_path: PathBuf,
    handle: Option<JoinHandle<()>>,
}

impl RelayMockDaemon {
    fn spawn(
        sock_path: PathBuf,
        telemetry_path: PathBuf,
        wire_cmd: &'static str,
        mode: MockMode,
    ) -> Self {
        let listener = UnixListener::bind(&sock_path)
            .unwrap_or_else(|e| panic!("bind {} failed: {e}", sock_path.display()));
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on mock listener");

        let stop = Arc::new(AtomicBool::new(false));
        let s2 = Arc::clone(&stop);

        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !s2.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        handle_conn(&mut stream, &telemetry_path, wire_cmd, mode, &s2);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            stop,
            sock_path,
            handle: Some(handle),
        }
    }
}

impl Drop for RelayMockDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

/// Append a `surface:"daemon"` invoke row, exactly as the real daemon books it
/// pre-dispatch (`view::dispatch_via_view` → `telemetry::log_command`).
fn book_daemon_invoke(telemetry_path: &Path, wire_cmd: &str) {
    let row = format!(
        r#"{{"ts":1700000000,"cmd":"{wire_cmd}","query":null,"results":null,"surface":"daemon"}}"#
    );
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(telemetry_path)
        .expect("open telemetry file for daemon invoke row");
    writeln!(f, "{row}").expect("write daemon invoke row");
    f.flush().expect("flush daemon invoke row");
}

fn handle_conn(
    stream: &mut std::os::unix::net::UnixStream,
    telemetry_path: &Path,
    wire_cmd: &str,
    mode: MockMode,
    stop: &AtomicBool,
) {
    match mode {
        MockMode::HangAfterBooking => {
            // Drain the request (the daemon reads it before booking).
            let mut buf = String::new();
            let _ = BufReader::new(&*stream).read_line(&mut buf);
            book_daemon_invoke(telemetry_path, wire_cmd);
            // Never respond — hold the connection until the test tears us down
            // so the client's read budget expires and it falls back.
            let hang_deadline = Instant::now() + Duration::from_secs(10);
            while !stop.load(Ordering::SeqCst) && Instant::now() < hang_deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        MockMode::OversizedAfterBooking => {
            let mut buf = String::new();
            let _ = BufReader::new(&*stream).read_line(&mut buf);
            book_daemon_invoke(telemetry_path, wire_cmd);
            // Reply with a payload past the (test-lowered) response cap.
            let big = "x".repeat(4096);
            let _ = writeln!(stream, r#"{{"status":"ok","output":"{big}"}}"#);
            let _ = stream.flush();
        }
        MockMode::BusyReject => {
            // Real daemon refuses at accept-time before reading the request and
            // books nothing. Reply with the retryable marker immediately.
            let _ = writeln!(
                stream,
                r#"{{"status":"error","message":"daemon busy (too many concurrent clients)","retryable":true}}"#
            );
            let _ = stream.flush();
        }
        MockMode::NormalOk => {
            let mut buf = String::new();
            let _ = BufReader::new(&*stream).read_line(&mut buf);
            book_daemon_invoke(telemetry_path, wire_cmd);
            let _ = writeln!(stream, r#"{{"status":"ok","output":"{{\"data\":[]}}"}}"#);
            let _ = stream.flush();
        }
    }
}

/// Build a temp project with an activated telemetry file and the socket path
/// the CLI will connect to.
fn setup() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    // Pre-create telemetry.jsonl so telemetry is active for the spawned CLI.
    let telemetry_path = cqs_dir.join("telemetry.jsonl");
    std::fs::write(&telemetry_path, "").expect("seed telemetry.jsonl");

    let cqs_dir_canonical = dunce::canonicalize(&cqs_dir).expect("canonicalize cqs_dir");
    let sock_path = daemon_socket_path_with_runtime_dir(&cqs_dir_canonical, dir.path());
    (dir, sock_path, telemetry_path)
}

/// Byte-equivalent to `cqs::daemon_translate::daemon_socket_path` with an
/// explicit runtime-dir override (blake3 of the canonical `.cqs` path, first 8
/// bytes, hex). Mirrors the helper in `daemon_forward_test.rs`.
fn daemon_socket_path_with_runtime_dir(cqs_dir: &Path, runtime_dir: &Path) -> PathBuf {
    let hash = blake3::hash(cqs_dir.as_os_str().as_encoded_bytes());
    let truncated = &hash.as_bytes()[..8];
    let mut hex = String::with_capacity(16);
    for b in truncated {
        use std::fmt::Write as _;
        let _ = write!(hex, "{:02x}", b);
    }
    runtime_dir.join(format!("cqs-{hex}.sock"))
}

/// (invoke_rows, complete_rows) parsed from a telemetry file. An invoke row has
/// no `event` field; a complete row is `event == "complete"`.
fn split_rows(telemetry_path: &Path) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let body = std::fs::read_to_string(telemetry_path).expect("read telemetry file");
    let mut invokes = Vec::new();
    let mut completes = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("bad telemetry line {line:?}: {e}"));
        match v.get("event").and_then(|e| e.as_str()) {
            None => invokes.push(v),
            Some("complete") => completes.push(v),
            Some(_) => {}
        }
    }
    (invokes, completes)
}

/// Run `cqs impact <arg> --json` against a mock in `mode`, with telemetry on and
/// the daemon read budget lowered so a hang falls back quickly.
fn run_against_mock(mode: MockMode) -> (TempDir, RelayMockDaemon, PathBuf) {
    let (dir, sock_path, telemetry_path) = setup();
    let mock = RelayMockDaemon::spawn(sock_path.clone(), telemetry_path.clone(), "impact", mode);

    let canonical_dir = dunce::canonicalize(dir.path()).expect("canonicalize cwd");
    let mut cmd = cqs();
    cmd.env_remove("CQS_NO_DAEMON");
    cmd.env("RUST_LOG", "warn");
    cmd.env("CQS_TELEMETRY", "1");
    cmd.env("XDG_RUNTIME_DIR", dir.path());
    // Lowered read budget so the hang falls back in well under a second.
    cmd.env("CQS_DAEMON_TIMEOUT_MS", "250");
    // Lowered response cap so the oversized reply trips the fallback.
    cmd.env("CQS_DAEMON_MAX_RESPONSE_BYTES", "512");
    cmd.current_dir(&canonical_dir)
        .args(["impact", "some_symbol", "--json"]);
    let _ = cmd.output().expect("cqs impact spawn");

    (dir, mock, telemetry_path)
}

#[test]
fn read_timeout_fallback_books_exactly_one_invoke_daemon_surface() {
    // Repro: a daemon-booked invoke + a read-budget expiry must NOT produce a
    // second cli invoke. Before the fix this wrote two invoke rows.
    let (_dir, _mock, telemetry_path) = run_against_mock(MockMode::HangAfterBooking);
    let (invokes, completes) = split_rows(&telemetry_path);
    assert_eq!(
        invokes.len(),
        1,
        "post-write timeout fallback must book exactly one invoke, got {invokes:?}"
    );
    assert_eq!(
        invokes[0]["surface"], "daemon",
        "the surviving invoke is the daemon's pre-dispatch booking"
    );
    assert_eq!(
        completes.len(),
        1,
        "exactly one complete event pairs the invoke, got {completes:?}"
    );
}

#[test]
fn cap_exceeded_fallback_books_exactly_one_invoke_daemon_surface() {
    // Same double-book class via the oversized-response fallback: the daemon
    // dispatched (and booked) before the client capped out.
    let (_dir, _mock, telemetry_path) = run_against_mock(MockMode::OversizedAfterBooking);
    let (invokes, completes) = split_rows(&telemetry_path);
    assert_eq!(
        invokes.len(),
        1,
        "cap-exceeded fallback must book exactly one invoke, got {invokes:?}"
    );
    assert_eq!(invokes[0]["surface"], "daemon");
    assert_eq!(completes.len(), 1);
}

#[test]
fn busy_reject_books_exactly_one_invoke_cli_surface() {
    // Zero-book mirror: a retryable reject books nothing daemon-side, so the CLI
    // must fall back and book its own single cli invoke (not zero).
    let (_dir, _mock, telemetry_path) = run_against_mock(MockMode::BusyReject);
    let (invokes, completes) = split_rows(&telemetry_path);
    assert_eq!(
        invokes.len(),
        1,
        "busy reject must book exactly one invoke (cli), got {invokes:?}"
    );
    assert_eq!(
        invokes[0]["surface"], "cli",
        "the busy-reject invoke is booked CLI-side after fallback"
    );
    assert_eq!(completes.len(), 1);
}

#[test]
fn normal_forward_books_no_extra_cli_invoke() {
    // A daemon-handled request keeps the single daemon invoke; the CLI adds no
    // cli invoke because it returned before the inline chokepoint.
    let (_dir, _mock, telemetry_path) = run_against_mock(MockMode::NormalOk);
    let (invokes, completes) = split_rows(&telemetry_path);
    assert_eq!(
        invokes.len(),
        1,
        "normal daemon forward books exactly one (daemon) invoke, got {invokes:?}"
    );
    assert_eq!(invokes[0]["surface"], "daemon");
    assert_eq!(completes.len(), 1);
}

#[test]
fn no_daemon_inline_books_exactly_one_cli_invoke() {
    // Control: with the daemon path disabled, a normal inline run books exactly
    // one cli invoke and one complete — the never-forwarded baseline.
    let (dir, _sock_path, telemetry_path) = setup();
    let canonical_dir = dunce::canonicalize(dir.path()).expect("canonicalize cwd");
    let mut cmd = cqs();
    cmd.env("CQS_NO_DAEMON", "1");
    cmd.env("RUST_LOG", "warn");
    cmd.env("CQS_TELEMETRY", "1");
    cmd.current_dir(&canonical_dir)
        .args(["impact", "some_symbol", "--json"]);
    let _ = cmd.output().expect("cqs impact spawn");

    let (invokes, completes) = split_rows(&telemetry_path);
    assert_eq!(
        invokes.len(),
        1,
        "inline run books exactly one invoke, got {invokes:?}"
    );
    assert_eq!(invokes[0]["surface"], "cli");
    assert_eq!(completes.len(), 1);
}
