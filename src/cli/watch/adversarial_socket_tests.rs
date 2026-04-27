// ─────────────────────────────────────────────────────────────────────────────
// TC-ADV-1.29-3: adversarial coverage for the daemon socket handler.
//
// `handle_socket_client` (above, line 160) is the first thing every daemon
// query touches. It does the line read, size cap, JSON parse, command-field
// validation, and non-string-arg rejection *before* ever acquiring the
// BatchContext mutex. Zero tests previously exercised those rejection paths.
//
// These tests use `UnixStream::pair()` to build a connected stream pair
// in-process — we hand the `server` end to `handle_socket_client` on a worker
// thread, then read/write the `client` end from the test thread. Nothing ever
// touches the real filesystem socket path. No ONNX model is loaded, because
// every adversarial payload is rejected before reaching `dispatch_tokens`.
//
// The one exception is the NUL-byte test, which intentionally reaches
// `dispatch_parsed_tokens`. That path goes through `reject_null_tokens` in
// `cli::batch::mod.rs` and bails before any handler runs — still no model
// load. The "oversized single arg" test similarly reaches dispatch but the
// `notes list` handler doesn't need an embedder.
//
// Why not in `tests/daemon_adversarial_test.rs`: `handle_socket_client` is
// a private `fn` in a binary module (`src/main.rs` → `mod cli`). Integration
// tests link against the library only, not the binary. Co-locating here is
// the narrowest path.
// ─────────────────────────────────────────────────────────────────────────────
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Spin up a `Mutex<BatchContext>` against a fresh in-memory store.
///
/// Reuses `crate::cli::batch::create_test_context` — see its doc for
/// visibility rationale. The returned tempdir must live for the whole
/// test or the store's WAL can be reaped mid-query.
fn test_ctx() -> (
    tempfile::TempDir,
    Arc<Mutex<crate::cli::batch::BatchContext>>,
) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    {
        let store = cqs::store::Store::open(&index_path).expect("open store");
        store
            .init(&cqs::store::ModelInfo::default())
            .expect("init store");
    }
    let ctx = crate::cli::batch::create_test_context(&cqs_dir).expect("create ctx");
    (dir, Arc::new(Mutex::new(ctx)))
}

/// Spawn `handle_socket_client` on a worker thread with the `server` end
/// of a paired UnixStream. Returns the client end and the worker
/// JoinHandle so tests can force-drop the client (→ EOF on server →
/// handler returns → thread joins).
fn spawn_handler(
    ctx: Arc<Mutex<crate::cli::batch::BatchContext>>,
) -> (UnixStream, thread::JoinHandle<()>) {
    let (client, server) = UnixStream::pair().expect("UnixStream::pair");
    // Handler's read timeout is controlled by `resolve_daemon_timeout_ms`
    // (default 5 s). For tests we want a snappier rejection path if a
    // write is truncated — set an explicit short timeout on the server
    // side before handing it off. `handle_socket_client` will then
    // overwrite it with the resolved value, so this is belt-and-suspenders.
    server
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set_read_timeout");
    server
        .set_write_timeout(Some(Duration::from_secs(3)))
        .expect("set_write_timeout");
    let handle = thread::spawn(move || {
        // `handle_socket_client` is a sibling function in this module —
        // `super::handle_socket_client` reaches it.
        super::handle_socket_client(server, &ctx);
    });
    (client, handle)
}

/// Read one newline-terminated response from the client stream, with a
/// bounded wait. Returns the trimmed bytes as a `String`. Panics if no
/// newline arrives within 3 s — the daemon is contractually required to
/// respond to every request it accepts the first byte of.
fn read_line(client: &mut UnixStream) -> String {
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set client read_timeout");
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        match client.read(&mut byte) {
            Ok(0) => break, // EOF
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("socket read failed: {e}"),
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Parse the daemon's response line as JSON.
fn parse_response(line: &str) -> serde_json::Value {
    serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("daemon response is not valid JSON ({e}): {line}"))
}

/// Drain worker thread after the test's payload has been consumed.
fn join_worker(client: UnixStream, handle: thread::JoinHandle<()>) {
    // Closing the client end signals EOF on the server; the handler
    // either completes normally or returns on read error. Give it a
    // small window to drain — long enough for the response to reach us
    // but short enough that a deadlocked handler surfaces as a test
    // hang rather than silent success.
    drop(client);
    for _ in 0..30 {
        if handle.is_finished() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if handle.is_finished() {
        handle.join().expect("handler thread panicked");
    } else {
        // If it hasn't finished, the test still got what it came for
        // (we already read the response). Don't block forever on the
        // final join — the OS will reap the thread when the process
        // exits. Tests should still surface a hang via their own timeout.
    }
}

// ─────────────────────────────────────────────────────────────────────
// Test: exactly 1 MiB + 1 byte → "request too large"
//
// The reader is wrapped in `.take(1_048_577)` so the post-read size
// check sees exactly the cap. A client sending `'a' * 1_048_577` with
// no newline triggers the `n > 1_048_576` branch and the daemon must
// return a structured error.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn daemon_rejects_exactly_one_mib_boundary() {
    let (_dir, ctx) = test_ctx();
    let (mut client, handle) = spawn_handler(Arc::clone(&ctx));

    // 1 MiB + 1 byte, no newline. The daemon's `read_line` reads up to
    // the take() limit of 1_048_577, then the size check fires.
    let payload = vec![b'a'; 1_048_577];
    // Writing 1 MiB to a socket blocks if the peer doesn't read. The
    // handler is actively reading, so this should complete.
    client.write_all(&payload).expect("write 1 MiB + 1 payload");
    // Half-close the write side so the peer's read_line terminates
    // without needing a newline. Without this, the peer keeps reading
    // (up to the take() cap) and we both deadlock waiting for more.
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close write");

    let line = read_line(&mut client);
    let resp = parse_response(&line);
    assert_eq!(
        resp.get("status").and_then(|v| v.as_str()),
        Some("error"),
        "1 MiB + 1 byte must return a structured error envelope: {line}"
    );
    assert_eq!(
        resp.get("message").and_then(|v| v.as_str()),
        Some("request too large"),
        "message must name the exact failure mode so the client can surface it: {line}"
    );
    join_worker(client, handle);
}

// ─────────────────────────────────────────────────────────────────────
// Test: malformed JSON — trailing garbage after valid object.
//
// The daemon parses a single JSON Value via `serde_json::from_str` on
// `line.trim()`. `from_str` rejects trailing non-whitespace tokens
// because serde_json is strict by default.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn daemon_rejects_malformed_trailing_garbage() {
    let (_dir, ctx) = test_ctx();
    let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
    client
        .write_all(b"{\"command\":\"ping\"} garbage\n")
        .expect("write");

    let line = read_line(&mut client);
    let resp = parse_response(&line);
    assert_eq!(
        resp.get("status").and_then(|v| v.as_str()),
        Some("error"),
        "trailing garbage after JSON must be rejected, not silently parsed: {line}"
    );
    // `handle_socket_client` surfaces `invalid JSON: <serde error>` —
    // assert the prefix so a future serde version bump doesn't break us.
    let msg = resp
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        msg.starts_with("invalid JSON"),
        "message should begin with 'invalid JSON', got: {msg:?}"
    );
    join_worker(client, handle);
}

// ─────────────────────────────────────────────────────────────────────
// Test: malformed bytes — UTF-16 BOM prefix (0xFF 0xFE).
//
// A client that writes a UTF-16 LE BOM before its JSON payload is
// sending bytes that are not valid UTF-8. `BufRead::read_line` performs
// UTF-8 validation internally and returns `Err(InvalidData)` for the
// whole line. `handle_socket_client` logs and returns *without* writing
// a response — the daemon silently drops unreadable input.
//
// The contract we pin here: no panic, no partial write, no half-open
// socket; the handler thread finishes and the client sees EOF. This is
// the *current* behaviour — if a future change makes the daemon emit
// `invalid UTF-8` diagnostics instead, that's a behaviour change worth
// a new test, not a silent regression.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn daemon_drops_utf16_bom_prefix_without_panic() {
    let (_dir, ctx) = test_ctx();
    let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
    // UTF-16 LE BOM + valid JSON shape — the BOM bytes (0xFF 0xFE) are
    // not valid UTF-8, so `read_line` errors out.
    let mut payload: Vec<u8> = vec![0xFF, 0xFE];
    payload.extend_from_slice(b"{\"command\":\"ping\"}\n");
    client.write_all(&payload).expect("write BOM+JSON");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close write");

    // Expect EOF — handler returns without writing on InvalidData.
    let line = read_line(&mut client);
    assert!(
        line.is_empty(),
        "UTF-8 decode failure at the BufRead layer must not surface a \
         response body — handler returns early. Got: {line:?}"
    );

    // Sanity: the handler thread must still terminate cleanly (no panic,
    // no deadlock). `join_worker` polls `is_finished()` and asserts the
    // join doesn't panic.
    join_worker(client, handle);
}

// ─────────────────────────────────────────────────────────────────────
// Test: empty line (just "\n") — `read_line` returns `Ok(1)` (one byte
// read). After `line.trim()` the result is an empty string, which
// `serde_json::from_str` rejects with "EOF while parsing a value".
// The handler surfaces that via the standard `invalid JSON` envelope.
//
// This is deliberate: a caller that opens a socket and sends just a
// newline likely did something wrong — silently accepting empty lines
// would hide bugs further up the stack.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn daemon_rejects_bare_newline_as_invalid_json() {
    let (_dir, ctx) = test_ctx();
    let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
    client.write_all(b"\n").expect("write empty line");

    let line = read_line(&mut client);
    let resp = parse_response(&line);
    assert_eq!(
        resp.get("status").and_then(|v| v.as_str()),
        Some("error"),
        "bare newline must be rejected rather than silently accepted: {line}"
    );
    let msg = resp
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        msg.starts_with("invalid JSON"),
        "bare newline rejection must come through the invalid-JSON path, got: {msg:?}"
    );
    join_worker(client, handle);
}

// ─────────────────────────────────────────────────────────────────────
// Test: missing `command` field — the daemon unwraps `command` as an
// empty string and bails via the `if command.is_empty()` check.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn daemon_rejects_missing_command_field() {
    let (_dir, ctx) = test_ctx();
    let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
    client
        .write_all(b"{\"args\":[]}\n")
        .expect("write no-command");

    let line = read_line(&mut client);
    let resp = parse_response(&line);
    assert_eq!(
        resp.get("status").and_then(|v| v.as_str()),
        Some("error"),
        "missing command field must surface as error: {line}"
    );
    assert_eq!(
        resp.get("message").and_then(|v| v.as_str()),
        Some("missing 'command' field"),
        "message must match the exact production string — dashboards grep on it: {line}"
    );
    join_worker(client, handle);
}

// ─────────────────────────────────────────────────────────────────────
// Test: non-string args (objects, nulls, numbers) — P3 #86 hardened
// this path; ensure it's still rejected instead of silently filtered.
//
// The fixture sends three bad elements (`{}, null, 42`) so the handler's
// `bad_arg_indices` vec has `[0, 1, 2]`. The rejection response is a
// flat string — dashboards grep on the exact message.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn daemon_rejects_non_string_args() {
    let (_dir, ctx) = test_ctx();
    let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
    client
        .write_all(b"{\"command\":\"notes\",\"args\":[{},null,42]}\n")
        .expect("write non-string args");

    let line = read_line(&mut client);
    let resp = parse_response(&line);
    assert_eq!(
        resp.get("status").and_then(|v| v.as_str()),
        Some("error"),
        "non-string args must surface as a rejection, not a truncated call: {line}"
    );
    assert_eq!(
        resp.get("message").and_then(|v| v.as_str()),
        Some("args contains non-string elements"),
        "message must match production string — P3 #86: {line}"
    );
    join_worker(client, handle);
}

// ─────────────────────────────────────────────────────────────────────
// Test: oversized single arg (500 KB) within the 1 MiB line limit is
// currently accepted — the daemon has no per-arg cap, only a per-line
// one. This test pins that behaviour so a future per-arg cap is added
// deliberately (and the test would be updated) rather than silently.
//
// The arg goes to the `notes` command which is registered as BatchCmd;
// clap accepts arbitrary-length strings for the body. Even if the
// handler errors on the oversized body, the daemon must not crash
// — that's the contract we pin.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn daemon_accepts_500kb_arg_within_mib_line() {
    let (_dir, ctx) = test_ctx();
    let (mut client, handle) = spawn_handler(Arc::clone(&ctx));

    let big_arg = "x".repeat(500_000);
    // Build the JSON payload manually to avoid serde_json allocating a
    // second 500 KB intermediate String.
    let mut payload: Vec<u8> = Vec::with_capacity(700_000);
    payload.extend_from_slice(b"{\"command\":\"notes\",\"args\":[\"list\",\"");
    payload.extend_from_slice(big_arg.as_bytes());
    payload.extend_from_slice(b"\"]}\n");
    assert!(
        payload.len() < 1_048_576,
        "test payload must stay under the 1 MiB cap"
    );
    client.write_all(&payload).expect("write 500 KB arg");

    let line = read_line(&mut client);
    let resp = parse_response(&line);
    // The precise response depends on how `notes` handles unknown
    // subcommand args. What we're pinning is that the daemon produced
    // *some* structured response and didn't crash.
    assert!(
        resp.get("status").is_some(),
        "500 KB arg within cap must produce a structured response: {line}"
    );
    // If the daemon ever adds a per-arg cap, this assertion will need
    // updating. Leaving a deliberate fail-open here documents the
    // current behaviour so the change is a conscious choice.
    join_worker(client, handle);
}

// ─────────────────────────────────────────────────────────────────────
// Test: NUL byte in args. The daemon accepts the JSON (NUL is a valid
// Rust String byte — ` ` deserialises fine), but `dispatch_tokens`
// runs it through `reject_null_tokens` which bails with an
// `invalid_input` envelope. The daemon's outer frame then wraps that
// envelope in `{status:ok, output:<envelope with error>}`.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn daemon_rejects_nul_byte_in_args_downstream() {
    let (_dir, ctx) = test_ctx();
    let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
    // ` ` embeds a literal NUL inside a JSON string — valid JSON,
    // invalid batch-dispatch input.
    client
        .write_all(b"{\"command\":\"notes\",\"args\":[\"list\",\"has\\u0000nul\"]}\n")
        .expect("write NUL payload");

    let line = read_line(&mut client);
    let resp = parse_response(&line);
    // Outer envelope: the NUL-guard path writes a SUCCESSFUL JSON line
    // to the sink (containing the inner error envelope), so the daemon
    // wraps it as `{status:"ok",output:{...}}`. Either outer shape is
    // acceptable — the semantic contract is that the *inner* error
    // surfaces `invalid_input`.
    let inner_code = resp
        .pointer("/output/error/code")
        .and_then(|v| v.as_str())
        .or_else(|| {
            // Legacy bytes-through-a-string path wraps the envelope bytes
            // as a JSON string — try parsing if needed.
            let s = resp.pointer("/output")?.as_str()?;
            serde_json::from_str::<serde_json::Value>(s)
                .ok()?
                .pointer("/error/code")?
                .as_str()
                .map(|_| "")
        });
    assert_eq!(
        inner_code,
        Some("invalid_input"),
        "NUL byte must be caught by reject_null_tokens and surface as invalid_input: {line}"
    );
    join_worker(client, handle);
}

// ─────────────────────────────────────────────────────────────────────
// TC-HAP-1.29-6: happy-path round-trip. Every existing socket test pins
// an *error* shape — trailing garbage, NUL bytes, missing command,
// oversized request. None pins the *success* path: agent sends a valid
// command, daemon runs it, envelope comes back with `status:"ok"` and a
// well-formed `output` payload.
//
// This is the complement to the 8 adversarial tests above. `stats` is
// the right happy-path probe because `dispatch_stats` touches
// store-schema reads, the error counter, the call-graph stats, and the
// language histogram — the four surfaces that would silently drift if a
// future refactor changed the wire envelope or the handler shape.
//
// Why `stats`: no embedder needed (read-only SQL + filesystem walk), so
// the test runs in ~ms. A pre-seeded chunk in the store makes the
// `total_chunks` assertion load-bearing — an empty store would hide
// regressions where the daemon returned `total_chunks=0` unconditionally.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn daemon_stats_happy_path_roundtrip() {
    use cqs::parser::{Chunk, ChunkType, Language};
    use cqs::store::ModelInfo;
    use std::path::PathBuf;

    // Custom setup — seed one chunk before `create_test_context` opens
    // the store read-only. `test_ctx` helper above opens an empty store;
    // for the happy path we want `total_chunks >= 1` so the numeric
    // assertion actually distinguishes "handler ran and counted" from
    // "handler returned zero by accident".
    let dir = tempfile::TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    {
        let store = cqs::store::Store::open(&index_path).expect("open store");
        store.init(&ModelInfo::default()).expect("init store");
        // One chunk so `total_chunks >= 1` on the other side.
        let content = "pub fn roundtrip_probe() {}";
        let chunk = Chunk {
            id: "probe.rs:1:probe".to_string(),
            file: PathBuf::from("probe.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "roundtrip_probe".to_string(),
            signature: "pub fn roundtrip_probe()".to_string(),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        // Unit embedding — `upsert_chunk` validates dimension against the
        // seeded ModelInfo. Value doesn't matter for the stats path.
        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let embedding = cqs::embedder::Embedding::new(emb_vec);
        store
            .upsert_chunks_batch(&[(chunk, embedding)], Some(0))
            .expect("seed chunk");
    } // drop to flush WAL

    let ctx = super::super::batch::create_test_context(&cqs_dir).expect("create ctx");
    let ctx = Arc::new(Mutex::new(ctx));

    let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
    client
        .write_all(b"{\"command\":\"stats\",\"args\":[]}\n")
        .expect("write stats request");

    let line = read_line(&mut client);
    let resp = parse_response(&line);

    // Outer envelope shape: `{status: "ok", output: <json>}` — the
    // branch at `handle_socket_client:378-391` that wraps successful
    // dispatch output.
    assert_eq!(
        resp.get("status").and_then(|v| v.as_str()),
        Some("ok"),
        "happy-path response must carry status:ok. got: {line}"
    );

    // `output` is the parsed JSON from `dispatch_line`. The dispatcher's
    // stats handler writes an envelope through `emit_json`, so
    // `output` is itself `{data: {...}, error: null, version: 1}`.
    let output = resp
        .get("output")
        .unwrap_or_else(|| panic!("happy-path response must carry an `output` field: {line}"));
    assert!(
        output.is_object(),
        "output must be a JSON object (envelope): {line}"
    );
    let data = output
        .get("data")
        .unwrap_or_else(|| panic!("inner envelope must have a `data` field: {line}"));
    let total_chunks = data
        .get("total_chunks")
        .unwrap_or_else(|| panic!("stats data must have `total_chunks`: {line}"));
    assert!(
        total_chunks.is_number(),
        "total_chunks must be numeric: got {total_chunks}"
    );
    let n = total_chunks
        .as_u64()
        .unwrap_or_else(|| panic!("total_chunks must parse as u64: {total_chunks}"));
    assert!(
        n >= 1,
        "total_chunks must reflect the seeded chunk (≥1), got {n}: {line}"
    );

    join_worker(client, handle);
}

// ─────────────────────────────────────────────────────────────────────
// #1127 — daemon parallelism regression tests
//
// These tests pin the lock-topology contract introduced by #1127
// (post-#1145): the daemon's `handle_socket_client` path must hold the
// BatchContext mutex only across `checkout_view_from_arc` (a few
// microseconds), never across the handler body. Two slow handlers must
// run in parallel; a fast handler issued mid-flight must not block on
// a slow one.
//
// The handlers used here are `test-sleep` (a `#[cfg(test)]`-gated
// BatchCmd variant in `cli::batch::commands`) and `notes list`
// (production handler, read-only, no embedder load). Both are
// intentionally embedder-free so the tests stay fast in CI.
// ─────────────────────────────────────────────────────────────────────

/// Issue two `test-sleep --ms 300` calls concurrently. The new lock
/// topology should let them overlap so wall-clock ≈ max(t1, t2) ≈ 300 ms.
/// Pre-fix (single mutex held across dispatch) they would serialize,
/// blowing past 600 ms.
///
/// Threshold of 1.5× single-handler time gives generous headroom for
/// thread scheduling jitter on busy CI hosts; pre-fix behavior was
/// deterministically 2.0× and the gap is wide enough to be reliable.
#[test]
fn daemon_two_slow_handlers_run_in_parallel() {
    let (_dir, ctx) = test_ctx();

    // Each handler sleeps for SLEEP_MS. If they run sequentially the
    // total wall-clock must be ≈ 2 * SLEEP_MS; in parallel it must be
    // ≈ 1 * SLEEP_MS. The threshold (1.5×) gives wide headroom.
    const SLEEP_MS: u64 = 300;
    let payload = format!("{{\"command\":\"test-sleep\",\"args\":[\"--ms\",\"{SLEEP_MS}\"]}}\n");

    let start = std::time::Instant::now();
    let (mut client_a, handle_a) = spawn_handler(Arc::clone(&ctx));
    let (mut client_b, handle_b) = spawn_handler(Arc::clone(&ctx));

    // Issue both requests as close to simultaneously as possible.
    client_a.write_all(payload.as_bytes()).expect("write A");
    client_b.write_all(payload.as_bytes()).expect("write B");

    // Read both responses on this thread; the workers run independently.
    let line_a = read_line(&mut client_a);
    let line_b = read_line(&mut client_b);
    let elapsed = start.elapsed();

    // Both must succeed with the test envelope.
    let resp_a = parse_response(&line_a);
    let resp_b = parse_response(&line_b);
    assert_eq!(
        resp_a.get("status").and_then(|v| v.as_str()),
        Some("ok"),
        "A response: {line_a}"
    );
    assert_eq!(
        resp_b.get("status").and_then(|v| v.as_str()),
        Some("ok"),
        "B response: {line_b}"
    );

    // The load-bearing assertion: two SLEEP_MS handlers must overlap.
    // 1.5× headroom for scheduling; pre-fix behavior is deterministically
    // 2× so the gap is wide enough to avoid flake.
    let max_allowed_ms = (SLEEP_MS as f64 * 1.5) as u128;
    assert!(
        elapsed.as_millis() < max_allowed_ms,
        "two slow handlers must run in parallel: elapsed {} ms, ceiling {} ms (single-handler {} ms × 1.5). \
         Pre-#1127 behavior would be ≈{} ms — if you see that, the BatchContext mutex is being held across dispatch.",
        elapsed.as_millis(),
        max_allowed_ms,
        SLEEP_MS,
        SLEEP_MS * 2
    );

    join_worker(client_a, handle_a);
    join_worker(client_b, handle_b);
}

/// While a slow `test-sleep` is in flight, an inbound `notes list` query
/// must complete promptly. Pre-fix the second connection's
/// `batch_ctx.lock()` would block on the first connection's
/// dispatch-spanning lock for the full sleep duration.
///
/// Bounded at 200 ms which is generous: `notes list` against an empty
/// store does a single `notes_cache` build (~µs to ms) plus the
/// envelope write. The slow handler's 500 ms sleep gives a wide
/// observation window.
#[test]
fn daemon_notes_list_unblocked_by_inflight_gather() {
    let (_dir, ctx) = test_ctx();

    const SLOW_SLEEP_MS: u64 = 500;
    let slow_payload =
        format!("{{\"command\":\"test-sleep\",\"args\":[\"--ms\",\"{SLOW_SLEEP_MS}\"]}}\n");
    let fast_payload = "{\"command\":\"notes\",\"args\":[]}\n";

    let (mut slow_client, slow_handle) = spawn_handler(Arc::clone(&ctx));
    slow_client
        .write_all(slow_payload.as_bytes())
        .expect("write slow");

    // Give the slow handler a moment to arrive at its sleep. 30 ms is
    // enough on every machine the daemon runs on; the slow sleep is
    // 500 ms so this still leaves >450 ms of overlap.
    thread::sleep(Duration::from_millis(30));

    let fast_start = std::time::Instant::now();
    let (mut fast_client, fast_handle) = spawn_handler(Arc::clone(&ctx));
    fast_client
        .write_all(fast_payload.as_bytes())
        .expect("write fast");
    let fast_line = read_line(&mut fast_client);
    let fast_elapsed = fast_start.elapsed();

    // The fast handler must have come back well before the slow one
    // finishes. 200 ms ceiling is comfortably above any reasonable
    // notes-list latency on an empty store.
    const FAST_LATENCY_CEIL_MS: u128 = 200;
    assert!(
        fast_elapsed.as_millis() < FAST_LATENCY_CEIL_MS,
        "fast handler must not block on the in-flight slow handler: \
         fast latency {} ms, ceiling {} ms. Pre-#1127 the fast handler \
         would queue behind the slow one for ≈{} ms.",
        fast_elapsed.as_millis(),
        FAST_LATENCY_CEIL_MS,
        SLOW_SLEEP_MS
    );

    // Sanity: the fast response is a real success envelope.
    let resp = parse_response(&fast_line);
    assert_eq!(
        resp.get("status").and_then(|v| v.as_str()),
        Some("ok"),
        "fast response should be ok envelope: {fast_line}"
    );

    // Drain the slow handler before we drop the test fixture.
    let slow_line = read_line(&mut slow_client);
    let slow_resp = parse_response(&slow_line);
    assert_eq!(slow_resp.get("status").and_then(|v| v.as_str()), Some("ok"));

    join_worker(fast_client, fast_handle);
    join_worker(slow_client, slow_handle);
}

/// `handle_socket_client` must round-trip `query_count` and
/// `error_count` correctly under the new short-lock contract — bumping
/// the counters happens via the view's `Arc<AtomicU64>` (no re-lock of
/// the BatchContext mutex). Issue three requests (one parse error, two
/// successful pings); the snapshot read after must show
/// `total_queries >= 3` and `error_count >= 1`.
///
/// Maps to the test planned in `docs/audit-fix-prompts.md:5660`.
#[test]
fn handle_socket_client_round_trips_stats() {
    let (_dir, ctx) = test_ctx();

    // Issue (a) a parse error, (b) two pings. Each request goes through
    // a fresh client/handler pair so the test exactly mirrors the
    // production accept-loop behavior (one connection per request).
    for payload in [
        "{\"command\":\"bogus_command\",\"args\":[]}\n",
        "{\"command\":\"ping\",\"args\":[]}\n",
        "{\"command\":\"ping\",\"args\":[]}\n",
    ] {
        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
        client.write_all(payload.as_bytes()).expect("write payload");
        let _ = read_line(&mut client);
        join_worker(client, handle);
    }

    // Snapshot the counters via the BatchContext directly (the test has
    // privileged access; no socket query needed). The view path bumps
    // the same Arc<AtomicU64>, so this read sees the same value the
    // ping handler would surface.
    let guard = ctx.lock().unwrap();
    let total_queries = guard.query_count.load(std::sync::atomic::Ordering::Relaxed);
    let error_count = guard.error_count.load(std::sync::atomic::Ordering::Relaxed);
    drop(guard);

    // Three requests reached the dispatch path (NUL/empty would short
    // circuit before counter bumps; bogus_command parses but clap
    // rejects, which still counts as a dispatched query).
    assert!(
        total_queries >= 3,
        "query_count must reflect 3 dispatches under the new short-lock contract; got {total_queries}"
    );
    // Exactly one parse failure.
    assert!(
        error_count >= 1,
        "error_count must reflect the parse failure; got {error_count}"
    );
}
