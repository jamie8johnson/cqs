//! `cqs mcp --http` — the Streamable HTTP MCP transport listener (Phase 3a:
//! listener + sessions + auth + Origin/Host + plain-JSON POST, plus the P3a-2 GET
//! SSE progress stream).
//!
//! A network security surface. Design fixed by the four-adversary panel in
//! `docs/plans/2026-07-03-mcp-phase3-decisions.md` (3a-D1/D2/D3). Key posture:
//!
//! - **Thin front-end (3a-D1).** Like the stdio bridge, this is a transport-only
//!   client of the daemon unix socket: every `tools/call` relays through the
//!   SAME [`super::bridge::route_with`]. It holds only session state; there is no
//!   in-process fallback (D4a preserved), and a daemon-absent call is a clean
//!   JSON-RPC error, not a panic/500.
//! - **Auth is UNCONDITIONAL (3a-D2).** Any TCP listener widens the daemon's
//!   same-UID `0o600`-socket boundary to every local UID (and, under WSL,
//!   Windows-side processes), and the MCP read set exfiltrates the indexed tree.
//!   So a bearer token is always required — there is no `--no-auth`. The compare
//!   reuses serve's timing-safe `cqs::serve::ct_eq`; the token alphabet is
//!   validated by `cqs::serve::AuthToken`; the token is NEVER logged.
//! - **Loopback default + loopback-only mutations (3a-D2).** A non-loopback bind
//!   fires its OWN loud warning (distinct from serve's `--no-auth` trigger) and
//!   withholds every mutating tool from `tools/list` AND from `tools/call` — the
//!   triple gate `flag ∧ token ∧ loopback`, enforced listener-side by threading
//!   `mutations_enabled() && is_loopback` into `route_with` (the daemon's own
//!   `require_mutations_enabled` is transport-blind).
//! - **Origin/Host (3a-D2).** Present-and-invalid Origin → 403; `Origin: null`
//!   → reject (sandboxed-iframe class); ABSENT Origin → allow (headless agents
//!   send none). serve's `cqs::serve::enforce_host_allowlist` closes
//!   DNS-rebinding for non-browser clients (Host is always present).
//! - **Sessions (3a-D2/D3).** `MCP-Session-Id` is server-issued ONLY — a
//!   client-proposed id on `initialize` is ignored (fixation closed). Ids are
//!   crypto-random visible-ASCII. An unknown/expired session → 404 (client
//!   re-inits). Bearer auth gates POST/GET/DELETE uniformly, so an
//!   unauthenticated DELETE cannot tear down a victim session.
//! - **Protocol floor (3a-D3).** Single endpoint; a POST body is a SINGLE
//!   JSON-RPC message (arrays → 400); a POSTed notification/response → 202 no
//!   body; a POSTed request → `application/json`; an invalid
//!   `MCP-Protocol-Version` → 400, absent → assume `2025-03-26` (not 400).
//! - **SSE progress stream (P3a-2, D1/D3/D4).** A GET with `Accept:
//!   text/event-stream` on a live session opens a notifications-only SSE stream
//!   (`notifications/progress` + an optional terminal `notifications/tasks/status`
//!   + keep-alive comments); a non-SSE GET keeps the 405 floor. The stream carries
//!   no JSON-RPC responses and no server→client requests. It rides a dedicated
//!   16-stream budget (invisible to the 256 one-shot permits), transfers its
//!   permit on takeover, and ends on the teed graceful-shutdown signal. Progress
//!   is monotone-per-token (a session-scoped high-water gate).
//!
//! The whole module is `#[cfg(all(feature = "mcp-http", unix))]` — it relays to
//! the unix daemon socket, so it has no meaning off-unix. A non-unix or
//! feature-off build gets the clean-erroring `serve_http` stub in the parent
//! module.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::{from_fn, from_fn_with_state, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::post,
    Router,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{watch, Notify, OwnedSemaphorePermit, Semaphore};

use super::bridge::route_with;
use super::lifecycle::{
    is_supported_protocol_version, JsonRpcRequest, JsonRpcResponse, DEFAULT_HTTP_PROTOCOL_VERSION,
};
// serve lives in the LIB crate (`cqs::serve`); this listener lives in the
// binary crate, so the reused primitives are named across the lib/bin boundary.
use cqs::serve::{allowed_host_set, ct_eq, enforce_host_allowlist, AllowedHosts, AuthToken};

/// The `MCP-Session-Id` header name (lowercased — HTTP header names are
/// case-insensitive; `HeaderMap::get(&str)` matches case-insensitively and
/// `HeaderName::from_static` requires lowercase).
const SESSION_HEADER: &str = "mcp-session-id";

/// The `MCP-Protocol-Version` header name.
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

/// Idle time-to-live for an HTTP session. A session unused for longer is
/// treated as expired: the next request bearing its id gets a 404 (client
/// re-initializes). Session state is explicitly NON-durable (3a-D1).
const SESSION_IDLE_TTL: Duration = Duration::from_secs(60 * 60);

/// Cap on concurrently-live sessions. A defensive bound so an authenticated but
/// steered client (the RT-RELAY "authorized ≠ benign" threat) cannot exhaust
/// memory by spamming `initialize`. Expired sessions are evicted before the cap
/// is checked, so only genuinely-live sessions count against it.
const MAX_SESSIONS: usize = 4096;

/// Maximum POST body: a single JSON-RPC message is tiny; 1 MiB is ample and
/// bounds a hostile client's per-request buffer.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Isolated blocking budget for `tasks/result`: at most this many
/// block-until-terminal calls run concurrently — a small `K ≪ 256` so a fully
/// saturated blocking budget can never starve the 256 outermost one-shot request
/// permits (the block-budget isolation). Single-flight caps running eval tasks to 1, so realistic
/// demand is a couple of blocks on the one live task; this is generous. A const,
/// not an env var (matching the daemon-side cap pattern).
const TASKS_RESULT_BLOCKING_BUDGET: usize = 8;

/// Per-`taskId` fan-out cap: one client cannot mount more than this many
/// concurrent `tasks/result` blocks on a single task (there is only ever one live
/// eval task under single-flight). Bounds a fan-out attack on the blocking budget.
const MAX_BLOCKS_PER_TASK: usize = 2;

/// Cap on concurrently-live SSE progress streams (P3a-2, D4). A dedicated budget
/// separate from the 256 one-shot request permits: an SSE stream is starvation-
/// safe and invisible to the outer cap (the cap permit is released before the
/// stream body is polled), so it needs its own bound so a steered client cannot
/// open unbounded long-lived streams. A const, not an env var (matching the
/// blocking-budget cap pattern).
const MAX_SSE_STREAMS: usize = 16;

/// SSE keep-alive comment cadence. Also the WSL localhost idle-drop mitigation
/// for a long-lived stream that emits no progress frame for a while (degraded /
/// status-only ticks). A keep-alive is an SSE comment — it carries no `id:` and
/// no event type.
const SSE_KEEPALIVE_SECS: u64 = 15;

/// Transport-side poll cadence for a live SSE stream: dial `task-get` about once
/// a second, diff the descriptor's progress, emit through the high-water gate.
/// Doubles as the dial-failure backoff (a failed poll retries on the next tick).
const SSE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Bound on a session's `(taskId, progressToken)` watch list: a defensive cap so
/// a steered client cannot accumulate an unbounded third per-session table by
/// spamming augmented `tools/call`s. The oldest entry is evicted past the bound;
/// under single-flight only one task is ever live at a time.
const MAX_WATCH_ENTRIES_PER_SESSION: usize = 64;

/// A `(taskId, progressToken)` watch entry for the SSE progress stream, plus the
/// per-token high-water: the greatest `done` ever EMITTED for this token on this
/// session. The stream emits a `notifications/progress` frame only for a strictly
/// greater `done`, so progress is monotone-per-token across reconnect/takeover
/// (the spec's MUST-increase). Debug-redacted: the taskId is a bearer capability
/// and the token is client-controlled.
struct WatchEntry {
    task_id: String,
    progress_token: Value,
    high_water: Option<u64>,
}

impl std::fmt::Debug for WatchEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchEntry")
            .field("task_id", &"redacted")
            .field("progress_token", &"redacted")
            .field("high_water", &self.high_water)
            .finish()
    }
}

/// The single live SSE stream a session may hold. The `permit` (one of the
/// [`MAX_SSE_STREAMS`]) lives HERE, not in the stream task, so a takeover can
/// TRANSFER it old→new under the session lock without a second acquire; it is
/// released when the stream ends (the [`StreamGuard`] clears this slot, dropping
/// the permit). `cancel` ends the current stream — a takeover or DELETE notifies
/// it, and the stream's `select!` wakes and returns.
struct LiveStream {
    permit: OwnedSemaphorePermit,
    cancel: Arc<Notify>,
}

/// Per-session state (P3a-2): last-seen for idle expiry, the single live-stream
/// slot, and the bounded progress watch list. Held behind an `Arc` so a live SSE
/// stream keeps its state alive even after the session is lazily evicted from the
/// store — the stream continues against its own `Arc`; the next request bearing
/// the evicted id gets 404. Debug-redacted: the watch list holds taskId handles.
struct SessionState {
    last_seen: Mutex<Instant>,
    stream: Mutex<Option<LiveStream>>,
    watch: Mutex<Vec<WatchEntry>>,
}

impl std::fmt::Debug for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never lock (a `?state` in a trace must not deadlock) and never render
        // the watch contents (taskId capabilities).
        f.debug_struct("SessionState")
            .field("watch", &"redacted")
            .finish()
    }
}

impl SessionState {
    fn new(now: Instant) -> Self {
        Self {
            last_seen: Mutex::new(now),
            stream: Mutex::new(None),
            watch: Mutex::new(Vec::new()),
        }
    }

    fn is_expired(&self, now: Instant, ttl: Duration) -> bool {
        let last = *self.last_seen.lock().unwrap_or_else(|p| p.into_inner());
        now.duration_since(last) >= ttl
    }

    fn touch_now(&self, now: Instant) {
        *self.last_seen.lock().unwrap_or_else(|p| p.into_inner()) = now;
    }

    /// Register (or refresh) a watch entry, deduped by taskId and bounded by
    /// [`MAX_WATCH_ENTRIES_PER_SESSION`] (oldest evicted past the bound).
    fn record_watch(&self, task_id: &str, token: Value) {
        let mut w = self.watch.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(e) = w.iter_mut().find(|e| e.task_id == task_id) {
            e.progress_token = token;
            return;
        }
        if w.len() >= MAX_WATCH_ENTRIES_PER_SESSION {
            w.remove(0);
        }
        w.push(WatchEntry {
            task_id: task_id.to_string(),
            progress_token: token,
            high_water: None,
        });
    }

    /// The most-recently-registered watch entry (taskId, token, high-water) — the
    /// task a fresh stream watches. Under single-flight at most one task is live,
    /// and the newest entry is the one the client just augmented.
    fn latest_watch(&self) -> Option<(String, Value, Option<u64>)> {
        let w = self.watch.lock().unwrap_or_else(|p| p.into_inner());
        w.last()
            .map(|e| (e.task_id.clone(), e.progress_token.clone(), e.high_water))
    }

    /// Compare-and-set the per-token high-water in ONE locked step: raise it to
    /// `done` and return `true` iff `done` strictly exceeds the stored value;
    /// otherwise leave it and return `false`. The gate reads and advances under the
    /// same lock so a takeover cannot regress the high-water — an old stream whose
    /// stale poll returns a LOWER `done` after the new stream already advanced
    /// fails the CAS (returns `false`, stored value untouched), so it neither
    /// emits a lower frame to the live client nor lets the new stream re-emit an
    /// already-seen value. (A late lower frame on the SUPERSEDED connection before
    /// its cancel is observed is acceptable — that connection is per-connection
    /// monotone and dying; the CAS kills the cross-takeover regression + duplicate.)
    fn try_advance_high_water(&self, task_id: &str, done: u64) -> bool {
        let mut w = self.watch.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(e) = w.iter_mut().find(|e| e.task_id == task_id) {
            if done > e.high_water.unwrap_or(0) {
                e.high_water = Some(done);
                return true;
            }
        }
        false
    }

    /// Prune a watch entry at terminal / gone. A reconnect afterward sees no
    /// entry (keep-alive only) and the client re-syncs the terminal via task-get.
    fn prune_watch(&self, task_id: &str) {
        let mut w = self.watch.lock().unwrap_or_else(|p| p.into_inner());
        w.retain(|e| e.task_id != task_id);
    }

    /// Acquire or TRANSFER the SSE stream permit for a new connection, returning
    /// the new stream's cancel handle — or `None` at the stream budget cap. A
    /// FIRST stream for the session draws a permit from `budget`; a second GET
    /// while a stream is live TRANSFERS that permit old→new under this lock (never
    /// a second `try_acquire`) and cancels the old stream. This keeps a session to
    /// one permit and lets a crash-reconnect at cap take over its own zombie.
    fn take_stream_slot(&self, budget: &Arc<Semaphore>) -> Option<Arc<Notify>> {
        let mut slot = self.stream.lock().unwrap_or_else(|p| p.into_inner());
        let cancel = Arc::new(Notify::new());
        let permit = match slot.take() {
            Some(old) => {
                old.cancel.notify_one();
                old.permit
            }
            None => Arc::clone(budget).try_acquire_owned().ok()?,
        };
        *slot = Some(LiveStream {
            permit,
            cancel: Arc::clone(&cancel),
        });
        Some(cancel)
    }

    /// Clear the stream slot IFF it still holds `cancel`'s stream (the
    /// [`StreamGuard`] cleanup path). A takeover already installed a newer stream,
    /// so a superseded stream's guard leaves the slot — the permit was transferred,
    /// not double-freed.
    fn clear_stream_if(&self, cancel: &Arc<Notify>) {
        let mut slot = self.stream.lock().unwrap_or_else(|p| p.into_inner());
        if slot
            .as_ref()
            .is_some_and(|l| Arc::ptr_eq(&l.cancel, cancel))
        {
            *slot = None;
        }
    }

    /// Cancel the live stream (explicit DELETE teardown). Idle expiry does NOT
    /// call this — a lazily-evicted session leaves its stream running.
    fn cancel_stream(&self) {
        let slot = self.stream.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(live) = slot.as_ref() {
            live.cancel.notify_one();
        }
    }

    #[cfg(test)]
    fn watch_len(&self) -> usize {
        self.watch.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// The in-memory, non-durable session store: `session-id → Arc<SessionState>`.
///
/// Server-issued ids only. Lazy expiry on access (no background sweeper):
/// [`SessionStore::touch_get`] drops an expired entry and reports it gone, and
/// [`SessionStore::create`] evicts all expired entries before the cap check. A
/// poisoned mutex is recovered via `into_inner` so one panicked request can't
/// wedge the whole listener.
struct SessionStore {
    inner: Mutex<HashMap<String, Arc<SessionState>>>,
    ttl: Duration,
    cap: usize,
}

impl SessionStore {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl: SESSION_IDLE_TTL,
            cap: MAX_SESSIONS,
        }
    }

    /// Issue a fresh server-generated session id, or `None` if the cap is still
    /// reached after evicting expired entries.
    fn create(&self) -> Option<String> {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        // Idle-expiry eviction. A live SSE stream holds its OWN `Arc<SessionState>`,
        // so evicting the map entry here does not stop the stream — it runs until
        // its connection closes, and the next request on the evicted id 404s. This
        // is deliberate: a silent poll-emit never touches `last_seen` (touch-per-
        // emit would defeat idle expiry, a self-DoS concern), so a live stream CAN
        // be evicted under itself. The `Arc` bounds the lifetime, so this is not a
        // leak; eviction never cancels a stream (only explicit DELETE does).
        map.retain(|_, st| !st.is_expired(now, self.ttl));
        if map.len() >= self.cap {
            return None;
        }
        let id = new_session_id();
        map.insert(id.clone(), Arc::new(SessionState::new(now)));
        Some(id)
    }

    /// Touch a session: `true` if it exists and is unexpired; `false` otherwise.
    fn touch(&self, id: &str) -> bool {
        self.touch_get(id).is_some()
    }

    /// Touch a session and return its shared state: `Some` if it exists and is
    /// unexpired (its last-seen is refreshed); `None` if unknown or expired (an
    /// expired entry is dropped from the map). See [`SessionStore::create`] for why
    /// dropping the map entry does not cancel a live stream on that session.
    fn touch_get(&self, id: &str) -> Option<Arc<SessionState>> {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match map.get(id) {
            Some(st) if !st.is_expired(now, self.ttl) => {
                st.touch_now(now);
                Some(Arc::clone(st))
            }
            // Lazy idle expiry: drop the map entry (the next request 404s); a live
            // stream keeps its own `Arc` and runs on — expiry never cancels it.
            Some(_) => {
                map.remove(id);
                None
            }
            None => None,
        }
    }

    /// Remove a session; `true` if it was present. Used by DELETE teardown, which
    /// ALSO cancels any live stream (distinct from idle expiry, which leaves it).
    fn remove(&self, id: &str) -> bool {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match map.remove(id) {
            Some(st) => {
                st.cancel_stream();
                true
            }
            None => false,
        }
    }

    /// Record a `(taskId ← progressToken)` watch entry on a live session (a no-op
    /// if the session is gone). Called from `handle_post` for an augmented
    /// `tools/call` that carries a client `progressToken`.
    fn record_watch(&self, id: &str, task_id: &str, token: Value) {
        let map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(st) = map.get(id) {
            st.record_watch(task_id, token);
        }
    }
}

/// Generate a crypto-random session id: 32 random bytes URL-safe base64-encoded
/// (no padding) — 43 chars, all in the visible-ASCII `[A-Za-z0-9_-]` set, which
/// satisfies the Transports session-id charset MUST (0x21–0x7E) and the
/// crypto-secure SHOULD. Same entropy source as [`AuthToken::random`].
fn new_session_id() -> String {
    use base64::Engine;
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Shared handler state. Cloned per layer; the token is `Arc`-backed and the
/// session store is behind an `Arc`, so clones share one store.
#[derive(Clone)]
struct HttpState {
    /// The `.cqs` dir whose daemon socket `route_with` relays to.
    cqs_dir: Arc<PathBuf>,
    /// The required bearer token (unconditional auth — 3a-D2).
    token: AuthToken,
    /// The non-durable session table.
    sessions: Arc<SessionStore>,
    /// Whether mutating tools are permitted on this listener: the triple gate
    /// `mutations_enabled() && bind_is_loopback`. When `false`, mutators are
    /// neither listed nor callable (withheld listener-side, before relay).
    mutations_permitted: bool,
    /// The isolated `tasks/result` blocking budget (a small `K ≪ 256`). A block
    /// holds one of these permits — NOT one of the outermost 256 — so a saturated
    /// blocking budget never starves one-shot requests (the block-budget isolation).
    result_budget: Arc<tokio::sync::Semaphore>,
    /// Per-`taskId` active-block counts, for the fan-out cap
    /// ([`MAX_BLOCKS_PER_TASK`]). A poisoned lock is recovered via `into_inner`.
    result_fanout: Arc<Mutex<HashMap<String, usize>>>,
    /// The dedicated SSE stream budget ([`MAX_SSE_STREAMS`]). A first stream for a
    /// session draws a permit; a takeover transfers it (never a second acquire).
    sse_budget: Arc<Semaphore>,
    /// Broadcast shutdown tee: `serve_http` fires `send(true)` when the shipped
    /// `shutdown_signal()` resolves, and every live SSE stream `select!`s on a
    /// `subscribe()`d receiver so graceful shutdown does not hang on an open
    /// stream. Held as an `Arc<Sender>` (never cloned per layer) so the channel
    /// stays open for the listener's lifetime.
    shutdown: Arc<watch::Sender<bool>>,
}

/// Run the `cqs mcp --http` listener against the daemon serving `cqs_dir`.
///
/// Binds `bind:port` (default loopback), enforces the unconditional bearer
/// token, and relays each `tools/call` through the daemon socket. Returns when
/// the listener fails or the process is interrupted (SIGINT/SIGTERM).
pub fn serve_http(cqs_dir: &Path, bind: &str, port: u16, token_file: Option<&Path>) -> Result<()> {
    let _span = tracing::info_span!("mcp_serve_http", bind = %bind, port).entered();

    // Resolve "localhost" to a numeric IP before parse (SocketAddr::parse only
    // takes numeric IPs) — mirrors `cmd_serve`.
    let bind_str = if bind == "localhost" {
        "127.0.0.1"
    } else {
        bind
    };
    let bind_addr: SocketAddr = format!("{bind_str}:{port}")
        .parse()
        .with_context(|| format!("failed to parse {bind_str}:{port} as a socket address"))?;
    let is_loopback = bind_addr.ip().is_loopback();

    // Unconditional auth (3a-D2): a token is required to start at all. The
    // token value is never logged — only the source is noted, path-only.
    let token = load_token(token_file)?;

    // Non-loopback bind: its OWN loud-warn trigger (fires WITH a valid token,
    // unlike serve's `--no-auth` trigger) plus the mutation withhold below.
    if !is_loopback {
        tracing::warn!(
            bind = %bind_addr,
            "cqs mcp --http bound to a NON-LOOPBACK address: the bearer token (and any \
             mutation) would travel the LAN in cleartext — there is no TLS in this stack. \
             Mutating tools are withheld from tools/list. Prefer loopback plus an SSH \
             tunnel, or terminate TLS at a reverse proxy."
        );
    }

    // The triple gate, enforced listener-side. The daemon's own mutation gate is
    // transport-blind, so a non-loopback bind narrows the surface here.
    let mutations_permitted = super::mutations_enabled() && is_loopback;

    // Shutdown tee: a watch channel every SSE stream subscribes to. `serve_http`
    // fires `send(true)` when `shutdown_signal()` resolves (below), so a graceful
    // shutdown ends every open stream instead of hanging on one. The `Sender` is
    // held in the state `Arc` (never cloned per layer) so the channel outlives the
    // per-connection receivers.
    let shutdown_tx = Arc::new(watch::channel(false).0);

    let state = HttpState {
        cqs_dir: Arc::new(cqs_dir.to_path_buf()),
        token,
        sessions: Arc::new(SessionStore::new()),
        mutations_permitted,
        result_budget: Arc::new(Semaphore::new(TASKS_RESULT_BLOCKING_BUDGET)),
        result_fanout: Arc::new(Mutex::new(HashMap::new())),
        sse_budget: Arc::new(Semaphore::new(MAX_SSE_STREAMS)),
        shutdown: Arc::clone(&shutdown_tx),
    };
    let allowed_hosts = allowed_host_set(&bind_addr);
    let app = build_router(state, allowed_hosts);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for the MCP HTTP listener")?;

    runtime.block_on(async move {
        let listener = TcpListener::bind(bind_addr)
            .await
            .with_context(|| format!("failed to bind {bind_addr}"))?;
        let actual = listener.local_addr().unwrap_or(bind_addr);
        tracing::info!(
            addr = %actual,
            mutations = mutations_permitted,
            "cqs mcp --http listening (Streamable HTTP transport, SSE progress stream on GET); press Ctrl-C to stop"
        );
        // Tee the shipped signal into the watch BEFORE it reaches axum: the same
        // future drives `with_graceful_shutdown` AND wakes every live SSE stream.
        let shutdown_fut = {
            let tx = Arc::clone(&shutdown_tx);
            async move {
                shutdown_signal().await;
                let _ = tx.send(true);
            }
        };
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_fut)
            .await
            .context("MCP HTTP listener failed")?;
        tracing::info!("cqs mcp --http shut down cleanly");
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

/// Build the listener's router. Layer order (outermost → innermost):
/// concurrency-cap → body-limit → Host allowlist → Origin → bearer auth →
/// handler. Auth is the innermost security layer so it runs before any handler
/// for EVERY method — an unauthenticated GET/DELETE never reaches the (inert)
/// GET-405 or the DELETE teardown. Host/Origin sit outside auth (cheaper header
/// rejects first), which leaks no more than serve's own posture. The
/// concurrency cap sits OUTSIDE everything so a saturated listener 503s before
/// any body buffer is allocated or any token compare runs.
fn build_router(state: HttpState, allowed_hosts: AllowedHosts) -> Router {
    // Reuse serve's request cap: both are axum front-ends relaying to the SAME
    // daemon socket, so they share one backpressure budget (no new env var, no
    // new dead code).
    let conn_sem = Arc::new(tokio::sync::Semaphore::new(
        cqs::limits::serve_max_concurrent_requests(),
    ));
    build_router_with_sem(state, allowed_hosts, conn_sem)
}

/// Assemble the router with an explicit in-flight semaphore. Split out so a test
/// can inject a size-1 (or pre-drained) semaphore and drive cap saturation
/// deterministically through the FULL layer stack.
fn build_router_with_sem(
    state: HttpState,
    allowed_hosts: AllowedHosts,
    conn_sem: Arc<tokio::sync::Semaphore>,
) -> Router {
    // NO compression layer on this router (unlike `cqs serve`): a `CompressionLayer`
    // buffers the response body to compress it, which would defeat the incremental
    // flush an SSE stream depends on (each progress frame must reach the client
    // immediately, not sit in a compressor's buffer). The GET SSE handler also sets
    // `X-Accel-Buffering: no` for the same reason at any reverse proxy. Keep this
    // router compression-free.
    Router::new()
        .route(
            "/mcp",
            post(handle_post).get(handle_get).delete(handle_delete),
        )
        .with_state(state.clone())
        .layer(from_fn_with_state(state, enforce_bearer_auth))
        .layer(from_fn(enforce_origin))
        .layer(from_fn_with_state(allowed_hosts, enforce_host_allowlist))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            MAX_BODY_BYTES,
        ))
        // Outermost layer: cap concurrent in-flight requests so one
        // authenticated-but-steered client cannot fan out N connections — each
        // holding a pre-auth body buffer plus a blocking daemon round-trip —
        // and exhaust the shared daemon's connection / blocking-pool budget,
        // degrading serve / CLI / other consumers. Sits outside body-limit,
        // Host, Origin, and auth so an over-cap request 503s before any of
        // those run.
        .layer(from_fn_with_state(conn_sem, enforce_concurrency_cap))
}

/// Graceful-shutdown future: resolves on Ctrl-C or SIGTERM (systemd stop).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        _ = ctrl_c => tracing::info!("ctrl-c received; MCP HTTP listener shutting down"),
        _ = terminate => tracing::info!("SIGTERM received; MCP HTTP listener shutting down"),
    }
}

// ─── auth / origin middleware ────────────────────────────────────────────────

/// Bearer-token auth, applied uniformly to POST/GET/DELETE. The token compare
/// is serve's timing-safe [`ct_eq`]; the server token is never logged, and the
/// reject warn carries only method + path (not the presented credential).
async fn enforce_bearer_auth(State(state): State<HttpState>, req: Request, next: Next) -> Response {
    // Compute the verdict as an owned bool so the header borrow ends before
    // `req` is moved into `next.run`.
    let authed = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|presented| ct_eq(presented, state.token.as_str()))
        .unwrap_or(false);
    if authed {
        next.run(req).await
    } else {
        tracing::warn!(
            method = %req.method(),
            path = %req.uri().path(),
            "mcp-http: rejected unauthenticated request",
        );
        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}

/// Origin enforcement (3a-D2). ABSENT Origin → allow (headless agents send
/// none). `Origin: null` → reject (sandboxed-iframe class), distinct from
/// absent. A present, non-localhost Origin → 403. An empty Origin value carries
/// no cross-origin information and is treated as absent.
async fn enforce_origin(req: Request, next: Next) -> Response {
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let o = origin.to_str().unwrap_or("");
        if !o.is_empty() && (o == "null" || !is_localhost_origin(o)) {
            tracing::warn!(origin = %o, "mcp-http: rejected request with invalid Origin");
            return (StatusCode::FORBIDDEN, "invalid Origin").into_response();
        }
    }
    next.run(req).await
}

/// Whether `origin` is a localhost origin. Guards the suffix boundary so
/// `http://localhost.evil.com` does NOT match (`localhost` must be followed by
/// end, `:port`, or `/path`).
fn is_localhost_origin(origin: &str) -> bool {
    let origin = origin.to_ascii_lowercase();
    const PREFIXES: [&str; 6] = [
        "http://localhost",
        "http://127.0.0.1",
        "https://localhost",
        "https://127.0.0.1",
        "http://[::1]",
        "https://[::1]",
    ];
    for prefix in PREFIXES {
        if let Some(rest) = origin.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with(':') || rest.starts_with('/') {
                return true;
            }
        }
    }
    false
}

/// Cap concurrent in-flight requests. The OUTERMOST layer, so a saturated
/// listener returns `503 Service Unavailable` before any downstream layer runs:
/// no body buffer is allocated and no timing-safe token compare executes until a
/// request holds a permit. `try_acquire` returns immediately — saturation is a
/// 503, never a queued allocation — and the permit is dropped when the response
/// completes, so a finished request frees its slot for the next one.
async fn enforce_concurrency_cap(
    State(sem): State<Arc<tokio::sync::Semaphore>>,
    mut req: Request,
    next: Next,
) -> Response {
    let permit = match Arc::clone(&sem).try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            tracing::warn!("mcp-http: concurrent-request cap saturated — returning 503");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Server at concurrency cap; retry shortly",
            )
                .into_response();
        }
    };
    // Hand the OWNED permit to the handler via request extensions instead of
    // holding it across `next.run`. A normal request extracts it and holds it for
    // its whole duration (bounding in-flight daemon relays exactly as before), but
    // a blocking `tasks/result` DROPS it before the ~20-min block so the wait
    // never pins one of the 256 outermost permits (the block-budget isolation). If the handler never
    // extracts it (GET/DELETE, an early return), it drops when the request is
    // dropped — still released after the response, never leaked.
    //
    // `Extensions::insert` requires `Clone`, and `OwnedSemaphorePermit` is not, so
    // it rides an `Arc`. The request is never cloned in this chain, so the Arc has
    // exactly one strong ref: dropping it releases the permit.
    req.extensions_mut().insert(std::sync::Arc::new(permit));
    next.run(req).await
}

// ─── method handlers ─────────────────────────────────────────────────────────

/// POST `/mcp` — the single JSON-RPC endpoint (no SSE in rung 1).
async fn handle_post(State(state): State<HttpState>, mut req: Request) -> Response {
    // Take the outermost concurrency permit OUT of the request so this handler
    // owns its lifetime: a normal request holds it across the daemon round-trip
    // (bounding in-flight relays), while a blocking `tasks/result` drops it before
    // the block (the block-budget isolation). `None` only when the cap layer isn't installed (some
    // unit-test routers) — then there is no permit to manage.
    let outer_permit = req
        .extensions_mut()
        .remove::<std::sync::Arc<tokio::sync::OwnedSemaphorePermit>>();
    let headers = req.headers().clone();

    // Protocol-version gate: present-and-unsupported → 400; absent → assume the
    // backward-compat default (NOT a 400).
    if let Some(resp) = check_protocol_version(&headers) {
        return resp;
    }

    // Buffer the body (bounded by the RequestBodyLimitLayer above; an undeclared
    // oversized body that slips the Content-Length check surfaces here as a 413).
    let body = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeds the size limit",
            )
                .into_response()
        }
    };

    // A single JSON-RPC message. Reject arrays (batches) with 400.
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("parse error: body is not valid JSON ({e})"),
            )
                .into_response()
        }
    };
    if value.is_array() {
        return (
            StatusCode::BAD_REQUEST,
            "JSON-RPC batches are not supported; send a single message",
        )
            .into_response();
    }

    // A message with no `method` is a JSON-RPC response (server→client requests
    // are P3a-2+). Accept and ignore → 202 no body.
    let Some(method) = value.get("method").and_then(|m| m.as_str()) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let session_header = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok());

    if method == "initialize" {
        // Server-issued session ONLY: any client-proposed MCP-Session-Id header
        // is IGNORED here (fixation closed) — a fresh id is always minted.
        let new_id = match state.sessions.create() {
            Some(id) => id,
            None => {
                tracing::warn!("mcp-http: session cap reached; refusing new initialize");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "session capacity reached; retry later",
                )
                    .into_response();
            }
        };
        let request: JsonRpcRequest = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(e) => {
                state.sessions.remove(&new_id);
                return (
                    StatusCode::BAD_REQUEST,
                    format!("malformed initialize request: {e}"),
                )
                    .into_response();
            }
        };
        let response = match route_blocking(&state, request).await {
            Ok(r) => r,
            Err(resp) => {
                state.sessions.remove(&new_id);
                return resp;
            }
        };
        return json_response(&response, Some(&new_id));
    }

    // Non-initialize: a session is required. Missing header → 400 (client must
    // initialize first); present-but-unknown/expired → 404 (client re-inits).
    let session_id = match session_header {
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "missing MCP-Session-Id header (initialize first)",
            )
                .into_response()
        }
        Some(id) => id.to_string(),
    };
    if !state.sessions.touch(&session_id) {
        return (
            StatusCode::NOT_FOUND,
            "unknown or expired MCP session; re-initialize",
        )
            .into_response();
    }

    let request: JsonRpcRequest = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("malformed JSON-RPC request: {e}"),
            )
                .into_response()
        }
    };
    // `tasks/result` blocks until terminal. It runs on the isolated blocking
    // budget with the outermost permit RELEASED, so the ~20-min wait never pins
    // one of the 256 one-shot permits (the block-budget isolation). Every other method — an
    // augmented `tools/call`, `tasks/get`, a normal call — is a quick daemon dial
    // routed through `route_blocking` while still holding the outer permit.
    if request.method == "tasks/result" {
        return serve_blocking_result(&state, outer_permit, request).await;
    }

    // Progress bookkeeping (P3a-2): an augmented `tools/call` carrying a client
    // `progressToken` registers a `(taskId ← progressToken)` watch entry so a live
    // SSE stream on this session can emit `notifications/progress`. Capture the
    // token BEFORE `request` is moved into the relay; record the (taskId, token)
    // pair AFTER a successful admission yields the taskId. No token → no entry →
    // no frames for that task (the spec MUST: only reference provided tokens).
    let progress_token = augmented_progress_token(&request);

    // A notification (no id) gets a 202 with no body; a request (id present)
    // gets its JSON-RPC response as application/json.
    let is_notification = request.id.is_none();
    let response = match route_blocking(&state, request).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if let Some(token) = progress_token {
        if let Some(task_id) = response
            .result
            .as_ref()
            .and_then(|r| r.get("task"))
            .and_then(|t| t.get("taskId"))
            .and_then(|v| v.as_str())
        {
            state.sessions.record_watch(&session_id, task_id, token);
        }
    }
    if is_notification {
        StatusCode::ACCEPTED.into_response()
    } else {
        json_response(&response, None)
    }
}

/// GET `/mcp` — open the SSE progress stream (P3a-2), or preserve the 405 floor
/// for a non-SSE GET.
///
/// The flip gate mirrors POST's security layers plus SSE-specific checks:
/// `Accept: text/event-stream` (else 405) · protocol-version (invalid → 400,
/// absent → the negotiated default) · a live session (missing header → 400,
/// unknown/expired → 404). Origin / Host / auth are router-wide layers that
/// already run on GET, so they need no handler code (they get GET-shaped tests).
/// The per-session SSE budget is drawn (or transferred on takeover) under the
/// session lock; at cap → 503.
async fn handle_get(State(state): State<HttpState>, mut req: Request) -> Response {
    // The SSE stream body is long-lived; it MUST NOT pin one of the 256 outermost
    // one-shot permits (that would let a client starve the request budget by
    // opening streams). Extract and drop the cap permit here, mirroring
    // `serve_blocking_result`'s explicit release. (It would drop with `req` at the
    // end of this fn anyway — the response body does not carry request extensions
    // — but the explicit drop pins the invariant against a future change.)
    let outer_permit = req
        .extensions_mut()
        .remove::<std::sync::Arc<OwnedSemaphorePermit>>();
    drop(outer_permit);

    let headers = req.headers().clone();

    // Non-SSE Accept keeps the shipped 405: this endpoint opens a stream only on
    // an explicit `text/event-stream` Accept; a plain GET is still not-allowed, so
    // a non-SSE client is not misled into treating GET as a data endpoint.
    if !accepts_event_stream(&headers) {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            "GET without `Accept: text/event-stream` opens no stream; POST a JSON-RPC message, \
             or GET with that Accept for the SSE progress stream",
        )
            .into_response();
    }

    // Protocol-version gate (the shipped `check_protocol_version` is POST-only;
    // the GET flip MUST run it): present-and-invalid → 400; absent → the default.
    if let Some(resp) = check_protocol_version(&headers) {
        return resp;
    }

    // A live session is required (never `initialize` on GET). Missing header →
    // 400; present-but-unknown/expired → 404. Mirrors the POST split.
    let session_id = match headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "missing MCP-Session-Id header (initialize first)",
            )
                .into_response()
        }
        Some(id) => id.to_string(),
    };
    let session = match state.sessions.touch_get(&session_id) {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                "unknown or expired MCP session; re-initialize",
            )
                .into_response()
        }
    };

    // Draw (or take over) the per-session SSE stream slot. First stream draws from
    // the 16-stream budget; a takeover transfers the existing permit and cancels
    // the old stream. At cap (a first stream with the budget drained) → 503.
    let cancel = match session.take_stream_slot(&state.sse_budget) {
        Some(c) => c,
        None => {
            tracing::warn!("mcp-http: SSE stream budget saturated — returning 503");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "SSE stream budget saturated; retry shortly",
            )
                .into_response();
        }
    };

    // Build the poll-and-emit stream. The guard releases the permit on stream end
    // (disconnect / terminal / takeover / shutdown) by clearing the slot iff it is
    // still ours; the poll dials `task-get` on a blocking thread each tick.
    let guard = StreamGuard {
        session: Arc::clone(&session),
        cancel: Arc::clone(&cancel),
    };
    let shutdown_rx = state.shutdown.subscribe();
    let poll = {
        let cqs_dir = Arc::clone(&state.cqs_dir);
        move |task_id: String| {
            let cqs_dir = Arc::clone(&cqs_dir);
            async move {
                match tokio::task::spawn_blocking(move || {
                    super::tasks::poll_progress_once(&cqs_dir, &task_id)
                })
                .await
                {
                    Ok(step) => step,
                    // A poll-worker panic is transient — retry on the next tick.
                    Err(_) => super::tasks::ProgressStep::DialFailed,
                }
            }
        }
    };
    let stream = build_sse_stream(session, cancel, shutdown_rx, SSE_POLL_INTERVAL, guard, poll);

    let sse = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(SSE_KEEPALIVE_SECS)));
    let mut resp = sse.into_response();
    // Defeat proxy response buffering so each progress frame flushes immediately.
    resp.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    resp
}

/// Whether the request explicitly accepts the SSE media type. Requires a literal
/// `text/event-stream` media-range token (a bare `*/*` or `text/html` does NOT
/// open a stream — that keeps the 405 floor for non-SSE clients).
fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| {
            accept
                .split(',')
                .any(|part| part.split(';').next().map(str::trim) == Some("text/event-stream"))
        })
}

/// Extract a client `progressToken` from an augmented `tools/call` request, for
/// the session watch bookkeeping. Returns `None` for a non-augmented call, a
/// non-`tools/call` method, or an absent/ill-typed token. The MCP progress token
/// is a string OR an integer (`params._meta.progressToken`), carried VERBATIM.
fn augmented_progress_token(request: &JsonRpcRequest) -> Option<Value> {
    if request.method != "tools/call" || !super::tasks::is_augmented(&request.params) {
        return None;
    }
    request
        .params
        .as_ref()
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get("progressToken"))
        .filter(|t| t.is_string() || t.is_number())
        .cloned()
}

// ─── SSE progress stream (P3a-2) ─────────────────────────────────────────────

/// RAII cleanup for a live SSE stream: on drop (client disconnect, terminal,
/// takeover, or shutdown) it clears the session's stream slot IFF the slot still
/// holds THIS stream's cancel handle, releasing the [`MAX_SSE_STREAMS`] permit.
/// A superseded (taken-over) stream leaves the slot to its successor — the permit
/// was transferred under the session lock, so it is never double-freed. Moved
/// into the stream's state, so it drops exactly when the stream body drops.
struct StreamGuard {
    session: Arc<SessionState>,
    cancel: Arc<Notify>,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.session.clear_stream_if(&self.cancel);
    }
}

/// Build the per-connection SSE event stream: an `unfold` poll loop, generic over
/// an injected `poll` so a test can drive it deterministically without a live
/// daemon (mirrors `blocking_result_with_poll`).
///
/// Each tick it consults the session's newest watch entry, polls it, and applies
/// the per-token high-water gate: a progress frame is emitted ONLY for a strictly
/// greater `done` while the task is `working` (a status-only tick emits nothing),
/// so progress is monotone-per-token across reconnect/takeover. At terminal it
/// emits the optional `notifications/tasks/status` frame (a spec MAY) and ends —
/// NO progress frame is emitted at terminal (progress only while working). An
/// authoritative `Gone` ends the stream with no frame; a dial failure keeps the
/// stream alive and retries. An empty / degraded (no `{done,total}`) watch is
/// keep-alive-only — NEVER a synthesized value. Cancel (takeover/DELETE) and the
/// shutdown watch both end the stream.
fn build_sse_stream<P, Fut>(
    session: Arc<SessionState>,
    cancel: Arc<Notify>,
    shutdown: watch::Receiver<bool>,
    interval: Duration,
    guard: StreamGuard,
    poll: P,
) -> impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>> + Send + 'static
where
    P: FnMut(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = super::tasks::ProgressStep> + Send,
{
    struct LoopState<P> {
        session: Arc<SessionState>,
        cancel: Arc<Notify>,
        shutdown: watch::Receiver<bool>,
        interval: Duration,
        poll: P,
        // Dropped when the stream ends; releases the SSE permit.
        _guard: StreamGuard,
        // First tick polls immediately (the reconnect/open snapshot); later ticks
        // wait `interval` (and race cancel/shutdown).
        first: bool,
        // Set after the optional terminal frame is yielded, so the next unfold
        // step ends the stream.
        ending: bool,
    }

    let state = LoopState {
        session,
        cancel,
        shutdown,
        interval,
        poll,
        _guard: guard,
        first: true,
        ending: false,
    };

    futures_util::stream::unfold(state, |mut st| async move {
        if st.ending {
            return None;
        }
        loop {
            if !st.first {
                let cancel = Arc::clone(&st.cancel);
                let sleep = tokio::time::sleep(st.interval);
                tokio::select! {
                    biased;
                    _ = cancel.notified() => return None,
                    _ = st.shutdown.changed() => return None,
                    _ = sleep => {}
                }
            }
            st.first = false;
            // A shutdown that fired before this stream subscribed (already-true
            // value) is caught here, since `changed()` above only sees NEW sends.
            if *st.shutdown.borrow() {
                return None;
            }

            let (task_id, token) = match st.session.latest_watch() {
                // The stored high-water is not read here: the gate compare-and-sets
                // it under the watch lock (`try_advance_high_water`) so a takeover
                // can't regress it off a stale read.
                Some((task_id, token, _high_water)) => (task_id, token),
                // No watched task yet (stream opened before the augmented call, or
                // no progressToken) → keep-alive: loop and re-check next tick.
                None => continue,
            };

            match (st.poll)(task_id.clone()).await {
                super::tasks::ProgressStep::Found {
                    progress,
                    terminal,
                    descriptor,
                } => {
                    if terminal {
                        // Progress frames only while working; convey terminal via
                        // the optional `tasks/status` frame (or task-get), then end.
                        st.session.prune_watch(&task_id);
                        if let Some(ev) = tasks_status_event(&descriptor) {
                            st.ending = true;
                            return Some((Ok(ev), st));
                        }
                        return None;
                    }
                    if let Some((done, total)) = progress {
                        // High-water gate (compare-and-set under the watch lock):
                        // emit only a strictly-greater `done`. `done == 0` never
                        // advances past the `unwrap_or(0)` floor — deliberate: a
                        // zero-progress frame carries nothing, so the client first
                        // learns `total` at the first `done >= 1` frame.
                        if st.session.try_advance_high_water(&task_id, done) {
                            if let Some(ev) = progress_event(&token, done, total, &task_id) {
                                return Some((Ok(ev), st));
                            }
                        }
                        // No advance (status-only) → nothing; keep-alive, loop.
                    }
                    // Degraded (no `{done,total}`) → keep-alive only, loop.
                }
                // Authoritative gone (GC'd / daemon restarted): drop the token and
                // close the stream — never a fabricated terminal frame.
                super::tasks::ProgressStep::Gone => {
                    st.session.prune_watch(&task_id);
                    return None;
                }
                // Transient dial failure: keep the stream alive, retry next tick.
                super::tasks::ProgressStep::DialFailed => {}
            }
        }
    })
}

/// Build a `notifications/progress` SSE event: the verbatim client `progressToken`
/// (string or integer), `progress`/`total`, and the `related-task` `_meta` (a spec
/// MUST on task-associated notifications). Default event type, no `id:` line.
fn progress_event(token: &Value, done: u64, total: u64, task_id: &str) -> Option<Event> {
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {
            "progressToken": token,
            "progress": done,
            "total": total,
            "_meta": super::tasks::related_task_meta(task_id),
        }
    });
    Event::default().json_data(frame).ok()
}

/// Build the optional terminal `notifications/tasks/status` SSE event from the
/// flat daemon descriptor: the Task shape (`taskId`/`status`/`createdAt`/
/// `lastUpdatedAt`/`ttl`), with `ttl` converted seconds→milliseconds (the spec
/// wire unit). The transport-only `progress` / `pollInterval` / `_meta` keys are
/// dropped; the frame SHOULD-NOT carry `related-task` `_meta` (the taskId is in
/// params). A pure MAY — no monotonicity constraint.
fn tasks_status_event(descriptor: &Value) -> Option<Event> {
    let mut task = descriptor.clone();
    if !task.is_object() {
        return None;
    }
    super::tasks::descriptor_ttl_secs_to_ms(&mut task);
    if let Some(obj) = task.as_object_mut() {
        obj.remove("progress");
        obj.remove("pollInterval");
        obj.remove("_meta");
    }
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/tasks/status",
        "params": task,
    });
    Event::default().json_data(frame).ok()
}

/// DELETE `/mcp` — explicit session teardown. Auth is enforced by the layer, so
/// an UNAUTHENTICATED DELETE never reaches here (it cannot tear down a victim
/// session). A missing session header → 400; an unknown session → 404.
async fn handle_delete(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    let session_id = match headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "missing MCP-Session-Id header").into_response(),
    };
    if state.sessions.remove(session_id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "unknown MCP session").into_response()
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Route one request through the shared bridge, off the async worker: the
/// `tools/call` leg does a blocking daemon-socket round-trip, so it runs on a
/// blocking thread. A routing-task panic maps to a 500 rather than aborting.
async fn route_blocking(
    state: &HttpState,
    request: JsonRpcRequest,
) -> Result<JsonRpcResponse, Response> {
    let cqs_dir = Arc::clone(&state.cqs_dir);
    let mutations = state.mutations_permitted;
    match tokio::task::spawn_blocking(move || route_with(&cqs_dir, request, mutations)).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            tracing::error!(error = %e, "mcp-http: routing task panicked");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response())
        }
    }
}

// ─── tasks/result blocking budget (the block-budget isolation) ─────────────────────────────────

/// RAII per-`taskId` fan-out guard: increments the active-block count on acquire
/// (refusing past [`MAX_BLOCKS_PER_TASK`]) and decrements it on drop, reclaiming
/// the map entry at zero so the table never grows unbounded.
struct FanoutGuard {
    map: Arc<Mutex<HashMap<String, usize>>>,
    task_id: String,
}

impl FanoutGuard {
    /// Acquire a fan-out slot for `task_id`, or `None` at the per-task cap.
    fn try_acquire(
        map: Arc<Mutex<HashMap<String, usize>>>,
        task_id: String,
    ) -> Option<FanoutGuard> {
        {
            let mut m = map.lock().unwrap_or_else(|p| p.into_inner());
            let count = m.entry(task_id.clone()).or_insert(0);
            if *count >= MAX_BLOCKS_PER_TASK {
                return None;
            }
            *count += 1;
        }
        Some(FanoutGuard { map, task_id })
    }
}

impl Drop for FanoutGuard {
    fn drop(&mut self) {
        let mut m = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(count) = m.get_mut(&self.task_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                m.remove(&self.task_id);
            }
        }
    }
}

/// Serve a blocking `tasks/result`: extract the taskId, then run the
/// budget-isolated block loop with a real daemon poll (each poll a `spawn_blocking`
/// off the async worker).
async fn serve_blocking_result(
    state: &HttpState,
    outer_permit: Option<std::sync::Arc<tokio::sync::OwnedSemaphorePermit>>,
    request: JsonRpcRequest,
) -> Response {
    let id = request.id.clone();
    let task_id = match super::tasks::task_id_from_params(&request.params) {
        Ok(t) => t,
        Err(()) => return json_response(&super::tasks::task_gone(id), None),
    };

    let cqs_dir = Arc::clone(&state.cqs_dir);
    let poll_id = id.clone();
    let poll_task = task_id.clone();
    let poll = move || {
        let cqs_dir = Arc::clone(&cqs_dir);
        let id = poll_id.clone();
        let task_id = poll_task.clone();
        async move {
            let err_id = id.clone();
            match tokio::task::spawn_blocking(move || {
                super::tasks::poll_result_once(&cqs_dir, &id, &task_id)
            })
            .await
            {
                Ok(step) => step,
                Err(_) => super::tasks::ResultStep::Done(super::tasks::internal_error(
                    err_id,
                    "tasks/result poll worker failed",
                )),
            }
        }
    };

    let resp = blocking_result_with_poll(
        outer_permit,
        Arc::clone(&state.result_budget),
        Arc::clone(&state.result_fanout),
        id,
        task_id,
        poll,
    )
    .await;
    json_response(&resp, None)
}

/// The budget-isolated block loop, factored out with an injected `poll` so an
/// the block-budget isolation test can drive it deterministically without a live daemon: acquire
/// the fan-out + blocking-budget guards, RELEASE the outermost permit, then poll
/// to terminal (or the anti-wedge backstop).
async fn blocking_result_with_poll<P, Fut>(
    outer_permit: Option<std::sync::Arc<tokio::sync::OwnedSemaphorePermit>>,
    budget: Arc<tokio::sync::Semaphore>,
    fanout: Arc<Mutex<HashMap<String, usize>>>,
    id: Option<Value>,
    task_id: String,
    mut poll: P,
) -> JsonRpcResponse
where
    P: FnMut() -> Fut,
    Fut: std::future::Future<Output = super::tasks::ResultStep>,
{
    // Per-taskId fan-out cap: one client can't mount many blocks on the one task.
    let _fanout = match FanoutGuard::try_acquire(fanout, task_id) {
        Some(g) => g,
        None => {
            return super::tasks::retryable_busy(
                id,
                "too many concurrent tasks/result blocks for this task; poll tasks/get instead",
            )
        }
    };
    // Isolated blocking budget (K ≪ 256): bounds concurrent blocks so they can
    // never starve the outermost one-shot permits.
    let _budget = match budget.try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return super::tasks::retryable_busy(
                id,
                "tasks/result blocking budget saturated; poll tasks/get instead",
            )
        }
    };
    // the block-budget isolation: release the outermost permit BEFORE the block. The wait now holds
    // only the tiny budget permit above — ZERO of the 256 one-shot permits — so
    // one-shot requests keep being served while blocks are outstanding.
    drop(outer_permit);

    let start = Instant::now();
    loop {
        match poll().await {
            super::tasks::ResultStep::Done(resp) => return resp,
            super::tasks::ResultStep::Pending => {
                if start.elapsed() > super::tasks::MAX_BLOCK_DURATION {
                    tracing::warn!(
                        "mcp-http tasks/result: block backstop tripped (task never terminalized)"
                    );
                    return super::tasks::block_backstop_error(id);
                }
                tokio::time::sleep(super::tasks::INTERNAL_POLL_INTERVAL).await;
            }
        }
    }
}

/// Serialize a JSON-RPC response as `application/json` (200), attaching the
/// `MCP-Session-Id` header when `session_id` is `Some` (the initialize reply).
fn json_response(response: &JsonRpcResponse, session_id: Option<&str>) -> Response {
    let body = match serde_json::to_vec(response) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "mcp-http: failed to serialize response");
            return (StatusCode::INTERNAL_SERVER_ERROR, "serialization error").into_response();
        }
    };
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(id) = session_id {
        // The id is base64 URL-safe (visible ASCII) → always a valid header
        // value; the fallible path only logs, never panics.
        match HeaderValue::from_str(id) {
            Ok(v) => {
                resp.headers_mut()
                    .insert(HeaderName::from_static(SESSION_HEADER), v);
            }
            Err(e) => {
                tracing::error!(error = %e, "mcp-http: session id is not a valid header value");
            }
        }
    }
    resp
}

/// Validate the `MCP-Protocol-Version` header. Present-and-unsupported → 400
/// (the Transports MUST); an empty value is treated as absent; absent assumes
/// [`DEFAULT_HTTP_PROTOCOL_VERSION`] (a 400-on-missing would break the
/// backward-compat SHOULD). Returns `Some(response)` for the 400 case, `None`
/// when the header is acceptable (an `Option` rather than `Result<(), Response>`
/// because the axum `Response` Err-variant is large — `result_large_err`).
fn check_protocol_version(headers: &HeaderMap) -> Option<Response> {
    let v = headers.get(PROTOCOL_VERSION_HEADER)?;
    let s = v.to_str().unwrap_or("");
    if !s.is_empty() && !is_supported_protocol_version(s) {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "unsupported MCP-Protocol-Version: {} (absent assumes {})",
                    clip(s, 32),
                    DEFAULT_HTTP_PROTOCOL_VERSION
                ),
            )
                .into_response(),
        );
    }
    None
}

/// Load the required bearer token: `--token-file` (preferred, ownership+mode
/// checked) then `CQS_MCP_HTTP_TOKEN`. Neither present → error (auth is
/// unconditional). The token VALUE is never logged.
fn load_token(token_file: Option<&Path>) -> Result<AuthToken> {
    if let Some(path) = token_file {
        let raw = read_token_file_checked(path)?;
        return AuthToken::try_from_string(raw.trim()).map_err(|e| {
            anyhow::anyhow!(
                "token file {} does not contain a valid token: {e}",
                path.display()
            )
        });
    }
    if let Ok(raw) = std::env::var("CQS_MCP_HTTP_TOKEN") {
        // Path-only tracing: the value is never logged; the env source is noted
        // (and discouraged) so an operator sees where the token came from.
        tracing::info!(
            "cqs mcp --http: bearer token loaded from CQS_MCP_HTTP_TOKEN (env source is \
             discouraged — its value leaks through unit files / exec-capture / child env; \
             prefer --token-file)"
        );
        return AuthToken::try_from_string(raw.trim()).map_err(|e| {
            anyhow::anyhow!("CQS_MCP_HTTP_TOKEN does not contain a valid token: {e}")
        });
    }
    anyhow::bail!(
        "cqs mcp --http requires a bearer token (auth is unconditional): pass --token-file \
         <path> (preferred) or set CQS_MCP_HTTP_TOKEN"
    )
}

/// Read a token file after an ownership + mode check (mirrors the
/// `CQS_PDF_SCRIPT` precedent): the file must be owned by the effective user and
/// not group/world-accessible. Only on a genuine WSL drvfs mount (`/mnt/c/...`),
/// where unix modes are synthetic `0777` and cannot be tightened, is the mode
/// check skipped (with a loud warning) — a path-scoped skip, not a host-global
/// one; the ownership check still applies.
fn read_token_file_checked(path: &Path) -> Result<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let meta = std::fs::metadata(path)
        .with_context(|| format!("cannot stat token file {}", path.display()))?;

    // Size guard: a token file is tiny; refuse an implausibly large one before
    // reading it into memory.
    if meta.len() > 64 * 1024 {
        anyhow::bail!(
            "token file {} is implausibly large ({} bytes); refusing to read it as a token",
            path.display(),
            meta.len()
        );
    }

    // Ownership: the file must be owned by the effective user. A file another
    // user owns could be swapped out from under us.
    let euid = u64::from(unsafe { libc::geteuid() });
    if u64::from(meta.uid()) != euid {
        anyhow::bail!(
            "token file {} is not owned by the current user (file uid {}, effective uid {}); \
             refusing a token from a file another user controls",
            path.display(),
            meta.uid(),
            euid
        );
    }

    // Mode: not group- or world-accessible. Skipped ONLY on genuine WSL drvfs
    // mounts (mode is synthetic 0777 there and cannot be tightened) — the
    // ownership check above still stands. The skip is deliberately PATH-scoped
    // (`is_wsl_drvfs_path`), never the host-global `is_wsl()`: a bearer token on
    // a native unix fs (tmpfs / ext4) reports real modes even on a WSL host, and
    // skipping the refusal there would disclose it to every local UID — the exact
    // boundary a TCP listener widens. (config.rs ORs in `is_wsl()` for its config
    // permission NAG, a soft warning; this is a hard security refusal, so it must
    // not inherit that host-global lenience.)
    let on_wsl_mount = cqs::config::is_wsl_drvfs_path(path);
    if on_wsl_mount {
        tracing::warn!(
            path = %path.display(),
            "token file is on a WSL drvfs mount; the group/world-readable mode check is \
             skipped (NTFS reports synthetic 0777). Keep the token file on a native unix \
             filesystem (e.g. $XDG_RUNTIME_DIR) for the mode check to apply."
        );
    } else {
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "token file {} is group/world-accessible (mode {:o}); tighten it with \
                 `chmod 600 {}`",
                path.display(),
                mode & 0o777,
                path.display()
            );
        }
    }

    std::fs::read_to_string(path)
        .with_context(|| format!("cannot read token file {}", path.display()))
}

/// Clip a string to at most `max` chars on a char boundary — bounds an oversized
/// header value echoed into an error message.
fn clip(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::util::ServiceExt;

    /// A valid-alphabet server token used across the router tests.
    const TEST_TOKEN: &str = "test-token-abcDEF-123_xyz";
    /// A wrong-but-valid-alphabet credential a client might present.
    const WRONG_TOKEN: &str = "wrong-token-000";
    /// The port the test router's Host allowlist is built for.
    const TEST_PORT: u16 = 8099;

    fn test_addr() -> SocketAddr {
        format!("127.0.0.1:{TEST_PORT}").parse().expect("addr")
    }

    fn test_state(mutations_permitted: bool) -> HttpState {
        test_state_with_sse_budget(mutations_permitted, MAX_SSE_STREAMS)
    }

    /// A test state with an explicit SSE stream budget, so a cap test can drive
    /// takeover / 503-at-cap deterministically with a size-1 budget.
    fn test_state_with_sse_budget(mutations_permitted: bool, sse_budget: usize) -> HttpState {
        HttpState {
            cqs_dir: Arc::new(PathBuf::from("/nonexistent-cqs-test-dir/.cqs")),
            token: AuthToken::try_from_string(TEST_TOKEN).expect("valid test token"),
            sessions: Arc::new(SessionStore::new()),
            mutations_permitted,
            result_budget: Arc::new(tokio::sync::Semaphore::new(TASKS_RESULT_BLOCKING_BUDGET)),
            result_fanout: Arc::new(Mutex::new(HashMap::new())),
            sse_budget: Arc::new(Semaphore::new(sse_budget)),
            // The shutdown tee. The `Sender` is held in the state `Arc` for the
            // test's lifetime, so a stream's `subscribe()`d receiver stays open;
            // a test fires `state.shutdown.send(true)` to exercise the real path.
            shutdown: Arc::new(watch::channel(false).0),
        }
    }

    fn test_router(mutations_permitted: bool) -> Router {
        build_router(
            test_state(mutations_permitted),
            allowed_host_set(&test_addr()),
        )
    }

    /// Builder for a `/mcp` request with a valid Host and optional headers.
    #[derive(Default)]
    struct Req {
        method: &'static str,
        body: String,
        bearer: Option<String>,
        session: Option<String>,
        origin: Option<String>,
        protocol: Option<String>,
        host: Option<String>,
        accept: Option<String>,
    }

    impl Req {
        fn post(body: &str) -> Self {
            Self {
                method: "POST",
                body: body.to_string(),
                bearer: Some(TEST_TOKEN.to_string()),
                ..Default::default()
            }
        }
        fn get() -> Self {
            Self {
                method: "GET",
                bearer: Some(TEST_TOKEN.to_string()),
                ..Default::default()
            }
        }
        fn delete() -> Self {
            Self {
                method: "DELETE",
                bearer: Some(TEST_TOKEN.to_string()),
                ..Default::default()
            }
        }
        fn bearer(mut self, t: Option<&str>) -> Self {
            self.bearer = t.map(str::to_string);
            self
        }
        fn session(mut self, s: &str) -> Self {
            self.session = Some(s.to_string());
            self
        }
        fn origin(mut self, o: &str) -> Self {
            self.origin = Some(o.to_string());
            self
        }
        fn protocol(mut self, p: &str) -> Self {
            self.protocol = Some(p.to_string());
            self
        }
        fn host(mut self, h: &str) -> Self {
            self.host = Some(h.to_string());
            self
        }
        fn accept(mut self, a: &str) -> Self {
            self.accept = Some(a.to_string());
            self
        }
        /// A GET that accepts the SSE media type (the stream-open Accept).
        fn get_sse() -> Self {
            Self::get().accept("text/event-stream")
        }
        fn build(self) -> HttpRequest<Body> {
            let host = self
                .host
                .unwrap_or_else(|| format!("127.0.0.1:{TEST_PORT}"));
            let mut b = HttpRequest::builder()
                .method(self.method)
                .uri("/mcp")
                .header("host", host);
            if let Some(t) = self.bearer {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            if let Some(s) = self.session {
                b = b.header(SESSION_HEADER, s);
            }
            if let Some(o) = self.origin {
                b = b.header("origin", o);
            }
            if let Some(p) = self.protocol {
                b = b.header(PROTOCOL_VERSION_HEADER, p);
            }
            if let Some(a) = self.accept {
                b = b.header(header::ACCEPT, a);
            }
            b.body(Body::from(self.body)).expect("build request")
        }
    }

    async fn send(router: &Router, req: Req) -> axum::http::Response<Body> {
        router.clone().oneshot(req.build()).await.expect("oneshot")
    }

    async fn body_json(resp: axum::http::Response<Body>) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("collect body");
        if bytes.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&bytes).expect("parse json body")
    }

    const INIT_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#;

    /// Initialize and return the server-issued session id.
    async fn init_session(router: &Router) -> String {
        let resp = send(router, Req::post(INIT_BODY)).await;
        assert_eq!(resp.status(), StatusCode::OK, "initialize must return 200");
        resp.headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .expect("initialize must issue an MCP-Session-Id")
            .to_string()
    }

    // ── endpoint methods ──────────────────────────────────────────────

    #[tokio::test]
    async fn initialize_issues_session_and_returns_json() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let sid = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .expect("session id header");
        assert!(!sid.is_empty());
        // Visible-ASCII charset MUST (0x21–0x7E).
        assert!(sid.bytes().all(|b| (0x21..=0x7e).contains(&b)));
        let body = body_json(resp).await;
        assert_eq!(
            body.get("result")
                .and_then(|r| r.get("protocolVersion"))
                .and_then(|v| v.as_str()),
            Some(super::super::lifecycle::MCP_PROTOCOL_VERSION)
        );
    }

    #[tokio::test]
    async fn post_notification_returns_202_no_body() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(bytes.is_empty(), "202 notification must carry no body");
    }

    #[tokio::test]
    async fn post_response_no_method_returns_202() {
        // A JSON-RPC response (no `method`) posted by the client is inert → 202.
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":7,"result":{}}"#),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn get_without_sse_accept_returns_405() {
        // A GET with no Accept (or a non-SSE Accept) opens no stream → the 405
        // floor is preserved; only an explicit `text/event-stream` flips to SSE.
        let router = test_router(true);
        let resp = send(&router, Req::get()).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        let resp = send(&router, Req::get().accept("application/json")).await;
        assert_eq!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "a non-SSE Accept must still 405"
        );
    }

    #[tokio::test]
    async fn delete_tears_down_session() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(&router, Req::delete().session(&sid)).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        // The session is gone: a follow-up non-initialize request 404s.
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_method_verb_returns_405() {
        let router = test_router(true);
        let req = HttpRequest::builder()
            .method("PUT")
            .uri("/mcp")
            .header("host", format!("127.0.0.1:{TEST_PORT}"))
            .header("authorization", format!("Bearer {TEST_TOKEN}"))
            .body(Body::empty())
            .expect("req");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // ── auth ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn valid_token_authenticates() {
        let router = test_router(true);
        // A valid token reaches the handler (initialize succeeds).
        let resp = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_token_is_401() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).bearer(Some(WRONG_TOKEN))).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_token_is_401() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).bearer(None)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unauthenticated_delete_is_401_not_teardown() {
        // A session exists; an UNAUTH DELETE naming it must 401, not tear it down.
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(&router, Req::delete().bearer(None).session(&sid)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // The victim session survives: an authed request on it still works.
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unauthenticated_get_is_401() {
        let router = test_router(true);
        let resp = send(&router, Req::get().bearer(None)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// The token (server's OR a presented one) never appears in any log line.
    #[test]
    fn token_never_logged_on_auth_reject() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(CaptureWriter(Arc::clone(&buf)))
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async {
                let router = test_router(true);
                // Reject path logs the warn (method+path only).
                let resp = send(&router, Req::post(INIT_BODY).bearer(Some(WRONG_TOKEN))).await;
                assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
            });
        });
        let logs = String::from_utf8(buf.lock().expect("lock").clone()).expect("utf8");
        assert!(
            !logs.contains(TEST_TOKEN),
            "server token must never appear in logs; got:\n{logs}"
        );
        assert!(
            !logs.contains(WRONG_TOKEN),
            "presented credential must never appear in logs; got:\n{logs}"
        );
    }

    /// A capturing `MakeWriter` that appends to a shared buffer.
    #[derive(Clone)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    // ── token source (file mode / env) ────────────────────────────────

    #[test]
    fn token_file_good_mode_loads() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(f, "{TEST_TOKEN}").expect("write"); // trailing newline is trimmed
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        // On a native unix fs (tempdir → /tmp) the mode check applies and passes.
        let token = load_token(Some(&path)).expect("token file should load");
        assert_eq!(token.as_str(), TEST_TOKEN);
    }

    #[test]
    fn token_file_group_readable_refused() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(f, "{TEST_TOKEN}").expect("write");
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        // Skip ONLY on a genuine drvfs tempdir where modes are synthetic (the
        // mode check is intentionally skipped there, so this negative case can't
        // fire). Path-scoped, NOT host-global `is_wsl()`: on a native tempdir a
        // WSL host still reports real modes, and the refusal must apply — else
        // this guard is vacuous on the project's own dev box.
        if cqs::config::is_wsl_drvfs_path(&path) {
            return;
        }
        let err = load_token(Some(&path)).expect_err("group-readable token must be refused");
        assert!(
            err.to_string().contains("group/world-accessible"),
            "unexpected error: {err}"
        );
    }

    #[test]
    #[serial_test::serial(cqs_mcp_http_token_env)]
    fn env_token_fallback_loads() {
        // SAFETY: serial-gated; restored below.
        let prior = std::env::var("CQS_MCP_HTTP_TOKEN").ok();
        std::env::set_var("CQS_MCP_HTTP_TOKEN", TEST_TOKEN);
        let token = load_token(None).expect("env token should load");
        assert_eq!(token.as_str(), TEST_TOKEN);
        match prior {
            Some(p) => std::env::set_var("CQS_MCP_HTTP_TOKEN", p),
            None => std::env::remove_var("CQS_MCP_HTTP_TOKEN"),
        }
    }

    #[test]
    #[serial_test::serial(cqs_mcp_http_token_env)]
    fn no_token_source_is_error() {
        let prior = std::env::var("CQS_MCP_HTTP_TOKEN").ok();
        std::env::remove_var("CQS_MCP_HTTP_TOKEN");
        let err = load_token(None).expect_err("no token source must error");
        assert!(err.to_string().contains("requires a bearer token"));
        if let Some(p) = prior {
            std::env::set_var("CQS_MCP_HTTP_TOKEN", p);
        }
    }

    /// REGRESSION GUARD — RT token-file breach.
    ///
    /// A group/world-readable token file on a NATIVE unix filesystem must be
    /// REFUSED (3a-D2: the mode check is the protection that keeps the bearer
    /// token from every local UID, the boundary a TCP listener widens).
    ///
    /// This guards against a reintroduction of the host-global skip disjunct:
    /// `read_token_file_checked` once ORed `is_wsl()` (host-global) with
    /// `is_wsl_drvfs_path(path)` (path-scoped), so on ANY WSL host the mode
    /// check was skipped for EVERY path — a 0644 token on tmpfs/ext4 (e.g.
    /// `$XDG_RUNTIME_DIR`) was ACCEPTED, disclosing it to every local UID. The
    /// skip is now path-scoped (`is_wsl_drvfs_path` alone), so a native-fs
    /// tempdir must trip the refusal on a WSL dev box and non-WSL CI alike. The
    /// `assert!` below pins the precondition (tempdir is native, not drvfs) so a
    /// green pass can only mean the refusal actually fired.
    #[test]
    fn token_file_group_readable_refused_on_native_fs() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(f, "{TEST_TOKEN}").expect("write");
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        // The precondition the finding rests on: a tempdir is a NATIVE fs, NOT
        // a `/mnt/<x>` drvfs mount, so the mode check MUST apply here — the skip
        // must be path-scoped, never host-global.
        assert!(
            !cqs::config::is_wsl_drvfs_path(&path),
            "test dir must be native fs (not a drvfs mount): {}",
            path.display()
        );
        let err = load_token(Some(&path))
            .expect_err("a group/world-readable token file on a NATIVE unix fs must be refused");
        assert!(
            err.to_string().contains("group/world-accessible"),
            "unexpected error: {err}"
        );
    }

    // ── origin / host ─────────────────────────────────────────────────

    #[tokio::test]
    async fn origin_present_valid_ok() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(INIT_BODY).origin("http://localhost:5173"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn origin_present_invalid_403() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(INIT_BODY).origin("http://evil.example.com"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn origin_absent_ok() {
        // No Origin header → allowed (headless agents send none).
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn origin_null_rejected() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).origin("null")).await;
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "Origin: null is rejected (distinct from absent)"
        );
    }

    #[tokio::test]
    async fn localhost_subdomain_origin_rejected() {
        // `http://localhost.evil.com` must NOT pass the suffix-boundary check.
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(INIT_BODY).origin("http://localhost.evil.com"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn missing_host_rejected() {
        // The Host allowlist (reused from serve) rejects a missing/disallowed Host.
        let router = test_router(true);
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", format!("Bearer {TEST_TOKEN}"))
            .body(Body::from(INIT_BODY))
            .expect("req"); // no Host header
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn disallowed_host_rejected() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).host("evil.example.com")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── sessions ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn client_proposed_session_id_ignored_fixation() {
        // A client presents its OWN MCP-Session-Id on initialize; the server
        // MUST issue a fresh one instead (fixation closed).
        let router = test_router(true);
        let attacker_id = "attacker-chosen-session-id";
        let resp = send(&router, Req::post(INIT_BODY).session(attacker_id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let issued = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .expect("issued session id");
        assert_ne!(
            issued, attacker_id,
            "server must NOT adopt a client-proposed session id"
        );
    }

    #[tokio::test]
    async fn unknown_session_is_404() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
                .session("never-issued-session"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn non_initialize_without_session_is_400() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn session_store_expiry_and_teardown() {
        let store = SessionStore {
            inner: Mutex::new(HashMap::new()),
            ttl: Duration::from_millis(0), // everything is immediately expired
            cap: MAX_SESSIONS,
        };
        let id = store.create().expect("create");
        // ttl=0 → any elapsed time expires it; touch reports gone and removes it.
        assert!(!store.touch(&id), "zero-ttl session must read as expired");
        assert!(
            !store.remove(&id),
            "expired session already evicted by touch"
        );

        let live = SessionStore::new();
        let id = live.create().expect("create");
        assert!(live.touch(&id), "fresh session is live");
        assert!(live.remove(&id), "remove reports the session was present");
        assert!(!live.touch(&id), "removed session is gone");
    }

    #[test]
    fn session_cap_refuses_beyond_limit() {
        let store = SessionStore {
            inner: Mutex::new(HashMap::new()),
            ttl: SESSION_IDLE_TTL,
            cap: 2,
        };
        assert!(store.create().is_some());
        assert!(store.create().is_some());
        assert!(store.create().is_none(), "third create must hit the cap");
    }

    // ── mutation withhold (listener-side triple gate) ─────────────────

    #[tokio::test]
    async fn mutation_tools_absent_when_not_permitted() {
        // mutations_permitted=false models a non-loopback bind (or flag off):
        // tools/list must NOT advertise cqs_notes_add.
        let router = test_router(false);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":5,"method":"tools/list"}"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let names = tool_names(&body);
        assert!(
            !names.iter().any(|n| n == "cqs_notes_add"),
            "mutators must be withheld from tools/list off-loopback; got {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "cqs_search"),
            "read tools must still be present"
        );
    }

    #[tokio::test]
    async fn mutation_tools_present_when_permitted() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":6,"method":"tools/list"}"#).session(&sid),
        )
        .await;
        let body = body_json(resp).await;
        let names = tool_names(&body);
        assert!(
            names.iter().any(|n| n == "cqs_notes_add"),
            "mutators must be present when permitted; got {names:?}"
        );
    }

    #[tokio::test]
    async fn mutation_call_withheld_is_method_not_found() {
        // Off-loopback, a tools/call naming a mutator resolves to no tool →
        // -32601, BEFORE any daemon relay (so the daemon-missing -32603 the
        // permitted path would give never appears).
        let router = test_router(false);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(
                r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"cqs_notes_add","arguments":{"text":"x","sentiment":0.5}}}"#,
            )
            .session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64()),
            Some(-32601),
            "withheld mutator must be method-not-found, not relayed: {body}"
        );
    }

    fn tool_names(body: &Value) -> Vec<String> {
        body.get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    // ── protocol version ──────────────────────────────────────────────

    #[tokio::test]
    async fn protocol_version_invalid_400() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).protocol("1999-01-01")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn protocol_version_absent_assumes_default_not_400() {
        // No MCP-Protocol-Version header → assume 2025-03-26, NOT a 400.
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "absent protocol version must assume the default, not 400"
        );
    }

    #[tokio::test]
    async fn protocol_version_supported_ok() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).protocol("2025-06-18")).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── batch rejection / relay ───────────────────────────────────────

    #[tokio::test]
    async fn json_array_batch_rejected_400() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// REGRESSION GUARD — a POST body over [`MAX_BODY_BYTES`] (1 MiB) that
    /// DECLARES its size via `Content-Length` is refused by the outermost
    /// body-limit layer with 413 BEFORE the handler (or auth) allocates it,
    /// bounding a hostile client's per-request buffer. The layer sits OUTSIDE
    /// auth, so the cap applies even to an unauthed request (no OOM-then-401).
    /// Verified against the live listener (a real 2 MiB POST, which curl sends
    /// with `Content-Length`, → `413 length limit exceeded`); the header is set
    /// explicitly here because the in-process `oneshot` path does not synthesize
    /// one, and without a declared length the limit layer defers to lazy body
    /// reads (auth then 401s before the body is ever buffered — also safe, but a
    /// different code path). Pinned so a future loosening of the cap — or a
    /// reorder that moves the limit inside auth — trips.
    #[tokio::test]
    async fn oversized_body_rejected_by_body_limit() {
        let router = test_router(true);
        let pad = "x".repeat(MAX_BODY_BYTES + 1);
        let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"ping","pad":"{pad}"}}"#);
        let declared_len = body.len();
        // Unauthenticated on purpose: the body cap must bite before auth. The
        // declared Content-Length mirrors what a real HTTP client sends.
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", format!("127.0.0.1:{TEST_PORT}"))
            .header(header::CONTENT_LENGTH, declared_len)
            .body(Body::from(body))
            .expect("req");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn tool_call_daemon_absent_is_clean_jsonrpc_error() {
        // A real tool call relays through bridge::route_with to the daemon;
        // with no daemon at the (nonexistent) cqs_dir it is a clean JSON-RPC
        // -32603 in the body — NOT a panic or a 500.
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(
                r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"cqs_search","arguments":{"query":"retry backoff"}}}"#,
            )
            .session(&sid),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "daemon-absent is a body-level error, not HTTP 500"
        );
        let body = body_json(resp).await;
        assert_eq!(
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64()),
            Some(-32603),
            "daemon-absent relay must be a clean -32603: {body}"
        );
        let msg = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(
            msg.contains("daemon"),
            "error must mention the daemon: {msg}"
        );
    }

    // ── concurrency cap ───────────────────────────────────────────────

    fn test_router_with_sem(sem: Arc<tokio::sync::Semaphore>) -> Router {
        build_router_with_sem(test_state(true), allowed_host_set(&test_addr()), sem)
    }

    /// Saturation: with every permit held, a further request 503s — and does so
    /// BEFORE auth (the request is unauthenticated, yet gets 503, not 401),
    /// which proves the cap is the outermost layer. This is the availability
    /// guarantee: one steered-but-authenticated client cannot fan out past the
    /// cap and exhaust the shared daemon for serve / CLI / other consumers.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrency_cap_saturated_503s_before_auth() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        // Hold the sole permit so the middleware sees a saturated semaphore on
        // `try_acquire`.
        let _held = Arc::clone(&sem)
            .try_acquire_owned()
            .expect("initial permit");
        let router = test_router_with_sem(sem);

        // Unauthenticated on purpose: were the cap inside auth, this would 401.
        let resp = send(&router, Req::post(INIT_BODY).bearer(None)).await;
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a saturated cap must 503 before auth (an unauth request must NOT 401)"
        );
    }

    /// A permit is released once the response completes, so the cap is never
    /// permanently consumed: a second request under a size-1 cap still succeeds.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrency_cap_permit_released_after_response() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let router = test_router_with_sem(sem);

        // First request acquires and (on completion) releases the sole permit.
        let resp1 = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(resp1.status(), StatusCode::OK);

        // Second request: the permit was freed, so the cap does not block it.
        let resp2 = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(
            resp2.status(),
            StatusCode::OK,
            "permit must be released after the first response completes"
        );
    }

    // ── tasks surface ─────────────────────────────────────────────────

    /// the block-budget isolation (the load-bearing guard): a blocking `tasks/result` must NOT hold
    /// one of the outermost one-shot permits for the duration of the block. With a
    /// size-1 outer cap held (as the middleware would), a block that is in
    /// progress releases that permit — so a one-shot request can still acquire it.
    /// Mirrors the style of `concurrency_cap_saturated_503s_before_auth`, driven
    /// through the real `blocking_result_with_poll` stack with an injected poll.
    #[tokio::test(flavor = "multi_thread")]
    async fn tasks_result_block_releases_outer_permit_rt_dos_2() {
        use std::sync::atomic::{AtomicBool, Ordering};

        // Size-1 outer cap, its sole permit held (the middleware's owned permit).
        let outer = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&outer).try_acquire_owned().expect("hold outer");
        let outer_permit = Some(Arc::new(held));
        assert_eq!(outer.available_permits(), 0, "outer starts saturated");

        let budget = Arc::new(tokio::sync::Semaphore::new(TASKS_RESULT_BLOCKING_BUDGET));
        let fanout = Arc::new(Mutex::new(HashMap::new()));

        // The poll stays Pending until released, then terminates the block.
        let release = Arc::new(AtomicBool::new(false));
        let poll_release = Arc::clone(&release);
        let poll = move || {
            let release = Arc::clone(&poll_release);
            async move {
                if release.load(Ordering::SeqCst) {
                    super::super::tasks::ResultStep::Done(super::super::tasks::task_gone(Some(
                        Value::from(1),
                    )))
                } else {
                    super::super::tasks::ResultStep::Pending
                }
            }
        };

        let handle = tokio::spawn(blocking_result_with_poll(
            outer_permit,
            Arc::clone(&budget),
            Arc::clone(&fanout),
            Some(Value::from(1)),
            "tid".to_string(),
            poll,
        ));

        // Let the block acquire its budget, RELEASE the outer permit, and enter
        // the poll loop (its first poll returns Pending, then it sleeps).
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        // The load-bearing assertion: the outermost permit is free DURING the
        // block, so a one-shot request would get it (no 503).
        assert_eq!(
            outer.available_permits(),
            1,
            "a blocking tasks/result must release the outermost permit (the block-budget isolation)"
        );
        // The block holds exactly one budget permit.
        assert_eq!(
            budget.available_permits(),
            TASKS_RESULT_BLOCKING_BUDGET - 1,
            "the block holds one isolated budget permit, not an outer permit"
        );

        // Terminate the block; the budget permit releases afterward.
        release.store(true, Ordering::SeqCst);
        let _ = handle.await.expect("block joins");
        assert_eq!(
            budget.available_permits(),
            TASKS_RESULT_BLOCKING_BUDGET,
            "the budget permit is released after the block ends"
        );
    }

    /// Per-`taskId` fan-out cap: at most `MAX_BLOCKS_PER_TASK` concurrent blocks
    /// on one task; a further block on the SAME task is refused, while a DIFFERENT
    /// task is independent, and dropping a guard frees a slot.
    #[test]
    fn fanout_guard_caps_blocks_per_task() {
        let map = Arc::new(Mutex::new(HashMap::new()));
        let g1 = FanoutGuard::try_acquire(Arc::clone(&map), "t".to_string());
        let g2 = FanoutGuard::try_acquire(Arc::clone(&map), "t".to_string());
        assert!(g1.is_some() && g2.is_some(), "up to the cap acquires");
        assert!(
            FanoutGuard::try_acquire(Arc::clone(&map), "t".to_string()).is_none(),
            "a block past the per-task cap is refused"
        );
        assert!(
            FanoutGuard::try_acquire(Arc::clone(&map), "other".to_string()).is_some(),
            "a different task is independent of the cap"
        );
        drop(g1);
        assert!(
            FanoutGuard::try_acquire(Arc::clone(&map), "t".to_string()).is_some(),
            "dropping a guard frees a per-task slot"
        );
    }

    /// The blocking budget saturating returns a retryable JSON-RPC error (not a
    /// 503, not a hang) so a client falls back to `tasks/get` polling.
    #[tokio::test]
    async fn tasks_result_budget_saturated_is_retryable_error() {
        let outer_permit = None; // no outer cap in this unit
        let budget = Arc::new(tokio::sync::Semaphore::new(0)); // pre-drained
        let fanout = Arc::new(Mutex::new(HashMap::new()));
        let poll = || async { super::super::tasks::ResultStep::Pending };
        let resp = blocking_result_with_poll(
            outer_permit,
            budget,
            fanout,
            Some(Value::from(1)),
            "tid".to_string(),
            poll,
        )
        .await;
        let err = resp.error.expect("budget saturation is an error");
        assert_eq!(err.code, super::super::lifecycle::INTERNAL_ERROR);
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d.get("retryable"))
                .and_then(|b| b.as_bool()),
            Some(true),
            "the reject must be marked retryable"
        );
    }

    /// `tasks/result` requires a session (non-initialize): missing header → 400,
    /// unknown session → 404 (never an HTTP 404 from a gone task — that is
    /// `-32602`; the 404 here is strictly session-expiry).
    #[tokio::test]
    async fn tasks_result_without_session_is_400() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(
                r#"{"jsonrpc":"2.0","id":1,"method":"tasks/result","params":{"taskId":"x"}}"#,
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn tasks_result_unknown_session_is_404() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(
                r#"{"jsonrpc":"2.0","id":1,"method":"tasks/result","params":{"taskId":"x"}}"#,
            )
            .session("never-issued-session"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// A `tasks/result` / `tasks/get` with a missing `taskId` is `-32602`
    /// (task-gone code), never an HTTP 404.
    #[tokio::test]
    async fn tasks_result_missing_task_id_is_minus_32602() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":1,"method":"tasks/result","params":{}}"#)
                .session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64()),
            Some(-32602),
            "missing taskId must be -32602 (never HTTP 404): {body}"
        );
    }

    /// `tasks/get` with a taskId but no daemon relays a clean JSON-RPC error in
    /// the body (never a 500), and the outer permit is not leaked (a subsequent
    /// request under a size-1 cap still succeeds).
    #[tokio::test(flavor = "multi_thread")]
    async fn tasks_result_daemon_absent_does_not_leak_permit() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let router = test_router_with_sem(sem);
        let sid = init_session(&router).await;
        // Daemon absent (nonexistent cqs_dir): the block's first poll fails and
        // returns immediately as a body-level error, not a hang or a 500.
        let resp = send(
            &router,
            Req::post(
                r#"{"jsonrpc":"2.0","id":2,"method":"tasks/result","params":{"taskId":"tid"}}"#,
            )
            .session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(
            body.get("error").is_some(),
            "daemon-absent tasks/result is a clean JSON-RPC error: {body}"
        );
        // The permit was released: another request under the size-1 cap succeeds.
        let resp2 = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(
            resp2.status(),
            StatusCode::OK,
            "tasks/result must not leak the outer permit"
        );
    }

    /// Augmenting a non-eval tool with a `task` field is `-32601` (only cqs_eval
    /// is task-augmentable).
    #[tokio::test]
    async fn augmenting_non_eval_tool_is_minus_32601() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cqs_search","arguments":{"query":"x"},"task":{}}}"#,
            )
            .session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64()),
            Some(-32601),
            "augmenting a non-eval tool must be -32601: {body}"
        );
    }

    // ── origin helper unit ────────────────────────────────────────────

    #[test]
    fn is_localhost_origin_boundaries() {
        assert!(is_localhost_origin("http://localhost"));
        assert!(is_localhost_origin("http://localhost:3000"));
        assert!(is_localhost_origin("http://127.0.0.1:8080"));
        assert!(is_localhost_origin("https://[::1]:9000"));
        assert!(is_localhost_origin("http://LOCALHOST:3000")); // case-folded
        assert!(!is_localhost_origin("http://localhost.evil.com"));
        assert!(!is_localhost_origin("http://notlocalhost"));
        assert!(!is_localhost_origin("null"));
        assert!(!is_localhost_origin("https://example.com"));
    }

    // ── SSE progress stream (P3a-2) ───────────────────────────────────

    use super::super::tasks::ProgressStep;
    use futures_util::StreamExt as _;
    use serde_json::json;

    /// A `found` + `working` step at the given progress.
    fn working(done: u64, total: u64) -> ProgressStep {
        ProgressStep::Found {
            progress: Some((done, total)),
            terminal: false,
            descriptor: json!({
                "taskId": "tid", "status": "working",
                "createdAt": "t0", "lastUpdatedAt": "t1", "ttl": 900,
                "progress": { "done": done, "total": total }
            }),
        }
    }

    /// A terminal (`completed`) step; the descriptor is the flat daemon Task the
    /// optional `tasks/status` frame is shaped from (`ttl` in SECONDS on the wire).
    fn completed() -> ProgressStep {
        ProgressStep::Found {
            progress: Some((5, 5)),
            terminal: true,
            descriptor: json!({
                "taskId": "tid", "status": "completed",
                "createdAt": "t0", "lastUpdatedAt": "t2", "ttl": 900
            }),
        }
    }

    /// A degraded `found` step: NO `{done,total}` (a pre-PR1 / counter-less
    /// daemon) → keep-alive only, never a synthesized value.
    fn degraded() -> ProgressStep {
        ProgressStep::Found {
            progress: None,
            terminal: false,
            descriptor: json!({ "taskId": "tid", "status": "working" }),
        }
    }

    type BoxStep = std::pin::Pin<Box<dyn std::future::Future<Output = ProgressStep> + Send>>;

    /// A poll yielding a scripted queue of steps, then `Gone` when drained.
    fn scripted(steps: Vec<ProgressStep>) -> impl FnMut(String) -> BoxStep {
        let q = Arc::new(Mutex::new(std::collections::VecDeque::from(steps)));
        move |_task_id| {
            let q = Arc::clone(&q);
            Box::pin(async move {
                q.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .pop_front()
                    .unwrap_or(ProgressStep::Gone)
            })
        }
    }

    /// A poll yielding `make()` forever (keep-alive / shutdown / cancel tests where
    /// the stream must not self-terminate).
    fn repeat(make: fn() -> ProgressStep) -> impl FnMut(String) -> BoxStep {
        move |_task_id| Box::pin(async move { make() })
    }

    fn session_with_watch(task_id: &str, token: Value) -> Arc<SessionState> {
        let s = Arc::new(SessionState::new(Instant::now()));
        s.record_watch(task_id, token);
        s
    }

    /// Drive an SSE stream to completion and return the parsed `data:` frames.
    async fn drain_frames<S>(stream: S) -> Vec<Value>
    where
        S: futures_util::Stream<Item = Result<Event, std::convert::Infallible>> + Send + 'static,
    {
        let resp = Sse::new(stream).into_response();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("collect sse body");
        let text = String::from_utf8(bytes.to_vec()).expect("utf8");
        text.lines()
            .filter_map(|l| l.strip_prefix("data:").map(str::trim))
            .filter(|s| !s.is_empty())
            .map(|s| serde_json::from_str::<Value>(s).expect("json data line"))
            .collect()
    }

    /// Build a stream over `session` with a scripted poll, a live SSE budget, a
    /// held-open shutdown sender, and a zero poll interval (deterministic + fast).
    fn build_test_stream<P, Fut>(
        session: Arc<SessionState>,
        poll: P,
    ) -> (
        impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>> + Send + 'static,
        watch::Sender<bool>,
    )
    where
        P: FnMut(String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ProgressStep> + Send,
    {
        let (tx, _rx0) = watch::channel(false);
        let rx = tx.subscribe();
        let budget = Arc::new(Semaphore::new(16));
        let cancel = session
            .take_stream_slot(&budget)
            .expect("first slot acquires");
        let guard = StreamGuard {
            session: Arc::clone(&session),
            cancel: Arc::clone(&cancel),
        };
        let stream = build_sse_stream(session, cancel, rx, Duration::ZERO, guard, poll);
        (stream, tx)
    }

    fn progress_dones(frames: &[Value]) -> Vec<u64> {
        frames
            .iter()
            .filter(|f| f["method"] == "notifications/progress")
            .map(|f| f["params"]["progress"].as_u64().expect("progress u64"))
            .collect()
    }

    /// Monotone-per-token high-water gate: a poll observing 3, 3, 5 emits progress
    /// frames for 3 and 5 only (the middle status-only 3 emits NOTHING).
    #[tokio::test]
    async fn sse_progress_high_water_gate_3_3_5() {
        let session = session_with_watch("tid", Value::from("tok-1"));
        let (stream, _tx) = build_test_stream(
            Arc::clone(&session),
            scripted(vec![
                working(3, 10),
                working(3, 10),
                working(5, 10),
                completed(),
            ]),
        );
        let frames = drain_frames(stream).await;
        assert_eq!(
            progress_dones(&frames),
            vec![3, 5],
            "gate emits 3 then 5, never the middle status-only 3: {frames:?}"
        );
        // First progress frame carries total + verbatim token + related-task _meta.
        let p0 = &frames[0];
        assert_eq!(p0["params"]["total"].as_u64(), Some(10));
        assert_eq!(p0["params"]["progressToken"], Value::from("tok-1"));
        assert_eq!(
            p0["params"]["_meta"]["io.modelcontextprotocol/related-task"]["taskId"],
            Value::from("tid")
        );
    }

    /// Terminal ordering: exactly one progress frame (done=5), then ONE terminal
    /// `tasks/status` frame, then the stream ends — zero progress frames at/after
    /// terminal. Also pins the integer progressToken carried verbatim.
    #[tokio::test]
    async fn sse_terminal_emits_status_not_progress() {
        let session = session_with_watch("tid", Value::from(7));
        let (stream, _tx) = build_test_stream(
            Arc::clone(&session),
            scripted(vec![working(5, 5), completed()]),
        );
        let frames = drain_frames(stream).await;
        assert_eq!(
            frames.len(),
            2,
            "progress(5) then tasks/status only: {frames:?}"
        );
        assert_eq!(frames[0]["method"], "notifications/progress");
        assert_eq!(frames[0]["params"]["progress"].as_u64(), Some(5));
        assert_eq!(
            frames[0]["params"]["progressToken"],
            Value::from(7),
            "integer progressToken carried verbatim"
        );
        assert_eq!(frames[1]["method"], "notifications/tasks/status");
        assert_eq!(frames[1]["params"]["taskId"], Value::from("tid"));
        assert_eq!(frames[1]["params"]["status"], Value::from("completed"));
        // ttl converted secs→ms at the boundary; no progress / related-task _meta.
        assert_eq!(frames[1]["params"]["ttl"].as_u64(), Some(900_000));
        assert!(
            frames[1]["params"]["_meta"].is_null(),
            "tasks/status SHOULD-NOT carry related-task _meta"
        );
        assert!(frames[1]["params"]["progress"].is_null());
        assert!(
            session.latest_watch().is_none(),
            "the watch entry is pruned at terminal"
        );
    }

    /// Reconnect: a fresh stream re-observing an already-seen `done` (5) emits
    /// nothing (the session-scoped high-water persists across the disconnect); only
    /// the advance to 7 emits.
    #[tokio::test]
    async fn sse_reconnect_high_water_suppresses_seen_done() {
        let session = session_with_watch("tid", Value::from("tok"));
        // Stream 1: emit done=5, then DISCONNECT (drop) — the entry is NOT pruned.
        {
            let (stream, _tx) =
                build_test_stream(Arc::clone(&session), scripted(vec![working(5, 10)]));
            let mut s = std::pin::pin!(stream);
            assert!(s.next().await.is_some(), "stream 1 emits its first frame");
        }
        assert_eq!(
            session.latest_watch().map(|w| w.2),
            Some(Some(5)),
            "the high-water persists across the disconnect"
        );
        // Stream 2 (reconnect): a re-observed 5 is suppressed; only 7 emits.
        let (stream2, _tx2) = build_test_stream(
            Arc::clone(&session),
            scripted(vec![working(5, 10), working(7, 10), completed()]),
        );
        let frames = drain_frames(stream2).await;
        assert_eq!(
            progress_dones(&frames),
            vec![7],
            "reconnect re-emits nothing already seen (5), only the advance to 7: {frames:?}"
        );
    }

    /// Degraded mode: a descriptor with no `{done,total}` never synthesizes a
    /// progress frame — keep-alive only, then the terminal frame.
    #[tokio::test]
    async fn sse_degraded_descriptor_emits_no_progress() {
        let session = session_with_watch("tid", Value::from("tok"));
        let (stream, _tx) = build_test_stream(
            Arc::clone(&session),
            scripted(vec![degraded(), degraded(), completed()]),
        );
        let frames = drain_frames(stream).await;
        assert!(
            progress_dones(&frames).is_empty(),
            "a counter-less descriptor must NEVER synthesize progress: {frames:?}"
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["method"], "notifications/tasks/status");
    }

    /// Daemon-restart rule: an authoritative `Gone` closes the stream with NO
    /// frame (never a fabricated terminal), and prunes the watch entry.
    #[tokio::test]
    async fn sse_gone_closes_stream_no_frame() {
        let session = session_with_watch("tid", Value::from("tok"));
        let (stream, _tx) =
            build_test_stream(Arc::clone(&session), scripted(vec![ProgressStep::Gone]));
        let frames = drain_frames(stream).await;
        assert!(
            frames.is_empty(),
            "authoritative Gone closes with no frame: {frames:?}"
        );
        assert!(
            session.latest_watch().is_none(),
            "Gone prunes the watch entry"
        );
    }

    /// Daemon-restart rule: a transient dial failure RETRIES (does not close) — a
    /// later progress + terminal still arrive.
    #[tokio::test]
    async fn sse_dial_failure_retries_not_closes() {
        let session = session_with_watch("tid", Value::from("tok"));
        let (stream, _tx) = build_test_stream(
            Arc::clone(&session),
            scripted(vec![ProgressStep::DialFailed, working(3, 10), completed()]),
        );
        let frames = drain_frames(stream).await;
        assert_eq!(
            progress_dones(&frames),
            vec![3],
            "a dial failure retries; the later progress still emits: {frames:?}"
        );
        assert_eq!(
            frames.last().unwrap()["method"],
            "notifications/tasks/status"
        );
    }

    /// The stream ends on the TEED shutdown signal (the real watch the listener
    /// feeds from `shutdown_signal()`), not a hang — driven through the actual
    /// `select!` path, not the watch in isolation.
    #[tokio::test(flavor = "multi_thread")]
    async fn sse_stream_ends_on_teed_shutdown() {
        let session = session_with_watch("tid", Value::from("tok"));
        let (tx, _rx0) = watch::channel(false);
        let rx = tx.subscribe();
        let budget = Arc::new(Semaphore::new(1));
        let cancel = session.take_stream_slot(&budget).expect("slot");
        let guard = StreamGuard {
            session: Arc::clone(&session),
            cancel: Arc::clone(&cancel),
        };
        // A poll that never emits (done=0) so the stream ends ONLY via shutdown.
        let stream = build_sse_stream(
            session,
            cancel,
            rx,
            Duration::from_millis(5),
            guard,
            repeat(|| working(0, 10)),
        );
        let mut s = std::pin::pin!(stream);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = tx.send(true);
        });
        let ended = tokio::time::timeout(Duration::from_secs(2), s.next()).await;
        assert!(ended.is_ok(), "shutdown must end the stream, not hang");
        assert!(
            ended.unwrap().is_none(),
            "the stream ends (None) on the teed shutdown signal"
        );
    }

    /// An empty watch list (no progressToken on the augmented call, or the stream
    /// opened before any call) is keep-alive only: the poll is NEVER consulted, so
    /// no frame is ever emitted; the stream ends only on shutdown.
    #[tokio::test(flavor = "multi_thread")]
    async fn sse_no_watch_entry_is_keepalive_only() {
        let session = Arc::new(SessionState::new(Instant::now())); // NO watch entry
        let (tx, _rx0) = watch::channel(false);
        let rx = tx.subscribe();
        let budget = Arc::new(Semaphore::new(1));
        let cancel = session.take_stream_slot(&budget).expect("slot");
        let guard = StreamGuard {
            session: Arc::clone(&session),
            cancel: Arc::clone(&cancel),
        };
        // If ever consulted this poll WOULD emit (done=9); the empty watch means it
        // is not, proving no-watch ⇒ no frame.
        let stream = build_sse_stream(
            session,
            cancel,
            rx,
            Duration::from_millis(5),
            guard,
            repeat(|| working(9, 10)),
        );
        let mut s = std::pin::pin!(stream);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = tx.send(true);
        });
        let first = tokio::time::timeout(Duration::from_secs(2), s.next())
            .await
            .expect("no hang");
        assert!(
            first.is_none(),
            "an empty watch list is keep-alive only — no frame, ends at shutdown"
        );
    }

    /// `augmented_progress_token` extracts a string OR integer token from an
    /// augmented `tools/call`, and NONE otherwise (no token ⇒ no watch ⇒ no frames).
    #[test]
    fn augmented_progress_token_extraction() {
        let mk = |b: Value| -> JsonRpcRequest { serde_json::from_value(b).expect("req") };
        let string_tok = mk(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"cqs_eval","task":{},"_meta":{"progressToken":"tok-1"}}}));
        assert_eq!(
            augmented_progress_token(&string_tok),
            Some(Value::from("tok-1"))
        );
        let int_tok = mk(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"cqs_eval","task":{},"_meta":{"progressToken":42}}}));
        assert_eq!(augmented_progress_token(&int_tok), Some(Value::from(42)));
        // Augmented but no token → None (no frames for that task).
        let no_meta = mk(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"cqs_eval","task":{}}}));
        assert_eq!(augmented_progress_token(&no_meta), None);
        // A token on a NON-augmented call → None (not a task).
        let not_aug = mk(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"cqs_search","_meta":{"progressToken":"t"}}}));
        assert_eq!(augmented_progress_token(&not_aug), None);
        // A token on a non tools/call method → None.
        let wrong_method = mk(json!({"jsonrpc":"2.0","id":1,"method":"tasks/get",
            "params":{"_meta":{"progressToken":"t"}}}));
        assert_eq!(augmented_progress_token(&wrong_method), None);
    }

    /// The watch list is deduped by taskId and bounded.
    #[test]
    fn watch_list_deduped_and_bounded() {
        let s = SessionState::new(Instant::now());
        s.record_watch("a", Value::from("t1"));
        s.record_watch("a", Value::from("t2")); // dedupe → refresh token
        assert_eq!(s.watch_len(), 1, "same taskId dedupes");
        assert_eq!(
            s.latest_watch().map(|w| w.1),
            Some(Value::from("t2")),
            "the token is refreshed on re-register"
        );
        for i in 0..MAX_WATCH_ENTRIES_PER_SESSION + 5 {
            s.record_watch(&format!("k{i}"), Value::from(i as u64));
        }
        assert!(
            s.watch_len() <= MAX_WATCH_ENTRIES_PER_SESSION,
            "the watch list stays bounded"
        );
        assert_eq!(
            s.latest_watch().map(|w| w.0),
            Some(format!("k{}", MAX_WATCH_ENTRIES_PER_SESSION + 4)),
            "the newest entry is the latest"
        );
    }

    /// The high-water compare-and-set is monotone: a strictly-greater `done`
    /// advances and returns true; a lower OR equal `done` returns false and does
    /// NOT regress the stored value. This is the cross-takeover guard — a stale
    /// lower poll from a superseded stream can never roll the high-water back (and
    /// so can never let the live stream re-emit an already-seen value).
    #[test]
    fn high_water_cas_is_monotone() {
        let s = SessionState::new(Instant::now());
        s.record_watch("tid", Value::from("tok"));
        assert!(s.try_advance_high_water("tid", 4), "0→4 advances");
        assert_eq!(s.latest_watch().map(|w| w.2), Some(Some(4)));
        // A stale lower advance (the takeover race) is refused and does not regress.
        assert!(!s.try_advance_high_water("tid", 3), "4→3 is refused");
        assert_eq!(
            s.latest_watch().map(|w| w.2),
            Some(Some(4)),
            "a refused lower advance must NOT regress the high-water"
        );
        // An equal advance is also refused (strictly-greater only).
        assert!(!s.try_advance_high_water("tid", 4), "4→4 is refused");
        assert_eq!(s.latest_watch().map(|w| w.2), Some(Some(4)));
        // A higher advance still works.
        assert!(s.try_advance_high_water("tid", 7), "4→7 advances");
        assert_eq!(s.latest_watch().map(|w| w.2), Some(Some(7)));
        // done == 0 never advances past the floor (deliberate: no zero frame).
        let s2 = SessionState::new(Instant::now());
        s2.record_watch("tid", Value::from("tok"));
        assert!(!s2.try_advance_high_water("tid", 0), "done=0 never emits");
        assert_eq!(s2.latest_watch().map(|w| w.2), Some(None));
    }

    /// Permit handover: a takeover TRANSFERS the existing permit old→new under the
    /// session lock (no second acquire), and the superseded stream's guard does not
    /// release the transferred permit — only the live stream's guard does.
    #[test]
    fn take_stream_slot_transfers_permit_on_takeover() {
        let s = SessionState::new(Instant::now());
        let budget = Arc::new(Semaphore::new(1));
        let c1 = s.take_stream_slot(&budget).expect("first acquires");
        assert_eq!(
            budget.available_permits(),
            0,
            "first stream drains the budget"
        );
        let c2 = s
            .take_stream_slot(&budget)
            .expect("takeover succeeds even at cap");
        assert_eq!(
            budget.available_permits(),
            0,
            "takeover did NOT acquire a second permit"
        );
        assert!(!Arc::ptr_eq(&c1, &c2), "a fresh cancel handle per stream");
        // The superseded (c1) guard must NOT release the transferred permit.
        s.clear_stream_if(&c1);
        assert_eq!(
            budget.available_permits(),
            0,
            "the superseded guard leaves the transferred permit to the live stream"
        );
        // The live (c2) guard releases it.
        s.clear_stream_if(&c2);
        assert_eq!(
            budget.available_permits(),
            1,
            "the live guard releases the permit"
        );
    }

    /// A FIRST stream on a different session at cap gets `None` (→ 503) — one
    /// session cannot hold N permits and a stranger cannot exceed the budget.
    #[test]
    fn take_stream_slot_at_cap_different_session_is_none() {
        let budget = Arc::new(Semaphore::new(1));
        let a = SessionState::new(Instant::now());
        let b = SessionState::new(Instant::now());
        let _c = a
            .take_stream_slot(&budget)
            .expect("a acquires the sole permit");
        assert!(
            b.take_stream_slot(&budget).is_none(),
            "a different session at cap gets None (→ 503)"
        );
    }

    /// Idle expiry evicts the session from the map (the next request 404s) but does
    /// NOT cancel a live stream — the stream's Arc keeps its state alive (the
    /// deliberate anti-abuse choice; poll-emit never touches last_seen).
    #[test]
    fn idle_expiry_evicts_without_cancelling_stream() {
        let store = SessionStore::new();
        let id = store.create().expect("create");
        let st = store.touch_get(&id).expect("live");
        let budget = Arc::new(Semaphore::new(1));
        let cancel = st.take_stream_slot(&budget).expect("slot");
        // Force expiry: rewrite last_seen well past the 1h TTL.
        st.touch_now(Instant::now() - Duration::from_secs(60 * 60 * 2));
        assert!(
            store.touch_get(&id).is_none(),
            "an expired session 404s on the next request (map entry dropped)"
        );
        assert_eq!(
            budget.available_permits(),
            0,
            "idle expiry must NOT cancel a live stream (its permit is untouched)"
        );
        // The stream's own guard still reclaims the permit on its end.
        st.clear_stream_if(&cancel);
        assert_eq!(budget.available_permits(), 1);
    }

    /// Explicit DELETE, unlike idle expiry, cancels a live stream (notifies its
    /// cancel handle).
    #[tokio::test]
    async fn delete_cancels_live_stream() {
        let store = SessionStore::new();
        let id = store.create().expect("create");
        let st = store.touch_get(&id).expect("live");
        let budget = Arc::new(Semaphore::new(1));
        let cancel = st.take_stream_slot(&budget).expect("slot");
        // A waiter parked on the cancel handle, as the stream's select! would be.
        let waiter = {
            let c = Arc::clone(&cancel);
            tokio::spawn(async move { c.notified().await })
        };
        tokio::task::yield_now().await;
        assert!(store.remove(&id), "DELETE removes the session");
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("DELETE must cancel the live stream")
            .expect("waiter joins");
    }

    // ── GET SSE flip: HTTP-shape mirrors through the full layer stack ──

    /// A GET with `Accept: text/event-stream` on a live session opens the stream:
    /// 200 with the SSE content-type and `X-Accel-Buffering: no`. No protocol
    /// header (absent assumes the default, not 400).
    #[tokio::test]
    async fn get_sse_opens_stream_200_with_headers() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(&router, Req::get_sse().session(&sid)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .contains("text/event-stream"),
            "SSE response must carry the text/event-stream content-type"
        );
        assert_eq!(
            resp.headers()
                .get("x-accel-buffering")
                .and_then(|v| v.to_str().ok()),
            Some("no"),
            "SSE response must disable proxy buffering"
        );
        drop(resp);
    }

    #[tokio::test]
    async fn get_sse_missing_session_is_400() {
        let router = test_router(true);
        let resp = send(&router, Req::get_sse()).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_sse_unknown_session_is_404() {
        let router = test_router(true);
        let resp = send(&router, Req::get_sse().session("never-issued-session")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_sse_invalid_protocol_is_400() {
        // The protocol-version gate now runs on GET (POST-only before P3a-2).
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(&router, Req::get_sse().session(&sid).protocol("1999-01-01")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_sse_invalid_origin_is_403() {
        // The Origin layer runs router-wide, so it rejects a bad Origin on GET too
        // (no handler code — a GET-shaped test of the existing layer).
        let router = test_router(true);
        let resp = send(&router, Req::get_sse().origin("http://evil.example.com")).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_sse_disallowed_host_is_400() {
        // The Host allowlist is router-wide too: a DNS-rebinding Host on GET is
        // rejected exactly as on POST (a GET-shaped test of the existing layer).
        let router = test_router(true);
        let resp = send(&router, Req::get_sse().host("evil.example.com")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// An open SSE stream must NOT pin one of the outermost 256 one-shot permits:
    /// under a size-1 outer cap, a POST still succeeds while the SSE response is
    /// held open.
    #[tokio::test(flavor = "multi_thread")]
    async fn sse_stream_does_not_hold_outer_permit() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let router = test_router_with_sem(sem);
        let sid = init_session(&router).await;
        let get_resp = send(&router, Req::get_sse().session(&sid)).await;
        assert_eq!(get_resp.status(), StatusCode::OK);
        // Hold the SSE response (body open) and POST under the size-1 cap.
        let post_resp = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(
            post_resp.status(),
            StatusCode::OK,
            "an open SSE stream must not hold an outer 256-cap permit"
        );
        drop(get_resp);
    }

    /// Stream budget: a takeover on the same session succeeds even at cap (permit
    /// handover), while a DIFFERENT session at cap gets 503.
    #[tokio::test(flavor = "multi_thread")]
    async fn get_sse_takeover_succeeds_other_session_503s_at_cap() {
        let router = build_router(
            test_state_with_sse_budget(true, 1),
            allowed_host_set(&test_addr()),
        );
        let sid_a = init_session(&router).await;
        let sid_b = init_session(&router).await;
        // A opens the sole stream (budget=1 now drained). Hold the response.
        let a1 = send(&router, Req::get_sse().session(&sid_a)).await;
        assert_eq!(a1.status(), StatusCode::OK);
        // A DIFFERENT session at cap → 503.
        let b = send(&router, Req::get_sse().session(&sid_b)).await;
        assert_eq!(
            b.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a second session at cap must 503"
        );
        // A saturated SSE budget is separate from the one-shot request path: a POST
        // still succeeds while every stream slot is taken (streams-at-cap must not
        // block one-shot requests for serve / CLI / other consumers).
        let post = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(
            post.status(),
            StatusCode::OK,
            "a saturated SSE budget must not block one-shot POSTs"
        );
        // The SAME session reconnects → takeover transfers the permit → 200.
        let a2 = send(&router, Req::get_sse().session(&sid_a)).await;
        assert_eq!(
            a2.status(),
            StatusCode::OK,
            "a reconnect on the same session takes over its own permit, never 503"
        );
        drop((a1, a2, b));
    }

    /// F3: the router-level shutdown seam — a stream opened through the FULL router
    /// (its handler subscribes to `state.shutdown`) ends when the teed sender fires,
    /// so graceful shutdown never hangs on an open stream.
    #[tokio::test(flavor = "multi_thread")]
    async fn get_sse_router_stream_ends_on_shutdown_send() {
        let state = test_state(true);
        // The SAME sender the handler subscribes to (serve_http feeds it from
        // shutdown_signal()).
        let shutdown = Arc::clone(&state.shutdown);
        let router = build_router(state, allowed_host_set(&test_addr()));
        let sid = init_session(&router).await;
        let resp = send(&router, Req::get_sse().session(&sid)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        // Fire the teed shutdown; the open stream must end (its body completes).
        let _ = shutdown.send(true);
        let body = tokio::time::timeout(
            Duration::from_secs(5),
            axum::body::to_bytes(resp.into_body(), usize::MAX),
        )
        .await;
        assert!(
            body.is_ok(),
            "the SSE body must end on the teed shutdown signal, not hang"
        );
        assert!(body.unwrap().is_ok(), "body collection succeeds");
    }
}
