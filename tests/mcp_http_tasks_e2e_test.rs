//! Live-daemon end-to-end lifecycle test for the MCP HTTP Tasks surface.
//!
//! The transport shaping, the blocking budget, the session table, and the
//! daemon-side TaskStore each have thorough in-process unit coverage. This test
//! owns the ONE seam no mock can reach: the daemon-side eval-task admission
//! (`dispatch_eval_task`) hard-rejects unless it holds the serving daemon's
//! `outer_lock`, so the task store and its eval worker are only reachable
//! through a live `cqs watch --serve` process. Every existing daemon test binds
//! a mock `UnixListener` that cannot supply that lock; only a real daemon does.
//!
//! Two children are stitched together, each from an existing template shape:
//!   * a real `cqs watch --serve` daemon (socket aligned via `XDG_RUNTIME_DIR`,
//!     the socket-override pattern), and
//!   * a real `cqs mcp --http --port 0` listener whose ephemeral port is read
//!     from its startup banner.
//!
//! The client speaks raw HTTP over a `TcpStream` (there is no HTTP client in
//! dev-deps) and drives the full lifecycle: initialize, then an augmented
//! `tools/call` on `cqs_eval`, then poll `tasks/get` to terminal, then
//! `tasks/result`.
//!
//! GPU-free by construction: the eval runs against an EMPTY query fixture, which
//! short-circuits `eval_core` before the embedder / vector index is built, so
//! the task terminalizes in milliseconds and the poll loop stays tight. The
//! daemon still needs a real index to boot (`cqs watch --serve` refuses to start
//! without one), so the harness builds one with `cqs index`; if that cold-load
//! of the embedder fails (model absent in the test env), the test soft-skips
//! rather than failing — the deployed-binary drift the model-present CI catches.
//!
//! POST + poll only: the GET (SSE) surface is deliberately untouched here so a
//! concurrent lane can evolve it without a merge collision.
//!
//! Gated `mcp-http` (the listener's build feature), `slow-tests` (out-of-process
//! child spawns that cold-load the embedder), and `unix` (the daemon socket is
//! unix-domain).
#![cfg(all(unix, feature = "mcp-http", feature = "slow-tests"))]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::serial;
use tempfile::TempDir;

/// The just-built binary under test — spawned as both the daemon and the
/// listener so the seam under test is production wiring, not a harness stand-in.
const CQS_BIN: &str = env!("CARGO_BIN_EXE_cqs");

/// A fixed bearer token supplied to the listener via `CQS_MCP_HTTP_TOKEN` and
/// presented on every request. The listener's auth is unconditional; a mismatch
/// is a 401, so this doubles as a check that the token path is wired.
const BEARER_TOKEN: &str = "e2e-mcp-http-tasks-bearer-token-0123456789";

// ── temp project + empty fixture ─────────────────────────────────────────────

/// Build the temp project the daemon serves: a `.git` marker so the child's
/// `find_project_root` stops here, one tiny source file so `cqs index` has
/// something to embed, and an EMPTY eval fixture (`{"queries":[]}`) that the
/// in-root path guard admits and `eval_core` short-circuits on. Returns the
/// CANONICAL root — the bytes the daemon-socket hash is taken over must match
/// between this harness and the two children.
fn make_project() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonicalize root");
    std::fs::create_dir_all(root.join(".git")).expect("mkdir .git");
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::write(
        src.join("lib.rs"),
        "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .expect("write lib.rs");
    // Empty query set: zero queries, well under the 8 MiB daemon fixture cap.
    // `eval_core` returns a deterministic zero without touching the embedder.
    std::fs::write(root.join("eval_fixture.json"), r#"{"queries":[]}"#).expect("write fixture");
    (dir, root)
}

/// Run `cqs init` then `cqs index` in `root`. Returns `true` on a built index,
/// `false` if `cqs index` failed — the harness-level embedder soft-skip: the
/// index build cold-loads the embedder, so its failure is the model-absent
/// signal a spawned-binary test cannot observe from inside the child.
fn init_and_index(root: &Path) -> bool {
    let init = Command::new(CQS_BIN)
        .args(["init"])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn cqs init");
    assert!(init.success(), "cqs init must succeed");

    let index = Command::new(CQS_BIN)
        .args(["index"])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn cqs index");
    if !index.status.success() {
        eprintln!(
            "cqs index failed (embedder likely unavailable in test env); skipping (#1305)\n{}",
            String::from_utf8_lossy(&index.stderr)
        );
    }
    index.status.success()
}

/// Compute the daemon socket path the two children agree on. Both children take
/// `XDG_RUNTIME_DIR = socket_dir`; the harness mirrors that via the thread-local
/// test override so the readiness probe dials the SAME path the daemon binds.
fn daemon_socket_for(root: &Path, socket_dir: &Path) -> PathBuf {
    cqs::daemon_translate::set_socket_dir_override_for_test(Some(socket_dir.to_path_buf()));
    let path = cqs::daemon_translate::daemon_socket_path(&root.join(".cqs"));
    cqs::daemon_translate::set_socket_dir_override_for_test(None);
    path
}

// ── daemon child (real `cqs watch --serve`) ──────────────────────────────────

/// RAII owner of the spawned daemon. `Drop` SIGKILLs + reaps so a panicking test
/// never leaks a live daemon holding the socket.
struct DaemonHarness {
    child: Option<Child>,
}

impl DaemonHarness {
    fn spawn(root: &Path, socket_dir: &Path) -> Self {
        let child = Command::new(CQS_BIN)
            .args(["watch", "--serve"])
            .current_dir(root)
            .env("XDG_RUNTIME_DIR", socket_dir)
            // Quiet the daemon's own churn; the harness probes the socket, not
            // the log, so nothing here is parsed.
            .env("RUST_LOG", "cqs=warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cqs watch --serve");
        DaemonHarness { child: Some(child) }
    }
}

impl Drop for DaemonHarness {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Poll `UnixStream::connect` on the daemon socket until it accepts or the
/// deadline lapses. Connect success means the accept loop is bound; the eval-task
/// dial that follows waits briefly in the backlog if the loop is mid-boot. Bounded
/// active liveness — never a bare sleep-then-assume.
fn wait_socket_ready(sock_path: &Path, deadline: Duration) {
    let start = Instant::now();
    loop {
        if UnixStream::connect(sock_path).is_ok() {
            return;
        }
        if start.elapsed() > deadline {
            panic!(
                "daemon socket {} not accepting within {deadline:?}",
                sock_path.display()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

// ── listener child (real `cqs mcp --http --port 0`) ──────────────────────────

/// RAII owner of the spawned HTTP listener plus its parsed loopback address.
/// `Drop` SIGKILLs + reaps.
struct ListenerHarness {
    child: Option<Child>,
    addr: String,
}

impl ListenerHarness {
    fn spawn(root: &Path, socket_dir: &Path) -> Self {
        let mut child = Command::new(CQS_BIN)
            .args(["mcp", "--http", "0", "--bind", "127.0.0.1"])
            .current_dir(root)
            .env("XDG_RUNTIME_DIR", socket_dir)
            .env("CQS_MCP_HTTP_TOKEN", BEARER_TOKEN)
            // The listening banner is a `tracing::info!` on stderr; keep cqs at
            // info so it renders, and give the daemon dial a generous budget in
            // case the accept loop is still booting when the first call lands.
            .env("RUST_LOG", "cqs=info")
            .env("CQS_DAEMON_TIMEOUT_MS", "15000")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cqs mcp --http");

        // The banner lands on stderr. Drain it on a thread (a dropped pipe would
        // EPIPE the child's next log write); the first line carrying both the
        // listening marker and the `addr=` field wins.
        let stderr = child.stderr.take().expect("child stderr");
        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut sent = false;
            for line in reader.lines().map_while(Result::ok) {
                // The banner is a `tracing::info!` whose fmt layer wraps field
                // separators in ANSI SGR codes even on a piped (non-TTY) stderr,
                // so `addr=` is not a literal substring until they are stripped.
                let clean = strip_ansi(&line);
                if !sent && clean.contains("cqs mcp --http listening") && clean.contains("addr=") {
                    let _ = tx.send(clean);
                    sent = true;
                }
                // Keep draining the rest for the child's lifetime.
            }
        });

        let banner = rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("timed out waiting for the mcp --http listening banner"));
        let addr = parse_addr(&banner);

        ListenerHarness {
            child: Some(child),
            addr,
        }
    }
}

impl Drop for ListenerHarness {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Strip ANSI CSI escape sequences (`ESC [ … <final-byte>`) from a line so the
/// tracing banner's color codes don't break substring matching / field parsing.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // Consume parameter/intermediate bytes up to the final byte
                // (the first ASCII letter, e.g. the `m` of an SGR sequence).
                for cc in chars.by_ref() {
                    if cc.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Extract `host:port` from the listening banner's `addr=127.0.0.1:<port>` field.
fn parse_addr(banner: &str) -> String {
    let idx = banner
        .find("addr=")
        .unwrap_or_else(|| panic!("banner missing `addr=` field: {banner}"));
    banner[idx + "addr=".len()..]
        .split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("banner `addr=` had no value: {banner}"))
        .to_string()
}

// ── raw HTTP/1.1 client ──────────────────────────────────────────────────────

struct HttpResponse {
    status: u16,
    /// Response header names lowercased.
    headers: HashMap<String, String>,
    body: String,
}

impl HttpResponse {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| {
            panic!(
                "response body not JSON ({e}): status={} {}",
                self.status, self.body
            )
        })
    }
}

/// POST a JSON-RPC message to `POST /mcp` on `addr` with the given extra headers.
/// `Connection: close` so `read_to_end` sees the whole response. Hand-rolled to
/// avoid a reqwest/ureq dev-dep for one endpoint.
fn http_post(addr: &str, extra_headers: &[(&str, &str)], body: &str) -> HttpResponse {
    let mut stream =
        TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect to {addr} failed: {e}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("set_read_timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .expect("set_write_timeout");

    let mut req = String::new();
    req.push_str("POST /mcp HTTP/1.1\r\n");
    req.push_str("Host: 127.0.0.1\r\n");
    req.push_str("Connection: close\r\n");
    req.push_str("Content-Type: application/json\r\n");
    req.push_str("Accept-Encoding: identity\r\n");
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    req.push_str(body);

    stream.write_all(req.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut buf = Vec::new();
    if let Err(e) = stream.read_to_end(&mut buf) {
        // A non-keepalive close can surface as RST on WSL/macOS after a complete
        // response — treat as EOF, not a failure.
        if e.kind() != std::io::ErrorKind::ConnectionReset {
            panic!("read response from {addr}: {e:?}");
        }
    }
    parse_http_response(&buf)
}

fn parse_http_response(buf: &[u8]) -> HttpResponse {
    let text = String::from_utf8_lossy(buf).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response (no header/body split): {text:?}"));

    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("unparseable status line: {status_line:?}"));

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    HttpResponse {
        status,
        headers,
        body: body.to_string(),
    }
}

/// Read `_meta["io.modelcontextprotocol/related-task"].taskId` from a result.
fn related_task_id(result: &Value) -> Option<&str> {
    result
        .get("_meta")
        .and_then(|m| m.get("io.modelcontextprotocol/related-task"))
        .and_then(|r| r.get("taskId"))
        .and_then(|v| v.as_str())
}

// ── the lifecycle test ───────────────────────────────────────────────────────

/// Drive the whole MCP HTTP task lifecycle against a live daemon: initialize →
/// augmented `tools/call` (nested `CreateTaskResult`) → poll `tasks/get` to a
/// terminal `completed` (flat `GetTaskResult`) → `tasks/result` (verbatim
/// `CallToolResult`). Pins the socket-round-trip wiring the mocked unit tests
/// structurally cannot see: envelope framing across the three verbs, `_meta`
/// survival through the real dispatch, and the daemon `outer_lock` gate that
/// makes the task store reachable at all.
#[test]
#[serial]
fn mcp_http_task_lifecycle_augment_get_result() {
    let (_dir, root) = make_project();
    // Dedicated per-project socket dir; the daemon secures it 0o700 at bind.
    let socket_dir = root.clone();

    if !init_and_index(&root) {
        // Model absent — degrade gracefully (no #[ignore]; model-present CI runs it).
        return;
    }

    let sock_path = daemon_socket_for(&root, &socket_dir);
    let _daemon = DaemonHarness::spawn(&root, &socket_dir);
    wait_socket_ready(&sock_path, Duration::from_secs(60));

    let listener = ListenerHarness::spawn(&root, &socket_dir);
    let auth = format!("Bearer {BEARER_TOKEN}");

    // 1. initialize → 200 + a server-minted MCP-Session-Id header.
    let init_body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": { "name": "e2e", "version": "1" } }
    })
    .to_string();
    let init = http_post(&listener.addr, &[("Authorization", &auth)], &init_body);
    assert_eq!(
        init.status, 200,
        "initialize must be 200: body={}",
        init.body
    );
    let session_id = init
        .headers
        .get("mcp-session-id")
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "initialize must issue an MCP-Session-Id: {:?}",
                init.headers
            )
        });
    assert!(!session_id.is_empty(), "session id must be non-empty");

    let session_headers = [
        ("Authorization", auth.as_str()),
        ("MCP-Session-Id", session_id.as_str()),
    ];

    // 2. augmented tools/call on cqs_eval → nested CreateTaskResult.
    //    `ttl` is the spec's milliseconds; 60_000ms resolves to 60s (≤ the 900s
    //    default max) and rides back out in ms, exercising the unit boundary.
    let create_body = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "cqs_eval",
            "arguments": { "query_file": "eval_fixture.json" },
            "task": { "ttl": 60_000 }
        }
    })
    .to_string();
    let create = http_post(&listener.addr, &session_headers, &create_body);
    assert_eq!(
        create.status, 200,
        "create must be 200: body={}",
        create.body
    );
    let create_json = create.json();
    assert_eq!(create_json.get("id").and_then(|v| v.as_u64()), Some(2));
    assert!(
        create_json.get("error").is_none(),
        "create must not be a JSON-RPC error: {create_json}"
    );
    let create_result = create_json
        .get("result")
        .unwrap_or_else(|| panic!("create must carry a result: {create_json}"));
    // NESTED: the task lives under `result.task`, never hoisted to the top level.
    let task = create_result
        .get("task")
        .unwrap_or_else(|| panic!("CreateTaskResult must nest the task: {create_result}"));
    assert!(
        create_result.get("taskId").is_none(),
        "CreateTaskResult must NOT hoist task fields to the top level"
    );
    let task_id = task
        .get("taskId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("created task must carry a non-empty taskId: {task}"))
        .to_string();
    assert!(
        task.get("status").and_then(|v| v.as_str()).is_some(),
        "created task must carry a status: {task}"
    );
    assert_eq!(
        task.get("pollInterval").and_then(|v| v.as_u64()),
        Some(5_000),
        "created task must advertise the client pollInterval"
    );
    assert_eq!(
        task.get("ttl").and_then(|v| v.as_u64()),
        Some(60_000),
        "wire ttl is milliseconds (60s round-tripped)"
    );
    assert_eq!(
        related_task_id(create_result),
        Some(task_id.as_str()),
        "CreateTaskResult must carry the related-task _meta"
    );

    // 3. tasks/get, polled to a terminal status. Flat GetTaskResult shape.
    let terminal_status = poll_until_terminal(&listener.addr, &session_headers, &task_id);
    assert_eq!(
        terminal_status, "completed",
        "an empty-fixture eval must terminalize as completed, got {terminal_status}"
    );

    // 4. tasks/result → the verbatim CallToolResult carrying the eval output.
    let result_body = json!({
        "jsonrpc": "2.0", "id": 99, "method": "tasks/result",
        "params": { "taskId": task_id }
    })
    .to_string();
    let result = http_post(&listener.addr, &session_headers, &result_body);
    assert_eq!(
        result.status, 200,
        "tasks/result must be 200: body={}",
        result.body
    );
    let result_json = result.json();
    assert_eq!(result_json.get("id").and_then(|v| v.as_u64()), Some(99));
    assert!(
        result_json.get("error").is_none(),
        "a completed eval's tasks/result is a successful response, not a JSON-RPC error: {result_json}"
    );
    let call_tool_result = result_json
        .get("result")
        .unwrap_or_else(|| panic!("tasks/result must carry a result: {result_json}"));
    assert_eq!(
        call_tool_result.get("isError").and_then(|v| v.as_bool()),
        Some(false),
        "a completed eval maps to isError:false: {call_tool_result}"
    );
    assert!(
        call_tool_result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .is_some(),
        "CallToolResult must carry a content[0].text mirror: {call_tool_result}"
    );
    // The verbatim eval payload: the empty fixture scored zero queries.
    assert_eq!(
        call_tool_result
            .get("structuredContent")
            .and_then(|s| s.get("query_count"))
            .and_then(|v| v.as_u64()),
        Some(0),
        "structuredContent must carry the empty-fixture eval output: {call_tool_result}"
    );
    assert_eq!(
        related_task_id(call_tool_result),
        Some(task_id.as_str()),
        "tasks/result must carry the related-task _meta"
    );

    drop(listener);
    drop(_daemon);
}

/// Poll `tasks/get` until the task reports a terminal status, returning it.
/// Asserts the flat `GetTaskResult` shape on the way. Bounded by a deadline with
/// a tight cadence (faster than the advertised client pollInterval) so an
/// empty-fixture eval that terminalizes in milliseconds is read promptly.
fn poll_until_terminal(addr: &str, headers: &[(&str, &str)], task_id: &str) -> String {
    let start = Instant::now();
    let deadline = Duration::from_secs(60);
    let mut rid = 10u64;
    loop {
        rid += 1;
        let body = json!({
            "jsonrpc": "2.0", "id": rid, "method": "tasks/get",
            "params": { "taskId": task_id }
        })
        .to_string();
        let resp = http_post(addr, headers, &body);
        assert_eq!(
            resp.status, 200,
            "tasks/get must be 200: body={}",
            resp.body
        );
        let resp_json = resp.json();
        assert!(
            resp_json.get("error").is_none(),
            "tasks/get on a live task must not error: {resp_json}"
        );
        let result = resp_json
            .get("result")
            .unwrap_or_else(|| panic!("tasks/get must carry a result: {resp_json}"));
        // FLAT: task fields at the top level of `result`, never nested.
        assert_eq!(
            result.get("taskId").and_then(|v| v.as_str()),
            Some(task_id),
            "GetTaskResult must be flat with taskId at the top level: {result}"
        );
        assert!(
            result.get("task").is_none(),
            "GetTaskResult must NOT nest the task: {result}"
        );
        assert_eq!(
            result.get("pollInterval").and_then(|v| v.as_u64()),
            Some(5_000),
            "GetTaskResult must advertise the client pollInterval"
        );
        assert_eq!(
            related_task_id(result),
            Some(task_id),
            "GetTaskResult must carry the related-task _meta"
        );

        let status = result
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("GetTaskResult must carry a status: {result}"))
            .to_string();
        if status != "working" {
            return status;
        }
        if start.elapsed() > deadline {
            panic!("task {task_id} did not terminalize within {deadline:?} (last status: working)");
        }
        thread::sleep(Duration::from_millis(100));
    }
}
