# Wrapping cqs as an MCP server — assessment (2026-06-16)

**Question:** what would the cqs CLI interface need to change to be cleanly wrapped in an MCP interface?

**Bottom line:** Very little structural change. The command-core discipline already factored cqs into exactly the shape MCP wants — a surface-agnostic core (`*_core` + typed `*Args`/`*Output`) with thin per-surface adapters (`cmd_*` for CLI, `dispatch_*` for the daemon). An MCP server is **a third adapter**, not a rewrite. The real work is (a) a JSON-RPC server shell + lifecycle, (b) deriving `inputSchema` from the `*Args` structs, and (c) a thin request/result/error reframe. No `*_core` logic changes. Estimated as a contained feature, not an architectural project.

This assessment targets **MCP revision 2025-11-25** (the current stable; the source-of-truth is `schema/2025-11-25/schema.ts` in `modelcontextprotocol/modelcontextprotocol`). Structured output (`outputSchema`/`structuredContent`) has been stable since 2025-06-18. Forward-looking work (stateless HTTP core, richer Tasks, `.well-known` discovery, enterprise auth) is on the 2026 roadmap and noted where it affects design choices.

---

## 1. The wrap target (what MCP requires of a server)

- **Tools** are the primary fit. A tool = `{name, title?, description, inputSchema, outputSchema?, annotations?, execution?}`.
  - `name`: 1–128 chars, `[A-Za-z0-9_.-]`, case-sensitive, unique. (cqs command names qualify; dots allowed → `notes.add` style is legal.)
  - `inputSchema`: a JSON Schema (2020-12 default) object — **MUST** be a valid object (use `{"type":"object","additionalProperties":false}` for no-arg tools).
  - `outputSchema`: optional JSON Schema describing the result; if present the server **MUST** return conforming `structuredContent`.
  - `annotations` (untrusted hints): `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, `title`.
  - `execution.taskSupport`: `"forbidden"` (default) | `"optional"` | `"required"` — the 2025-11-25 Tasks extension for long-running work.
- **tools/list** → `{tools:[...], nextCursor?}` (cursor pagination over the *tool list*, not over results). **tools/call** `{name, arguments}` → **CallToolResult**: `content[]` (text/image/audio/resource_link/embedded-resource) + `structuredContent` (a JSON object) + `isError`. A tool returning structured data **SHOULD** populate `structuredContent` *and* JSON-encode the same in a `text` content block (backwards-compat).
- **Errors**, two channels: **protocol errors** (JSON-RPC `-32xxx`: unknown tool, malformed request) vs **tool-execution errors** (`isError:true` in the result with actionable text the model can self-correct on). Clients are told to feed tool-execution errors back to the model.
- **Lifecycle**: `initialize` (protocolVersion + capabilities + clientInfo) → server replies (protocolVersion + capabilities + serverInfo + `instructions?`) → `notifications/initialized`. Server declares capabilities: `tools{listChanged}`, optionally `resources{subscribe,listChanged}`, `prompts`, `logging`, `completions`, `tasks`.
- **Transports**: **stdio** (newline-delimited JSON-RPC over stdin/stdout; logs to stderr; **the server MUST NOT write anything but MCP messages to stdout**) — clients SHOULD support stdio whenever possible, and it's the right default for a local single-user tool. **Streamable HTTP** (single endpoint, POST/GET, optional SSE, `MCP-Session-Id`, Origin validation, localhost bind) is for remote/multi-client.
- **Resources** (optional): `resources/list` + `resources/read` over URIs (e.g. `file://`), plus `resource_link` content blocks tools can return.

---

## 2. cqs is already most of the way there

| MCP need | cqs today | Evidence |
|---|---|---|
| Surface-agnostic logic to call | `*_core(args) -> Result<Value>`; never prints, never reads env directly | `query_core` (`search/query.rs:401`), `callers_core` (`graph/callers.rs:214`), `read_core` (`io/read.rs:441`) |
| A single enumerable tool registry | `for_each_batch_cmd!` macro lists **43 batch-dispatchable commands** with handler + classification, exhaustively matched at compile time (no silent drops) | `src/cli/batch/commands.rs:395-450` |
| Uniform handler signature to route to | `fn dispatch_X(ctx: &BatchView, args: &XArgs) -> Result<serde_json::Value>` (or ctx-only) | `src/cli/batch/handlers/{search,graph,analysis,info,misc}.rs` |
| Result that drops into `structuredContent` | handlers already return `serde_json::Value` (serialized from typed `*Output` structs) | `dispatch_explain` → `to_value(&ExplainOutput)` (`handlers/info.rs:81`) |
| Result `_meta` | slim envelope splices `_meta` (stale_origins, overlay_graph, worktree state) beside `data` | `merged_meta_value` (`json_envelope.rs:305`), `write_json_line` (`batch/mod.rs:390`) |
| Error taxonomy + privacy | `ErrorCode` enum (6 variants) + `redact_error` (redacts paths/queries, opaque chain-id) | `json_envelope.rs:123-154, 743-799` |
| Newline-delimited JSON transport | daemon already does `{command,args}` → `{status,output}` one-line frames over a Unix socket | `socket.rs:handle_socket_client` (`watch/socket.rs:75-358`) |
| Same-uid access control | socket is `0o600`, same-uid only (kernel-enforced) | `daemon_translate.rs:497-557` |
| Read-vs-mutate split for annotations | clean: ~34 read-only commands vs index/gc/notes-mutate/etc. | catalog in audit dimension 1 |
| Token-budget result sizing | `--tokens N` packs results to a budget (no cursor pagination needed — MCP doesn't paginate tool *results*) | `QueryArgs.tokens` (`search/query.rs:89`) |

The headline: **the daemon path (`dispatch_*` over the socket) is already 80% of an MCP server** — it's a request→response JSON protocol routing to surface-agnostic cores. MCP is the same shape with a different envelope.

---

## 3. The gaps (what to build), ranked

### G1 — MCP server transport + lifecycle  *(required; the bulk of the new code)*
Add a JSON-RPC 2.0 server implementing `initialize` / `notifications/initialized` / `tools/list` / `tools/call`, with capability + version negotiation. None of this exists today (the daemon protocol is a bespoke `{command,args}`/`{status,output}` frame with no `jsonrpc`/`id`/`method`, errors in-band — `socket.rs`).

**Recommended architecture — a stdio↔socket bridge.** Make `cqs mcp` a *thin stdio JSON-RPC front-end that relays to the existing `cqs watch --serve` daemon socket.* Rationale:
- The bridge process does only JSON-RPC framing + socket relay → its **stdout is fully under our control**, which is decisive given stdio's "MUST NOT write non-MCP to stdout" rule (see Risk R1).
- The heavy machinery (GPU, embeddings, hnsw_rs, the warm in-memory index) stays in the daemon — the same process that already serves 3–19 ms queries. MCP inherits that latency for free.
- It composes with the existing same-uid socket auth and the `daemon_translate` socket-path logic.
- Fallback when no daemon is running: the bridge can spawn an in-process `BatchView` (the CLI-mode path) — but the daemon path is the primary.

Reuse: dispatch routing (`dispatch_via_view`, `batch/view.rs:87`), the per-client read/dispatch/write loop pattern. New: JSON-RPC envelope (`jsonrpc`,`id`,`method`,`params`/`result`/`error`), `initialize` handshake, tool-registry → `tools/list`, request `id` correlation (the current socket frame has no id).

### G2 — `inputSchema` generation from `*Args`  *(required)*
`*Args` are **clap-only** today (`#[derive(Args, Debug, Clone)]`, no serde) and there is **no `schemars` dependency**. Deltas:
1. Add `schemars` to `Cargo.toml`.
2. Add `#[derive(Serialize, Deserialize, JsonSchema)]` to the ~30 `*Args` structs (`src/cli/args.rs`) and the shared flattened structs (`LimitArg`, `OverlayArgs`), with `#[serde(flatten)]` mirroring each `#[command(flatten)]` so flattened fields stay top-level in the schema.
3. Add `#[derive(Serialize, Deserialize)]` to **`RerankerMode`** — it's the *only* CLI-only enum lacking serde; the domain enums (`GatherDirection`, `DeadConfidence`, `GateThreshold`) already have it.
4. A generator that walks the `for_each_batch_cmd!` registry and emits `schema_for::<XArgs>()` per tool.

Exclusions are clean: `--json` is not in any `*Args` (it's `TextJsonArgs`/`OutputArgs`, surface-local) → never enters a schema; `--slot` is a transport flag → tool param or omitted; **`--tokens` IS a real `*Args` field → keep it in the schema** (it's a feature, not output shaping). The type inventory is otherwise primitives + `Option`/`Vec` + `Option<PathBuf>` (renders as string) → no JSON-Schema-resistant types.

### G3 — argument deserialization  *(required; trivially enabled by G2)*
MCP delivers `arguments` as a typed JSON object. With `Deserialize` on `*Args` (G2), `serde_json::from_value::<SearchArgs>(arguments)` yields the struct the handler already takes (`&SearchArgs`) — **zero handler refactoring**. This is actually *cleaner* than the current daemon path, which re-parses argv tokens through clap (`batch/view.rs:119`); MCP can deserialize JSON→Args directly.

### G4 — result + error reframing  *(required)*
- **Success**: handler `Value` → `CallToolResult { structuredContent: <data>, content: [{type:"text", text: <json-stringified data>}], _meta: <cqs _meta> }`. The slim envelope's `data`→`structuredContent`, `_meta`→result `_meta`. (Leaf security fields `trust_level`/`injection_flags` already live inside the data payload and carry through unchanged.)
- **Errors**, split the existing `ErrorCode`:
  - *Pre-dispatch / framing* (unknown tool, malformed params, bad JSON — today `socket.rs:145-214`) → **protocol errors**: `-32601` unknown tool, `-32602` invalid params, `-32700` parse.
  - *Dispatch/handler* (`not_found`, `invalid_input`, `internal`, `timeout`, …) → **tool-execution errors** (`isError:true` + the redacted message as actionable text), NOT protocol errors. `redact_error` is already privacy-preserving, so messages are safe to expose.

### G5 — `outputSchema`  *(recommended, NOT required for a first wrap)*
`outputSchema` is optional in MCP; `structuredContent` works without it (just unvalidated). The blocker to publishing it: handlers return loosely-built `serde_json::Value`, discarding the typed `*Output` at the boundary (`to_value()` in each handler). To add later: type the handler returns (`Result<XOutput>` instead of `Result<Value>`) and run `schema_for::<XOutput>()`, or codegen. **Ship Phase 1 without `outputSchema`; add it as a hardening pass.**

### G6 — subcommand & non-batch commands  *(scoped; mostly Phase 2)*
- **Subcommands** (`notes add/remove/update`, `ref`, `slot`, `project`, `cache`, `model`) don't map 1:1 — only `notes list` is batch-dispatchable; the mutating branches are `batch=runtime`. To expose: lift each branch into a command-core and present **flat** tools (`notes_add`, `notes_remove`, …) — MCP clients prefer flat surfaces over nested subcommands.
- **11 CLI-only commands** (`init`, `index`, `watch`, `chat`, `eval`, `convert`, …, marked `#[cqs_cmd(batch="cli")]`): most aren't tools. `index` has an `index_core` and could be exposed as a (mutating, Task-shaped) tool; `chat`/`watch`/`batch` are interactive/streaming and **out of scope** (§5).

---

## 4. Enhancements (optional, higher value once the core lands)

- **Resources** — `cqs read` maps cleanly to `resources/read` with `file://` URIs (full file) and an anchored form for `--focus` (`file://path#fn:line-range`); `read_core` already returns `{path, content, trust_level, injection_flags, notes_injected}` (`io/read.rs:441-503`). And search/callers/impact results can carry **`resource_link`** content blocks pointing at matched files — a natural agent affordance.
- **Tasks** (2025-11-25 extension) — `index`, `index --llm-summaries`, `eval` are the long-runners (seconds–minutes; they already track `took_ms` and render `indicatif` progress to stderr). Advertise `execution.taskSupport` and emit progress via the Tasks protocol. Fast commands (search/callers/impact, 3–19 ms) stay plain tools (`taskSupport:"forbidden"`).
- **Annotations** — derive from the read-vs-mutate split: `readOnlyHint:true` on the ~34 query commands; `destructiveHint:true` on `gc`, `notes remove`, `index --force`; `idempotentHint:true` on `refresh`/`reconcile`; `openWorldHint:false` (cqs operates on a local index, not an open world).
- **Streamable HTTP transport** — for remote/multi-client use; defer until there's a need (stdio↔socket covers local). If added: Origin validation + localhost bind + `MCP-Session-Id` are mandatory per spec.

---

## 5. Out of scope / design tensions

- **`chat`** (interactive REPL) and **`watch --serve`** (the daemon itself) don't fit the request→response tool model. `batch` (the JSONL composition layer) isn't a tool either, but it's the *engine* the MCP server rides on.
- **`--json`** is meaningless for MCP (always structured) — never enters a schema.
- **Statefulness gaps that become tool parameters or server context** (decisions, not blockers):
  - *Worktree overlay* — a daemon, cwd-relative feature (`_meta.overlay_graph`). An MCP server detached from a worktree loses it unless the caller supplies the worktree path or the bridge inherits cwd. UX gap, not a correctness blocker.
  - *Audit mode* — process-global (`.cqs/audit-mode.json`, TTL). Pass as a param or a config endpoint; the daemon already reads it per-query, so the bridge inherits it for free.
  - *Slot* (`--slot`) and *reference stores* (`--ref`/`--include-refs`, multi-project) — expose as optional tool params; the daemon already caches reference stores per project.
- **Pagination**: cqs uses output-size/token budgeting, not cursors. MCP doesn't paginate tool *results*, so this is a non-issue — keep `--tokens` as a per-tool knob and preserve the `json_overhead` accounting so packing stays accurate.

---

## 6. Risks / watch-items

- **R1 — stdout cleanliness (highest).** stdio MCP requires that *nothing but MCP messages* hit stdout. This is precisely the #2009/#2010 class (hnsw_rs's `modify_level_scale` `println!` leaking into a `--json` stream), just fixed with the `stdout_gag` fd-1 suppressor. A naive in-process stdio server is fragile to *any* dependency that prints. **The stdio↔socket bridge (G1) neutralizes this** — the bridge process never touches GPU/hnsw_rs, so its stdout is trivially clean; the daemon's prints go to its own stderr/journald. This is the single strongest argument for the bridge architecture.
- **R2 — schema/output drift.** `inputSchema` (and any future `outputSchema`) must track `*Args`/`*Output`. Derive them (schemars), never hand-write, and add a parity guard analogous to the existing CLI==daemon parity tests so a new field can't silently desync the advertised schema.
- **R3 — mutating tools + human-in-the-loop.** `index`, `notes add/remove`, `gc` mutate. Set the destructive/non-idempotent annotations honestly; the spec puts confirmation on the client, but accurate hints are our responsibility (and annotations are "untrusted" to clients, so they're hints, not enforcement).
- **R4 — `_meta` semantics.** The overlay markers (`overlay_graph: full|callers-only|seed-only`), `stale_origins`, `rank_signals`, `trust_level` are load-bearing agent-calibration signals (the result-trust program). Preserve them in MCP result `_meta` rather than dropping them in translation.

---

## 7. Recommended phasing

- **Phase 0 — plumbing (mechanical, low-risk):** add `schemars`; add `Serialize/Deserialize/JsonSchema` to `*Args` + `LimitArg`/`OverlayArgs` (+ `#[serde(flatten)]`); add serde to `RerankerMode`. Unblocks G2/G3. Shippable on its own; touches only derives.
- **Phase 1 — MVP MCP server (`cqs mcp`):** stdio↔socket bridge (G1); `tools/list` from the `for_each_batch_cmd!` registry; `inputSchema` from schemars (G2); `tools/call` → `dispatch_via_view` (G3); result/error reframe (G4); annotations from the read/mutate split. ~43 tools, `structuredContent` only (no `outputSchema`). No `*_core` changes. This is the "cleanly wrapped" milestone.
- **Phase 2 — fidelity:** `outputSchema` (type the handler returns, G5); flat `notes_*`/subcommand tools (G6); `resources/read` for `cqs read` + `resource_link`s in results; Tasks for `index`/`eval`.
- **Phase 3 — remote:** Streamable HTTP transport (Origin/localhost/session) for multi-client; revisit against the 2026 stateless-core direction before investing.

---

## 8. One-paragraph answer

cqs needs **no change to its command logic** to be wrapped in MCP — the command-core pattern already separated logic from surface. What's missing is a thin MCP adapter: a JSON-RPC server (best built as a stdio↔daemon-socket bridge, which also sidesteps the stdout-cleanliness hazard), `Serialize`/`Deserialize`/`JsonSchema` derives on the `*Args` structs to auto-generate `inputSchema`, and a small request/result/error reframe that reuses the existing `dispatch_*` routing and the `ErrorCode` taxonomy. The `for_each_batch_cmd!` macro is already the tool registry; the handlers already return JSON that drops straight into `structuredContent`. Output schemas, resources, and Tasks are valuable but optional follow-ons. The work is a contained adapter, deliberately enabled by the architecture (`project_mcp_direction`), not a refactor.
