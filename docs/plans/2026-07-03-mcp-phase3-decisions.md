# MCP Phase 3 — Decisions (Streamable HTTP transport; Tasks deferred)

Status: REVISED after a four-adversary attack (seam / red-team / spec-fidelity /
design-critique) on the DRAFT. Issue: #2124. Facts from the 2026-07-03
investigation brief; constraints from `2026-06-23-mcp-phase1-decisions.md`
(D4a) and `2026-06-24-mcp-phase2-design.md`; spec conformance checked against
modelcontextprotocol.io revision 2025-11-25 (Transports + Tasks).

## The headline decision: SPLIT the phase

The draft bundled a network transport with daemon-side Tasks for `index`/`eval`.
The attack panel converged (4/4 REVISE) on one restructuring:

- **Phase 3a — Streamable HTTP transport. Ships now.** Standalone value
  independent of Tasks: multi-client, remote/co-located agents, no per-client
  bridge process. Rides `bridge::route()`.
- **Phase 3b — Tasks. Deferred**, behind the SAME trigger as the eval descope
  (an eval-core lift + a GPU-contention policy). Tasks are additive under MCP
  capability negotiation, so deferring is not a protocol break.

Why the Tasks half was pulled, not just reordered:

1. **No consumer need for index tasks.** The users are agents. The agent story
   for index already shipped in P2b: `cqs_index` fire-and-forget +
   `wait_fresh`/`status` polling + `_meta.stale_origins`. A task adds a progress
   fraction (agents need done/not-done, which `wait_fresh` answers), cancel
   (self-neutered by the freshness invariant below), and `tasks/result`
   (undefined for an attachment to a coalesced job). The one tenant whose
   progress+cancel+result all matter (eval) is the one D3 descopes.
2. **The draft's progress/cancel premise was false.** "Checkpoints already
   exist" was true of `run_index_pipeline` (CLI: `src/cli/pipeline/mod.rs:83-84`
   atomics, stage-boundary interrupt checks) — but the daemon never calls it.
   The daemon's index job is `reindex_files` (`src/cli/watch/reindex.rs`):
   single-threaded on the watch thread, **no progress atomics, no cancellation
   checks**, one monolithic `embed_documents` over the whole batch. Phase 3b
   must ADD that infrastructure to `reindex_files`; it does not pre-exist.
3. **The stateless-WG hedge, applied consistently, defers Tasks.** The draft
   used the 2026 drift to trim session/task *durability* but not to question
   building the *experimental* Tasks extension for a tenant with no need. The
   issue's own instruction ("revisit against the 2026 stateless-core direction
   before investing") points here.

The eval-tasks follow-up and the Phase-3b tracking issue are filed NOW (house
rule: issues at identification), not "at landing time."

---

# PHASE 3a — Streamable HTTP transport

## 3a-D1 — A thin front-end process; transport holds only session state

`cqs mcp --http <port>` runs the Streamable HTTP listener in the same process
family as today's stdio bridge: a transport-only client of the daemon unix
socket, relaying tool calls via `bridge::route()`. It holds session state
(MCP-Session-Id, SSE stream bookkeeping, Last-Event-ID buffers) and nothing
else. D4a is preserved verbatim — separate `cqs mcp` process, daemon client,
no in-process fallback, no auto-spawn; daemon-absent is a clean JSON-RPC error.

Session state is explicitly NOT durable: transport restart ⇒ sessions 404 ⇒
clients re-initialize (Transports § Session Management #4 obliges this). This
survives a future stateless-core WG landing, which would only strand heavier
session machinery.

**Rejected — extend the `cqs serve` process with an MCP endpoint** (the axum
stack already lives there). Strong rejection: `cmd_serve` opens its own
`Store<ReadOnly>` and is NOT a daemon client (`src/cli/commands/serve.rs`), so
an MCP endpoint there would bypass the daemon execution authority (forking the
relay invariant + CLI==daemon parity), could not reach the mutation path
(`cqs_index` queues via the daemon), and would put two HTTP surfaces with
different auth postures (`--no-auth` exists for the web UI, forbidden here —
3a-D3) in one process. The listener is a daemon client, like the stdio bridge.

**HTTP stack decision:** the `cqs mcp` process gains a tokio runtime + axum
behind a new feature (reuse `serve`'s dep set; do not hand-roll HTTP/SSE).
Stated here so P3a-1 doesn't trip on Cargo composition.

## 3a-D2 — Auth: the boundary reason, not the mutation gate

The daemon's native boundary is a same-UID `0o600` unix socket
(SECURITY.md:82). ANY TCP listener — read-only included — widens that to every
local UID (and, under WSL, Windows-side processes via localhost forwarding),
and the MCP read set exfiltrates the indexed tree + notes. Therefore auth is
**unconditional** on the HTTP endpoint, for a boundary reason ORTHOGONAL to the
mutation gate. (The draft justified no-`--no-auth` by "this surface can mutate"
— false in the default `CQS_MCP_ENABLE_MUTATIONS=off` deployment, and a future
hand would pattern-match that into licensing a read-only no-auth endpoint. The
real reason is the widened boundary.)

- **Bearer token**, required. Source posture (mirroring the `CQS_PDF_SCRIPT`
  precedent, SECURITY.md:194): prefer `--token-file` with a unix
  ownership+mode check (owned by the effective user, not group/world-readable);
  `CQS_MCP_HTTP_TOKEN` env is DISCOURAGED — its capture surface (systemd unit
  files mode-0644, audit-subsystem exec capture, child-process env inheritance)
  is named in the docs. The token is never logged (path-only tracing, mirroring
  serve's `TraceLayer`; MCP-Session-Id also never logged).
- **Reuse `serve::auth::{AuthToken, ct_eq}`** — the constant-time compare and
  the CR/LF/`;`/`,`-rejecting alphabet validation are load-bearing; no
  hand-rolled `==`.
- **Bind 127.0.0.1 by default.** Non-loopback bind: the durable, potentially
  mutating token would travel LAN cleartext (no TLS in the stack). Decision:
  **loopback-only for mutations, unconditionally** — mutating tools are
  withheld from `tools/list` whenever the bind is non-loopback, EVEN with
  `CQS_MCP_ENABLE_MUTATIONS=1` and a valid token (the daemon gate
  `require_mutations_enabled` is transport-blind, so the listener enforces this
  before relaying, or threads an `origin:remote` marker the daemon gate
  rejects — a TRIPLE gate: flag ∧ token ∧ loopback). A non-loopback bind at all
  requires the token and fires the loud-warn family (its OWN trigger sentence,
  not serve's `--no-auth` trigger). Recommend TLS-or-loopback-only in the docs.
- **Origin (spec § Security Warning #1 MUST):** present-and-invalid → 403.
  **Absent Origin → allow** (headless agents send none; Origin is a
  browser-only CSRF/DNS-rebinding layer, the token is primary). **`Origin: null`
  → reject** (distinct from absent — sandboxed-iframe class). The listener also
  reuses serve's `enforce_host_allowlist` so DNS-rebinding stays closed for
  non-browser clients (Host is always present; Origin is not).
- **Session binding:** MCP-Session-Id is server-issued ONLY (a client-proposed
  ID on `initialize` is ignored — fixation closed). Bearer auth gates
  POST/GET/DELETE uniformly (an unauthenticated DELETE tears down a victim
  session; an unauthenticated GET+Last-Event-ID replays a buffer). Under one
  shared token, session isolation rests entirely on session-ID secrecy +
  no-log — stated as an accepted property.

## 3a-D3 — Protocol conformance posture (regraded MUST/SHOULD/MAY)

The draft mislabeled five of nine floor items. Correct grading, and the omitted
MUSTs the panel found against the fetched 2025-11-25 text:

**Transport MUSTs (binding):**
- Single endpoint supporting POST **and** GET (DELETE is MAY — server may 405).
- POST body is a SINGLE JSON-RPC message — reject arrays with 400.
- POSTed notification/response → 202 Accepted, no body (the
  InitializedNotification leg of the HTTP initialize flow).
- POSTed request → `text/event-stream` OR `application/json` (nothing else).
- GET → SSE (`text/event-stream`) OR 405. **In 3a (no SSE yet), GET returns
  405.** SSE arrives in 3a-2.
- Session-ID (if issued): visible-ASCII 0x21–0x7E only (crypto-random hex/UUID
  satisfies it).
- Session expiry → HTTP 404; client MUST then re-initialize.
- Invalid/unsupported `MCP-Protocol-Version` → 400. **Absent header → assume
  `2025-03-26`** (a 400-on-missing would break the backward-compat SHOULD).
- Origin present-and-invalid → 403.

**SSE MUSTs (binding in 3a-2, when resumability opts in):** event `id` globally
unique across all streams in the session; never replay a message that belonged
to a different stream; resumption only via GET + Last-Event-ID; no JSON-RPC
*response* on a GET stream except during resumption; no cross-stream broadcast
of the same message.

**SHOULD (posture, not floor):** crypto-secure session ID; localhost bind;
disconnect-is-not-cancellation.

## 3a-D4 — Task-store cap (forward-declared, enforced in 3b)

Even absent Tasks, record the availability constraint now: the daemon is the
single execution authority for every consumer (CLI, MCP, serve-legs). serve
caps its concurrency (`blocking_permits`, body-size); the future task store
gets the count analogue — capped live-task count + attachments-per-job, past
which `tasks/create` returns a spec error rather than allocating. Prevents a
single steered-but-authorized client (the RT-RELAY threat: authorized ≠ benign)
from OOMing the daemon.

## Phase 3a PR ladder

1. **P3a-1** HTTP listener: endpoint + server-issued sessions + Origin/Host +
   auth (token-file, ct_eq, loopback-default, mutation-withhold-off-loopback) +
   plain-JSON responses (GET → 405, no SSE yet), reusing `bridge::route()`.
   **Red-team lane after** (session fixation, Origin/`null`, DELETE/GET auth,
   token source, mutation-over-non-loopback, timing oracle).
2. **P3a-2** SSE streams + Last-Event-ID resumability (the SSE MUSTs above).
   **Second red-team pass after** — SSE replay / cross-session stream takeover
   is the canonical attack there and does not exist until this rung.
3. **P3a-3 (rides the surface PRs, not standalone):** SECURITY.md — AMEND
   line 27 ("The MCP bridge uses JSON-RPC over stdio. No network port is
   opened." → "stdio by default; `--http` opens a loopback-bound, token-gated
   port"), amend the line-96 Architecture framing, add the MCP-HTTP section with
   its own loud-warn trigger; README:335 / CONTRIBUTING:275 "stdio↔daemon
   bridge" staleness; a docs-drift grep guard for the retired sentence.
   PRIVACY.md:5 (outbound-only) is not contradicted by an inbound loopback
   listener — no edit forced.

---

# PHASE 3b — Tasks (DEFERRED; tracked separately)

Not entered now. Recorded so the deferral is honest about what 3b must build,
because the draft under-budgeted it. Trigger: eval-core lift + GPU-contention
policy (shared with the eval descope). The corrected premises:

- **3b-P1 must ADD progress + cancellation to `reindex_files`** (it has
  neither): coarse `files_done/files_total` around the per-file tx loop, cancel
  checks at per-file tx boundaries. The `run_index_pipeline` atomics are
  CLI-only and unreachable from the daemon.
- **Decidable cancel rule** (the draft's "job was task-initiated" is
  undecidable — provenance is destroyed when a task walk and inotify events
  merge into one `pending_files` HashSet): job cancellation is honored ONLY
  while the job is still QUEUED (attachments detach; the reconcile signal
  un-flips iff no watch events are pending). Once the flush drains
  `pending_files`, cancel = detach-only and the job runs to completion. A
  rebuild is useful work for every consumer; detach-only keeps freshness
  markers honest and deletes both misfire schedules the seam audit found.
- **Store-side cancel-safety** (the manifest-generation "abort-anywhere safe"
  claim covers ONLY the HNSW sidecar): if any mid-run abort survives the rule
  above, cancel must requeue unprocessed files into `pending_files` (the Err
  arm at `events.rs:566` does NOT today) and must not skip the post-run SPLADE
  encode for already-committed files (else the `events.rs:326` dense-only drift
  window becomes client-triggerable).
- **Ownership model** (none exists: shared token + always-advertised
  `tasks/list` → any holder lists+cancels any task, re-entering the
  cancel-one-cancels-all design D2 rejects): decide single-principal (all token
  holders are one principal; drop "YOUR"; qualify the rejection as
  cross-principal) OR taskId-as-capability (`tasks/list` returns
  non-cancellable metadata, possession of a CreateTaskResult taskId is
  ownership). Resolve "current/next": a task created mid-job attaches to the
  NEXT job.
- **`tasks/result` is a transport-side poll loop, not a blocking relay** (the
  sharpest defect: the daemon handler pool is capped with per-connection
  timeouts — a relay blocking for a multi-minute rebuild both times out AND, at
  N waiters, exhausts the pool for every consumer = self-DoS). The transport
  polls `task-get` internally.
- **Spec conformance (Tasks):** task-gone ⇒ JSON-RPC **`-32602`** ("Task not
  found / expired"), NEVER HTTP 404 (404 is session-expiry only; emitting it on
  a daemon bounce forces spurious whole-session re-init on every conforming
  client). CreateTaskResult includes `createdAt`/`lastUpdatedAt` (MUST in all
  task responses), `ttl` echoed in `tasks/get`; `-32602` on bad taskId /
  cancel-of-terminal / bad cursor; `io.modelcontextprotocol/related-task` in
  `_meta` on tasks/result; `tasks` capability declared at init;
  `execution.taskSupport` -32601 enforcement. Verify whether the experimental
  spec scopes tasks to the creating session — if so, the survive-transport-
  restart property is a recorded deviation.

---

## Out of scope (both phases)

The consent-surface re-evaluation (the third #2124 item) is NOT entered: the
mutation-gate posture is unchanged by transport, and re-evaluating the
destructive set is a separate judgment with its own review.
