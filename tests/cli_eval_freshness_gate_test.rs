//! P2 audit fix — TC-HAP-1.30.1-7: end-to-end exercise of the eval
//! freshness gate **without** the `CQS_EVAL_REQUIRE_FRESH=0` bypass that
//! every existing `eval` integration test sets.
//!
//! Why this is its own file: every test in `tests/eval_subcommand_test.rs`
//! sets `CQS_EVAL_REQUIRE_FRESH=0` (see `cqs_no_daemon` helper there) so
//! the freshness gate is disabled before `cmd_eval` ever runs. That's the
//! right move for those tests — they're pinning eval matcher / report
//! shape, not the gate. But it leaves the gate path itself untested
//! end-to-end: `require_fresh_gate` could be wired into a comment-only
//! branch and every existing test would still pass.
//!
//! These tests cover the missing surface:
//!
//!   1. **fresh-on-first-poll happy path** — bind a mock daemon at the
//!      exact `daemon_socket_path` the CLI computes, have it respond with
//!      `state == fresh`, run `cqs eval` with the gate **on**, and assert
//!      the gate emitted its stderr heads-up and got past the wait
//!      successfully.
//!   2. **no-daemon hard-fail path** — no listener, gate **on**, no
//!      `--no-require-fresh`, no `CQS_EVAL_REQUIRE_FRESH=0`. Assert the
//!      CLI exits non-zero with the documented "watch daemon not reachable"
//!      diagnostic.
//!
//! Pattern follows `tests/daemon_forward_test.rs` socket-mock setup.
//! `CQS_NO_DAEMON=1` is set so the dispatch-layer `try_daemon_query`
//! short-circuits without touching our mock — `wait_for_fresh` ignores
//! that env var, so the freshness gate still hits the socket.
//!
//! Gated behind `slow-tests` because the success path eventually loads
//! the embedder once the gate clears (same shape as
//! `tests/eval_subcommand_test.rs` integration tests).

#![cfg(all(unix, feature = "slow-tests"))]

mod common;

use assert_cmd::Command;
use serde_json::json;
use serial_test::serial;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tempfile::TempDir;

use common::mock_embedding;
use cqs::parser::{Chunk, ChunkType, Language};

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Mirror `cqs::daemon_translate::daemon_socket_path` but with explicit
/// runtime-dir override so the mock and the spawned CLI agree on which
/// path to use without mutating the test process's `XDG_RUNTIME_DIR`.
///
/// Must use BLAKE3 over the canonical `OsStr` bytes, truncated to 8 bytes
/// formatted as 16 lowercase hex chars — matches AC-V1.30.1-9 in
/// `daemon_translate.rs:216-228`. Earlier versions of this helper used
/// `DefaultHasher`, which silently diverges from the production socket name
/// on every Rust release; the mock would bind at one path and the spawned
/// CLI would look at another, panicking with "no daemon running" the first
/// time ci-slow.yml ran the slow-tests-feature suite (#1305).
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

/// Mock daemon for the freshness-gate path: responds to `daemon_status`
/// queries with a canned `WatchSnapshot { state: Fresh, .. }` envelope.
///
/// Wire format mirrors what the real watch daemon emits:
///   `{"status":"ok","output":{"data":<snapshot>,"error":null,"version":1}}`
/// (outer dispatch envelope wrapping the JSON-envelope payload).
struct FreshDaemon {
    stop: Arc<AtomicBool>,
    sock_path: PathBuf,
    handle: Option<JoinHandle<()>>,
}

impl FreshDaemon {
    fn new(sock_path: PathBuf) -> Self {
        let listener = UnixListener::bind(&sock_path)
            .unwrap_or_else(|e| panic!("bind {} failed: {e}", sock_path.display()));
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on mock listener");

        let stop = Arc::new(AtomicBool::new(false));
        let s2 = Arc::clone(&stop);

        // Build a Fresh-state envelope. Field shape mirrors
        // `cqs::watch_status::WatchSnapshot` exactly — drift here would
        // cause a `WatchSnapshot deserialize failed` error in the CLI.
        // `FreshnessState` serializes as `serde(rename_all = "lowercase")`, so
        // the JSON tag must be `"fresh"` not `"Fresh"`.
        let snap = json!({
            "state": "fresh",
            "modified_files": 0,
            "pending_notes": false,
            "rebuild_in_flight": false,
            "delta_saturated": false,
            "incremental_count": 1,
            "dropped_this_cycle": 0,
            "idle_secs": 30,
            "last_synced_at": 1_734_120_000i64,
            "snapshot_at": 1_734_120_500i64,
        });
        let inner_envelope = json!({
            "data": snap,
            "error": null,
            "version": 1,
        });
        let outer_envelope = json!({"status": "ok", "output": inner_envelope});
        let response = outer_envelope.to_string();

        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !s2.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        // Drain the request line — content doesn't matter,
                        // every status request gets the same Fresh reply.
                        let mut buf = String::new();
                        let _ = BufReader::new(&stream).read_line(&mut buf);
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
            stop,
            sock_path,
            handle: Some(handle),
        }
    }
}

impl Drop for FreshDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

/// Build a tempdir with `.cqs/` + a seeded store, compute the daemon
/// socket path the spawned CLI will try to use (via the explicit
/// `XDG_RUNTIME_DIR` override).
///
/// A seeded store is required because `cqs eval` is a Group B command —
/// dispatch opens `Store<ReadOnly>` *before* `cmd_eval` runs, which means
/// before the freshness gate runs. Without a seeded store the CLI bails
/// with `Index not found` and the gate code is never exercised. The
/// seeded chunk's content doesn't matter for these tests; we're not
/// asserting on R@K.
fn setup_project() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("Failed to create .cqs dir");

    // Seed a single chunk into the legacy `.cqs/index.db` location.
    // `dispatch.rs::run_with_dispatch` migrates this to
    // `.cqs/slots/default/index.db` automatically on the first invocation.
    // Mirrors `tests/eval_subcommand_test.rs::seed_store_in`.
    let store_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    let store = cqs::Store::open(&store_path).expect("open seed store");
    store
        .init(&cqs::store::ModelInfo::default())
        .expect("init seed store");

    let chunk = Chunk {
        id: "src/lib.rs:1:dead".to_string(),
        file: PathBuf::from("src/lib.rs"),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: "stub".to_string(),
        signature: "fn stub()".to_string(),
        content: "fn stub() {}".to_string(),
        doc: None,
        line_start: 1,
        line_end: 1,
        content_hash: blake3::hash(b"fn stub() {}").to_hex().to_string(),
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
        parser_version: 0,
    };
    let pairs = vec![(chunk, mock_embedding(1.0_f32))];
    store
        .upsert_chunks_batch(&pairs, Some(1_700_000_000_000))
        .expect("seed store");

    let cqs_dir_canonical = dunce::canonicalize(&cqs_dir).expect("canonicalize cqs_dir");
    let sock_path = daemon_socket_path_with_runtime_dir(&cqs_dir_canonical, dir.path());

    (dir, sock_path)
}

/// Write a single-query queries.json file the eval runner can ingest.
fn write_queries(dir: &Path) -> PathBuf {
    let queries = json!({
        "queries": [
            {
                "query": "freshness gate exercise",
                "category": "behavioral_search",
                "gold_chunk": {
                    "name": "no_such_function",
                    "origin": "src/no_such_file.rs",
                    "line_start": 1,
                }
            }
        ]
    });
    let q_path = dir.join("queries.json");
    std::fs::write(&q_path, queries.to_string()).expect("write queries");
    q_path
}

/// TC-HAP-1.30.1-7 happy path: gate **on**, mock daemon answers `Fresh`,
/// gate clears within budget. Asserts the stderr heads-up line ran, the
/// "watch daemon not reachable" hard-fail did NOT fire, and (loosely) that
/// the binary made it past the gate into the eval handler.
#[test]
#[serial]
fn eval_freshness_gate_passes_when_daemon_reports_fresh() {
    let (dir, sock_path) = setup_project();
    let _mock = FreshDaemon::new(sock_path);

    let q_path = write_queries(dir.path());

    let mut cmd = cqs();
    // Skip the dispatch-layer daemon forwarding; only the gate's
    // `wait_for_fresh` should hit our mock socket. CQS_EVAL_REQUIRE_FRESH
    // is intentionally NOT set so the gate is on.
    cmd.env("CQS_NO_DAEMON", "1")
        .env("XDG_RUNTIME_DIR", dir.path())
        .env_remove("CQS_EVAL_REQUIRE_FRESH")
        .env("RUST_LOG", "warn")
        .args([
            "eval",
            q_path.to_str().unwrap(),
            "--require-fresh-secs",
            "5",
            "--json",
        ])
        .current_dir(dir.path());

    let result = cmd.output().expect("run cqs eval");
    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    // Gate must announce itself on stderr — that line is the operator's
    // signal that the wait is happening rather than a hang.
    assert!(
        stderr.contains("[eval] checking watch-mode freshness"),
        "expected gate heads-up on stderr; stderr={stderr}\nstdout={stdout}"
    );

    // Gate must NOT fall into the no-daemon hard-fail branch — our mock is
    // bound and responding with Fresh. A leak of that diagnostic means the
    // socket path mismatch crept back or the mock died early.
    assert!(
        !stderr.contains("watch daemon not reachable"),
        "gate hit the no-daemon path despite mock being live; stderr={stderr}"
    );

    // Gate must NOT fall into the timeout branch within 5s on a poll that
    // returns Fresh on the first round-trip. Drift here means
    // `wait_for_fresh`'s short-circuit broke.
    assert!(
        !stderr.contains("watch index is still stale"),
        "gate hit the timeout branch despite Fresh response; stderr={stderr}"
    );

    // Past the gate, eval may fail at embedder load (no model on disk in
    // the test sandbox) or at empty-store search — either is fine. What
    // we DON'T accept is the gate's own bail diagnostic.
    if !result.status.success() {
        // Soft pass: gate cleared, downstream pipeline failed for embedder
        // / index reasons. Surface enough context for triage.
        eprintln!(
            "eval_freshness_gate_passes_when_daemon_reports_fresh: \
             gate cleared, downstream pipeline failed (likely model unavailable). \
             stdout={stdout} stderr={stderr}"
        );
    }
}

/// TC-HAP-1.30.1-7 hard-fail path: gate **on**, no listener, no env-var
/// bypass, no `--no-require-fresh`. The CLI must exit non-zero with the
/// documented diagnostic. Pins the load-bearing default behavior — eval
/// against an unmonitored tree is a real-regression vs fixture-drift
/// confounder, the gate exists to refuse the run.
#[test]
#[serial]
fn eval_freshness_gate_fails_when_no_daemon_running() {
    let (dir, _sock_path) = setup_project();
    // Deliberately skip MockDaemon — socket does not exist.

    let q_path = write_queries(dir.path());

    let mut cmd = cqs();
    cmd.env("CQS_NO_DAEMON", "1")
        .env("XDG_RUNTIME_DIR", dir.path())
        .env_remove("CQS_EVAL_REQUIRE_FRESH")
        .env("RUST_LOG", "warn")
        .args([
            "eval",
            q_path.to_str().unwrap(),
            "--require-fresh-secs",
            "5",
        ])
        .current_dir(dir.path());

    let result = cmd.output().expect("run cqs eval");
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    // Gate must announce itself even on the hard-fail path.
    assert!(
        stderr.contains("[eval] checking watch-mode freshness"),
        "expected gate heads-up on stderr; stderr={stderr}"
    );

    // Hard-fail diagnostic must reach the operator. Earlier the gate had
    // no info-level "outcome=no_daemon" trace; the bail message is the
    // only operator-facing signal so this assertion is load-bearing.
    assert!(
        stderr.contains("watch daemon not reachable"),
        "expected no-daemon diagnostic; stderr={stderr}"
    );

    // CLI must exit non-zero on the hard-fail path. Test passes the wrong
    // diagnostic if this somehow returns 0.
    assert!(
        !result.status.success(),
        "expected non-zero exit on no-daemon hard-fail; stderr={stderr}"
    );
}
