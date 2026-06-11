//! #972: pin the behaviour of the daemon-forward path in
//! `src/cli/dispatch.rs::try_daemon_query`.
//!
//! Two concerns:
//!
//! 1. **Pure arg-translation** — `cqs::daemon_translate::translate_cli_args_to_batch`
//!    is the extracted helper. Black-box tests here pin the top-level-region
//!    stripping and the verbatim forwarding of subcommand args so a future
//!    edit to the helper doesn't silently ship a different wire format to
//!    the daemon.
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

mod common;

use assert_cmd::Command;
use common::cqs_v1 as cqs;
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

/// Hand-built [`cqs::daemon_translate::CliArgSpec`] mirroring the production
/// classification (which `cli::dispatch` derives from the live clap
/// definition — its derivation is pinned by unit tests in the binary crate
/// and exercised end-to-end by the socket-mock tests below).
fn spec() -> cqs::daemon_translate::CliArgSpec {
    let set = |items: &[&str]| {
        items
            .iter()
            .map(|s| s.to_string())
            .collect::<std::collections::BTreeSet<String>>()
    };
    cqs::daemon_translate::CliArgSpec {
        value_flags: set(&[
            "--model",
            "--slot",
            "-n",
            "--limit",
            "-t",
            "--threshold",
            "--tokens",
            "-l",
            "--lang",
        ]),
        bare_query_strip: set(&[
            "--json",
            "-q",
            "--quiet",
            "-v",
            "--verbose",
            "--model",
            "--slot",
        ]),
    }
}

#[test]
fn test_translate_strips_top_level_json_flag() {
    // `--json` before the subcommand is a top-level `Cli` flag: the whole
    // top-level region is dropped on daemon forwarding.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["--json", "impact", "foo"]),
        true,
        &spec(),
    );
    assert_eq!(cmd, "impact");
    assert_eq!(args, v(&["foo"]));

    // Bare-query form: `cqs --json "hello"` also drops --json.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["--json", "hello"]),
        false,
        &spec(),
    );
    assert_eq!(cmd, "search");
    assert_eq!(args, v(&["hello"]));
}

#[test]
fn test_translate_forwards_subcommand_args_verbatim() {
    // Post-subcommand tokens forward verbatim — the batch parser flattens
    // the same shared args structs as the CLI, so `-n` keeps its
    // per-subcommand meaning (`LimitArg` for impact, `--commits` for blame).
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["impact", "foo", "-n", "5"]),
        true,
        &spec(),
    );
    assert_eq!(cmd, "impact");
    assert_eq!(args, v(&["foo", "-n", "5"]));

    // Equals form is one token, forwarded whole.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["impact", "foo", "-n=5"]),
        true,
        &spec(),
    );
    assert_eq!(cmd, "impact");
    assert_eq!(args, v(&["foo", "-n=5"]));

    // blame's `-n` means `--commits` — the old `-n` → `--limit` remap made
    // `cqs blame foo -n 3` a parse error daemon-up. Verbatim forwarding is
    // the fix.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["blame", "foo", "-n", "3"]),
        true,
        &spec(),
    );
    assert_eq!(cmd, "blame");
    assert_eq!(args, v(&["foo", "-n", "3"]));

    // `--limit` long form forwards verbatim too.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["impact", "foo", "--limit", "7"]),
        true,
        &spec(),
    );
    assert_eq!(cmd, "impact");
    assert_eq!(args, v(&["foo", "--limit", "7"]));
}

#[test]
fn test_translate_prepends_search_for_bare_query() {
    // `cqs "hello world"` → the caller passes `has_subcommand = false`; the
    // translator synthesises `search` as the subcommand.
    let (cmd, args) =
        cqs::daemon_translate::translate_cli_args_to_batch(&v(&["hello world"]), false, &spec());
    assert_eq!(cmd, "search");
    assert_eq!(args, v(&["hello world"]));

    // Multi-token bare query: `cqs "alpha" "beta" --quiet`. The --quiet is
    // stripped; the two positional tokens remain as the query args.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["alpha", "beta", "--quiet"]),
        false,
        &spec(),
    );
    assert_eq!(cmd, "search");
    assert_eq!(args, v(&["alpha", "beta"]));
}

#[test]
fn test_translate_preserves_subcommand_flags() {
    // With an explicit subcommand, the subcommand name stays first and its
    // flags pass through untouched: `--threshold 0.5` is subcommand-scoped
    // for `impact` and must reach the daemon unchanged. `--json` after the
    // subcommand is the subcommand's own flag (the shared output structs
    // accept it on the batch side too) and also forwards.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["impact", "foo", "--threshold", "0.5", "--json"]),
        true,
        &spec(),
    );
    assert_eq!(cmd, "impact");
    assert_eq!(args, v(&["foo", "--threshold", "0.5", "--json"]));
}

#[test]
fn test_translate_drops_top_level_region_with_subcommand() {
    // `-v` / `--rrf` before the subcommand are top-level `Cli` flags. The
    // old hand-mirrored strip list forwarded them into the batch subcommand
    // parser, hard-erroring daemon-up while working daemon-down.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["-v", "callers", "foo"]),
        true,
        &spec(),
    );
    assert_eq!(cmd, "callers");
    assert_eq!(args, v(&["foo"]));

    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["--rrf", "callers", "foo"]),
        true,
        &spec(),
    );
    assert_eq!(cmd, "callers");
    assert_eq!(args, v(&["foo"]));

    // Top-level value flag consumes its value: the value must not be
    // mistaken for the subcommand name.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["--model", "bge-large", "callers", "foo"]),
        true,
        &spec(),
    );
    assert_eq!(cmd, "callers");
    assert_eq!(args, v(&["foo"]));
}

#[test]
fn test_translate_bare_query_forwards_search_knobs() {
    // Search knobs (`--rrf`, `-n`) forward verbatim to the batch `search`
    // parser, which accepts them via the shared `SearchArgs`/`LimitArg`;
    // process-local flags (`--model VAL`, `-v`) are stripped.
    let (cmd, args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &v(&["hello", "--rrf", "-n", "8", "--model", "bge-large", "-v"]),
        false,
        &spec(),
    );
    assert_eq!(cmd, "search");
    assert_eq!(args, v(&["hello", "--rrf", "-n", "8"]));
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
    last_request: Arc<std::sync::Mutex<String>>,
    stop: Arc<AtomicBool>,
    sock_path: PathBuf,
    handle: Option<JoinHandle<()>>,
}

impl MockDaemon {
    /// Like [`MockDaemon::new`] but replies with an arbitrary pre-framed
    /// response line (for structured `output` values rather than string
    /// sentinels).
    fn with_response_line(sock_path: PathBuf, response: String) -> Self {
        Self::spawn(sock_path, response)
    }

    fn new(sock_path: PathBuf, sentinel: &'static str) -> Self {
        let response = format!(r#"{{"status":"ok","output":"{sentinel}"}}"#);
        Self::spawn(sock_path, response)
    }

    fn spawn(sock_path: PathBuf, response: String) -> Self {
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

        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !s2.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        c2.fetch_add(1, Ordering::SeqCst);
                        // Drain the request line and record it so tests can
                        // assert on the translated wire frame.
                        let mut buf = String::new();
                        let _ = BufReader::new(&stream).read_line(&mut buf);
                        if let Ok(mut g) = r2.lock() {
                            *g = buf.clone();
                        }
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
///
/// AC-V1.30.1-9: must stay byte-equivalent to the production helper —
/// production switched from `std::collections::hash_map::DefaultHasher`
/// (Rust-version-dependent SipHash) to `blake3` truncated to 8 bytes.
fn daemon_socket_path_with_runtime_dir(cqs_dir: &Path, runtime_dir: &Path) -> PathBuf {
    let canonical_path_bytes = cqs_dir.as_os_str().as_encoded_bytes();
    let hash = blake3::hash(canonical_path_bytes);
    let truncated = &hash.as_bytes()[..8];
    let mut hex = String::with_capacity(16);
    for b in truncated {
        use std::fmt::Write as _;
        let _ = write!(hex, "{:02x}", b);
    }
    let sock_name = format!("cqs-{}.sock", hex);
    runtime_dir.join(sock_name)
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

/// Spawn the binary with `args` against a `MockDaemon`, assert it exited 0
/// and reached the socket exactly once, and return the parsed request frame
/// `{"command": ..., "args": [...]}` the CLI sent. Shared by the
/// arg-translation parity tests below — they pin the *production* derived
/// `CliArgSpec`, not the hand-built one the pure tests use.
fn forwarded_request(cli_args: &[&str]) -> serde_json::Value {
    let (dir, sock_path) = setup_project();
    let mock = MockDaemon::new(sock_path.clone(), "DAEMON_MOCK_SENTINEL");

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(cli_args);

    let output = cmd.output().expect("cqs spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "`cqs {cli_args:?}` failed daemon-up; stdout=<{stdout}> stderr=<{stderr}>"
    );
    assert_eq!(
        mock.conn_count(),
        1,
        "expected exactly one daemon connection for {cli_args:?}; stdout=<{stdout}> stderr=<{stderr}>"
    );
    let req = mock.last_request();
    serde_json::from_str(req.trim())
        .unwrap_or_else(|e| panic!("request frame is not JSON: {req} ({e})"))
}

#[test]
fn test_blame_dash_n_forwards_as_commits_flag() {
    // blame's `-n` means `--commits`, not `--limit`. The old translation
    // remapped every `-n` to `--limit`, so `cqs blame foo -n 3` returned a
    // parse_error envelope daemon-up while working daemon-down. The frame
    // must carry `-n 3` verbatim for the batch BlameArgs parser.
    let req = forwarded_request(&["blame", "foo", "-n", "3"]);
    assert_eq!(req["command"], "blame");
    assert_eq!(
        req["args"],
        serde_json::json!(["foo", "-n", "3"]),
        "blame args must forward verbatim, got: {req}"
    );
}

#[test]
fn test_top_level_verbose_flag_is_stripped_on_forward() {
    // `cqs -v callers foo`: `-v` is a top-level Cli flag the old
    // hand-mirrored strip list didn't know, so it leaked into the batch
    // `callers` parser and hard-errored daemon-up.
    let req = forwarded_request(&["-v", "callers", "foo"]);
    assert_eq!(req["command"], "callers");
    assert_eq!(
        req["args"],
        serde_json::json!(["foo"]),
        "top-level -v must be stripped, got: {req}"
    );
}

#[test]
fn test_top_level_rrf_flag_is_stripped_on_forward() {
    // `cqs --rrf callers foo`: same drift class as `-v`.
    let req = forwarded_request(&["--rrf", "callers", "foo"]);
    assert_eq!(req["command"], "callers");
    assert_eq!(
        req["args"],
        serde_json::json!(["foo"]),
        "top-level --rrf must be stripped, got: {req}"
    );
}

#[test]
fn test_bare_query_forwards_search_knobs_to_daemon() {
    // Bare-query search knobs forward to the batch `search` parser; the
    // process-local `--json` is dropped.
    let req = forwarded_request(&["--json", "find the thing", "--rrf", "-n", "8"]);
    assert_eq!(req["command"], "search");
    assert_eq!(
        req["args"],
        serde_json::json!(["find the thing", "--rrf", "-n", "8"]),
        "search knobs must forward verbatim, got: {req}"
    );
}

#[test]
fn test_daemon_stale_origins_meta_prints_cli_warning() {
    // Staleness parity: when the daemon's search response carries
    // `_meta.stale_origins`, the CLI client must print the SAME stderr
    // warning the CLI-direct path emits (`print_stale_warning`, shared by
    // `warn_stale_results` and the dispatch.rs translation path). Before the
    // fix, daemon-served searches silently dropped the staleness signal.
    let (dir, sock_path) = setup_project();
    let response = serde_json::json!({
        "status": "ok",
        "output": {
            "data": {"query": "find the thing", "results": [], "total": 0},
            "_meta": {"stale_origins": ["src/lib.rs", "src/other.rs"]},
        },
    });
    let mock = MockDaemon::with_response_line(sock_path.clone(), response.to_string());

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    // Bare default surface (no CQS_OUTPUT_FORMAT pin) — the `_meta` splice
    // assertion below targets the V2Bare shape.
    let mut cmd = cqs_bare();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["find the thing", "--json"]);

    let output = cmd.output().expect("cqs spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "bare query failed daemon-up; stdout=<{stdout}> stderr=<{stderr}>"
    );
    assert_eq!(
        mock.conn_count(),
        1,
        "expected exactly one daemon connection; stdout=<{stdout}> stderr=<{stderr}>"
    );
    // The canonical warning line from `print_stale_warning` — same text the
    // CLI-direct path produces for two stale files.
    assert!(
        stderr.contains("2 result files changed since last index"),
        "stale warning missing from stderr; stderr=<{stderr}>"
    );
    assert!(
        stderr.contains("src/lib.rs") && stderr.contains("src/other.rs"),
        "stale file list missing from stderr; stderr=<{stderr}>"
    );
    // Machine-readable signal: the daemon's `_meta` is spliced into the bare
    // JSON payload on stdout (V2Bare default), same as `worktree_stale`.
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");
    assert_eq!(
        parsed["_meta"]["stale_origins"],
        serde_json::json!(["src/lib.rs", "src/other.rs"]),
        "stale_origins must reach the JSON consumer; stdout=<{stdout}>"
    );
}

#[test]
fn test_daemon_quiet_suppresses_stale_warning() {
    // `--quiet` parity: CLI-direct gates the staleness warning on
    // `!cli.quiet` (`render_query_output`); the daemon-client translation
    // must do the same. The JSON signal stays — only the human warning goes.
    let (dir, sock_path) = setup_project();
    let response = serde_json::json!({
        "status": "ok",
        "output": {
            "data": {"query": "find the thing", "results": [], "total": 0},
            "_meta": {"stale_origins": ["src/lib.rs"]},
        },
    });
    let mock = MockDaemon::with_response_line(sock_path.clone(), response.to_string());

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["--quiet", "find the thing", "--json"]);

    let output = cmd.output().expect("cqs spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "bare query failed daemon-up; stdout=<{stdout}> stderr=<{stderr}>"
    );
    assert_eq!(mock.conn_count(), 1);
    assert!(
        !stderr.contains("changed since last index"),
        "--quiet must suppress the stale warning; stderr=<{stderr}>"
    );
}

#[test]
fn test_cqs_slot_env_bypasses_daemon() {
    // `CQS_SLOT` is documented as equivalent to `--slot` and honored by
    // `slot::resolve_slot_name`. The daemon serves whichever slot was active
    // at *its* startup, so a slot-pinned invocation must bypass the daemon —
    // same rationale as the `--slot` flag gate. Before the fix,
    // `CQS_SLOT=experiment cqs <query>` silently returned the daemon's
    // startup-slot results.
    let (dir, sock_path) = setup_project();
    let mock = MockDaemon::new(sock_path.clone(), "DAEMON_SHOULD_NOT_RESPOND");

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .env("CQS_SLOT", "experiment")
        .current_dir(&canonical_dir)
        .args(["notes", "list", "--json"]);

    // The CLI fallback may fail (no index in the temp project) — the pin is
    // purely that the daemon socket was never touched and the mock's reply
    // never reached stdout.
    let output = cmd.output().expect("cqs notes list spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        mock.conn_count(),
        0,
        "CQS_SLOT must bypass the daemon (slot-pinned queries can't be served by the \
         daemon's startup slot); mock saw {} connection(s). stdout=<{stdout}> stderr=<{stderr}>",
        mock.conn_count()
    );
    assert!(
        !stdout.contains("DAEMON_SHOULD_NOT_RESPOND"),
        "mock response leaked into stdout despite CQS_SLOT: {stdout}"
    );
}

#[test]
fn test_empty_cqs_slot_env_keeps_daemon_path() {
    // `slot::resolve_slot_name` trims `CQS_SLOT` and treats empty/whitespace
    // as UNSET, so the daemon gate must too: `CQS_SLOT= cqs …` (a script
    // clearing the var) pins no slot and must keep the daemon fast path. A
    // bare `is_some()` env check would silently lose it.
    let (dir, sock_path) = setup_project();
    let mock = MockDaemon::new(sock_path.clone(), "DAEMON_MOCK_SENTINEL");

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");

    for (case, slot_value) in [("empty", ""), ("whitespace", "   ")] {
        let mut cmd = cqs();
        clean_cqs_env(&mut cmd);
        cmd.env("XDG_RUNTIME_DIR", dir.path())
            .env("CQS_SLOT", slot_value)
            .current_dir(&canonical_dir)
            .args(["notes", "list", "--json"]);

        let output = cmd.output().expect("cqs notes list spawn");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "`cqs notes list` ({case} CQS_SLOT) failed; stdout=<{stdout}> stderr=<{stderr}>"
        );
        assert!(
            stdout.contains("DAEMON_MOCK_SENTINEL"),
            "{case} CQS_SLOT pins no slot and must take the daemon path; \
             stdout=<{stdout}> stderr=<{stderr}>"
        );
    }
    assert_eq!(
        mock.conn_count(),
        2,
        "both unset-equivalent CQS_SLOT values must reach the daemon"
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
    // CLI emits via `emit_json`, so the PingResponse is wrapped in the
    // standard `{data, error, version}` envelope.
    let trimmed = stdout.trim();
    let parsed: serde_json::Value = serde_json::from_str(trimmed)
        .unwrap_or_else(|e| panic!("CLI did not print valid JSON; stdout=<{stdout}> err={e}"));
    assert_eq!(parsed["data"]["model"], "BAAI/bge-large-en-v1.5");
    assert_eq!(parsed["data"]["dim"], 1024);
    assert_eq!(parsed["data"]["uptime_secs"], 9_375);
    assert_eq!(parsed["data"]["last_indexed_at"], 1_734_120_000_i64);
    assert_eq!(parsed["data"]["error_count"], 3);
    assert_eq!(parsed["data"]["total_queries"], 12_453);
    assert_eq!(parsed["data"]["splade_loaded"], true);
    assert_eq!(parsed["data"]["reranker_loaded"], false);
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

// ─────────────────────────────────────────────────────────────────────────────
// `cqs status --watch` against a mock daemon.
//
// Same fixture pattern as PingMockDaemon: bind a UnixListener at the exact
// socket path the CLI computes, reply with a canned WatchSnapshot whose
// `ops` block is fully populated, and assert the CLI surfaces every field.
// The daemon-absent path must return the structured error (exit 1 +
// error envelope), matching `--watch-fresh`.
// ─────────────────────────────────────────────────────────────────────────────

/// Mock daemon replying to the `status` command with a canned
/// `WatchSnapshot` carrying a fully-populated `ops` block.
struct StatusMockDaemon {
    conn_count: Arc<AtomicUsize>,
    last_request: Arc<std::sync::Mutex<String>>,
    stop: Arc<AtomicBool>,
    sock_path: PathBuf,
    handle: Option<JoinHandle<()>>,
}

impl StatusMockDaemon {
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

        // Canned WatchSnapshot. Field values pinned by the assertions in
        // the round-trip tests below — this is the daemon-side wire shape
        // `dispatch_status` produces by serializing the shared snapshot.
        let payload = serde_json::json!({
            "state": "fresh",
            "modified_files": 4,
            "pending_notes": false,
            "rebuild_in_flight": false,
            "delta_saturated": false,
            "incremental_count": 12,
            "dropped_this_cycle": 1,
            "last_event_unix_secs": 1_750_000_000_i64,
            "last_synced_at": 1_750_000_050_i64,
            "snapshot_at": 1_750_000_060_i64,
            "active_slot": "default",
            "ops": {
                "in_flight_clients": 3,
                "reconcile_pending": true,
                "last_reindex": {
                    "at_unix_secs": 1_750_000_050_i64,
                    "duration_ms": 842,
                    "files": 4,
                },
                "last_error": {
                    "at_unix_secs": 1_749_999_000_i64,
                    "message": "reindex failed: synthetic disk full",
                },
                "slots": [{
                    "name": "default",
                    "state": "fresh",
                    "last_synced_at": 1_750_000_050_i64,
                    "last_reindex": {
                        "at_unix_secs": 1_750_000_050_i64,
                        "duration_ms": 842,
                        "files": 4,
                    },
                }],
            },
        })
        .to_string();
        // String-payload transport form, same as PingMockDaemon.
        let envelope = format!(
            r#"{{"status":"ok","output":{}}}"#,
            serde_json::to_string(&payload).unwrap()
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

impl Drop for StatusMockDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

#[test]
fn test_status_watch_json_round_trip_returns_all_ops_fields() {
    // `cqs status --watch --json` must connect to the daemon, issue the
    // `status` command, and emit the full WatchSnapshot — including every
    // ops-block field (gate: every field returned).
    let (dir, sock_path) = setup_project();
    let mock = StatusMockDaemon::new(sock_path.clone());

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["status", "--watch", "--json"]);

    let output = cmd.output().expect("cqs status spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "`cqs status --watch --json` failed; stdout=<{stdout}> stderr=<{stderr}>"
    );
    assert_eq!(
        mock.conn_count(),
        1,
        "expected exactly one daemon connection"
    );
    let req = mock.last_request();
    assert!(
        req.contains("\"command\":\"status\""),
        "expected status command in request, got: {req}"
    );

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("CLI did not print valid JSON; stdout=<{stdout}> err={e}"));
    let data = &parsed["data"];
    // Freshness core (queue depth = modified_files, dropped events).
    assert_eq!(data["state"], "fresh");
    assert_eq!(data["modified_files"], 4);
    assert_eq!(data["dropped_this_cycle"], 1);
    // Ops block: every field from the issue's list.
    let ops = &data["ops"];
    assert_eq!(ops["in_flight_clients"], 3);
    assert_eq!(ops["reconcile_pending"], true);
    assert_eq!(ops["last_reindex"]["at_unix_secs"], 1_750_000_050_i64);
    assert_eq!(ops["last_reindex"]["duration_ms"], 842);
    assert_eq!(ops["last_reindex"]["files"], 4);
    assert_eq!(
        ops["last_error"]["message"],
        "reindex failed: synthetic disk full"
    );
    assert_eq!(ops["last_error"]["at_unix_secs"], 1_749_999_000_i64);
    // Per-slot vec carries the active slot.
    assert_eq!(ops["slots"][0]["name"], "default");
    assert_eq!(ops["slots"][0]["state"], "fresh");
    assert_eq!(ops["slots"][0]["last_synced_at"], 1_750_000_050_i64);
}

#[test]
fn test_status_watch_text_renders_ops_block() {
    // Text mode appends the grep-friendly ops lines after the freshness
    // summary. Pins the structurally load-bearing keys, not the full
    // formatting.
    let (dir, sock_path) = setup_project();
    let _mock = StatusMockDaemon::new(sock_path.clone());

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["status", "--watch"]);

    let output = cmd.output().expect("cqs status spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "`cqs status --watch` failed; stdout=<{stdout}> stderr=<{stderr}>"
    );
    assert!(
        stdout.contains("state: fresh"),
        "freshness line first: {stdout}"
    );
    assert!(
        stdout.contains("clients_in_flight=3"),
        "in-flight clients missing: {stdout}"
    );
    assert!(
        stdout.contains("reconcile_pending=true"),
        "reconcile state missing: {stdout}"
    );
    assert!(
        stdout.contains("last_reindex_ms=842"),
        "reindex latency missing: {stdout}"
    );
    assert!(
        stdout.contains("last_error=reindex failed: synthetic disk full"),
        "last error missing: {stdout}"
    );
    assert!(
        stdout.contains("slot=default state=fresh"),
        "per-slot line missing: {stdout}"
    );
}

#[test]
fn test_status_watch_no_daemon_exits_one_with_structured_error() {
    // Daemon-absent path: `--watch` must match `--watch-fresh` — exit 1,
    // friendly stderr in text mode, error envelope in JSON mode.
    let (dir, _sock_path) = setup_project();
    // No mock bound — socket file is absent.

    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");

    // Text mode.
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["status", "--watch"]);
    let output = cmd.output().expect("cqs status spawn");
    assert_eq!(output.status.code(), Some(1), "no daemon must exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cqs:") || stderr.contains("daemon"),
        "stderr should describe the no-daemon condition, got: {stderr}"
    );

    // JSON mode: structured error envelope.
    let mut cmd = cqs();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .env("CQS_OUTPUT_FORMAT", "v1")
        .current_dir(&canonical_dir)
        .args(["status", "--watch", "--json"]);
    let output = cmd.output().expect("cqs status spawn");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"error\"") || stdout.contains("\"code\""),
        "stdout should contain a JSON error envelope, got: {stdout}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Daemon slim-envelope translation: the CLI surface emits a bare payload
// (or full v1 envelope under CQS_OUTPUT_FORMAT=v1) regardless of whether a
// daemon served the query. Regression seed for the {"data": ...} leak that
// /cqs-verify caught on 2026-06-10: daemon-forwarded output printed the
// batch envelope verbatim, so output shape depended on daemon presence.
// ─────────────────────────────────────────────────────────────────────────────

/// Bare-default binary command: cqs_v1 minus the v1 pin.
fn cqs_bare() -> assert_cmd::Command {
    #[allow(deprecated)]
    let mut c = assert_cmd::Command::cargo_bin("cqs").expect("Failed to find cqs binary");
    c.env_remove("CQS_OUTPUT_FORMAT");
    c
}

fn slim_envelope_response() -> String {
    r#"{"status":"ok","output":{"data":{"notes":[],"count":0},"_meta":{"worktree_stale":true}}}"#
        .to_string()
}

#[test]
fn test_slim_data_envelope_unwraps_to_bare_payload() {
    let (dir, sock_path) = setup_project();
    let _mock = MockDaemon::with_response_line(sock_path, slim_envelope_response());
    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");

    let mut cmd = cqs_bare();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["notes", "list", "--json"]);

    let output = cmd.output().expect("cqs notes list spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout=<{stdout}>");

    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert!(
        v.get("data").is_none(),
        "bare surface must not carry the batch envelope; got <{stdout}>"
    );
    assert_eq!(v["count"], 0, "payload fields at top level");
    assert_eq!(
        v["_meta"]["worktree_stale"], true,
        "daemon _meta spliced onto the bare object payload"
    );
    assert!(
        stdout.ends_with('\n'),
        "trailing newline parity with println"
    );
}

#[test]
fn test_slim_data_envelope_rebuilds_v1_envelope() {
    let (dir, sock_path) = setup_project();
    let _mock = MockDaemon::with_response_line(sock_path, slim_envelope_response());
    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");

    let mut cmd = cqs(); // v1-pinned
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["notes", "list", "--json"]);

    let output = cmd.output().expect("cqs notes list spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout=<{stdout}>");

    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(v["data"]["count"], 0, "v1 surface keeps the full envelope");
    assert!(v.get("version").is_some(), "v1 envelope carries version");
}

#[test]
fn test_slim_error_envelope_exits_nonzero() {
    let (dir, sock_path) = setup_project();
    let _mock = MockDaemon::with_response_line(
        sock_path,
        r#"{"status":"ok","output":{"error":{"code":"not_found","message":"no such note"}}}"#
            .to_string(),
    );
    let canonical_dir =
        dunce::canonicalize(dir.path()).expect("canonicalize temp dir for CWD override");

    let mut cmd = cqs_bare();
    clean_cqs_env(&mut cmd);
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .current_dir(&canonical_dir)
        .args(["notes", "list", "--json"]);

    let output = cmd.output().expect("cqs notes list spawn");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "slim error envelope must exit non-zero, not print an error with exit 0"
    );
    assert!(
        stderr.contains("not_found") && stderr.contains("no such note"),
        "error code+message surfaced; stderr=<{stderr}>"
    );
}
