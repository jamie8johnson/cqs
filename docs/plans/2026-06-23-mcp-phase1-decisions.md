# MCP Phase 1 — Locked Decisions (2026-06-23)

Decision ledger from the adversarial design audit (4 analysts + seam + red-team + judge).
Companion to the implementation brief `2026-06-23-mcp-phase1-implementation-brief.md` and the
design doc `2026-06-16-mcp-server-design.md`. **This file is the authoritative spec for the
Phase 1 lanes.**

## Locked decisions

| # | Decision | Choice | Confidence |
|---|----------|--------|-----------|
| D1 | Tool naming | **`cqs_`-prefixed, underscore, noun-first subcommands** (`cqs_search`, `cqs_callers`, `cqs_gather`, `cqs_notes_add`, `cqs_notes_list`). | user-decided |
| D2 | Context/statefulness | **One server per project root, cwd-inherited.** Bridge computes `overlay_root` client-side (CLI's client position) and forwards it; daemon validates at `view.rs:1067`. `slot`/`overlay` optional params. | high |
| D3 | Relay format | **(b) Extend the daemon socket to accept a JSON `arguments` object.** Deserializes directly into the Phase-0 core struct on both ends. | high |
| D4a | Bridge model | Separate `cqs mcp` process, auto-connect to daemon, **no in-process fallback in P1** (require running daemon, fail clean if absent), no auto-spawn. | high |
| D4b | P1 scope | **Read-only batch commands only**, AND **withhold `context`/`explain` from P1** until the doc/signature injection-scan gap is closed (RT-RELAY-1/2). Mutating (`index`/`notes_*`/`gc`) → P2 with destructive annotations + Tasks. | high |
| D4c | not_found semantics | **Shape-driven:** handler **error envelope** OR **empty-with-candidates** → `isError:true` (model retries with a candidate); **genuinely-empty / no-candidates / `dead` verdict** → empty-but-ok. | high |
| D4d | Protocol version | Advertise `2025-11-25`, accept client `>= 2025-06-18`. | high |
| D4e | Token budget | Expose `tokens` optional, no enforced default in P1. | high |

### D1 rationale (user-decided; the only judgment call)
**`cqs_`-prefixed wins because MCP is a community of interfaces.** The tool name must carry its
own identity into any room it's invoked from — the `mcp__cqs__<tool>` namespace is *Claude Code's*
presentation convention, not an MCP guarantee, so the **server-side name is the stable identity
layer**; a bare `search` borrows its identity from one host. The "we own the only consumer, so
bare is cleaner / a rename is cheap" argument is self-defeating: **adopting MCP at all is the act
of joining the community of interfaces** — if it were only-us-forever, the raw CLI already
suffices, so designing names on a no-external-consumer assumption contradicts the reason for the
feature. Matches the v0.10.0 precedent (`git show 291ec6b0^:src/mcp/tools/mod.rs`: `cqs_search`,
`cqs_callers`, …). Cost: a cosmetic doubled `cqs` *only* under Claude Code's namespacing —
accepted as cheap insurance on an irreversible name. Refinements to raw precedent: noun-first
subcommands (`cqs_notes_add`, not the old verb-first `cqs_add_note`) to match the CLI's
`cqs notes add` ordering; subcommands stay **separate tools** so each gets a focused `inputSchema`
+ honest `readOnly` annotation. Close the docs-vs-tool-name drift in the same PR (P1-bug policy).

### D3 rationale (the headline — overturns the synthesizer's pragmatic (a))
- The argv round-trip is **structurally lossy**: `search`'s positional `query` is ambiguous if it
  starts with `-`; **45 presence-only bool flags can't carry a JSON `false`** back through clap.
  Option (a) would reimplement clap's encoding in reverse for 27 structs; the parity guard does
  not cover that leg.
- The request frame is parsed **untyped** (`Value.get` on `command`/`args`) — adding `arguments`
  is **purely additive: no wire-version bump, no CLI back-compat break**.
- Phase-0 cores already `Deserialize`/`schemars`, so the **same struct deserializes on both ends**
  — this *eliminates* the seam-auditor's schema-from-core-vs-daemon-reparses-clap divergence.

## Blockers (bake into implementation)

1. **[CRITICAL — Lane 2] Handler errors ride under outer `status:"ok"`.** A core `Err` becomes
   `wrap_error` → `{data:null, error:{code,message}}` in the dispatch output, which the daemon
   wraps as `{"status":"ok","output":{data:null,error:{…}}}`. Outer `status:"error"` is reserved
   for transport/parse failures only. The bridge MUST run the equivalent of
   `classify_slim_envelope` (`daemon_translate.rs:441` — keys off data-present vs error-present) on
   `output`: error form → `CallToolResult{isError:true, _meta=error.code}`; data form →
   `structuredContent` + `isError:false`. Test: daemon returns a `status:"ok"`-wrapped error →
   assert `isError:true`. Sources: `src/cli/batch/view.rs:140-148`, `socket.rs`.
2. **[Lane 1 — RT-RELAY-3] The JSON-args relay MUST route through `dispatch_via_view` →
   `dispatch_*`, never `*_core` directly** — that preserves the overlay/path gates
   (`prepare_overlay_request_fields` / `set_validated_overlay_request`; `read.rs`
   canonicalize + starts_with). Add **wire-level** regression tests over the JSON-args frame:
   `--overlay-root /etc` rejected, `read` path-traversal blocked. (If P2 adds `slot` to `*Args`,
   validate slot-selection against the launched-project boundary — today inert, the batch parser
   drops `--slot`.)
3. **[Lane 2 — RT-RELAY-1/2] `context`/`explain` relay UNSCANNED doc + signature content.**
   `context.rs:390` hardcodes `injection_flags:[]` / `trust_level:user-code` while relaying `doc`
   verbatim; `ExplainOutput` (`explain.rs:201`) has no trust fields at all; every
   `detect_all_injection_patterns` caller scans `.content` only. → **withhold `context`/`explain`
   from the P1 tool set** (D4b) until a production lane scans the union (doc+signature+content) via
   the shared leaf serializer and adds trust fields to `ExplainOutput`. File as a tracking issue.
   Add an output-side trust-signal conformance test.
4. **[D4a fallback] `stdout_gag` is a per-call fd-1 redirect** (`hnsw/mod.rs:367,386`), NOT
   process-wide; an in-process fallback's embedder/ORT/tokenizer load is ungagged and its stdout
   IS the JSON-RPC channel. **P1 decision: bridge-only — drop the fallback** (require a running
   daemon; clean error if absent), keeping the P1 stdout-leak surface at zero. *Judge's
   alternative (P2): keep the fallback + install a process-lifetime fd-1 redirect before building
   `BatchView`, restore on exit; extend the §11 subprocess stdout test to force an embedder load.*
5. **[Lane 2 — `_meta` seam] The bridge must NOT blindly copy `output._meta` into
   `CallToolResult._meta`.** Envelope `_meta` carries only `stale_origins`/`worktree_overlay`/
   `worktree_stale`; `rank_signals`/`trust_level` are **per-result fields inside
   `structuredContent` (`output.data`)** — copying `_meta` alone silently omits them. Decide
   explicitly whether to hoist any per-result signal up to `CallToolResult._meta`.

## Resolved favorably (no work needed)
- `trust_level`/`rank_signals` ride **inside `output.data`** (per-result), so `structuredContent`
  preserves them automatically for the commands that DO populate them — the RT-RELAY carry-through
  is free *except* for the `context`/`explain` doc/signature gap (Blocker #3).
- D2 overlay forwarding is a known port, not new logic (Blocker handled in D2 row).

## Lanes
- **Lane 1 — daemon JSON-args path (D3-b foundation).** Extend `dispatch_via_view` to accept
  `{"command","arguments":{...}}` → deserialize into the core struct → same downstream dispatch +
  validation. Parity test: JSON-args output == argv output for a representative spread.
  Independently landable.
- **Lane 2 — the MCP bridge.** `cqs mcp` subcommand + `src/mcp/{bridge,lifecycle,tools}.rs`:
  JSON-RPC stdio loop (NDJSON framing reused from `291ec6b0^:src/mcp/transports/stdio.rs`),
  `initialize`/`initialized`, `tools/list` from `for_each_batch_cmd!` + Phase-0 schemas (bare
  names, read-only set), `tools/call` → daemon JSON-args path → CallToolResult with the
  error-mapping blocker. Plus stdio round-trip smoke + tools/list-matches-registry guard.
  Rides on Lane 1.

## Follow-ups (tracking issues)
- Command-core completion: **19 of 39 commands lack a JsonSchema core struct** — file one issue.
- P2: mutating tools + annotations, `outputSchema`, resources/`resource_links`, Tasks for `index`.
