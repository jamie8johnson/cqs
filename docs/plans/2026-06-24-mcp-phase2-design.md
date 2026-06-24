# MCP Phase 2 — Mutating Tools Design

Status: design. Extends the Phase-1 read-only bridge (19 exposed tools = the 20 JSON-args-capable commands minus the withheld `context`/`explain`, per `src/cli/mcp/tools.rs:88-91` and the registry-parity guard `src/cli/mcp/mod.rs:52-91`) to a curated set of mutating commands.

Grounding: every claim below cites the four scout maps and the source they reference. Where a map fact was verified directly against source, the source line is cited.

---

## 1. Scope

### 1.1 The mutating command inventory

The full mutating set is enumerated by the `mutates_index` method at `src/cli/definitions.rs:1180-1228`: `index`, `gc`, `reconcile`, `refresh`, `notes` (add/update/remove), `cache` (clear/prune/compact), `slot` (create/promote/remove), `model swap`, `ref` (add/update/remove), `audit-mode` (on/off), `watch`.

Two gating axes decide P2 eligibility, both structural (not policy):

- **Phase-0 JsonSchema core** (the #2021 dependency). A daemon-routed MCP tool MUST deserialize its `arguments` into a `schemars::JsonSchema` core — the same struct the bridge generates `inputSchema` from and the daemon deserializes (`src/cli/mcp/tools.rs:14-16`, `:62-68`). Only 20 commands have such a core (`src/cli/batch/json_args.rs:151-309`); **zero mutating commands do** (map 1 fact, "PHASE-0 JSONSCHEMA COVERAGE"). A command without a core hits the explicit `bail!` "does not accept a JSON `arguments` object (no Phase-0 core struct)" at `src/cli/batch/json_args.rs:304-307`.
- **Daemon dispatch that can mutate.** The daemon opens `Store<ReadOnly>` (map 1 constraint, "Daemon socket boundary"). A command that writes `index.db` cannot reach a writable store over the socket. `gc` makes this a *compile-time* fact: `dispatch_gc` is unreachable and `bail!`s "gc requires a writable store … Commands::Gc is BatchSupport::Cli" (`src/cli/batch/handlers/misc.rs:440-447`).

### 1.2 What P2 EXPOSES

Only the commands that mutate **ephemeral daemon state** (no new core, no writable store, already daemon-wired) plus the one command whose mutation is a human-readable manifest with a small, well-shaped argument surface:

| Tool | Mutation | Daemon-wired? | Core today? | P2 phase |
|------|----------|---------------|-------------|----------|
| `cqs_refresh` | invalidate in-memory caches + re-open Store; **no file/schema write** | YES (`src/cli/batch/handlers/misc.rs:457-461`) | zero-arg (no payload needed) | P2a |
| `cqs_reconcile` | flip `SharedReconcileSignal` AtomicBool; advisory async signal | YES (`src/cli/batch/handlers/misc.rs:583-604`) | `ReconcileArgs` exists (`src/cli/batch/commands.rs:330-333`) | P2a |
| `cqs_notes_add` | append to `notes.toml` + watch-triggered reindex | NO (json_args rejects, `:304-307`) | **must build (#2021)** | P2a |
| `cqs_notes_update` | rewrite a note in `notes.toml` + reindex | NO | **must build (#2021)** | P2a |
| `cqs_notes_remove` | delete a note from `notes.toml` + reindex | NO | **must build (#2021)** | P2a |
| `cqs_index` | full index rebuild (long-runner) | NO (CLI-only, `#[cqs_cmd(batch="cli")]`, map 2) | **must build (#2021)** | P2b (fire-and-forget) / P3 (Tasks) |

`refresh` and `reconcile` are the *only* mutators that are both daemon-wired and gate-safe today (map 1 fact, "DAEMON DISPATCH WIRED (Lane 1 JSON-args path)"). Everything else needs the #2021 core lift; the notes-write trio and `index` are the only such commands worth the lift in P2 (rationale below).

### 1.3 What P2 WITHHOLDS, and why

**The destructive set — withheld unconditionally in P2 (revisit only behind the consent gate of §2):**

- **`gc`** — irreversibly prunes `chunks`/`calls`/`type_edges`/`summaries`/`sparse_vectors` from the index DB (map 1). Highest blast radius after a bad invocation: silent recall loss. Compile-time CLI-only (`src/cli/batch/handlers/misc.rs:440-447`); exposing it means BOTH building a core AND defeating the typestate that currently guarantees the daemon can't run it. Withhold: the guardrail it would remove is load-bearing.
- **`slot remove`** — deletes a slot tree `.cqs/slots/<name>` from the filesystem (`src/cli/definitions.rs:1205-1210`, map 1). No undo, no DB transaction to roll back. A name-typo from an RT-RELAY-steered client (§2) erases an index. Withhold.
- **`index --force` / `model swap`** — full reindex; `model swap` additionally rewrites every embedding with a new model (`src/cli/definitions.rs:1017-1022`). Both are long-runners with no core today. `index` (non-force) returns in P2b as fire-and-forget (§4); `index --force` and `model swap` stay withheld in P2 — a forced full rebuild from an MCP client is the destructive variant of a fire-and-forget add.
- **`cache clear`** — deletes `embeddings_cache.db` outright (`src/cli/definitions.rs:904-910`, map 1); discards paid embedding work (the cross-slot copy economics in MEMORY make this real cost). Withhold. (`cache prune`/`cache compact` are non-destructive in-place ops but lack a core and carry no MCP-client value — withheld for cost-of-lift, not danger.)
- **`slot create` / `slot promote` / `ref add|update|remove`** — filesystem/registry mutations, no core, no daemon path (map 1). `slot promote` swaps the active index (`mv` dance, `src/cli/definitions.rs:1205-1210`); promote-to-wrong-slot is a quiet correctness fault. Withheld for cost-of-lift + low MCP-client demand; not in the danger tier but not earning the #2021 lift.
- **`audit-mode on|off`** — toggles a per-user state file affecting search ranking (`src/cli/commands/infra/audit_mode.rs:126`). An MCP client silently flipping the *human operator's* audit mode is a cross-principal side effect. Withhold.
- **`watch`** — not a request/response command; it is a long-lived background process (`src/cli/definitions.rs:453-476`, map 1). Structurally not an MCP tool. Withhold permanently.

**Summary of the #2021 dependency:** P2a's notes trio and P2b's `index` are *blocked on #2021* (build the JsonSchema core + daemon dispatch). `refresh`/`reconcile` are *not* blocked — they can land the instant the annotation plumbing (§5) exists.

---

## 2. The Trust Boundary (headline)

### 2.1 The threat delta of mutations

Phase 1's boundary argument is "annotations are hints; the daemon's path/overlay gates are the real boundary" (map 3 constraint). That argument holds for reads because a read cannot *change* state — the worst an RT-RELAY-steered read does is exfiltrate (handled by the `context`/`explain` withhold, `src/cli/mcp/tools.rs:8-10`). **Mutations break that symmetry: the worst case is now persistent state change, and for the destructive set it is irreversible state destruction.**

The RT-RELAY vector is concrete here. Indexed content (a malicious comment, a doc string) can steer the agent-consumer (map 1's red-team family, CLAUDE.md). With reads, the steered action is "call `cqs_search` with an attacker-chosen query" — annoying, bounded. With mutations the steered action becomes "call `cqs_gc`" (recall destroyed) or "call `cqs_slot_remove --name primary`" (index erased) — a single tool call from a steered client is unrecoverable. The annotation `destructiveHint:true` does **nothing** to stop this; annotations are advisory metadata the client may ignore (map 3 constraint, "annotations are hints only, not enforcement").

### 2.2 The guardrail decision

The four candidate guardrails:

1. **Annotations-only** — mark destructive tools and trust the client. Rejected: annotations are not a boundary (§2.1); this is the read-era argument applied where it no longer holds.
2. **Opt-in `enable-mutations` env var** — gate the whole mutating surface behind one flag. Insufficient alone: a single binary flag can't distinguish "let the agent add a note" from "let the agent gc the index"; it collapses a 100×-blast-radius spread into one yes/no.
3. **Withhold the destructive set** — never expose `gc`/`slot remove`/`index --force`/`model swap`/`cache clear`/`audit-mode` as tools at all. Strong: an un-exposed tool cannot be steered into. This is the §1.3 decision.
4. **Consent model** — per-call human confirmation for destructive ops. Out of scope for the stdio bridge (no UI surface; the bridge is request→response, map 2).

**Decision (decisive):** **Withhold the destructive set (3) as the primary boundary, layered with a tiered enable flag (2-refined) for the merely-state-changing set.** Concretely:

- The destructive tier (§1.3) is **not in `tool_table()`** — boundary by absence, the same mechanism that makes the `context` withhold real. No flag re-enables it in P2; re-exposure is a separate design behind a real consent surface (P3+).
- The mutating-but-recoverable tier (`refresh`, `reconcile`, `notes_*`, `index` fire-and-forget) is exposed but gated behind `CQS_MCP_ENABLE_MUTATIONS=1` (default off). When off, `tools/list` emits only the 19 read tools (Phase-1 behavior, zero delta); when on, it adds the P2a/P2b mutating tools. This makes "an MCP client can change my index state" an explicit operator opt-in, not a silent capability upgrade on binary update.
- `refresh`/`reconcile` invalidate only ephemeral cache/signal state (no persisted write, map 1 facts) — they are the *safest* mutations and the strongest argument that the flag's default-off is conservative, not paranoid. They could arguably ship un-gated; we gate them anyway so the flag's semantics stay simple ("mutations need the flag, full stop") rather than carving a "soft mutation" exception the operator has to reason about.

The boundary is therefore **absence (destructive) + explicit opt-in (recoverable)**, never **annotation-trust**. Annotations (§3) are shipped as honest metadata for clients that *do* honor them, not as the guardrail.

---

## 3. Annotations

Per-tool annotations for the exposed P2 set. The MCP 2025-11-25 defaults are `readOnlyHint:false`, `destructiveHint:true`, `idempotentHint:false`, `openWorldHint:false` (map 3 fact); cqs overrides per command semantics rather than accepting the destructive-by-default (map 3 open question). All exposed P2 tools are local-only → `openWorldHint:false`.

| Tool | readOnlyHint | destructiveHint | idempotentHint | Rationale |
|------|:---:|:---:|:---:|-----------|
| `cqs_refresh` | false | false | true | Invalidates in-memory caches; no persisted write (`src/cli/batch/handlers/misc.rs:457-461`). Repeating it converges to the same re-opened Store → idempotent. Not destructive: a cache is reconstructible. |
| `cqs_reconcile` | false | false | true | Flips an AtomicBool the watch loop coalesces (`src/cli/batch/handlers/misc.rs:583-604`; `was_pending` documents the coalesce). Repeated calls collapse to one walk → idempotent. Not destructive: advisory signal, eventual-consistency. |
| `cqs_notes_add` | false | false | false | Appends a note + triggers reindex (`src/cli/commands/io/notes.rs:434-570`). NOT idempotent — calling twice writes two notes. NOT destructive — additive only. |
| `cqs_notes_update` | false | false | true | Rewrites a matched note (`src/cli/commands/io/notes.rs:571-708`). Idempotent — re-applying the same update is a no-op rewrite. Not destructive: the prior text is replaced, not the file. |
| `cqs_notes_remove` | false | **true** | true | Deletes a note from `notes.toml` (`src/cli/commands/io/notes.rs:709-789`). Destructive — the note text is gone (no soft-delete). Idempotent — removing an absent note is a no-op. The lone destructive flag in the exposed set; honest signal for honoring clients, but the §2 boundary (opt-in flag) is what actually contains it. |
| `cqs_index` (P2b) | false | false | false | Full rebuild (additive/refresh of the same DB, not a delete). NOT idempotent at the daemon-state level (each run re-embeds; content-hash cache makes it cheap-but-not-no-op). Not destructive in the data-loss sense — it rebuilds from source-of-truth (the tree). |

Two notes on the table:
- `notes_remove` is `destructiveHint:true` but stays *in* the exposed set (unlike `gc`) because the blast radius is one note vs. the whole index, and it sits behind the same opt-in flag. The honest annotation lets a consent-capable client (P3) prompt on it specifically.
- The ToolDef struct currently stores no annotations (`src/cli/mcp/tools.rs:47-57`; `list()` hardcodes them at `:275-282`). §5 makes them per-tool — the prerequisite for this table to exist at all.

---

## 4. Tasks vs Fire-and-Forget for `index` (the key fork)

`index` is the only long-runner (seconds–minutes) in scope; reads are 3-19 ms and `refresh`/`reconcile`/`notes_*` are fast (map 1). This is the fork that shapes the bridge's threading model.

### 4.1 The constraint that decides it

The stdio bridge is **synchronous per request**: read one line, dispatch, write one response line, return (`socket.rs` handle_socket_client, map 2 facts/constraints). MCP stdio forbids non-MCP writes to stdout and expects one response per request. **Emitting Tasks progress notifications mid-dispatch requires a background thread that writes to stdout while the request handler parks** — which violates the "one request-response pair per socket read" model and risks a notification racing the response envelope (map 2 constraint, "progress notifications across stdio bridge"). Tasks is therefore architecturally expensive *on the stdio transport specifically*, independent of `index`'s own readiness.

Separately, the daemon already has an async-queue primitive: `reconcile_signal` is an `Arc<AtomicBool>` the watch loop drains at 100 ms cadence; `request_reconcile` (`view.rs:1589-1598`, map 2) flips it and returns immediately, decoupling the caller from the rebuild latency. The watch loop is the background executor — **the daemon already does fire-and-forget index work natively.**

### 4.2 Recommendation: fire-and-forget in P2b, defer Tasks to P3

Expose `cqs_index` as **fire-and-forget** in P2b and defer the MCP Tasks extension to P3 (which already owns the HTTP transport per the design-doc §9 phasing, map 2). Justification, by bridge-architecture cost:

- **Fire-and-forget reuses the existing async primitive.** `cqs_index` becomes "queue a rebuild, return `{queued:true}` immediately" — the same shape as `reconcile` (`dispatch_reconcile` returns synchronously after queueing, `src/cli/batch/handlers/misc.rs:583-604`). The client then polls freshness with the *already-shipped read* `wait_fresh`/`status` (`dispatch_wait_fresh` at `src/cli/batch/handlers/misc.rs:527-566`, `dispatch_status` at `:499-504`). No new threading model, no stdout-race, no Tasks protocol. This is the "expose index fire-and-forget" arm — and cqs is unusually well-suited to it because the poll endpoint already exists as a Phase-1 read tool.
- **Tasks on the stdio bridge is a different bridge.** Implementing Tasks means: a background task thread on the bridge, mid-dispatch notification streaming over stdout, and the main handler parking until job completion (map 2 constraints). That is a rewrite of the bridge's core loop for one tool, against a transport the spec makes hostile to it. The daemon socket *could* carry Tasks (it has a dispatch loop), but the stdio bridge — the P1/P2 transport — cannot cleanly (map 2 constraint, "MCP Tasks protocol is OUT-OF-SCOPE for daemon-routed index"; design-doc-OQ-8 already defers Tasks to P2/later).
- **Tasks earns its cost only with the HTTP transport.** Progress + cancellation are valuable when the client wants a live status stream or to abort an in-flight rebuild. HTTP (P3) is the natural place: it supports server→client streaming without the stdout-line contract. Building Tasks in P2 spends the bridge-rewrite cost before the transport that makes it cheap exists.

So: **P2b = `index` fire-and-forget + reuse `wait_fresh` for polling. P3 = Tasks (progress/cancel) on the HTTP transport for `index`/`eval`.** This still requires the #2021 `index` core (§5) — fire-and-forget needs `IndexArgs`/`index_core`/a daemon dispatch that *queues* rather than blocks (model on `request_reconcile`, not a blocking build).

---

## 5. Implementation Plan

### 5.1 `src/cli/mcp/tools.rs` — annotations become per-tool

Today `ToolDef` carries no annotation fields (`:47-57`) and `list()` hardcodes `readOnlyHint:true … destructiveHint:false` for every tool (`:275-282`). P2 prerequisite (the §3 table cannot exist otherwise):

1. Add an `annotations: ToolAnnotations` field to `ToolDef` (`:47-57`), a small struct of the four hint bools. Existing P1 rows get the read-only quartet (no behavior change — same values, now per-row).
2. Rewrite `list()` (`:267-287`) to read `t.annotations` instead of the hardcoded block.
3. Add the mutating rows to `tool_table()` (`:92`) — gated: when `CQS_MCP_ENABLE_MUTATIONS` is unset, `tool_table()` returns the P1 slice; when set, the P2a (and P2b) rows append. (Map 3 open question — "added immediately with P2 annotations or deferred": added, but flag-gated, so default `tools/list` is byte-identical to P1.)

### 5.2 Dispatch path — reuse, don't fork

`tools/call` routes through `relay_and_classify` → the Lane 1 JSON-args frame (`src/cli/mcp/tools.rs:355-356`, map 3). **The mutating tools reuse the same dispatch path** (map 3 open question — "share the same tools/call dispatch path?": yes). No separate mutating dispatcher. The only requirement is that each mutating command have a daemon arm that *can* mutate — which for the notes trio and `index` is the #2021 lift, and for `refresh`/`reconcile` already exists.

### 5.3 The #2021 core-completion dependency (per command)

For `cqs_notes_add|update|remove` and `cqs_index`, #2021 must (mirroring map 2's index recipe):
1. Build a `*Args` core with `serde` + `schemars::JsonSchema` derives (`NotesAddArgs`, `NotesUpdateArgs`, `NotesRemoveArgs`, `IndexArgs`).
2. Lift the handler body into a `*_core(args) -> Result<*Output>` (notes logic currently at `src/cli/commands/io/notes.rs:434-789`; index build at `src/cli/commands/index/build.rs`).
3. Add the `BatchCmd` variant + the `build_batch_cmd` match arm in `src/cli/batch/json_args.rs` (today these fall through to the `bail!` at `:304-307`).
4. Add the daemon `dispatch_*` handler.

Critical wiring subtlety for notes: the json_args path *currently rejects notes mutations explicitly* (map 1 fact, "json_args.rs … explicitly rejects notes mutations"). #2021 replaces that rejection with a real arm. **The notes daemon handler must obtain a writable path** to `notes.toml` — note that the notes write is to a *file* that the watch loop reindexes, NOT to `index.db` directly (`src/cli/commands/io/notes.rs:434-570`), so it does NOT need a writable `Store` and does NOT hit the `Store<ReadOnly>` wall that blocks `gc`. This is why notes is daemon-viable and `gc` is not. (Without this distinction, notes would be in the withheld set with `gc`.)

For `index` fire-and-forget (§4): the daemon `dispatch_index` must *queue* (model on `request_reconcile`, `view.rs:1589-1598`), not block the socket handler for minutes.

### 5.4 Error mapping — pure reuse

No new error code. `classify_output` (`src/cli/mcp/tools.rs:461-481`) already maps a handler error riding under `status:"ok"` to `isError:true` via `classify_slim_envelope` (`src/daemon_translate.rs:441-467`). A `notes_remove` "note not found" or an `index` "tree locked" surfaces as `isError:true` with the redacted message — identical to how a read handler error surfaces today. The empty-with-candidates retry path (`success_result`, `:485-489`) is read-specific and harmless for mutators (they don't emit candidates).

### 5.5 Registry-parity guard — extend, don't break

The guard `tools_list_matches_json_args_registry` (`src/cli/mcp/mod.rs:52-91`) pins exposed == json_args_capable − withheld. P2 must extend its `json_args_capable` set to include the newly-cored mutating commands AND extend `withheld` to name the destructive set explicitly, so the guard *enforces* §1.3 (a future hand that adds `cqs_gc` to the table fails the test). Add a sibling assertion: mutating rows present iff `CQS_MCP_ENABLE_MUTATIONS` is set.

---

## 6. Phasing (each independently landable)

**P2a — safe-additive (no #2021 for refresh/reconcile; #2021 for notes):**
- Land the per-tool annotation plumbing (§5.1) — pure refactor, P1 output byte-identical.
- Expose `cqs_refresh` + `cqs_reconcile` (daemon-wired already; `ReconcileArgs` exists at `src/cli/batch/commands.rs:330-333`; `refresh` is zero-arg). Behind `CQS_MCP_ENABLE_MUTATIONS`.
- Then `cqs_notes_add` first (additive, `idempotentHint:false`, `destructiveHint:false` — the lowest-risk #2021 mutator), then `notes_update`, then `notes_remove` (the one `destructiveHint:true`). Each is an independent #2021 core + dispatch + table row.
- Landable in slices: annotations refactor → refresh/reconcile → notes_add → notes_update → notes_remove. Each is a green PR on its own.

**P2b — `index` fire-and-forget (#2021 index core + queueing dispatch):**
- `IndexArgs` core, `dispatch_index` that queues (modeled on `request_reconcile`), `cqs_index` tool row. Client polls via the existing `wait_fresh`/`status` reads. No Tasks. Independently landable after P2a (depends only on the annotation plumbing, not on the notes work).

**P3 — Tasks + HTTP (deferred):**
- MCP 2025-11-25 Tasks (progress + cancel) for `index`/`eval`, on the HTTP transport where mid-flight server→client streaming is native (§4.2). Re-evaluate the destructive set behind a real consent surface here — HTTP can carry an elicitation/consent round-trip the stdio bridge cannot. Independently landable; nothing in P2 depends on it.

The destructive set (`gc`, `slot remove`, `index --force`, `model swap`, `cache clear`, `audit-mode`, `watch`) is **not phased in** — it is withheld until a consent model exists (P3+ at the earliest), not "P3 work."

---

## 7. Open Questions (genuine forks for the user)

1. **Gate granularity.** §2 proposes one flag `CQS_MCP_ENABLE_MUTATIONS` for the whole recoverable tier. Fork: do you want a *single* flag (simple, but bundles `notes_remove`'s destructive-hint in with `notes_add`), or a two-level split (`…_NOTES` vs `…_INDEX`) so an operator can allow note-writing without allowing rebuilds? Recommendation leans single-flag for P2a simplicity; the split is cheap to add at P2b.

2. **Should `refresh`/`reconcile` be exempt from the flag?** They mutate only ephemeral cache/signal state (`src/cli/batch/handlers/misc.rs:457-604`) — semantically read-adjacent (map 1 risk notes). Fork: gate them (clean "all mutations need the flag" rule) vs. expose them un-gated as P1.5 (they're genuinely safe; gating them may frustrate the common "clear my stale cache" agent flow). I gated them in §2 for rule-simplicity; reasonable to flip.

3. **`reconcile` semantics honesty.** The tool name implies a synchronous re-walk but the behavior is advisory/eventual-consistency — the signal is lost if the daemon crashes between request and tick (map 1 open question). Fork: expose it named `cqs_reconcile` with the caveat in the description, expose it as `cqs_reconcile_request` (name encodes the async semantics), or don't expose it at all (low MCP-client value — agents rarely fire git hooks). 

4. **Notes daemon path vs. the historical rejection.** json_args *currently rejects* notes mutations on purpose (`src/cli/batch/json_args.rs:304-307`, map 1). #2021 reverses that. Fork: is a daemon-side notes write acceptable, or do you prefer the "daemon relays back to a CLI handler" pattern (map 1 open question) to keep all index-touching writes off the daemon process? The daemon-side write is simpler and is gate-safe (notes writes a *file*, not `Store`, §5.3) — recommendation is daemon-side, but this is your "no debt in foundation layers" call.

5. **`index` Tasks timing.** §4 defers Tasks to P3/HTTP. Fork: is fire-and-forget + `wait_fresh` polling an acceptable P2b UX for `index`, or do you want cancellation badly enough to pull Tasks forward onto the daemon socket in P2 (accepting the bridge-loop rewrite cost, map 2 open question on the notification channel)? Recommendation: fire-and-forget now; Tasks when HTTP lands.

6. **Re-exposing the destructive set, ever.** §1.3 withholds `gc`/`slot remove`/etc. permanently-for-now. Fork: is there a real agent workflow that needs `gc` from MCP (e.g. an autonomous maintenance agent), or is "destructive ops are operator-only, CLI-only, forever" the intended stance? This determines whether P3's consent surface is worth building or whether the destructive set is simply out of MCP's charter.

This design doc is complete and grounded entirely in the four maps plus direct source verification. Key load-bearing files cited throughout: `src/cli/mcp/tools.rs` (annotation hardcoding at :275-282, ToolDef struct at :47-57, dispatch at :355-356, error mapping at :461-481), `src/cli/batch/handlers/misc.rs` (gc compile-time wall :440-447, refresh :457-461, reconcile :583-604, wait_fresh :527-566), `src/cli/batch/json_args.rs` (core-required bail :304-307), `src/cli/mcp/mod.rs` (registry-parity guard :52-91), `src/cli/commands/io/notes.rs` (notes handlers :434-789), and `src/cli/definitions.rs` (mutates_index inventory :1180-1228).

The two decisive calls: (§2) **boundary = absence for the destructive set + opt-in flag for the recoverable set, never annotation-trust**; (§4) **`index` fire-and-forget in P2b reusing the existing `request_reconcile`/`wait_fresh` async primitives, Tasks deferred to P3/HTTP** because the stdio bridge's synchronous one-response-per-request loop makes mid-dispatch progress notifications a bridge rewrite for one tool.
