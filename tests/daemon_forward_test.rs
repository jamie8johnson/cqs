//! #972: pin the behaviour of the daemon-forward path in
//! `src/cli/dispatch.rs::try_daemon_query`.
//!
//! Two concerns:
//!
//! 1. **Pure arg-translation** — `cqs::daemon_translate::translate_cli_args_to_batch`
//!    is the extracted helper. Black-box tests here pin the stripping and
//!    `-n` → `--limit` remap so a future edit to the helper doesn't silently
//!    ship a different wire format to the daemon.
//!
//! 2. **Daemon bypass + forwarding** — exercised end-to-end by spawning the
//!    real `cqs` binary against a mock `UnixListener` bound at the exact
//!    socket path the CLI computes. `notes add` must *not* touch the socket
//!    (PR #945 structurally locked in via `Commands::batch_support`);
//!    `notes list` must round-trip through it.
//!
//! The file is `#![cfg(unix)]`-gated at top level — it must not even compile
//! on Windows, because Unix domain sockets aren't a thing there.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use tempfile::TempDir;

// ─────────────────────────────────────────────────────────────────────────────
// Pure-function tests for `translate_cli_args_to_batch`.
//
// These are black-box tests of the library helper. They do NOT spawn the
// binary or touch sockets, so they run in microseconds and never flake.
// ─────────────────────────────────────────────────────────────────────────────

fn v(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_translate_strips_global_json_flag() {
    // `--json` is a top-level `Cli` flag, not a subcommand flag; the batch
    // handler always emits JSON. The translator drops it entirely.
    let (cmd, args) =
        cqs::daemon_translate::translate_cli_args_to_batch(&v(&["impact", "foo", "--json"]), true);
    assert_eq!(cmd, "impact");
    assert!(
        !args.iter().any(|a| a == "--json"),
        "--json must be stripped; got {args:?}"
    );

    // Bare-query form: `cqs --json "hello"` also drops --json.
    let (cmd, args) =
        cqs::daemon_translate::translate_cli_args_to_batch(&v(&["--json", "hello"]), false);
    assert_eq!(cmd, "search");
    assert_eq!(args, v(&["hello"]));
}

#[test]
fn test_translate_remaps_n_to_limit() {
    // Spaced form: `-n 5` → `--limit 5` (two tokens).
    let (cmd, args) =
        cqs::daemon_translate::translate_cli_args_to_batch(&v(&["impact", "foo", "-n", "5"]), true);
    assert_eq!(cmd, "impact");
    assert_eq!(args, v(&["foo", "--limit", "5"]));

    // Equals form: `-n=5` → `--limit=5` (one token).
    let (cmd, args) =
        cqs::daemon_translate::translate_cli_args_to_batch(&v(&["impact", "foo", "-n=5"]), true);
    assert_eq!(cmd, "impact");
    assert_eq!(args, v(&["foo", "--limit=5"]));

    // Already-canonical `--limit` is preserved verbatim (with a remap through
    // the same branch — no double-insertion).
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["impact", "foo", "--limit", "7"]),
        true,
    );
    assert_eq!(cmd, "impact");
    assert_eq!(args, v(&["foo", "--limit", "7"]));
}

#[test]
fn test_translate_prepends_search_for_bare_query() {
    // `cqs "hello world"` → the caller passes `has_subcommand = false`; the
    // translator synthesises `search` as the subcommand.
    let (cmd, args) =
        cqs::daemon_translate::translate_cli_args_to_batch(&v(&["hello world"]), false);
    assert_eq!(cmd, "search");
    assert_eq!(args, v(&["hello world"]));

    // Multi-token bare query: `cqs "alpha" "beta" --quiet`. The --quiet is
    // stripped; the two positional tokens remain as the query args.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["alpha", "beta", "--quiet"]),
        false,
    );
    assert_eq!(cmd, "search");
    assert_eq!(args, v(&["alpha", "beta"]));
}

#[test]
fn test_translate_preserves_subcommand_flags() {
    // With an explicit subcommand, the subcommand name stays first and its
    // flags (those we don't strip as global) pass through untouched. Here
    // `--threshold 0.5` is subcommand-scoped for `impact` and must reach
    // the daemon unchanged; `--json` is global and gets dropped.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["impact", "foo", "--threshold", "0.5", "--json"]),
        true,
    );
    assert_eq!(cmd, "impact");
    assert_eq!(args, v(&["foo", "--threshold", "0.5"]));

    // Another spot-check: subcommand-level `-n` still gets remapped to
    // `--limit`, so the batch clap parser sees the canonical long form.
    // This is the one case where "subcommand flag" is rewritten on purpose
    // — same flag, canonical name. Not a behaviour change vs. pre-#972.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["similar", "bar", "-n", "3"]),
        true,
    );
    assert_eq!(cmd, "similar");
    assert_eq!(args, v(&["bar", "--limit", "3"]));
}

// ─────────────────────────────────────────────────────────────────────────────
// Socket-mock tests.
//
// These spawn the real `cqs` binary via `assert_cmd` against a mock
// `UnixListener` bound at the exact path `daemon_socket_path(cqs_dir)`
// computes. `XDG_RUNTIME_DIR` is overridden per test so the socket lives in
// the temp dir (keeps tests isolated from any real running `cqs watch`).
//
// `notes add --no-reindex` is intentionally chosen for the bypass test: it
// doesn't need an open `Store`, so the CLI fallback succeeds even without a
// built index. If the bypass ever regresses, the command would hit the mock
// and we'd observe a non-zero `conn_count`.
//
// These tests do spawn the binary, so they're a bit slower than the pure
// tests above — but they don't cold-load any ML model, so they stay well
// under a second each.
// ─────────────────────────────────────────────────────────────────────────────

/// Mock socket fixture: binds a `UnixListener` at `sock_path`, spawns a
/// background thread that accepts connections (up to `deadline`) and replies
/// with a canned `{"status":"ok","output":"<sentinel>"}` line.
///
/// `.conn_count()` reports how many connections the mock accepted. Dropping
/// the fixture signals the thread to exit and removes the socket file.
struct MockDaemon {
    conn_count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    sock_path: PathBuf,
    handle: Option<JoinHandle<()>>,
}

impl MockDaemon {
    fn new(sock_path: PathBuf, sentinel: &'static str) -> Self {
        let listener = UnixListener::bind(&sock_path)
            .unwrap_or_else(|e| panic!("bind {} failed: {e}", sock_path.display()));
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on mock listener");

        let conn_count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&conn_count);
        let s2 = Arc::clone(&stop);

        let response = format!(r#"{{"status":"ok","output":"{sentinel}"}}"#);
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !s2.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        c2.fetch_add(1, Ordering::SeqCst);
                        // Drain the request line. We don't care what the CLI
                        // sent — any valid frame is treated as a ping.
                        let mut buf = String::new();
                        let _ = BufReader::new(&stream).read_line(&mut buf);
                        // Reply with the sentinel output frame. The CLI side
                        // will `print!("{output}")` for status=ok and exit 0.
                        let _ = writeln!(stream, "{response}");
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            conn_count,
            stop,
            sock_path,
            handle: Some(handle),
        }
    }

    fn conn_count(&self) -> usize {
        self.conn_count.load(Ordering::SeqCst)
    }
}

impl Drop for MockDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

/// Build a minimal temp project and compute the daemon socket path the CLI
/// will try to connect to. Precondition for every socket-mock test.
fn setup_project() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("Failed to create temp dir");
    // `.cqs/` makes `resolve_index_dir` return `<temp>/.cqs` deterministically
    // (it prefers an existing dir). `find_project_root` falls back to CWD
    // when no marker files are found up the walk, which we set to `<temp>`.
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("Failed to create .cqs dir");

    // `XDG_RUNTIME_DIR` points at the same temp dir so the socket lives
    // next to the project, fully isolated from any real `cqs watch`.
    // `cqs::daemon_translate::daemon_socket_path` honours `XDG_RUNTIME_DIR`
    // when computing the socket filename, same as the in-tree wrapper.
    let cqs_dir_canonical = dunce::canonicalize(&cqs_dir).expect("canonicalize cqs_dir");
    let sock_path = daemon_socket_path_with_runtime_dir(&cqs_dir_canonical, dir.path());

    (dir, sock_path)
}

/// Mirror `cqs::daemon_translate::daemon_socket_path` but with an explicit
/// runtime-dir override so the test doesn't need to mutate env vars in the
/// current process. The mutation happens on the spawned CLI only.
fn daemon_socket_path_with_runtime_dir(cqs_dir: &Path, runtime_dir: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    cqs_dir.hash(&mut h);
    let sock_name = format!("cqs-{:x}.sock", h.finish());
    runtime_dir.join(sock_name)
}

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Strip stray env vars so the CLI under test doesn't accidentally inherit
/// the test runner's telemetry / daemon hints. `CQS_NO_DAEMON` must be unset
/// — otherwise the daemon path short-circuits and we'd never exercise the
/// code under test. `RUST_LOG` is reset to `warn` to keep stderr quiet so
/// assertions on stdout aren't polluted.
fn clean_cqs_env(cmd: &mut Command) {
    cmd.env_remove("CQS_NO_DAEMON");
    cmd.env_remove("CQS_TELEMETRY");
    cmd.env("RUST_LOG", "warn");
}

#[test]
fn test_try_daemon_query_bypasses_notes_mutations() {
    // PR #945 regression seed: `notes add|update|remove` must hit the CLI
    // path (they mutate `docs/notes.toml` and reindex). If the bypass at
    // `dispatch.rs` (via `Commands::batch_support() == BatchSupport::Cli`
    // for `NotesCommand::Add|Update|Remove`) ever regresses, this test
    // fires because the mock listener would see the connection attempt.
    let (dir, sock_path) = setup_project();
    let mock = MockDaemon::new(sock_path.clone(), "DAEMON_SHOULD_NOT_RESPOND");

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args([
            "notes",
            "add",
            "bypass-regression-seed",
            "--sentiment",
            "0",
            "--no-reindex",
        ]);

    let output = cmd.output().expect("cqs notes add spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "`cqs notes add` failed; stdout=<{stdout}> stderr=<{stderr}>"
    );
    // The smoking gun: the mock saw zero connections. If the bypass broke,
    // the CLI would have connected, the mock would have replied with the
    // sentinel "DAEMON_SHOULD_NOT_RESPOND", and `conn_count` would be >= 1.
    assert_eq!(
        mock.conn_count(),
        0,
        "notes add reached the daemon socket (bypass regressed); mock saw {} connection(s). stdout=<{stdout}> stderr=<{stderr}>",
        mock.conn_count()
    );
    // Belt and suspenders: the sentinel must not have leaked into stdout.
    // If it did, the command silently forwarded and printed the mock reply.
    assert!(
        !stdout.contains("DAEMON_SHOULD_NOT_RESPOND"),
        "mock response leaked into stdout: {stdout}"
    );
}

#[test]
fn test_mock_socket_round_trip_for_daemon_command() {
    // Complement to the bypass test: a daemon-dispatchable command
    // (`notes list --json`) must forward to the socket and print the
    // mock's response verbatim. Exercises the full frame: connect, write
    // request, read response, parse `{status, output}`, print output.
    let (dir, sock_path) = setup_project();
    let mock = MockDaemon::new(sock_path.clone(), "DAEMON_MOCK_SENTINEL");

    // `notes list --json` is classified `BatchSupport::Daemon`
    // (definitions.rs, `NotesCommand::List` arm). The daemon-forward path
    // doesn't open the store, so no index is required for the test.
    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["notes", "list", "--json"]);

    let output = cmd.output().expect("cqs notes list spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "`cqs notes list --json` failed; stdout=<{stdout}> stderr=<{stderr}>"
    );
    assert_eq!(
        mock.conn_count(),
        1,
        "expected exactly one daemon connection, got {}; stdout=<{stdout}> stderr=<{stderr}>",
        mock.conn_count()
    );
    assert!(
        stdout.contains("DAEMON_MOCK_SENTINEL"),
        "daemon sentinel missing from stdout; stdout=<{stdout}> stderr=<{stderr}>"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Task B2: `cqs ping` against a mock listener.
//
// Two flavours: connect-success (mock replies with a canned PingResponse,
// the CLI prints the formatted output) and connect-failure (no socket,
// CLI must exit 1 with "no daemon running").
// ─────────────────────────────────────────────────────────────────────────────

/// Mock daemon that recognises the `ping` request and replies with a
/// canned PingResponse. Drains the request, asserts it looks like a ping
/// frame, then writes the envelope `{status:ok,output:<PingResponse JSON>}`.
///
/// Why a separate fixture from `MockDaemon`? `MockDaemon` returns a string
/// sentinel; `cqs ping` parses the output as JSON, so we need a structured
/// response. Keeping the sentinel mock for the existing tests (so a future
/// ping-fixture refactor doesn't disturb the bypass-regression seed).
struct PingMockDaemon {
    conn_count: Arc<AtomicUsize>,
    last_request: Arc<std::sync::Mutex<String>>,
    stop: Arc<AtomicBool>,
    sock_path: PathBuf,
    handle: Option<JoinHandle<()>>,
}

impl PingMockDaemon {
    fn new(sock_path: PathBuf) -> Self {
        let listener = UnixListener::bind(&sock_path)
            .unwrap_or_else(|e| panic!("bind {} failed: {e}", sock_path.display()));
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on mock listener");

        let conn_count = Arc::new(AtomicUsize::new(0));
        let last_request = Arc::new(std::sync::Mutex::new(String::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&conn_count);
        let r2 = Arc::clone(&last_request);
        let s2 = Arc::clone(&stop);

        // Build the canned PingResponse payload — values pinned in the
        // assertions below. Real daemon response is a JSON-string-encoded
        // payload inside the `output` field (see PingResponse docstring).
        let payload = r#"{"model":"BAAI/bge-large-en-v1.5","dim":1024,"uptime_secs":9375,"last_indexed_at":1734120000,"error_count":3,"total_queries":12453,"splade_loaded":true,"reranker_loaded":false}"#;
        // The envelope embeds the payload as a JSON string (current
        // wire format). `serde_json::to_string` on the inner payload
        // gives us the escaped form.
        let envelope = format!(
            r#"{{"status":"ok","output":{}}}"#,
            serde_json::to_string(payload).unwrap()
        );

        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !s2.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        c2.fetch_add(1, Ordering::SeqCst);
                        let mut buf = String::new();
                        let _ = BufReader::new(&stream).read_line(&mut buf);
                        if let Ok(mut g) = r2.lock() {
                            *g = buf.clone();
                        }
                        let _ = writeln!(stream, "{envelope}");
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            conn_count,
            last_request,
            stop,
            sock_path,
            handle: Some(handle),
        }
    }

    fn conn_count(&self) -> usize {
        self.conn_count.load(Ordering::SeqCst)
    }

    fn last_request(&self) -> String {
        self.last_request
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

impl Drop for PingMockDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

#[test]
fn test_ping_round_trip() {
    // Task B2: `cqs ping --json` must connect to the daemon, send a ping
    // frame, read the envelope, deserialize the inner PingResponse, and
    // print it as JSON. Pins the wire shape on both sides.
    let (dir, sock_path) = setup_project();
    let mock = PingMockDaemon::new(sock_path.clone());

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["ping", "--json"]);

    let output = cmd.output().expect("cqs ping spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "`cqs ping --json` failed; stdout=<{stdout}> stderr=<{stderr}>"
    );
    assert_eq!(
        mock.conn_count(),
        1,
        "expected exactly one daemon connection, got {}; stdout=<{stdout}> stderr=<{stderr}>",
        mock.conn_count()
    );
    // The CLI sent the canonical ping frame.
    let req = mock.last_request();
    assert!(
        req.contains("\"command\":\"ping\""),
        "expected ping command in request, got: {req}"
    );

    // The CLI parsed the envelope and printed PingResponse as JSON.
    // The trailing newline from println! is fine for the JSON parser.
    let trimmed = stdout.trim();
    let parsed: serde_json::Value = serde_json::from_str(trimmed)
        .unwrap_or_else(|e| panic!("CLI did not print valid JSON; stdout=<{stdout}> err={e}"));
    assert_eq!(parsed["model"], "BAAI/bge-large-en-v1.5");
    assert_eq!(parsed["dim"], 1024);
    assert_eq!(parsed["uptime_secs"], 9_375);
    assert_eq!(parsed["last_indexed_at"], 1_734_120_000_i64);
    assert_eq!(parsed["error_count"], 3);
    assert_eq!(parsed["total_queries"], 12_453);
    assert_eq!(parsed["splade_loaded"], true);
    assert_eq!(parsed["reranker_loaded"], false);
}

#[test]
fn test_ping_text_output() {
    // Task B2: text mode must include the "daemon: running" header and
    // the loaded= status line so the operator can read the daemon's
    // state at a glance. Doesn't pin every field — leaves wiggle room
    // for cosmetic tweaks — but pins the structurally-load-bearing bits.
    let (dir, sock_path) = setup_project();
    let _mock = PingMockDaemon::new(sock_path.clone());

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["ping"]);

    let output = cmd.output().expect("cqs ping spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "`cqs ping` failed; stdout=<{stdout}> stderr=<{stderr}>"
    );
    // Spec output:
    //   daemon: running
    //   uptime: 2h 35m
    //   model: BAAI/bge-large-en-v1.5 (1024-dim)
    //   ...
    //   loaded: splade=yes reranker=no
    assert!(
        stdout.contains("daemon: running"),
        "expected 'daemon: running' line; got: {stdout}"
    );
    assert!(
        stdout.contains("model: BAAI/bge-large-en-v1.5 (1024-dim)"),
        "expected model line with dim; got: {stdout}"
    );
    assert!(
        stdout.contains("queries: 12,453 served (3 errors)"),
        "expected counters with thousands separator; got: {stdout}"
    );
    assert!(
        stdout.contains("loaded: splade=yes reranker=no"),
        "expected loaded= line; got: {stdout}"
    );
}

#[test]
fn test_ping_no_daemon_exits_one() {
    // Task B2: when the socket is missing, `cqs ping` must exit 1 with a
    // friendly stderr message. Differentiates the healthcheck from the
    // regular daemon-forward path which silently falls back to CLI.
    let (dir, _sock_path) = setup_project();
    // Deliberately do NOT bind a mock — the socket file is absent.

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["ping"]);

    let output = cmd.output().expect("cqs ping spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "`cqs ping` should fail when no daemon; stdout=<{stdout}> stderr=<{stderr}>"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 for missing daemon; got {:?}",
        output.status.code()
    );
    assert!(
        stderr.contains("no daemon running"),
        "expected friendly 'no daemon running' message; stderr=<{stderr}>"
    );
}
