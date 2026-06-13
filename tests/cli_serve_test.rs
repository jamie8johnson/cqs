//! Audit P4-23 / TC-HAP-V1.33-11 — `cqs serve` end-to-end smoke test.
//!
//! `src/serve/tests.rs` has thorough unit tests on `build_router`,
//! `build_graph`, `build_chunk_detail`, `build_cluster`, and `auth::check_request`,
//! but no test spins up `run_server` against a real port and issues HTTP
//! requests. The router includes 6+ routes and a 4-layer middleware stack
//! (auth → host allowlist → body limit → trace + compression) plus a
//! graceful-shutdown handler. Each layer has unit-level coverage, but the
//! composition order — which is exactly where the SEC-1.30-V1 chain of
//! token-leak fixes lives — is untested. A regression that re-ordered
//! `RequestBodyLimitLayer` to fire after auth (so unauthenticated clients
//! could OOM the server before the 401) would compile and pass every
//! existing unit test.
//!
//! This integration test exercises the full layer stack in production
//! order: spawn `cqs serve --port 0 --no-auth`, parse the listening
//! banner to extract the port, issue HTTP requests against the live
//! server.
//!
//! The round-trip runs against a `--no-auth` server *by design*. The
//! per-launch auth token is unrecoverable from a non-TTY launch — it is
//! `AuthToken::random()` per start, withheld from the non-TTY banner so it
//! never lands in journald/container logs, and has no injection channel
//! (no env, no flag). A piped-stdout child
//! (which is the only thing a test harness can spawn without a pty crate)
//! therefore cannot authenticate. Rather than re-open that leak, this test
//! covers the auth-independent half of the layer stack (bind → axum accept
//! → routing → JSON shape) and leaves the auth half to unit coverage.
//!
//! End-to-end auth-composition coverage (401 on missing/wrong token,
//! 200 with Bearer, 200 with cookie, the `?token=…` → 303 + Set-Cookie
//! redirect handoff, per-port cookie scoping, cross-instance rejection)
//! lives in `src/serve/tests.rs` — those tower-layer tests drive the real
//! middleware stack via `tower::ServiceExt::oneshot`, so they exercise the
//! same auth layer this binary runs, just without the process boundary.
//!
//! Pinned contracts (auth-independent):
//!   * `/health` returns 200 from the live `--no-auth` server.
//!   * `/api/graph` returns 200 + a JSON object with `nodes` / `edges`
//!     arrays.
//!
//! `Drop` on the harness sends SIGTERM to the child so the server tears
//! down even if a panic skips an explicit teardown.
//!
//! Gated `slow-tests` because `cqs index` cold-loads the embedder.

#![cfg(feature = "slow-tests")]
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use assert_cmd::Command as AssertCmd;
use serial_test::serial;
use tempfile::TempDir;

fn cqs() -> AssertCmd {
    #[allow(deprecated)]
    let mut c = AssertCmd::cargo_bin("cqs").expect("Failed to find cqs binary");
    // SNR Phase 4 (2026-05-08): default flipped to V2Bare. Pin tests
    // to legacy V1Envelope shape so existing assertions keep working.
    c.env("CQS_OUTPUT_FORMAT", "v1");
    c
}

fn cqs_path() -> std::path::PathBuf {
    #[allow(deprecated)]
    let cmd = AssertCmd::cargo_bin("cqs").expect("Failed to find cqs binary");
    cmd.get_program().to_owned().into()
}

fn setup_indexed_project() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir(&src).expect("mkdir src");
    std::fs::write(
        src.join("lib.rs"),
        "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .expect("write lib.rs");
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();
    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success();
    dir
}

/// RAII harness owning the spawned `cqs serve` child + its captured banner.
/// Drop sends SIGTERM and waits briefly so a panicking test still tears the
/// server down.
struct ServeHarness {
    child: Option<Child>,
    addr: String,
}

impl ServeHarness {
    fn spawn(workdir: &std::path::Path) -> Self {
        // PB-V1.30.1-2 path: `--port 0` resolves to an ephemeral port via
        // `TcpListener::bind`. The banner captures the actual port the
        // kernel assigned. `--no-auth` is required here: the per-launch
        // token is withheld from the non-TTY banner and has no injection
        // channel, so a piped-stdout child can't authenticate — see the
        // module header for why this is auth-independent coverage.
        let mut child = Command::new(cqs_path())
            .args(["serve", "--port", "0", "--no-auth"])
            .current_dir(workdir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cqs serve");

        // The `--no-auth` "listening on" banner lands on stdout. Walk both
        // streams concurrently with a short timeout — we don't depend on
        // which one the runtime picks under assert_cmd's process control,
        // and the loud `WARN: --no-auth in use` line goes to stderr.
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");
        let (tx, rx) = mpsc::channel::<String>();
        spawn_banner_reader(stdout, tx.clone());
        spawn_banner_reader(stderr, tx);

        let banner = recv_banner(&rx, Duration::from_secs(15))
            .unwrap_or_else(|| panic!("timed out waiting for `cqs serve` listening banner"));

        let addr = parse_banner(&banner);

        // Give the axum accept loop a moment to enter `.poll_accept()`
        // after `TcpListener::bind` succeeds. The banner fires before
        // axum starts accepting; on WSL the client connect can land in
        // that window and surface as ConnectionReset on read.
        thread::sleep(Duration::from_millis(500));

        Self {
            child: Some(child),
            addr,
        }
    }
}

impl Drop for ServeHarness {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_banner_reader<R: Read + Send + 'static>(stream: R, tx: mpsc::Sender<String>) {
    // Keep draining the stream long after the banner is captured —
    // dropping the BufReader closes the pipe back to the child, which
    // makes the server's *next* `println!`/tracing flush hit EPIPE and
    // panic out of `_print` (Rust's stdout helper unwraps the IO
    // error). Continuous draining keeps the kernel pipe buffer free
    // for the server's lifetime so that doesn't happen.
    thread::spawn(move || {
        let reader = BufReader::new(stream);
        let mut sent = false;
        for line in reader.lines().map_while(Result::ok) {
            if !sent && line.contains("cqs serve listening on") {
                let _ = tx.send(line);
                sent = true;
            }
            // discard remaining lines
        }
    });
}

fn recv_banner(rx: &mpsc::Receiver<String>, timeout: Duration) -> Option<String> {
    // Single-shot recv with the full timeout — both reader threads
    // fan into the same channel so the first to find the banner wins.
    rx.recv_timeout(timeout).ok()
}

fn parse_banner(banner: &str) -> String {
    // Banner shape (`--no-auth`): `cqs serve listening on http://<bind>/`.
    // The auth-on non-TTY banner withholds the token, so there is nothing
    // to parse out of it — this harness runs `--no-auth` and only needs the
    // bind address.
    let url = banner
        .split("listening on ")
        .nth(1)
        .unwrap_or_else(|| panic!("banner missing `listening on` marker: {banner}"))
        .trim()
        .to_string();
    let url = url.strip_prefix("http://").unwrap_or(&url).to_string();
    url.trim_end_matches('/').to_string()
}

/// Issue a raw HTTP/1.1 GET request against `addr` (host:port). Returns
/// `(status_code, body)`. Hand-rolled rather than pulling in `reqwest`
/// or `ureq` as a dev-dep — the test does two GET round-trips, no JSON
/// body. No auth header: the harness runs `--no-auth`, and the auth
/// channels (Bearer / cookie / `?token=`) are exercised in
/// `src/serve/tests.rs`.
fn http_get(addr: &str, path: &str) -> (u16, String) {
    let mut stream =
        TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect to {addr} failed: {e}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set_read_timeout");
    // SEC-1 host-allowlist accepts `127.0.0.1` bare and `<bind_ip>:<requested_port>`.
    // With `--port 0` the requested port is `0` (the kernel-assigned port
    // isn't known when `allowed_host_set` runs), so the actual ephemeral
    // port is *not* on the allowlist. Bare `127.0.0.1` is, and matches
    // every loopback request without per-port bookkeeping.
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nAccept-Encoding: identity\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).expect("write request");
    stream.flush().expect("flush");

    let mut buf = Vec::new();
    // ConnectionReset on read_to_end can land after a complete HTTP
    // response — axum closes a non-keepalive connection and the OS
    // surfaces RST on macOS/WSL even though the bytes already arrived.
    // Treat it as "we got everything we're going to get" rather than
    // failing the test.
    if let Err(e) = stream.read_to_end(&mut buf) {
        if e.kind() != std::io::ErrorKind::ConnectionReset {
            panic!("read response: {e:?}");
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();

    // Parse the status line: `HTTP/1.1 NNN ...`
    let status_line = text.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "unparseable status line for `GET {path}` against `{addr}` — buf_len={}, raw={text:?}",
                buf.len()
            )
        });

    // Body sits after the blank line separator.
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .map(|s| s.to_string())
        .unwrap_or_default();

    (status, body)
}

/// Pin the auth-independent layer-composition contract: a live
/// `--no-auth` server binds, accepts, routes, and returns a JSON envelope
/// on the API surface. The auth-on path (401 / Bearer / cookie / `?token=`
/// redirect) is covered by the tower-layer tests in `src/serve/tests.rs`
/// — see the module header for why end-to-end auth can't run here (the
/// per-launch token is unrecoverable from a non-TTY launch).
#[test]
#[serial]
fn cqs_serve_full_layer_stack_round_trip() {
    let dir = setup_indexed_project();
    let harness = ServeHarness::spawn(dir.path());

    // 1. `/health` → 200 (no-auth server, no credentials needed).
    let (status, body) = http_get(&harness.addr, "/health");
    assert_eq!(
        status, 200,
        "/health must return 200, got {status} body={body}"
    );

    // 2. `/api/graph` → 200 + a JSON object with `nodes` and `edges`
    //    arrays. The `cqs serve` API surface emits raw JSON rather than
    //    the CLI's `_meta` envelope (different consumer: Cytoscape-shaped
    //    data goes straight to the browser). We pin only the shape, not
    //    the payload — the seeded project is tiny, and graph-builder
    //    fidelity is covered by `src/serve/tests.rs`. The contract under
    //    test is the *layer stack composition* (bind → accept → route).
    let (status, body) = http_get(&harness.addr, "/api/graph");
    assert_eq!(status, 200, "/api/graph must return 200, got {status}");
    let json: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("/api/graph body not JSON: {e}\nbody={body}"));
    assert!(
        json.get("nodes").is_some_and(|v| v.is_array()),
        "/api/graph response must carry a `nodes` array, got: {body}"
    );
    assert!(
        json.get("edges").is_some_and(|v| v.is_array()),
        "/api/graph response must carry an `edges` array, got: {body}"
    );

    drop(harness);
    drop(dir);

    // Verify `cqs_path()` is alive — paranoia: a missing binary would
    // have surfaced at spawn time, but the helper's panic message would
    // have been less actionable. Touching the path here keeps the
    // failure mode obvious if the binary moves between cargo runs.
    assert!(
        cqs_path().exists(),
        "cqs binary disappeared between spawn and end of test"
    );
}
