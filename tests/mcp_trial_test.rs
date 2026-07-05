//! Full-surface MCP trial (issue #2176): drive EVERY advertised `cqs mcp` tool
//! through a real out-of-process stdio JSON-RPC session and assert each routes
//! to a well-formed `CallToolResult`. Complements `cli_mcp_bridge_test.rs`
//! (which spot-checks a handful of tools + the error/classification invariants)
//! by pinning the WHOLE tool set: a tool advertised in `tools/list` but not
//! routable (missing `command_core_map!` row, catch-all schema, drifted
//! dispatch) would go RED here rather than shipping an advertised-but-dead tool.
//!
//! HERMETIC + GPU-FREE: like the sibling bridge test, this spawns the just-built
//! `cqs mcp` child pointed (via a private `XDG_RUNTIME_DIR`) at a GENERIC MOCK
//! daemon that echoes a success envelope for ANY relayed command. No model /
//! HNSW / real index is loaded, so it is safe under `slow-tests` in CI. The real
//! json-args dispatch is Lane 1's tested territory; this lane owns the bridge's
//! advertise→route→classify surface across the full registry.
//!
//! Gated on `slow-tests` (out-of-process child spawns) and `unix` (the daemon
//! socket is unix-only).
#![cfg(all(unix, feature = "slow-tests"))]

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

/// The overlay-capable tools (11 as of #2114) — the tools whose daemon command
/// consumes the worktree-overlay tri-state via `overlay_from_args`, so whose
/// advertised `inputSchema` must declare `overlay` / `no_overlay` /
/// `overlay_root`. Mirrors `cli::mcp::tools`'s `OVERLAY_CAPABLE_TOOLS`; kept here
/// as the out-of-process expectation the live `tools/list` is checked against.
const OVERLAY_CAPABLE: [&str; 11] = [
    "cqs_search",
    "cqs_gather",
    "cqs_scout",
    "cqs_task",
    "cqs_callers",
    "cqs_callees",
    "cqs_impact",
    "cqs_dead",
    "cqs_ci",
    "cqs_review",
    "cqs_impact_diff",
];
const OVERLAY_KEYS: [&str; 3] = ["overlay", "no_overlay", "overlay_root"];

/// The gated mutation tools, present only with `CQS_MCP_ENABLE_MUTATIONS=1`.
const MUTATION_TOOLS: [&str; 4] = [
    "cqs_notes_add",
    "cqs_notes_update",
    "cqs_notes_remove",
    "cqs_index",
];

/// Build the temp project: a `.git` marker so the child's `find_project_root`
/// stops here, and a `.cqs/` dir so `resolve_index_dir` returns `<root>/.cqs`.
/// Returns the CANONICAL project root (the bytes the socket-path hash is taken
/// over must match between the mock side and the child side).
fn make_project() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonicalize root");
    std::fs::create_dir_all(root.join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(root.join(".cqs")).expect("mkdir .cqs");
    let cqs_dir = root.join(".cqs");
    (dir, root, cqs_dir)
}

/// A generic mock daemon: bind at `socket_path`, accept in a loop, and reply to
/// EVERY request with a success envelope echoing the request's `command`. It
/// enforces the Lane 1 frame shape (an `arguments` object) so a bridge that
/// relayed an argv `args` array instead would fail the response assertions.
struct MockDaemon {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockDaemon {
    fn start(socket_path: PathBuf) -> Self {
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
                    Ok((stream, _)) => handle_conn(stream),
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
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

/// Read one request line, echo a generic success envelope keyed by `command`.
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
    let has_arguments_object = matches!(req.get("arguments"), Some(Value::Object(_)));

    // A single generic success envelope for every command: data echoing the
    // command, plus an empty `_meta`. The bridge classifies this as a
    // non-error `CallToolResult` with `structuredContent == data` for EVERY
    // tool, so a well-formed response proves the tool advertised → routed.
    let envelope = if !has_arguments_object {
        json!({"status": "error", "message": "expected an `arguments` object frame"})
    } else {
        json!({
            "status": "ok",
            "output": { "data": { "command": command, "ok": true } }
        })
    };
    let mut buf = serde_json::to_vec(&envelope).expect("serialize envelope");
    buf.push(b'\n');
    let _ = stream.write_all(&buf);
    let _ = stream.flush();
}

/// A live `cqs mcp` child plus its piped stdin/stdout.
struct Bridge {
    child: Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

impl Bridge {
    /// Spawn `cqs mcp` with cwd = project root and `XDG_RUNTIME_DIR` =
    /// `socket_dir` (so the child computes the same daemon socket the mock
    /// bound), plus any extra env (e.g. the mutation opt-in).
    fn spawn(root: &Path, socket_dir: &Path, extra_env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cqs"));
        cmd.arg("mcp")
            .current_dir(root)
            .env("XDG_RUNTIME_DIR", socket_dir)
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

    fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).expect("serialize request");
        writeln!(self.stdin, "{line}").expect("write to child stdin");
        self.stdin.flush().expect("flush child stdin");
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).expect("read child stdout");
        assert!(n > 0, "bridge closed stdout before responding");
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("bridge response not JSON ({e}): {line}"))
    }

    fn initialize(&mut self) {
        self.send(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                        "clientInfo": {"name": "trial", "version": "1"} }
        }));
        let _ = self.recv();
    }

    fn list_tools(&mut self) -> Vec<Value> {
        self.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}));
        self.recv()
            .get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .cloned()
            .expect("tools array")
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Compute the socket path and spawn the mock, mirroring the sibling test's
/// thread-local override (avoids racing process-wide env across parallel tests).
fn mock_for(cqs_dir: &Path, socket_dir: &Path) -> MockDaemon {
    cqs::daemon_translate::set_socket_dir_override_for_test(Some(socket_dir.to_path_buf()));
    let socket_path = cqs::daemon_translate::daemon_socket_path(cqs_dir);
    MockDaemon::start(socket_path)
}

/// A minimal argument object that passes each tool's shape pre-check. Only
/// `cqs_explain` (`name`) and `cqs_plan` (`description`) carry a genuinely
/// required (non-`serde(default)`) field; every other core is fully defaulted,
/// so an empty object suffices.
fn args_for(tool: &str) -> Value {
    match tool {
        "cqs_explain" => json!({ "name": "foo" }),
        "cqs_plan" => json!({ "description": "add a flag" }),
        // `eval` requires `query_file` (no default), so the bridge pre-check
        // rejects a bare `{}`; supply a shape-valid path.
        "cqs_eval" => json!({ "query_file": "evals/q.json" }),
        _ => json!({}),
    }
}

/// Assert a `CallToolResult` is well-formed: a non-empty `content[text]` mirror,
/// a boolean `isError`, and (on success) a `structuredContent`.
fn assert_well_formed(tool: &str, result: &Value) {
    let content = result
        .get("content")
        .and_then(|c| c.as_array())
        .unwrap_or_else(|| panic!("{tool}: CallToolResult must carry a content array: {result}"));
    assert!(
        content
            .first()
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .is_some(),
        "{tool}: content[0].text must be a string: {result}"
    );
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| panic!("{tool}: CallToolResult must carry a boolean isError: {result}"));
    assert!(
        !is_error,
        "{tool}: the generic mock returns success, so isError must be false: {result}"
    );
    assert!(
        result.get("structuredContent").is_some(),
        "{tool}: a successful CallToolResult must carry structuredContent: {result}"
    );
}

/// Full-surface sweep: with the mutation flag ON, `tools/list` advertises 35
/// tools; every one of them, called once with a shape-valid argument object,
/// routes to the daemon and comes back a well-formed success `CallToolResult`.
/// Pins that NO advertised tool is unroutable (a drifted / rowless tool would
/// -32602 or -32601 here instead of returning a result).
#[test]
fn full_tool_surface_routes_through_bridge() {
    let (_dir, root, cqs_dir) = make_project();
    let socket_dir = root.clone();
    let _daemon = mock_for(&cqs_dir, &socket_dir);

    let mut bridge = Bridge::spawn(&root, &socket_dir, &[("CQS_MCP_ENABLE_MUTATIONS", "1")]);
    bridge.initialize();

    let tools = bridge.list_tools();
    let names: Vec<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    assert_eq!(
        names.len(),
        35,
        "flag-on tools/list must advertise 35 tools (31 read + 4 mutation): {names:?}"
    );

    // Call every advertised tool once; each must come back well-formed.
    let mut rid = 100u64;
    for name in &names {
        rid += 1;
        bridge.send(&json!({
            "jsonrpc": "2.0", "id": rid, "method": "tools/call",
            "params": { "name": name, "arguments": args_for(name) }
        }));
        let resp = bridge.recv();
        assert_eq!(
            resp.get("id").and_then(|v| v.as_u64()),
            Some(rid),
            "{name}: response id must echo the request"
        );
        assert!(
            resp.get("error").is_none(),
            "{name}: a valid call must not be a JSON-RPC protocol error: {resp}"
        );
        let result = resp
            .get("result")
            .unwrap_or_else(|| panic!("{name}: tools/call must return a result: {resp}"));
        assert_well_formed(name, result);
        // The relayed command round-tripped: the mock echoes it in `data.command`.
        let echoed = result
            .get("structuredContent")
            .and_then(|s| s.get("command"))
            .and_then(|c| c.as_str());
        assert!(
            echoed.is_some(),
            "{name}: structuredContent must carry the echoed command: {result}"
        );
    }
}

/// Overlay-key advertisement honesty across the wire: exactly the 11
/// overlay-capable tools advertise all three overlay tri-state keys in their
/// `inputSchema`, and no other tool advertises any of them. Driven through the
/// child so it pins the SERIALIZED `tools/list`, not just the in-process
/// registry.
#[test]
fn overlay_keys_advertised_exactly_for_capable_tools() {
    let (_dir, root, cqs_dir) = make_project();
    let socket_dir = root.clone();
    let _daemon = mock_for(&cqs_dir, &socket_dir);

    // Flag on so the (non-overlay) mutators are in scope for the negative check.
    let mut bridge = Bridge::spawn(&root, &socket_dir, &[("CQS_MCP_ENABLE_MUTATIONS", "1")]);
    bridge.initialize();

    let overlay_set: std::collections::BTreeSet<&str> = OVERLAY_CAPABLE.into_iter().collect();
    let mut advertised = std::collections::BTreeSet::new();
    for t in bridge.list_tools() {
        let name = t
            .get("name")
            .and_then(|n| n.as_str())
            .expect("tool name")
            .to_string();
        let props: std::collections::BTreeSet<String> = t
            .get("inputSchema")
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        let has_all = OVERLAY_KEYS.iter().all(|k| props.contains(*k));
        let has_any = OVERLAY_KEYS.iter().any(|k| props.contains(*k));
        if overlay_set.contains(name.as_str()) {
            assert!(
                has_all,
                "overlay-capable `{name}` must advertise all overlay keys; props: {props:?}"
            );
            advertised.insert(name);
        } else {
            assert!(
                !has_any,
                "non-overlay `{name}` must advertise NO overlay key; props: {props:?}"
            );
        }
    }
    let expected: std::collections::BTreeSet<String> =
        OVERLAY_CAPABLE.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        advertised, expected,
        "exactly the 11 overlay-capable tools must advertise the overlay tri-state"
    );
}

/// Bridge-layer mutation double-gate, both flag states, end-to-end through the
/// child. Flag OFF: `tools/list` is 31 read tools, no mutator, and a
/// `cqs_notes_add` call is an unknown tool (-32601 — the bridge can't route what
/// it doesn't advertise). Flag ON: `tools/list` is 35, the 4 mutators are
/// present, and `cqs_notes_add` routes to a success `CallToolResult`.
#[test]
fn bridge_mutation_gate_flag_off_then_on() {
    let (_dir, root, cqs_dir) = make_project();
    let socket_dir = root.clone();
    let _daemon = mock_for(&cqs_dir, &socket_dir);

    // ── Flag OFF ─────────────────────────────────────────────────────────
    {
        let mut bridge = Bridge::spawn(&root, &socket_dir, &[]);
        bridge.initialize();
        let names: Vec<String> = bridge
            .list_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert_eq!(names.len(), 31, "flag-off tools/list must be 31 read tools");
        for m in MUTATION_TOOLS {
            assert!(
                !names.contains(&m.to_string()),
                "flag-off tools/list must NOT advertise `{m}`"
            );
        }
        bridge.send(&json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": { "name": "cqs_notes_add", "arguments": { "text": "blocked" } }
        }));
        let r = bridge.recv();
        assert_eq!(
            r.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64()),
            Some(-32601),
            "flag-off cqs_notes_add must be an unknown tool (-32601): {r}"
        );
    }

    // ── Flag ON ──────────────────────────────────────────────────────────
    {
        let mut bridge = Bridge::spawn(&root, &socket_dir, &[("CQS_MCP_ENABLE_MUTATIONS", "1")]);
        bridge.initialize();
        let names: Vec<String> = bridge
            .list_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert_eq!(names.len(), 35, "flag-on tools/list must be 35 tools");
        for m in MUTATION_TOOLS {
            assert!(
                names.contains(&m.to_string()),
                "flag-on tools/list must advertise `{m}`"
            );
        }
        bridge.send(&json!({
            "jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": { "name": "cqs_notes_add",
                        "arguments": { "text": "from the trial", "sentiment": -0.5 } }
        }));
        let r = bridge.recv();
        assert!(
            r.get("error").is_none(),
            "flag-on cqs_notes_add must route (no protocol error): {r}"
        );
        let result = r.get("result").expect("tools/call result");
        assert_eq!(
            result.get("isError").and_then(|v| v.as_bool()),
            Some(false),
            "flag-on cqs_notes_add relayed to the daemon must be a success: {result}"
        );
    }
}
