# cqs MCP server — design (2026-06-16)

Companion to `2026-06-16-mcp-wrap-assessment.md` (the *what/why/gap*). This is the *how*: module layout, the bridge protocol, schema generation, request flow, and the decisions still open. Target spec revision: **MCP 2025-11-25**.

Status: **BUILD GREENLIT (2026-06-17).** Phase 0 is committed (this branch). The two foundational choices §10 left open are now decided — see §0.

---

## 0. Status & corrections (2026-06-17)

**Build greenlit (OQ-14 resolved).** The user confirmed re-introducing MCP. This is a *re-intro*, not greenfield — see §0b.

**inputSchema source = the `commands::*` core-input structs, NOT the `args.rs` clap structs** (this supersedes §6 below and assessment-doc G2). Discovered while building Phase 0: the command cores under `src/cli/commands/**` are ALREADY `#[derive(serde::Deserialize)] #[serde(default)]` with `///` doc comments → their schemas are correctly **optional** (a wire caller supplies just the fields it wants) and **described** (schemars harvests `///`). The `args.rs` clap structs mark every field *required* and their clap `#[arg(help)]` is invisible to schemars. The cores are also the canonical surface-agnostic input (`*_core` consumes them), so MCP becomes a true third surface — wire JSON → core struct → `*_core`, no clap re-parse — which is the anti-duplication discipline the command-core pattern exists for. Phase 0 (committed) derives `JsonSchema` on the ~21 such cores + their enums (schemars 1, JSON Schema 2020-12). Coverage: the ~22 commands without a core-input struct are mostly no-arg (`stats`/`health`/`ping`) → empty schema; the few real ones get a core struct as command-core completion in Phase 1.

## 0b. Prior art — the removed v0.10.0 MCP server

cqs shipped an MCP server through v0.9.x (`src/mcp/`, `cqs serve`): **in-process** (loaded GPU + index itself), **HTTP+SSE** transport, **27 files / ~3700 lines** with a **separate handler per tool** (`search.rs` alone was 428 lines) — each tool *reimplemented* its logic parallel to the CLI. That duplication is exactly what the CLI-first migration (#320/#333) and the command-core refactor set out to kill; the server was removed in **v0.10.0 (#352, commit `291ec6b0`)** for CLI-first + slim deps. **The re-intro inverts the old design**: thin (rides the cores → no per-tool files, ~10× smaller), an `stdio`↔daemon-socket bridge to the warm daemon (not in-process; supersedes HTTP+SSE; keeps stdout clean re the #2009 leak class), optional + slim (no HTTP-server deps). Mine the old code for reference via `git show 291ec6b0^:src/mcp/<file>` — especially `validation.rs` (resurrect its security tests), `server.rs`, `types.rs`, `transports/stdio.rs`.

---

## 1. Goal & scope

Ship `cqs mcp` — an MCP server exposing cqs's code-intelligence commands as MCP tools, with **no change to any `*_core` logic**. Phase 1 = a working stdio server over the read-mostly command set, `structuredContent` results, auto-generated `inputSchema`. Output schemas, resources, and Tasks are later phases (§9).

Non-goals (Phase 1): Streamable HTTP, `resources`/`prompts`, the Tasks extension, exposing `chat`/`watch`/`batch`, multi-client concurrency.

---

## 2. Architecture — the stdio↔socket bridge

```
  MCP client (Claude Code / Desktop)
        │  stdio: newline-delimited JSON-RPC 2.0
        ▼
  ┌─────────────────────────────┐
  │  cqs mcp   (the bridge)      │   thin, stateless-ish front-end
  │  - JSON-RPC framing + id     │   NO GPU / NO hnsw_rs / NO model
  │  - initialize / tools/list   │   → stdout is trivially clean (R1)
  │  - tools/call → relay        │
  └─────────────┬───────────────┘
                │  existing daemon socket frame
                │  {command, args}  →  {status, output}
                ▼
  ┌─────────────────────────────┐
  │  cqs watch --serve (daemon)  │   warm in-memory index, 3-19ms
  │  dispatch_via_view → *_core  │   (unchanged)
  └─────────────────────────────┘
```

**Why a bridge, not an in-process server:** (1) stdout cleanliness — the bridge never loads hnsw_rs/ORT/the model, so the stdio "MUST NOT write non-MCP to stdout" rule (the #2009 leak class) is satisfied by construction; (2) reuse the daemon's warm index + latency; (3) inherit same-uid socket auth.

**Fallback path:** if no daemon socket is reachable, the bridge builds an in-process `BatchView` (the CLI-mode dispatch) and serves from it. This path *does* load the model/hnsw_rs in the `cqs mcp` process → it MUST run under the same stdout-suppression discipline as the rest of cqs (the `stdout_gag` precedent). See OQ-1.

**Process model:** one stdio client per `cqs mcp` process (the client launches it as a subprocess). Requests are serialized per the daemon's existing per-connection model; `id` correlation lets us stay spec-correct even though we answer in order.

---

## 3. Module layout (new)

```
src/mcp/
  mod.rs           // `cqs mcp` entry: run the stdio loop
  jsonrpc.rs       // JSON-RPC 2.0 types (Request/Response/Error, id), framing
  server.rs        // lifecycle: initialize, initialized, capabilities, dispatch by method
  registry.rs      // tool registry built from for_each_batch_cmd! + annotations
  schema.rs        // inputSchema (+ later outputSchema) generation via schemars
  call.rs          // tools/call: arguments → *Args (serde) → relay → CallToolResult
  transport.rs     // bridge: connect to daemon socket; in-process fallback
  errors.rs        // ErrorCode ↔ JSON-RPC / isError mapping
```

`cqs mcp` is added to the `Commands` enum (`src/cli/definitions.rs`) as `#[cqs_cmd(batch = "cli")]` (it's a process mode, not a batch tool). It is a *consumer* of the batch dispatch layer, not a member of it.

---

## 4. Lifecycle & capabilities

`initialize` → reply with:
```json
{
  "protocolVersion": "2025-11-25",
  "capabilities": { "tools": { "listChanged": false } },
  "serverInfo": { "name": "cqs", "version": "<crate version>",
                  "title": "cqs code intelligence" },
  "instructions": "Semantic code search, call graphs, impact analysis... prefer `scout`/`task` for whole-task context; results carry calibration metadata in _meta."
}
```
- `listChanged:false` — the tool set is static per binary (no runtime registration). Revisit only if dynamic slots/refs become tools.
- Version negotiation: accept `2025-11-25`; if the client sends an older supported revision we recognize, echo theirs; else reply `2025-11-25` and let the client decide. (OQ-10: how far back to support.)
- `notifications/initialized` → enter operation. `ping` supported.

Capabilities NOT declared in P1: `resources`, `prompts`, `logging`, `completions`, `tasks` (each is a later phase).

---

## 5. Tool registry (`registry.rs`)

Source of truth: the `for_each_batch_cmd!` macro (`src/cli/batch/commands.rs:395-450`). For each variant produce a `ToolDef`:

```
ToolDef {
  name:        <command name>,                  // see naming, §7
  description: <leading /// doc on the Commands variant>,
  input_schema: schema::for_args::<XArgs>(),    // §6
  annotations: { read_only_hint, destructive_hint, idempotent_hint, open_world_hint:false },
  // output_schema: None in P1 (§9 G5)
}
```

- The macro's three categories map directly: `args_variants` → tools with a schema; `ctx_only_variants`/`unit_variants` → `{"type":"object","additionalProperties":false}`.
- Annotations are derived from a static read/mutate table (the audit's catalog): read-only set vs `index`/`gc`/`notes remove`/etc.
- A compile-time exhaustiveness check (the macro already gives this) guarantees no command is silently absent from `tools/list`.

`tools/list` returns the whole set in one page (43 tools fits comfortably; `nextCursor` omitted). Add cursor pagination only if the set grows past a sane single-response size.

---

## 6. Schema generation (`schema.rs`) — the one real cross-cutting change

Per the assessment G2/Phase-0:
1. `schemars` dependency.
2. `#[derive(Serialize, Deserialize, JsonSchema)]` on the `*Args` structs (`src/cli/args.rs`) + shared `LimitArg`/`OverlayArgs`, with `#[serde(flatten)]` mirroring each `#[command(flatten)]`.
3. `#[derive(Serialize, Deserialize)]` on `RerankerMode` (the only CLI-only enum missing serde).
4. `for_args::<T: JsonSchema>() -> serde_json::Value` wrapping `schemars::schema_for!(T)`, post-processed to: strip surface-local fields if any leak in, ensure 2020-12 dialect, attach `description` from clap `help` where schemars doesn't carry it.

Exclusions (confirmed by audit): `--json` is not in `*Args` (never appears); `--slot` is transport (omit / OQ-9); `--tokens` IS a real field → keep. Scope flags (`lang`,`path`) are per-`*Args` and included where present.

**Schema parity guard** (R2): a test that asserts, for every tool, the generated `inputSchema` round-trips a representative `arguments` object into the `*Args` struct via `serde_json::from_value` — so a field rename can't desync the advertised schema from the deserializer. Analogous to the existing CLI==daemon parity tests.

---

## 7. Tool naming & the subcommand problem

Phase-1 proposal: **flat, bare command names** (`search`, `callers`, `impact`, `scout`, `gather`, `task`, `read`, `dead`, `health`, `stats`, …). Rationale: matches how the cqs CLI/skills already refer to them; MCP allows `-`/`.`/`_` so we keep room to namespace later.

Subcommands (`notes`, `ref`, `slot`, …) don't map 1:1. Phase-1: expose only the batch-dispatchable `notes list` (read-only) as `notes_list`. Phase-2: lift `notes add/remove/update` into command-cores and expose flat tools `notes_add`/`notes_remove`/`notes_update`. (OQ-2 on the separator + whether to prefix everything with `cqs_`.)

---

## 8. Request flow (`call.rs`) + result/error mapping

**tools/call** `{name, arguments}`:
1. Look up `name` in the registry → unknown ⇒ JSON-RPC `-32601`.
2. `serde_json::from_value::<XArgs>(arguments)` → deserialize error ⇒ JSON-RPC `-32602` (invalid params).
3. Relay to the daemon: build the existing `{command, args}` frame. **Open**: relay the typed JSON directly vs. re-serialize `*Args`→argv tokens for the current socket protocol (OQ-3 — the socket today takes a string-arg array). Cleanest long-term: teach `dispatch_via_view` a JSON-args entry so MCP skips the argv round-trip.
4. Receive `{status, output}`; the inner dispatch envelope is `{data, _meta}` or `{error:{code,message}, _meta}`.

**Result mapping:**
```
CallToolResult {
  structuredContent: <output.data>,
  content: [ { type:"text", text: serde_json::to_string(&output.data) } ],
  _meta: <output._meta>,            // stale_origins, overlay_graph, rank_signals, trust_level…
  isError: false
}
```

**Error mapping** (the ErrorCode split):

| cqs error | origin | MCP channel |
|---|---|---|
| unknown tool | registry miss | protocol `-32601` |
| bad/missing `arguments` | from_value fail | protocol `-32602` |
| malformed JSON-RPC | framing | protocol `-32700` / `-32600` |
| `not_found`, `invalid_input`, `internal`, `io_error`, `timeout`, `parse_error` | handler dispatch | **tool error**: `isError:true`, `content:[{type:text, text:<redacted message>}]`, and surface `error.code` in `_meta` |

`redact_error` already strips paths/queries → messages are safe to expose to the model.

---

## 9. Phasing (deliverables)

- **Phase 0 — derives (mechanical):** schemars + serde/JsonSchema on `*Args` + `RerankerMode`. Ships independently; only derives + the parity guard. (Assessment G2 prereq.)
- **Phase 1 — MVP `cqs mcp`:** §2 bridge, §4 lifecycle, §5 registry, §6 schema, §8 call/error. Read-mostly tools, `structuredContent` only. Conformance + round-trip tests. (G1/G3/G4/G6-partial.)
- **Phase 2 — fidelity:** `outputSchema` (type the `dispatch_*` returns or codegen, G5); flat `notes_*` + subcommand cores (G6); `resources/read` for `cqs read` + `resource_link`s in results; Tasks for `index`/`eval`; full annotation set.
- **Phase 3 — remote:** Streamable HTTP transport (Origin/localhost/`MCP-Session-Id`); reassess against the 2026 stateless-core direction first.

---

## 10. Open questions

These are genuine decisions, not rhetorical — most gate Phase 1.

**OQ-1 — Bridge vs in-process, and daemon dependency.** Is `cqs mcp` *always* a bridge to a running `cqs watch --serve`, with an in-process fallback? Or should it auto-spawn a daemon if none is running? Or be in-process by default? The bridge is the clean answer for stdout-safety, but it makes "daemon must be running" a precondition for the headline feature. Auto-spawn adds lifecycle complexity (who owns the daemon's death?). **Recommendation to confirm:** bridge-first, auto-connect, in-process fallback (under stdout_gag), no auto-spawn.

**OQ-2 — Tool naming.** Bare (`search`) vs prefixed (`cqs_search`/`cqs.search`)? Prefixing avoids collisions when a client mounts many MCP servers; bare is cleaner and matches our docs. Separator for subcommands: `notes_add` vs `notes.add` (dots are legal). Pick one convention now — renaming tools later breaks client configs/muscle memory.

**OQ-3 — Relay format.** The daemon socket currently takes `args: [String]` (argv tokens, re-parsed by clap). Do we (a) serialize `*Args`→argv to fit the existing frame, or (b) extend the socket/dispatch to accept a JSON `arguments` object directly (cleaner, avoids a lossy round-trip, but a new daemon-protocol surface to version)? (b) is better long-term and also simplifies the CLI's own daemon path.

**OQ-4 — Which commands ship in Phase 1?** All 43? Read-only only (defer mutating `index`/`gc`/`notes`)? Mutating tools need honest destructive annotations and arguably a different trust posture. **Lean:** all read-only batch commands in P1; `index`/`notes_*`/`gc` in P2 with annotations + (for index) Tasks.

**OQ-5 — Context/statefulness (the thorniest).** How does a tool call learn its *project / cwd / worktree / slot / audit-mode*? Options: (a) the `cqs mcp` process is launched with a fixed project root (cwd) and binds to that project's daemon — simplest, one server per project; (b) every tool takes an explicit `project`/`worktree`/`slot` param — flexible but noisy and easy to get wrong; (c) a `roots` capability negotiation (MCP clients can advertise filesystem roots). The worktree-overlay feature specifically needs the worktree path. **Lean:** (a) for P1 (one server per project root, inherits cwd → daemon → overlay/audit/slot all resolve as they do for the CLI today), expose `slot`/`overlay` as optional params for power use.

**OQ-6 — Pipeable command chaining.** The 10 pipeable commands (`callers | test-map`) chain in the batch layer. MCP has no native piping. Do we (a) do nothing and let the model pass a name from one tool's output into the next, (b) add a `_result_of_tool` hint param, or (c) expose a single `pipeline` tool taking a pipe string? **Lean:** (a) for P1 — the model is good at this; revisit if it proves clumsy.

**OQ-7 — Token budgeting semantics.** Keep `--tokens` as an optional per-tool param (yes). But: should there be a *default* result budget for MCP (results land in the model's context, so unbounded is risky)? Does the MCP host's own token accounting double-count? **Lean:** expose `tokens` optional, document it, no enforced default in P1; measure.

**OQ-8 — Tasks for long-runners.** `index`/`eval` are seconds–minutes. Implement the 2025-11-25 Tasks extension in P1 (so `index` is a proper background task with progress), or defer to P2 and have P1 simply not expose them? Note daemon-routed `index` is already async (returns after queueing a reconcile). **Lean:** defer (don't expose `index` as a tool in P1).

**OQ-9 — `read` as resource vs tool vs both.** `cqs read` fits MCP `resources/read` (file:// URIs) but is also usable as a plain tool. Resources need the `resources` capability + `resources/list`. Do we expose `read` as a tool in P1 and add the resource surface in P2? Should search/callers results carry `resource_link` blocks pointing at matched files? **Lean:** `read` as a tool in P1; resources + resource_links in P2.

**OQ-10 — Protocol-version support window.** Support only `2025-11-25`, or also negotiate down to `2025-06-18` (first with structured output) / `2025-03-26`? Each older revision is a compatibility surface. **Lean:** advertise `2025-11-25`; accept a client's older request only if it's `>= 2025-06-18` (structured output present) and we can serve it identically; otherwise reply with ours.

**OQ-11 — `outputSchema` timing & approach.** Confirmed optional for P1. When we add it (P2): refactor `dispatch_*` to return typed `*Output` (loses the current `Value` flexibility but gains schema), or codegen schemas from the `*Output` structs without changing return types, or hand-maintain? The handler-return refactor is the cleanest but touches all 43 handlers.

**OQ-12 — `not_found` as error vs empty.** Today `callers <unknown>` etc. yield a `not_found` *error*. As an MCP tool, is "no callers found" better as `isError:true` (the model retries) or an empty-but-ok result (the model concludes "none")? This is a per-command semantic call — some `not_found`s are "you typo'd the symbol" (error, retry) vs "this symbol genuinely has no callers" (empty, ok). May need per-command treatment.

**OQ-13 — Distribution / client config.** How do users wire it up? The MCP config entry is `{ "command": "cqs", "args": ["mcp"] }` (stdio). Do we ship a `cqs mcp --print-config` helper and document it in README + the `cqs-bootstrap` skill? Does `cqs-bootstrap` auto-write the MCP config (it already mentions MCP config in its description)?

**OQ-14 — Relationship to the existing `MCP out of scope` stance.** `project_mcp_direction` says "shape cores as future MCP tools; do not implement MCP." Implementing this flips that. Confirm the intent is to *build* now (vs. keep it as readiness), and update the memory + CONTRIBUTING's NEW-COMMAND doc to add the MCP surface to the per-command checklist (alongside `cmd_*`/`dispatch_*`).

---

## 11. Testing strategy

- **JSON-RPC conformance:** initialize handshake, version negotiation, unknown-method/`-32601`, malformed `-32700`, `ping`.
- **Schema parity guard (R2):** every tool's `inputSchema` deserializes a representative `arguments` into its `*Args` (catches schema/deserializer drift).
- **Tool round-trip:** for a spread of tools, `tools/call` → assert `structuredContent` matches the daemon's `data` for the equivalent batch call (extends the existing CLI==daemon parity to CLI==daemon==MCP).
- **Error mapping:** unknown tool, bad args, and a handler `not_found` each land in the right channel (protocol vs `isError`).
- **stdout cleanliness (R1):** a subprocess test (per the `tests/hnsw_stdout_leak_test.rs` pattern) asserting the `cqs mcp` process emits *only* valid JSON-RPC on stdout across a session — including a call that triggers an HNSW build on the in-process fallback path.
