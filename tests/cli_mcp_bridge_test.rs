//! Integration smoke for the `cqs mcp` stdio↔daemon-socket bridge
//! (MCP Phase 1, Lane 2).
//!
//! HERMETIC: this does NOT touch the installed daemon / systemd service. It
//! spawns the just-built `cqs mcp` binary as a child, pointed (via a private
//! `XDG_RUNTIME_DIR`) at a MOCK daemon bound at the exact socket path the
//! bridge computes for the temp project. The mock returns canned daemon
//! envelopes, so the test exercises the bridge end-to-end — stdio NDJSON
//! framing, JSON-RPC routing, the Lane 1 JSON-args frame it sends, and the
//! envelope→`CallToolResult` classification (including the Blocker #1
//! error-mapping invariant) — without a GPU/model load. The real json-args
//! dispatch is Lane 1's tested territory; this lane owns the bridge.
//!
//! `#![cfg(unix)]`: the daemon socket is unix-only.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

/// Build the temp project: a `.git` marker (so the child's `find_project_root`
/// stops here deterministically) and a `.cqs/` dir (so `resolve_index_dir`
/// returns `<root>/.cqs` rather than walking a worktree fallback). Returns the
/// CANONICAL project root (the bytes the socket-path hash is taken over must
/// match between the mock side and the child side).
fn make_project() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonicalize root");
    std::fs::create_dir_all(root.join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(root.join(".cqs")).expect("mkdir .cqs");
    let cqs_dir = root.join(".cqs");
    (dir, root, cqs_dir)
}

/// A canned daemon: bind a `UnixListener` at `socket_path`, accept connections
/// in a loop, and reply to each with an envelope chosen by the request's
/// `command`. Returns a handle to stop it and join.
struct MockDaemon {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockDaemon {
    fn start(socket_path: PathBuf) -> Self {
        // Clean any stale socket.
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind mock daemon socket");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Each daemon connection is one request → one response.
                        handle_conn(stream);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        MockDaemon {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for MockDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Read one request line, choose a canned envelope by `command`, write it back.
fn handle_conn(mut stream: UnixStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let req: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return,
    };
    let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");
    // The bridge MUST send the Lane 1 JSON-args frame: an `arguments` object,
    // never an argv `args` array. Echo nothing if the contract is violated so
    // the test's response assertion fails loudly.
    let has_arguments_object = matches!(req.get("arguments"), Some(Value::Object(_)));

    let envelope = if !has_arguments_object {
        json!({"status": "error", "message": "expected an `arguments` object frame"})
    } else {
        match command {
            // A normal success: data + envelope _meta.
            "callers" => json!({
                "status": "ok",
                "output": {
                    "data": {
                        "function": "callee_fn",
                        "callers": [{ "name": "caller_fn", "edge_kind": "call" }]
                    },
                    "_meta": { "stale_origins": [] }
                }
            }),
            // A handler error riding under status:"ok" (Blocker #1).
            "impact" => json!({
                "status": "ok",
                "output": {
                    "error": { "code": "not_found", "message": "function 'ghost' not found" }
                }
            }),
            // search: minimal data payload, used for the structuredContent check.
            "search" => json!({
                "status": "ok",
                "output": { "data": { "results": [{ "name": "hit", "score": 0.9 }] } }
            }),
            // notes-add (Phase 2a gated mutation): the daemon's notes-write
            // success envelope. The bridge relayed the `notes-add` json-args
            // frame, proving the gated mutation tool reaches the daemon.
            "notes-add" => json!({
                "status": "ok",
                "output": { "data": {
                    "status": "added",
                    "text_preview": "from the bridge",
                    "file": "docs/notes.toml",
                    "indexed": false,
                    "total_notes": 0,
                    "reindex_deferred": true
                } }
            }),
            _ => json!({"status": "error", "message": format!("unexpected command {command}")}),
        }
    };
    let mut buf = serde_json::to_vec(&envelope).expect("serialize envelope");
    buf.push(b'\n');
    let _ = stream.write_all(&buf);
    let _ = stream.flush();
}

/// A live `cqs mcp` child plus its piped stdin/stdout, with a line reader.
struct Bridge {
    child: Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

impl Bridge {
    /// Spawn `cqs mcp` with cwd = project root and `XDG_RUNTIME_DIR` =
    /// `socket_dir`, so the child computes the same daemon socket path the mock
    /// bound. `CQS_NO_DAEMON` is intentionally NOT set (the bridge is itself the
    /// daemon client; the env knob governs the *other* daemon-forward path).
    fn spawn(root: &Path, socket_dir: &Path) -> Self {
        Self::spawn_with_env(root, socket_dir, &[])
    }

    /// Spawn with extra env pairs (e.g. `CQS_MCP_ENABLE_MUTATIONS=1` for the
    /// Phase-2a gated-mutation path). Each child has its OWN process env, so
    /// setting the flag here does not race other tests' process env.
    fn spawn_with_env(root: &Path, socket_dir: &Path, extra_env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cqs"));
        cmd.arg("mcp")
            .current_dir(root)
            .env("XDG_RUNTIME_DIR", socket_dir)
            // Keep the child's timeout snappy so a missing reply fails the test
            // fast rather than hanging.
            .env("CQS_DAEMON_TIMEOUT_MS", "5000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn cqs mcp");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        Bridge {
            child,
            stdin,
            reader: BufReader::new(stdout),
        }
    }

    /// Send one JSON-RPC request line.
    fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).expect("serialize request");
        writeln!(self.stdin, "{line}").expect("write to child stdin");
        self.stdin.flush().expect("flush child stdin");
    }

    /// Write raw bytes straight to the child stdin (no framing, no trailing
    /// newline) — used to feed an oversized no-newline blob.
    fn send_raw_bytes(&mut self, bytes: &[u8]) {
        self.stdin.write_all(bytes).expect("write raw bytes");
        self.stdin.flush().expect("flush raw bytes");
    }

    /// Read one response line, parsed as JSON. Panics on EOF / timeout.
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).expect("read child stdout");
        assert!(n > 0, "bridge closed stdout before responding");
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("bridge response not JSON ({e}): {line}"))
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // Closing stdin → the bridge's stdin loop hits EOF and exits.
        // (Take and drop the stdin handle explicitly.)
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// End-to-end session: initialize → tools/list → tools/call (success) drives
/// the full bridge with a mock daemon behind the socket.
#[test]
fn stdio_round_trip_smoke() {
    let (_dir, root, cqs_dir) = make_project();
    // The mock daemon and the child both resolve the socket path from
    // `XDG_RUNTIME_DIR = socket_dir` + the canonical cqs_dir bytes.
    let socket_dir = root.clone();
    // Compute the socket path WITHOUT mutating process-wide env (which races
    // across parallel tests). The thread-local override sets the dir on this
    // test thread; the child gets the matching dir via `Command::env`
    // (`XDG_RUNTIME_DIR`). Both compute `socket_dir/cqs-<hash(cqs_dir)>.sock`.
    cqs::daemon_translate::set_socket_dir_override_for_test(Some(socket_dir.clone()));
    let socket_path = cqs::daemon_translate::daemon_socket_path(&cqs_dir);
    let _daemon = MockDaemon::start(socket_path);

    let mut bridge = Bridge::spawn(&root, &socket_dir);

    // 1. initialize → handshake.
    bridge.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name":"smoke","version":"1"} }
    }));
    let init = bridge.recv();
    assert_eq!(init.get("id").and_then(|v| v.as_u64()), Some(1));
    let result = init.get("result").expect("initialize result");
    assert_eq!(
        result.get("protocolVersion").and_then(|v| v.as_str()),
        Some("2025-11-25"),
        "handshake must advertise the P1 protocol version"
    );
    assert_eq!(
        result
            .get("serverInfo")
            .and_then(|s| s.get("name"))
            .and_then(|v| v.as_str()),
        Some("cqs")
    );

    // 2. notifications/initialized → NO response (notification).
    bridge.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));

    // 3. tools/list → cqs_search present, context/explain absent, schemas present.
    bridge.send(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let listed = bridge.recv();
    assert_eq!(listed.get("id").and_then(|v| v.as_u64()), Some(2));
    let tools = listed
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(names.contains(&"cqs_search"), "cqs_search must be listed");
    assert!(names.contains(&"cqs_callers"), "cqs_callers must be listed");
    assert!(
        !names.contains(&"cqs_context"),
        "cqs_context must be withheld (D4b)"
    );
    assert!(
        !names.contains(&"cqs_explain"),
        "cqs_explain must be withheld (D4b)"
    );
    // Every listed tool carries an inputSchema object.
    for t in tools {
        let schema = t.get("inputSchema").expect("inputSchema");
        assert_eq!(
            schema.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "tool {} inputSchema must be an object",
            t.get("name").and_then(|n| n.as_str()).unwrap_or("?")
        );
    }

    // 4. tools/call cqs_search → valid CallToolResult with structuredContent.
    bridge.send(&json!({
        "jsonrpc":"2.0","id":3,"method":"tools/call",
        "params": { "name": "cqs_search", "arguments": { "query": "anything" } }
    }));
    let called = bridge.recv();
    assert_eq!(called.get("id").and_then(|v| v.as_u64()), Some(3));
    let call_result = called.get("result").expect("tools/call result");
    assert_eq!(
        call_result.get("isError").and_then(|v| v.as_bool()),
        Some(false),
        "a successful search must not be an error"
    );
    let structured = call_result
        .get("structuredContent")
        .expect("structuredContent present on success");
    assert!(
        structured.get("results").is_some(),
        "structuredContent must carry the handler data"
    );
    // The content[text] mirror is also present.
    assert!(call_result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|b| b.get("text"))
        .is_some());
}

/// Blocker #1: a handler error riding under the daemon's outer `status:"ok"`
/// becomes a `CallToolResult{isError:true}`, NOT a JSON-RPC protocol error and
/// NOT a false success. Driven end-to-end through the child.
#[test]
fn handler_error_becomes_is_error_true() {
    let (_dir, root, cqs_dir) = make_project();
    let socket_dir = root.clone();
    // Compute the socket path WITHOUT mutating process-wide env (which races
    // across parallel tests). The thread-local override sets the dir on this
    // test thread; the child gets the matching dir via `Command::env`
    // (`XDG_RUNTIME_DIR`). Both compute `socket_dir/cqs-<hash(cqs_dir)>.sock`.
    cqs::daemon_translate::set_socket_dir_override_for_test(Some(socket_dir.clone()));
    let socket_path = cqs::daemon_translate::daemon_socket_path(&cqs_dir);
    let _daemon = MockDaemon::start(socket_path);

    let mut bridge = Bridge::spawn(&root, &socket_dir);
    bridge.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    let _ = bridge.recv();

    // cqs_impact on a ghost symbol → the mock returns a status:"ok"-wrapped
    // handler error.
    bridge.send(&json!({
        "jsonrpc":"2.0","id":7,"method":"tools/call",
        "params": { "name": "cqs_impact", "arguments": { "name": "ghost" } }
    }));
    let resp = bridge.recv();
    // It is a SUCCESS at the JSON-RPC layer (has `result`, no `error`)...
    assert!(
        resp.get("error").is_none(),
        "handler error must NOT be a JSON-RPC protocol error: {resp}"
    );
    let result = resp.get("result").expect("tools/call result");
    // ...but the CallToolResult is flagged isError:true.
    assert_eq!(
        result.get("isError").and_then(|v| v.as_bool()),
        Some(true),
        "a status:ok-wrapped handler error must map to isError:true (Blocker #1): {result}"
    );
    // The redacted message reaches the client.
    let text = result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    assert!(
        text.contains("not found"),
        "error text must surface: {text}"
    );
}

/// Protocol-layer failures: an unknown method → -32601, malformed JSON →
/// -32700, an unknown tool → -32601. Driven through the child; no daemon needed
/// for the method/parse cases.
#[test]
fn protocol_errors_map_to_jsonrpc_codes() {
    let (_dir, root, cqs_dir) = make_project();
    let socket_dir = root.clone();
    // Compute the socket path WITHOUT mutating process-wide env (which races
    // across parallel tests). The thread-local override sets the dir on this
    // test thread; the child gets the matching dir via `Command::env`
    // (`XDG_RUNTIME_DIR`). Both compute `socket_dir/cqs-<hash(cqs_dir)>.sock`.
    cqs::daemon_translate::set_socket_dir_override_for_test(Some(socket_dir.clone()));
    let socket_path = cqs::daemon_translate::daemon_socket_path(&cqs_dir);
    let _daemon = MockDaemon::start(socket_path);

    let mut bridge = Bridge::spawn(&root, &socket_dir);
    bridge.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    let _ = bridge.recv();

    // Unknown method → -32601.
    bridge.send(&json!({"jsonrpc":"2.0","id":2,"method":"no/such/method","params":{}}));
    let r = bridge.recv();
    assert_eq!(
        r.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64()),
        Some(-32601),
        "unknown method must be -32601: {r}"
    );

    // Unknown tool name → -32601.
    bridge.send(&json!({
        "jsonrpc":"2.0","id":3,"method":"tools/call",
        "params": { "name": "cqs_does_not_exist", "arguments": {} }
    }));
    let r = bridge.recv();
    assert_eq!(
        r.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64()),
        Some(-32601),
        "unknown tool must be -32601: {r}"
    );

    // Malformed JSON line → -32700 (parse error, no id).
    write_raw_line(&mut bridge, "{ this is not json");
    let r = bridge.recv();
    assert_eq!(
        r.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64()),
        Some(-32700),
        "malformed JSON must be -32700: {r}"
    );

    // Malformed arguments (wrong-typed field) → -32602 invalid params.
    bridge.send(&json!({
        "jsonrpc":"2.0","id":5,"method":"tools/call",
        "params": { "name": "cqs_callers", "arguments": { "name": 42 } }
    }));
    let r = bridge.recv();
    assert_eq!(
        r.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64()),
        Some(-32602),
        "malformed arguments must be -32602: {r}"
    );
}

/// D4a: with NO daemon running, a `tools/call` fails clean with a transport
/// protocol error (no in-process fallback, no GPU load, no stdout leak).
#[test]
fn no_daemon_fails_clean() {
    let (_dir, root, _cqs_dir) = make_project();
    let socket_dir = root.clone();
    // Intentionally do NOT start a mock daemon — the socket does not exist. The
    // child gets `XDG_RUNTIME_DIR` via `Command::env`, so it computes a socket
    // path that points at nothing (no process-env mutation needed here).

    let mut bridge = Bridge::spawn(&root, &socket_dir);
    bridge.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    let _ = bridge.recv();

    bridge.send(&json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call",
        "params": { "name": "cqs_callers", "arguments": { "name": "foo" } }
    }));
    let r = bridge.recv();
    // No daemon → internal/transport protocol error (-32603), with advice.
    assert_eq!(
        r.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64()),
        Some(-32603),
        "missing daemon must be -32603: {r}"
    );
    let msg = r
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("daemon"),
        "the error must mention the daemon: {msg}"
    );
}

/// MCP Phase 2a: with `CQS_MCP_ENABLE_MUTATIONS=1` in the child env, the bridge
/// advertises `cqs_notes_add` in `tools/list` (with mutating annotations) and a
/// `tools/call cqs_notes_add` relays the `notes-add` json-args frame to the
/// daemon, mapping the success envelope to `isError:false`. Driven end-to-end
/// through the child — proves the gated mutation channel works over the bridge.
#[test]
fn gated_notes_add_round_trips_when_flag_set() {
    let (_dir, root, cqs_dir) = make_project();
    let socket_dir = root.clone();
    cqs::daemon_translate::set_socket_dir_override_for_test(Some(socket_dir.clone()));
    let socket_path = cqs::daemon_translate::daemon_socket_path(&cqs_dir);
    let _daemon = MockDaemon::start(socket_path);

    let mut bridge =
        Bridge::spawn_with_env(&root, &socket_dir, &[("CQS_MCP_ENABLE_MUTATIONS", "1")]);
    bridge.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    let _ = bridge.recv();

    // tools/list → cqs_notes_add present with mutating annotations.
    bridge.send(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let listed = bridge.recv();
    let tools = listed
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .expect("tools array");
    let add = tools
        .iter()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("cqs_notes_add"))
        .expect("cqs_notes_add must be listed when the flag is set");
    let ann = add.get("annotations").expect("annotations");
    assert_eq!(
        ann.get("readOnlyHint").and_then(|v| v.as_bool()),
        Some(false),
        "notes_add is mutating, not read-only"
    );
    assert_eq!(
        ann.get("destructiveHint").and_then(|v| v.as_bool()),
        Some(false),
        "notes_add is additive, not destructive"
    );
    // notes_remove must also be present and carry destructiveHint:true.
    let remove = tools
        .iter()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("cqs_notes_remove"))
        .expect("cqs_notes_remove must be listed when the flag is set");
    assert_eq!(
        remove
            .get("annotations")
            .and_then(|a| a.get("destructiveHint"))
            .and_then(|v| v.as_bool()),
        Some(true),
        "notes_remove carries destructiveHint:true"
    );

    // tools/call cqs_notes_add → success CallToolResult (relayed `notes-add`).
    bridge.send(&json!({
        "jsonrpc":"2.0","id":3,"method":"tools/call",
        "params": { "name": "cqs_notes_add", "arguments": { "text": "from the bridge", "sentiment": -0.5 } }
    }));
    let called = bridge.recv();
    let result = called.get("result").expect("tools/call result");
    assert_eq!(
        result.get("isError").and_then(|v| v.as_bool()),
        Some(false),
        "a successful notes-add must not be an error: {result}"
    );
    let structured = result
        .get("structuredContent")
        .expect("structuredContent present");
    assert_eq!(
        structured.get("status").and_then(|v| v.as_str()),
        Some("added")
    );
    assert_eq!(
        structured.get("reindex_deferred").and_then(|v| v.as_bool()),
        Some(true),
        "daemon defers the reindex to the watch loop"
    );
}

/// MCP Phase 2a: WITHOUT the flag, the bridge's `tools/list` must NOT advertise
/// any notes mutator, and a `tools/call cqs_notes_add` is an unknown tool
/// (-32601) — the bridge can't even route it. Boundary by absence + opt-in.
#[test]
fn gated_notes_tools_absent_without_flag() {
    let (_dir, root, cqs_dir) = make_project();
    let socket_dir = root.clone();
    cqs::daemon_translate::set_socket_dir_override_for_test(Some(socket_dir.clone()));
    let socket_path = cqs::daemon_translate::daemon_socket_path(&cqs_dir);
    let _daemon = MockDaemon::start(socket_path);

    // No CQS_MCP_ENABLE_MUTATIONS in the child env.
    let mut bridge = Bridge::spawn(&root, &socket_dir);
    bridge.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    let _ = bridge.recv();

    bridge.send(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let listed = bridge.recv();
    let names: Vec<String> = listed
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .expect("tools array")
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    for tool in ["cqs_notes_add", "cqs_notes_update", "cqs_notes_remove"] {
        assert!(
            !names.contains(&tool.to_string()),
            "{tool} must be absent from tools/list without the flag"
        );
    }

    // Calling it is an unknown tool — the bridge can't route what it doesn't list.
    bridge.send(&json!({
        "jsonrpc":"2.0","id":3,"method":"tools/call",
        "params": { "name": "cqs_notes_add", "arguments": { "text": "blocked" } }
    }));
    let r = bridge.recv();
    assert_eq!(
        r.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64()),
        Some(-32601),
        "notes_add must be unknown (-32601) without the flag: {r}"
    );
}

/// Resource bound: a >1 MiB stdin line with NO newline must NOT OOM the
/// long-lived bridge. The bounded reader caps the per-line read at 1 MiB + 1,
/// so the bridge responds with a clean PARSE_ERROR (request too large) rather
/// than buffering the whole blob. Driven through the child; no daemon needed
/// (the request never reaches relay). Pins item 10's bounded-read fix.
#[test]
fn oversized_no_newline_line_is_bounded_parse_error() {
    let (_dir, root, _cqs_dir) = make_project();
    let socket_dir = root.clone();
    let mut bridge = Bridge::spawn(&root, &socket_dir);
    bridge.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    let _ = bridge.recv();

    // 2 MiB of 'x' followed by a newline — the line body exceeds the 1 MiB cap.
    // The bounded reader stops at cap+1 bytes (well before the newline), fires
    // the oversized branch, then drains the rest of the line to the newline so
    // the next request starts on a clean boundary.
    let mut blob = vec![b'x'; 2 * 1024 * 1024];
    blob.push(b'\n');
    bridge.send_raw_bytes(&blob);

    let r = bridge.recv();
    assert_eq!(
        r.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64()),
        Some(-32700),
        "an oversized line must be a -32700 parse error: {r}"
    );
    let msg = r
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("too large"),
        "the error must say the request is too large: {msg}"
    );

    // The bridge is still ALIVE after the oversized line — a normal request
    // still gets a response (it didn't OOM or wedge).
    bridge.send(&json!({"jsonrpc":"2.0","id":2,"method":"ping","params":{}}));
    let pong = bridge.recv();
    assert_eq!(
        pong.get("id").and_then(|v| v.as_u64()),
        Some(2),
        "bridge must survive the oversized line and keep serving: {pong}"
    );
}

/// JSON-RPC `ping` keepalive: a client may send `ping` to keep the session
/// warm. The bridge replies with an empty-object result echoing the id (it is
/// the JSON-RPC utility method, distinct from the `cqs ping` daemon command).
/// Pins bridge.rs's `ping` arm. No daemon needed.
#[test]
fn ping_keepalive_returns_empty_result() {
    let (_dir, root, _cqs_dir) = make_project();
    let socket_dir = root.clone();
    let mut bridge = Bridge::spawn(&root, &socket_dir);
    bridge.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    let _ = bridge.recv();

    bridge.send(&json!({"jsonrpc":"2.0","id":42,"method":"ping","params":{}}));
    let r = bridge.recv();
    assert_eq!(
        r.get("id").and_then(|v| v.as_u64()),
        Some(42),
        "ping must echo the request id: {r}"
    );
    assert!(r.get("error").is_none(), "ping must not be an error: {r}");
    assert_eq!(
        r.get("result"),
        Some(&json!({})),
        "ping result must be an empty object: {r}"
    );
}

/// Write a raw (possibly invalid-JSON) line straight to the child stdin,
/// bypassing the `Value` serializer.
fn write_raw_line(bridge: &mut Bridge, raw: &str) {
    writeln!(bridge.stdin, "{raw}").expect("write raw line");
    bridge.stdin.flush().expect("flush raw line");
}
