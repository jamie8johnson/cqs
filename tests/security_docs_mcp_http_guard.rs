//! Anti-drift ratchet for the MCP-over-HTTP security documentation.
//!
//! `cqs mcp --http` opens a real inbound network listener. SECURITY.md once
//! asserted the MCP bridge opens no network port; that sentence is now false and
//! must stay retired. This guard reads the shipped doc and fails if either the
//! retired false claim reappears or the MCP-HTTP surface stops being documented —
//! so a future edit cannot silently reintroduce the lie or drop the disclosure.
//!
//! Default-feature and pure-read: it exercises no listener, only the prose that
//! must describe one. It therefore builds and runs without the `mcp-http`
//! feature.

use std::path::PathBuf;

fn security_md() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("SECURITY.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The retired false sentence must never reappear: the HTTP transport opens a
/// real inbound listener, so a blanket "no network port is opened" claim would
/// deny an existing network surface.
#[test]
fn security_md_does_not_reclaim_no_network_port() {
    let doc = security_md();
    assert!(
        !doc.contains("No network port is opened"),
        "SECURITY.md reintroduced the retired false claim \"No network port is \
         opened\" — cqs mcp --http opens a real inbound listener"
    );
}

/// The MCP-HTTP surface must stay documented: the transport flag, its loopback
/// default, and its token/auth requirement all have to appear, so a reader learns
/// the port exists and how it is gated.
#[test]
fn security_md_documents_mcp_http_surface() {
    let doc = security_md();
    let lower = doc.to_ascii_lowercase();
    assert!(
        doc.contains("--http"),
        "SECURITY.md must document the `cqs mcp --http` transport flag"
    );
    assert!(
        lower.contains("loopback"),
        "SECURITY.md must document the MCP-HTTP loopback default"
    );
    assert!(
        lower.contains("token") || lower.contains("auth"),
        "SECURITY.md must document the MCP-HTTP token/auth requirement"
    );
}

/// The MCP Tasks surface (Phase 3b) is a load-bearing security disclosure: a
/// `taskId` is a bearer capability with NO per-task ACL under the shared token,
/// and the blocking `tasks/result` runs on an isolated budget. These claims must
/// stay documented so a future edit can't silently drop the shared-token
/// limitation the spec directs the receiver to record, or the availability
/// guarantee that the block can't starve one-shot requests.
#[test]
fn security_md_documents_mcp_tasks_surface() {
    let doc = security_md();
    let lower = doc.to_ascii_lowercase();
    assert!(
        doc.contains("taskId"),
        "SECURITY.md must document the MCP Tasks `taskId` capability"
    );
    assert!(
        lower.contains("per-task acl"),
        "SECURITY.md must document the shared-token no-per-task-ACL limitation (D3)"
    );
    assert!(
        doc.contains("taskSupport"),
        "SECURITY.md must document that only cqs_eval is task-augmentable (taskSupport)"
    );
    assert!(
        doc.contains("-32602"),
        "SECURITY.md must document that a gone task is -32602, never an HTTP 404"
    );
}

/// The GET/SSE progress stream (P3a-2) is a new inbound surface: GET no longer
/// unconditionally 405s, so the retired "no SSE in this build" claim must never
/// reappear, and the stream posture (notifications-only, router-wide Origin/Host/
/// auth on GET, the shared-token watch capability) must stay documented so a
/// future edit cannot silently drop the disclosure or reintroduce the false floor.
#[test]
fn security_md_documents_sse_progress_stream() {
    let doc = security_md();
    let lower = doc.to_ascii_lowercase();
    assert!(
        !doc.contains("no SSE in this build"),
        "SECURITY.md reintroduced the retired \"no SSE in this build\" claim — a GET with \
         Accept: text/event-stream now opens a real SSE stream"
    );
    assert!(
        doc.contains("text/event-stream") || lower.contains("sse progress stream"),
        "SECURITY.md must document the GET SSE progress stream surface"
    );
    assert!(
        doc.contains("notifications/progress"),
        "SECURITY.md must document that the stream is notifications-only (progress frames)"
    );
    assert!(
        lower.contains("progresstoken"),
        "SECURITY.md must document that a taskId + progressToken holder can watch progress \
         under the shared token (no per-task ACL)"
    );
}
