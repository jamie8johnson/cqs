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
