# cqs MCP Phase 1 — Implementation Brief

**Status:** ready to code
**Locked design:** `docs/plans/2026-06-16-mcp-server-design.md` (merged)
**Companion assessment:** `docs/plans/2026-06-16-mcp-wrap-assessment.md`

Phase 1 is a thin `cqs mcp` stdio↔daemon-socket bridge process. It loads no
GPU/hnsw/model, speaks MCP JSON-RPC over stdio to the client, and forwards
`tools/call` to the warm daemon over its unix socket (acting as a daemon
client, exactly like `daemon_translate.rs`). `tools/list` is generated from the
`for_each_batch_cmd!` registry + Phase-0 schemars `inputSchema`s. Errors split:
protocol `-32xxx` for framing/unknown-tool/bad-args/transport; `isError:true`
inside an otherwise-successful result for handler `ErrorCode`s.

The headline constraint, discovered while grounding this brief: **the existing
daemon socket only accepts `args` as a JSON array of *strings*** (it rejects
non-string elements outright — `src/cli/watch/socket.rs:185-214`, error
`"args contains non-string elements"`). MCP delivers `arguments` as a JSON
*object* matching `inputSchema`. Phase 1 therefore re-serializes the object into
an argv string array before relaying (OQ-3 recommendation (b) deferred to a
later socket-extension; (a) unblocks Phase 1). See §5 and §10.

---

## 1. Files to create / modify

Target footprint: a small `src/mcp/` (~4 files, ~300-400 LOC total) — roughly
10× smaller than the old 27-file server (old `server.rs` alone was 398 lines,
`tools/mod.rs` 559 lines, `tools/search.rs` 428 lines — none of which return,
per old-mcp map (d)).

| Path | Responsibility |
|------|----------------|
| `src/mcp/mod.rs` (new) | Module root; `pub fn serve_stdio()` entry; re-exports; the `tools/list`-matches-registry guard test lives here. |
| `src/mcp/bridge.rs` (new) | The stdin→parse→route→stdout loop. Owns framing, JSON-RPC envelope read/write, method dispatch table, the daemon-socket client connection. |
| `src/mcp/lifecycle.rs` (new) | `handle_initialize` (echo/advertise) + `handle_initialized` (no-reply notification). Holds `MCP_PROTOCOL_VERSION` const. |
| `src/mcp/tools.rs` (new) | `tools/list` generation (registry walk + schemars `inputSchema` + annotations) and `tools/call` dispatch (object→argv relay→daemon→`CallToolResult`). |
| `src/mcp/jsonrpc.rs` (new, optional) | Copy `JsonRpcRequest` / `JsonRpcResponse` / `JsonRpcError` verbatim from `git show 291ec6b0^:src/mcp/types.rs:8-35` (old-mcp map reuse: "Copy them verbatim"). Can fold into `bridge.rs` to stay at 4 files. |
| `src/cli/commands/...` (modify) | Add a `const` tool registry table OR a `to_argv` shim per Phase-0 `*Args` (see §5/§8). Reuse the existing schemars derives — no new ones for the 20 ready commands. |
| `src/main.rs` / CLI command enum (modify) | Wire the `cqs mcp` subcommand → `mcp::serve_stdio()`. |
| `CHANGELOG.md`, `README.md`, `CONTRIBUTING.md` (modify) | New surface; per MEMORY "New CLI commands need full ecosystem updates" — at minimum CHANGELOG + the CONTRIBUTING architecture-overview section (new `src/mcp/` dir). |

Reuse — do **not** re-implement (old-mcp map "NOT REUSE"): no `tools/mod.rs`
per-tool handlers, no tokio/axum, no HTTP transport, no `OnceLock`/`RwLock`
interior-mutability state. The cores + the daemon replace all of it.

---

## 2. Bridge main loop (`bridge.rs`)

**Framing — newline-delimited JSON (NDJSON), NOT Content-Length headers.**
This matches both required surfaces:
- The MCP stdio transport rule: server MUST NOT write anything but MCP to
  stdout (mcp-spec map, Assessment §1). The old transport used exactly this:
  one request per line via `stdin.lock().lines()`, one response per line via
  `writeln!(stdout, ...)` + `stdout.flush()` (`git show
  291ec6b0^:src/mcp/transports/stdio.rs:29,95-97`, verified).
- The daemon socket below is *also* JSONL (`src/cli/watch/socket.rs` JSONL
  framing; client writes `writeln!(stream, "{}", request)` —
  `daemon_translate.rs:883`, verified). So the bridge speaks the same line
  discipline on both edges.

Loop (mirrors `serve_stdio` at old `stdio.rs:29-98`, verified):

```
for line in stdin.lock().lines():
    line = line?
    if line.len() > 1_048_576:            # 1 MiB cap, reuse old bound
        write -32700 "Request too large"; continue
    if line.trim().is_empty(): continue
    req: JsonRpcRequest = match from_str(&line):
        Err(e) => { write -32700 "Parse error: {e}"; continue }
    resp = route(&mut bridge_state, req)   # state holds cqs_dir for socket path
    # notifications (no id, null result) get NO response line
    if resp.id.is_none() && resp.result == Null: continue
    writeln!(stdout, "{}", to_string(&resp)?); stdout.flush()?
```

**Route by `req.method`:**
| method | handler |
|--------|---------|
| `initialize` | `lifecycle::handle_initialize(params) -> InitializeResult` |
| `notifications/initialized` (a.k.a. `initialized`) | `lifecycle::handle_initialized()` → `Value::Null`, no reply |
| `tools/list` | `tools::list()` → `{tools:[...]}` |
| `tools/call` | `tools::call(&bridge_state, params)` → `CallToolResult` |
| _anything else_ | JSON-RPC `-32601` (method not found) |

**GOTCHA (old-mcp map, verified):** NDJSON has no escaping. `serde_json::to_string`
produces single-line output by default — never pretty-print on the stdout path
or an embedded `\n` corrupts the stream. Keep all `tracing` on **stderr**; the
bridge loads no model so there's no GPU-lib stdout noise to gag (mcp-spec map,
Design §2: "stdout trivially clean").

**Daemon connection:** one daemon socket connect *per `tools/call`*, not a
persistent multiplexed connection. The socket is strictly one
request→one response→close (daemon-protocol map open-question: "Each client
connection is independent; the server does not multiplex"). Derive the path
exactly as the existing client does — `daemon_socket_path(cqs_dir)` at
`daemon_translate.rs:497-557` (blake3-of-canonical-`.cqs`-dir hash under
`$XDG_RUNTIME_DIR`). Reuse `daemon_request_with_timeout`
(`daemon_translate.rs:824-890`) rather than re-rolling the connect/timeout/read
logic.

---

## 3. Lifecycle handlers (`lifecycle.rs`)

**`initialize`** — deserialize `InitializeParams {protocolVersion, capabilities,
clientInfo}` for protocol compliance, but **do not negotiate**. The old server
marked every field `#[allow(dead_code)]` and accepted any version (old-mcp map
(b), `types.rs:42-53`, verified). Echo/advertise, per Design §4 + old
`server.rs:152-177`:

```json
{
  "protocolVersion": "2025-11-25",
  "capabilities": { "tools": { "listChanged": false } },
  "serverInfo": { "name": "cqs", "version": <crate version>, "title": "cqs" },
  "instructions": "<short usage string>"
}
```
- `protocolVersion`: constant `MCP_PROTOCOL_VERSION = "2025-11-25"` (old
  `server.rs` advertised exactly this; mcp-spec Design §4).
- `capabilities.tools.listChanged: false` — tools are static (old-mcp map:
  "tools are static and don't change at runtime").
- `serverInfo` adds `title` vs the old shape (mcp-spec Design §4 lists
  `{name, version, title}`); old server emitted `{name, version}` only — add
  `title`.
- `instructions` — new in 2025-11-25 lifecycle (Design §4). Short, static.
- **OQ-10:** advertise `2025-11-25`; accept the client's requested version if
  `>= 2025-06-18` (don't hard-reject) — recommendation in mcp-spec OQ-10. Echo
  back our `2025-11-25` regardless.

**`notifications/initialized`** — return `Value::Null` and send **no** response
line (it's a notification; old server did exactly this — `server.rs:150`,
old-mcp map (a)). The loop's "skip when id.is_none() && result==Null" guard
(§2) handles the suppression.

---

## 4. `tools/list` generation (`tools.rs`)

**Source of truth = the `for_each_batch_cmd!` registry**
(`src/cli/batch/commands.rs:395-450`, verified). It is a *compile-time
declarative* macro — cannot be iterated at runtime (command-registry map). So
expand it into a `const`/`static` tool table at compile time. Add a fourth
emitter arm alongside the existing `gen_is_pipeable_impl` / `gen_dispatch_impl`
(invocation sites `commands.rs:482,537`) — e.g. `gen_mcp_tools_impl!` — that the
same macro feeds, producing a `fn mcp_tool_table() -> &'static [ToolDef]`. This
inherits the macro's compile-time exhaustiveness: a new `BatchCmd` variant with
no row fails to compile (command-registry map: non-wildcard match arms,
double-pinned by `test_is_pipeable_exhaustive` at `commands.rs:1005`).

The registry enumerates **39 commands** (32 `args_variants` + 3 `ctx_only` +
4 `unit`; command-registry map facts).

**Each `ToolDef` carries** (mcp-spec Design §5, Assessment §1):
- `name`: bare flat command name, lowercase (`search`, `callers`, `impact`, …)
  — matching the `BatchCmd` variant lowercased (mcp-spec OQ-2 / Design §7:
  flat bare names, not `cqs_`-prefixed). `notes` renders flat as a single tool
  for P1 (notes subcommand split deferred — Design §7, §9 G6).
- `description`: from the command's `///` doc comments (Design §5).
- `inputSchema`: JSON Schema 2020-12 via **schemars** — `schemars::schema_for!
  (XArgs)` on the Phase-0 core-input struct (NOT the `args.rs` clap struct;
  mcp-spec reuse note + Design §0). Only **20** of 39 have a JsonSchema-derived
  core struct today (command-registry map: 19 in `cli/commands/*/*.rs` +
  `QueryArgs`). The 19 without one (§8) get a placeholder `inputSchema` (empty
  object `{"type":"object"}`) and a `// TODO(mcp-phase1-cores)` marker in P1, OR
  are excluded from the P1 read-only set — see §7.
- `annotations` (mcp-spec Assessment §4: "untrusted hints", not enforcement):
  - `readOnlyHint`: `true` for all read commands; `false` only for the
    mutating set. From the registry + cores, the **only** mutator dispatched
    on this path is `Refresh` (daemon-protocol map: "The `BatchCmd::Refresh`
    handler is the only dispatched command that mutates `BatchContext` state").
    `Notes` *can* mutate (add/remove) but P1 exposes only its read surface
    (`notes list`) — Design §7,§9. So in P1's read-only command set,
    `readOnlyHint:true` for all exposed tools.
  - `idempotentHint`: `true` for the pure read commands.
  - `destructiveHint`: `false` (no destructive tool in the P1 set).
  - `openWorldHint`: `false` (Design §5 — cqs queries a closed local index).

`outputSchema` is **omitted** in Phase 1 (Design §9; deferred to Phase 2 G5).

---

## 5. `tools/call` dispatch (`tools.rs`)

Request shape: `{method:"tools/call", params:{name, arguments}}` where
`arguments` is a JSON object matching the tool's `inputSchema` (mcp-spec
Design §8).

**Pipeline:**

1. **name → command.** `params.name` is the bare command. If it's not a known
   tool → JSON-RPC `-32601`. (Lowercase, case-sensitive match — the daemon's
   clap parse is case-sensitive: daemon-protocol map GOTCHA.)

2. **deserialize args into the core struct.** `serde_json::from_value::<XArgs>
   (arguments)`. Works partially because every Phase-0 `*Args` derives
   `serde::Deserialize` with `#[serde(default)]` on the struct + a custom
   `Default` mirroring clap defaults (command-cores map: verified on `QueryArgs`
   `query.rs:53,137-173`, `GatherArgs`, `ScoutArgs`). A caller omitting a field
   inherits the production default. On deserialize failure → JSON-RPC `-32602`
   (invalid params). This is **cleaner than the daemon socket path's clap
   re-parse** (mcp-spec Assessment G3) — but see step 3's constraint.

3. **relay to the daemon (THE relay fork, OQ-3).** The warm daemon socket
   accepts `{"command": <name>, "args": [<strings>]}` and **rejects any
   non-string `args` element** (`socket.rs:185-214`, verified;
   `daemon_translate.rs:883` sends `args` and the server validates them as
   strings). It then reconstructs clap tokens `[command, args[0], …]`
   (`view.rs:100-102`) and clap-parses. So Phase 1 cannot hand the JSON object
   straight through. **P1 choice (a):** serialize the deserialized `XArgs` back
   into an argv `Vec<String>` (`["--query","foo","-n","20",…]`) and send that as
   `args`. Implement a `fn to_argv(&self) -> Vec<String>` per `*Args` (or one
   generic walker over the serde value → `--kebab-key value`). **Deferred
   choice (b):** extend the socket to accept `"args": {object}` and have the
   daemon `from_value::<XArgs>` directly (the cleaner long-term path; mcp-spec
   OQ-3 recommendation). File (b) as the relay-format follow-up (§10).
   - *Fallback in-process path* (Design §2, mcp-spec reuse): if the socket is
     unreachable (`daemon_socket_path` doesn't exist / connect fails —
     `daemon_translate.rs:838-849`), a P1-optional in-process `BatchView` path
     may run the core directly, but it loads GPU/hnsw and MUST run under
     `stdout_gag` (mcp-spec OQ-1). **Recommend P1 = bridge-only**: if no daemon,
     return `isError:true` with a "daemon not running" message rather than
     ship the gag complexity. Decide in §10.

4. **map the daemon response into `CallToolResult`.** The daemon returns the
   two-layer envelope (daemon-protocol map (b)/CRITICAL):
   - Socket layer: `{"status":"ok","output":<dispatch>}` or
     `{"status":"error","message":"…"}`.
   - Dispatch layer (inside `output`): `{"data":<handler>,(opt)"_meta":{…}}`
     on success, or `{"error":{"code","message"},(opt)"_meta":{…}}` on
     handler failure.
   Reuse `unwrap_dispatch_payload` (`daemon_translate.rs:1006-1035`) to peel
   these layers. Then build (mcp-spec Design §8, Assessment G4):
   ```json
   {
     "structuredContent": <dispatch.data>,
     "content": [ { "type": "text", "text": <json-stringified dispatch.data> } ],
     "_meta": <dispatch._meta passed through: stale_origins, overlay_graph,
               rank_signals, trust_level>,
     "isError": false
   }
   ```
   `_meta` is **preserved**, not dropped (mcp-spec reuse: "`_meta` is preserved
   in `result._meta`"). The `content` text block is the json-stringified
   `data` for backwards-compat with clients that don't read
   `structuredContent` (Assessment §1).

---

## 6. Error mapping table

Two taxonomies (mcp-spec Design §8, Assessment G4; old-mcp map (b); error
codes verified at `json_envelope.rs:123-191`):

| Failure | Where it arises | Mapping |
|---------|-----------------|---------|
| Stdin line not valid JSON | bridge loop | JSON-RPC **-32700** (parse) — old `stdio.rs:50,62` |
| Stdin line > 1 MiB | bridge loop | JSON-RPC **-32700** "Request too large" |
| Unknown `method` | router | JSON-RPC **-32601** (method not found) |
| `tools/call` with unknown tool `name` | `tools::call` | JSON-RPC **-32601** |
| `arguments` fails `from_value::<XArgs>` | `tools::call` step 2 | JSON-RPC **-32602** (invalid params) |
| Daemon socket missing / connect fail / timeout / read fail (`DaemonRpcError::SocketMissing` / `::Transport`) | relay step 3 | **transport** → P1: JSON-RPC **-32603** (internal) *or* `isError:true` "daemon unavailable" — pick one in §10. (Old server used **-32000** generic for execution errors — but the spec-aligned modern choice is -32603 for internal/transport.) |
| Socket layer `{"status":"error","message":…}` (NUL bytes, bad args, missing command — daemon-protocol map (d)) | relay response | JSON-RPC **-32602** if it's our bad relay (bad args), else **-32603** |
| Handler `ErrorCode` in dispatch layer (`{"error":{"code":…}}`): `not_found`, `invalid_input`, `parse_error`, `io_error`, `internal`, `timeout` (`ErrorCode` enum `json_envelope.rs:146-151`) | relay response | **`isError:true`** in a successful (HTTP-200-equivalent) `CallToolResult`; `content` = redacted message; `_meta.error.code` = the code string (Assessment G4) |

Key line: **protocol vs framing failures = `-32xxx`; handler-semantic
failures = `isError:true` in an otherwise-OK result.** The dispatch-layer
error messages are already privacy-redacted (`redact_error` →
`json_envelope.rs`; daemon-protocol map (d)), so they're safe to surface to the
MCP client verbatim.

`JsonRpcError.data` stays `None` in P1 (old code left it None; populating it
with structured tool/arg info is a Phase-2 nicety — old-mcp open question).

---

## 7. In-scope vs out-of-scope (Phase 1)

**IN (Design §9):**
- `cqs mcp` stdio bridge process (loads no GPU/hnsw/model).
- Lifecycle: `initialize` + `notifications/initialized`.
- `tools/list` from the registry + schemars `inputSchema` + annotations.
- `tools/call`: object → `from_value::<XArgs>` → argv relay → daemon → result.
- Error split (§6): `-32xxx` vs `isError:true`.
- `structuredContent` + `content[text]` + `_meta` passthrough results.
- **Read-mostly command set** — the commands that already have a Phase-0
  JsonSchema core struct (the 20). Recommend P1 exposes exactly these 20 (plus
  the zero-arg infra reads `ping`/`status`/`stats`/`health` with empty schemas
  if desired). The 19 lacking a core struct (§8) are **excluded** from
  `tools/list` in P1 (cleaner than shipping empty-schema half-tools).
- 1 MiB request cap (reuse old bound).

**OUT (Design §9, mcp-spec Phase-2 deferred):**
- `outputSchema` (G5) — Phase 2.
- `resources` / `resources/read` (OQ-9) — Phase 2.
- Tasks / `execution.taskSupport` (OQ-8) — Phase 2.
- Flat subcommand tools (`notes_add`, `notes_remove`) — Phase 2 G6; P1 = read
  surface only.
- Mutating tools (`index`, `gc`, `notes` writes) with annotations — Phase 2
  (OQ-4).
- HTTP transport, SSE, CORS, API-key auth (old-mcp map: not needed for CLI).
- Protocol-version negotiation beyond "accept `>=2025-06-18`, echo
  2025-11-25" (OQ-10).
- `JsonRpcError.data` structured payloads.

---

## 8. Command-core completion gap → one tracking issue

The registry has 39 commands; only **20** carry a Phase-0
`serde::Deserialize + schemars::JsonSchema` core-input struct usable as an MCP
`inputSchema` (command-registry map facts). The gap:

**8 args-based commands lacking a JsonSchema core struct** (they use `args.rs`
clap variants without JsonSchema — command-registry map): `Search` (the
`args.rs` `SearchArgs`, distinct from the JsonSchema `QueryArgs`), `Explain`,
`Related`, `Where`, `Read`, `Notes` (`NotesListArgs`), `Suggest`, `ImpactDiff`.

**9 commands with no dedicated core struct** (ctx-only / unit / minimal-args —
command-registry map): `Stats`, `Health`, `Gc` (ctx-only); `Refresh`, `Ping`,
`Status`, `Help` (unit); `WaitFresh`, `Reconcile` (take args but are off the
standard `args_variants` typed-`&XArgs` dispatch — they need an empty/minimal
schema or custom handling).

> Note: `Search` is subtle — the daemon already adapts `args.rs SearchArgs` →
> the JsonSchema `QueryArgs` via `daemon_query_args`
> (`batch/handlers/search.rs:115-154`, command-cores map). For MCP, expose
> `search` using **`QueryArgs`** as the `inputSchema` directly (it's already
> Phase-0-ready). So `Search` may be a quick win rather than a gap — confirm
> the daemon relay accepts `QueryArgs`-shaped argv.

### Tracking issue (to file)

**Title:** `MCP Phase 1: complete the command-core input structs (19 of 39 commands lack a JsonSchema core)`

**Body bullets:**
- Add `serde::Deserialize + schemars::JsonSchema` core-input structs (matching
  the established pattern: `#[serde(default)]` on struct + custom `Default`
  mirroring clap, `///` docs for descriptions — see `QueryArgs`
  `query.rs:53,137-173`) for the args-based commands currently on `args.rs`:
  - [ ] `Explain`
  - [ ] `Related`
  - [ ] `Where`
  - [ ] `Read`
  - [ ] `Notes` (read/list shape; add/remove deferred to MCP Phase 2)
  - [ ] `Suggest`
  - [ ] `ImpactDiff`
  - [ ] `Search` — verify it can reuse the existing `QueryArgs` JsonSchema
    rather than needing a new struct (likely a no-op + relay confirmation).
- Decide the `inputSchema` for the no-args commands (empty `{"type":"object"}`
  vs omitted): `Stats`, `Health`, `Gc`, `Refresh`, `Ping`, `Status`, `Help`.
- Handle the off-pattern arg commands `WaitFresh`, `Reconcile` (minimal schema
  or exclude from the MCP tool set).
- Acceptance: `tools/list` count == registry count for every command we intend
  to expose, enforced by the §9 guard test.

(Per MEMORY "No Debt in Foundation Layers" + "Always Do Things Properly": these
cores are the proper long-term shape — they also unblock Phase 2 `outputSchema`
and serve CLI/daemon parity, so build them as real structs, not shims.)

---

## 9. Test strategy

**(a) Stdio round-trip smoke (integration test).**
Spawn `cqs mcp` as a child process (`std::process::Command` with piped
stdin/stdout), send three framed NDJSON lines, assert three framed responses.
This is the "smoke real shape" discipline (MEMORY) — exercise the actual child,
not a hand-built fixture.
1. Send `initialize` → assert reply has `protocolVersion:"2025-11-25"`,
   `capabilities.tools.listChanged:false`, `serverInfo.name:"cqs"`.
2. Send `notifications/initialized` → assert **no** response line is written
   (the loop suppresses it; §2/§3).
3. Send `tools/list` → assert it's a non-empty `tools` array, each entry has
   `name`, `description`, `inputSchema`, `annotations`.
4. Send one `tools/call` (e.g. `{"name":"ping","arguments":{}}` — cheapest;
   `dispatch_ping` is zero-arg and daemon-only, daemon-protocol map) → assert
   `isError:false` and a `structuredContent`/`content` shape.
   - Requires a warm daemon. Either start one in the test harness against a
     temp `.cqs` (use `set_socket_dir_override_for_test()` — daemon-protocol
     map reuse), or, for a hermetic variant, assert the **bridge** correctly
     emits `isError:true`/transport-error when no daemon is up (tests the §6
     mapping without a daemon dependency).
   - Negative-path assertions: an unknown method → `-32601`; a `tools/call`
     with a bogus tool name → `-32601`; malformed `arguments` → `-32602`;
     a non-JSON line → `-32700`.

**(b) `tools/list`-matches-registry guard (unit test, in `mod.rs`).**
Assert the generated tool table covers exactly the registry's intended
exposed-command set, with no drift. Because the tool table is emitted by the
same `for_each_batch_cmd!` macro (§4), a new `BatchCmd` variant without a row
already fails to compile (exhaustiveness — `commands.rs:1005`
`test_is_pipeable_exhaustive` is the existing precedent). This test additionally
pins: (i) every exposed tool's `name` is a valid lowercase command, (ii) every
exposed tool has a non-empty `inputSchema` (catches a command added to the
table before its core struct exists — the §8 gap), (iii) the count equals the
agreed P1 exposed set. Mirror the existing exhaustiveness-test style.

**(c) Schema sanity (unit).** For each exposed `XArgs`, assert
`schemars::schema_for!(XArgs)` produces `type: object` and that
`from_value::<XArgs>(json!({}))` succeeds (proves `#[serde(default)]` lets an
empty arguments object deserialize — the partial-JSON contract, command-cores
map).

---

## 10. Risks / open questions to resolve before coding

1. **Relay format (OQ-3) — the central fork.** Confirmed: the socket rejects
   non-string `args` (`socket.rs:185-214`). P1 must re-serialize `XArgs` →
   argv strings (choice (a)). Risk: `to_argv` must round-trip every field
   correctly (kebab-case flags, `Option` skipping, repeated args like
   `--mentions`, bool flags as presence). A buggy `to_argv` silently produces a
   half-formed command (daemon-protocol map warns the daemon runs "a
   half-formed command"). **Mitigation:** property test `XArgs ==
   parse(to_argv(XArgs))` per command (round-trip), and prefer one generic
   serde-value→argv walker over 20 hand-written shims. **Decide:** ship (a) in
   P1, file (b) (socket accepts JSON object) as a follow-up.

2. **Bridge-only vs in-process fallback (OQ-1).** If no daemon, do we (a) error
   cleanly (`isError:true` "daemon not running") or (b) run in-process under
   `stdout_gag` (loads GPU, risks stdout contamination of the MCP stream)?
   **Recommend (a) for P1** — keeps the "stdout trivially clean" guarantee
   absolute and the bridge tiny. Revisit (b) in Phase 2.

3. **Transport-error code choice.** Old server used `-32000` (generic) for all
   execution errors. Modern spec-aligned choice for transport/internal is
   `-32603`. Pin one in `lifecycle.rs`/`bridge.rs` consts and use consistently
   (§6 has both noted).

4. **`Search` double-struct.** `args.rs SearchArgs` (no JsonSchema) vs
   `QueryArgs` (JsonSchema, Phase-0). MCP should expose `QueryArgs` as the
   schema and relay accordingly — but the daemon sets surface-specific flags
   (`always_route=true`, `fts_first=false`, `json_overhead`) via
   `daemon_query_args` (command-cores map GOTCHA). Since the bridge relays to
   the daemon (not the core directly), those flags get set daemon-side
   automatically — confirm this holds for the argv relay so MCP `search` gets
   daemon-equivalent behavior, not CLI-equivalent.

5. **Protocol-version window (OQ-10).** Decide the accept range now
   (recommend: advertise `2025-11-25`, accept client `>=2025-06-18`). Cheap to
   set, annoying to change after a client integration.

6. **Notes single-tool vs split.** P1 exposes `notes` as one read-only tool
   (list). Confirm the relayed `notes` argv defaults to a read/list operation
   and cannot accidentally trigger a mutation (add/remove) from a permissive
   `arguments` object — the `readOnlyHint:true` annotation is a *hint only*
   (Assessment §4), so the relay itself must not pass through a mutating
   subcommand in P1.

7. **Exposed-set decision feeds §8 + §9.** Lock the exact P1 command list
   (the 20 JsonSchema-ready, ± the zero-arg infra reads) before writing the
   guard test, since the test asserts that exact count.
