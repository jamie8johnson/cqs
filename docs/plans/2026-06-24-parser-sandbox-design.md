# Design Doc: Per-Grammar Tree-Sitter External-Scanner Sandboxing (Outer Process-Isolation Layer)

Status: **PRIORITY-DEFERRED (fresh-eyes review, 2026-06-24).** This is the **outer** layer; the in-process depth-rail + parse-timeout (RT-PARSE, #2028) is the landed **inner** layer and is not changed by this work.

> **Defer verdict (do NOT build yet).** The landed inner rails already close both *demonstrated* DoS vectors (stack-overflow, hang). This subprocess layer closes only the *residual* — an arbitrary C-scanner SIGSEGV/UB/OOM — which has **no PoC**. Build it only when gated on an actual scanner-crash PoC or a real OOM/UB report (tracked in #2027). Corrections to the body below, found by the review:
> - **The "zero new deps / contained portable floor" claim (§2/§4) is FALSE.** The parse bundle needs `Serialize`+`Deserialize` on ~12 internal types (`Chunk`/`CallSite`/`FunctionCalls`/`CandidateSite`/`ChunkTypeRefs`/`TypeRef` derive neither; `Language`/`ChunkType`/`CallEdgeKind`/`TypeEdgeKind` derive `Serialize` only — and `CallEdgeKind` has a *tested* PascalCase/snake_case serde asymmetry a bare `Deserialize` would break) **plus** a `bincode` dep. Budget that work explicitly.
> - **§4 option-1 framing is wrong:** the daemon socket is **newline-delimited `serde_json::Value`, Unix-only** (`socket.rs` is `#![cfg(unix)]`), not a reusable length-framed binary codec.
> - **§3/§7 inconsistency:** Phase A must ship the **pre-forked worker pool** (subprocess-per-file's ~25–50 s bulk spawn overhead disqualifies it), so Phase A's real scope is pool + codec + reassembly + per-platform caps — not the "smallest slice" §7 implies.
> - **Crate fixes:** `rlimit` → 0.11.0 (MSRV 1.65); crate is `win32job` (not `win32job-rs`); `seccompiler` 0.5.0 is Linux-only Phase-C defense-in-depth, not the floor.
> The cheap *now*-work (wrap the two `pub` bare-parse sites with `parse_with_timeout`; explicit rayon parser-pool `stack_size`) is being done separately and sits **above** this in priority.

---

## 1. Threat & Goal

**Threat.** cqs parses *all* indexed content in-process **before any trust check runs** (SECURITY.md §34; map 3: "indexed files arrive via index/watch path BEFORE any trust check"). Tree-sitter 0.26 links hand-written **C external scanners** for ~35 of cqs's ~50 grammars (map 3 facts). A bug in any one of those scanners — triggered by *passive-hostile content* a user happened to index — runs in the daemon's address space:

- **Crash / UB**: a scanner segfaults or corrupts memory → `SIGABRT`/`SIGSEGV` takes down the whole daemon PID (map 3: "crashes/UB in scanner affects entire daemon (same PID)"; "A worker crash = daemon death").
- **OOM**: a scanner allocates unboundedly → daemon OOM-killed.
- **Hang**: tree-sitter error recovery pins the parser thread with no internal cancellation (map 3: "40 MB file took 74 s").
- **Stack overflow**: verified via `evil.rs` (~20 KB) — deeply-nested token trees overflow the 1 MiB rayon worker stack → `SIGABRT` (map 3).

**Goal.** Move parsing into a **child process** so that crash / OOM / CPU-spin / stack-overflow / UB in a C scanner **cannot abort the daemon**. The blast radius collapses to "one file skipped." This is **memory/CPU/crash isolation** — the 80% (map 3 constraint: "80% is memory/CPU/crash isolation (subprocess boundary)").

**Non-goal / explicit scope limit.** The actor is *content*, not a *malicious-syscall actor* — the parser is not a network service and has no intentional syscall surface (map 3 constraint). Therefore **seccomp is defense-in-depth, not the floor** (map 3: "seccomp is defense-in-depth only, not primary barrier"). A scanner that reads `/etc/passwd` is a far smaller problem than one that crashes the daemon; we fix the crash first, syscall-confinement later, Linux-only.

---

## 2. The Portable Floor vs Per-Platform Extras

cqs ships **three release targets equally** (Cargo.toml `rust-version = 1.96`, edition 2021; map 1): Linux x86_64, macOS ARM64, Windows x86_64. The design must **degrade gracefully per platform** — the floor is the *intersection* of primitives available on all three; everything else is opt-in defense-in-depth (map 1 constraint).

### The portable floor (all three targets)

| Primitive | Mechanism | Buys us |
|---|---|---|
| **Subprocess boundary** | `std::process::Command` (already used: `umap.rs`, `git` calls — map 1) | Crash isolation. A scanner `SIGABRT`/`SIGSEGV`/stack-overflow kills the *child*, not the daemon. This is the entire headline; it works identically on all three OSes with zero new crates. |
| **Wall-clock timeout** | Parent-side timer + kill (compose with the inner RT-PARSE timeout) | Hang isolation, even if the inner progress-callback budget is somehow defeated. |
| **Output bounds** | Bounded read of child stdout (the `umap.rs` `.take()` + `CQS_UMAP_MAX_STDOUT_BYTES` 1 GiB pattern — map 1) | Caps a child that emits unbounded output. |

Crash + hang + output-bound isolation is **fully portable with no new dependency**. That alone retires the daemon-death vectors (the headline risk). It is the must-ship floor.

### Per-platform memory/CPU hard caps (kernel-enforced, additive)

These strengthen the floor where the OS allows; absence degrades to "still crash-isolated, soft memory bound only."

| Target | Mechanism | Crate | Caveats (map 1) |
|---|---|---|---|
| **Linux x86_64** | `RLIMIT_AS` (address space), `RLIMIT_CPU` (CPU seconds), `RLIMIT_STACK` | `rlimit` 0.10.2 (1.8M+ downloads, maintained) | Fully supported. |
| **macOS ARM64** | `RLIMIT_AS`, `RLIMIT_CPU` only | `rlimit` / `libc` `setrlimit` | `RLIMIT_DATA`/`RLIMIT_STACK`/`RLIMIT_MEMLOCK` **fail with EINVAL** on Monterey ARM64; only `RLIMIT_AS` + `RLIMIT_CPU` are guaranteed. Use those two, nothing else. `sandbox_init` is deprecated — do **not** use it. |
| **Windows x86_64** | Job Object: `JOB_OBJECT_LIMIT_PROCESS_MEMORY` + `JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP` + kill-on-job-close | `win32job-rs` (dedicated crate) or raw `windows` bindings | `CreateJobObject` + `AssignProcessToJobObject` + `SetInformationJobObject`; **kill-on-close** means the daemon (parent) must own/manage the job handle so a dead daemon reaps its children. |

### Linux-only defense-in-depth (NOT the floor — Phase C)

| Primitive | Crate | Notes (map 1) |
|---|---|---|
| **seccomp-BPF** syscall filter | `seccompiler` (rust-vmm) or `extrasafe` | Linux 3.17+. `extrasafe` is **x86_64-focused** (Landlock v2, kernel 5.19+); `extrasafe-multiarch` exists but maintenance unverified. `seccompiler` (BPF compile, JSON filter, Firecracker origins) is the lower-level, multiarch-friendly choice. Deny-by-default on the parse worker's tiny real syscall set (read/write/mmap/exit). |

**Rejected cross-platform sandbox crates** (map 1, with the reason each fails our shape):
- **`birdcage`** (0.4.0, macOS/Linux): FS/network only, **no CPU/memory caps** — wrong axis for our threat (we need crash+OOM, not FS confinement), and ARM64-on-macOS untested.
- **`gaol`** (servo): multiprocess + **whitelist** model, explicitly **"not mature or battle-tested,"** stuck at 0.2.x. Not for a shipping tool.
- **Linux namespaces** (`unshare(2)` via `libc`): FS isolation only, **not portable** to macOS/Windows — can't be part of the floor; would be a Phase-C Linux extra at most, and FS isolation isn't our primary axis.

**Design rule:** the subprocess boundary + per-platform RLIMIT/Job-Object is the **shipping target**. seccomp is gravy on one platform.

---

## 3. The Architecture Decision (the Key Fork)

The fork is **subprocess-per-file** vs **pre-forked worker pool** vs **sandbox-only-risky-grammars**. The perf budget decides it.

### Perf budget (map 2)

- **Corpus**: ~16,900 chunks across ~700–800 files (`reconcile.rs:39` "~17k-chunk corpus").
- **Bulk** (`cqs index --force`): `file_batch_size = 5000`; staleness pre-filter runs **before** parsing (`parsing.rs:118-243`) — only survivors reach tree-sitter. Bulk reindex ~713 s, **dominated by embedding, not parsing** (`PROJECT_CONTINUITY.md:22`). Parsing is in-process rayon `par_iter` (`parsing.rs:301-499`).
- **Incremental** (watch): `reindex_files` processes N changed files in sequence (`reindex.rs:505-997`), ~10–100 ms/file parse. No fork/exec exists today (map 2: "SUBPROCESS/WORKER POOL: NOT USED").
- **Fork/exec cost** (map 2 open-question a/b): ~5–10 ms each. Subprocess-per-file across a 5000-file bulk batch = **25–50 s of pure spawn overhead** added to a pipeline whose parse stage is currently amortized OS-thread spawning (~µs total).

### The decision

> **Recommendation: Pre-forked worker pool (persistent parse workers, bounded set, work-stealing per-file dispatch).**
>
> **Runner-up: Sandbox-only-risky-grammars (the C-scanner subset), as a *worker-pool tuning knob*, not a separate architecture.**

**Why the worker pool wins.** Subprocess-per-file is disqualified by the bulk budget: 25–50 s of spawn overhead on a corpus where parsing is supposed to be free relative to embedding is a regression we can measure and won't accept (map 2 open-q b). A pool of, say, 8–32 persistent workers (sized off `max_concurrent_daemon_clients()`-style logic, default 16 — map 1) caps spawn cost to a **one-time** N-worker startup (map 3 constraint: "Startup cost acceptable (N worker spawns + N language imports), but per-file reuse expected (worker pool, not per-file spawn)"). Each worker loads grammars **once** at startup (mirrors today's `Language::try_grammar()` one-time load — map 3) and then services files over its lifetime. This matches the *existing* mental model: the bulk pipeline already runs a fixed rayon worker pool that lives for the run; we are replacing in-process rayon workers with out-of-process workers of the same cardinality.

**Why subprocess-per-file is the runner-up only for incremental.** On the watch path the volume is tiny (map 2 open-q c: 10–100 files/tick, 30 s cadence absorbs 1–3 spawns/sec trivially). Per-file spawn *would* be acceptable there. But maintaining **two** isolation architectures (pool for bulk, per-file for watch) doubles the surface for no benefit — the pool serves both. So per-file is rejected as a top-level architecture and kept only as the degenerate "pool size = on-demand" fallback if the pool proves hard to keep warm in the daemon.

**Why sandbox-only-risky-grammars is a knob, not an architecture.** It's a real optimization — pure-Rust-binding grammars with no C scanner don't need isolation, and ~15 of ~50 grammars are scanner-free (map 3). But "which grammars route to the pool vs parse in-process" is a **dispatch predicate on top of the pool**, not a different design. Ship the pool first with *blanket* isolation (simplest, safest); add the scanner-subset fast-path as a later optimization once the C-scanner audit (map 3 open-q 1) tells us the exact set. Premature splitting risks mis-classifying a scanner-bearing grammar as "safe."

### Constraints the pool must preserve (map 2)

- **Staleness pre-filter stays in the parent**, before dispatch (`parsing.rs:118-243`): unchanged files are never sent to a worker — the no-op-incremental optimization is preserved (map 2 open-q d).
- **File-aligned batching** (`parsing.rs:580-651`): a single file's chunks must not straddle two `ParsedBatch` messages (GPU-work-steal chunk-loss race). Workers return *per-file* result bundles; the parent reassembles file-aligned batches on the recv side, re-clustering out-of-order returns by file (map 2 open-q e). The unit of work is **one file → one result bundle**, which makes this natural.

---

## 4. The Serialization Boundary

### What crosses

The worker's output is the **full parse result the pipeline consumes per file** — i.e. `ParseAllWithChunkCallsResult` (`parser/mod.rs:70-76`):

```
(Vec<Chunk>, Vec<FunctionCalls>, Vec<ChunkTypeRefs>,
 Vec<(String, CallSite)>, Vec<CandidateSite>)
```

That is exactly what `parse_file_all_with_chunk_calls` returns and what `pipeline::parser_stage()` / `reindex.rs:522-527` feed downstream. The worker is a thin shell around that one call. **Parent → child** is small: the file path (the child reads the bytes itself, so the daemon never holds 50 MB in flight) plus the resolved limit set (`PARSER_MAX_FILE_SIZE` 50 MiB, `PARSER_MAX_CHUNK_BYTES` 100 KiB, the inner timeout/depth budgets from `limits.rs`). **Child → parent** is the result bundle, or a structured error.

### How it crosses

**Reuse serde + the existing framing; introduce no new wire format.** Two grounded options, recommendation first:

1. **Recommended — length-prefixed serde over the child's stdout pipe** (the daemon already does length-framed serde over a socket; map 1 daemon facts). The child writes `len: u32` + serialized bundle to stdout; the parent reads it with the **bounded-read discipline already in `umap.rs`** (`.take()` + a `CQS_PARSER_WORKER_MAX_STDOUT_BYTES`-style cap, defaulting like `CQS_UMAP_MAX_STDOUT_BYTES` 1 GiB — map 1). This is the smallest delta: it reuses an in-tree, already-hardened pattern (bounded subprocess I/O) and an already-present dependency (serde). The `Chunk`/`CallSite`/etc. types need a derived `Serialize`/`Deserialize` (verify which already have it; add where missing — these are internal types, no wire-stability concern per the "No External Users" principle).

2. **Considered — reuse the daemon socket framing module directly.** The daemon's request/response framing (`socket.rs`, `cfg(unix)`) is the natural codec, but it is **Unix-only** (map 1) and the worker boundary must be cross-platform (Windows daemon required — map 3 constraint). Lifting that framing to a platform-neutral helper is more refactor than a stdout length-prefix needs. Use it only if the worker protocol grows beyond a single request/response.

**Encoding:** `bincode` or length-prefixed JSON. Prefer `bincode` for the bulk volume (15k+ bundles/run); JSON only if a debug-introspection win justifies the size. Either way the bytes are length-framed and bounded.

**Do not** pass the raw tree-sitter `Tree` across the boundary — it is not serializable and is an in-process C structure. The boundary is the *extracted* result (`ParseAllWithChunkCallsResult`), which is plain Rust data. This is also why the worker must run the **full** extraction (chunks + calls + types + candidates) inside the child: the tree never escapes.

---

## 5. The Failure Contract

The contract must **compose with the existing skip path** and **not break the established `skip-with-warn` behavior** (map 3 constraint). Today: `ParserError::ParseFailed`/timeout → skip file with `warn`, continue indexing; no `parse_errors` table (map 3). The current `Err` arm in the pipeline is shared between tree-sitter `ParseFailed` and IO errors (`parsing.rs:1350, 1418, 1611, 1663`).

### Mapping worker outcomes → existing codepaths

| Worker outcome | Detected by | Maps to |
|---|---|---|
| **Clean parse** | length-framed bundle on stdout, exit 0 | normal result, feeds pipeline unchanged |
| **Worker crash** (SIGSEGV/SIGABRT/stack-overflow/UB) | non-zero/signal exit, truncated/absent bundle | `ParserError::ParseFailed("worker crashed: signal N")` → **existing skip-with-warn arm**. Daemon survives. |
| **Worker timeout** (hang) | parent kill after wall-clock budget | `ParserError::ParseFailed("worker timeout")` → same skip-with-warn arm |
| **Worker OOM** | RLIMIT_AS / Job-Object memory kill → child exits abnormally | `ParserError::ParseFailed("worker OOM")` → same arm |
| **Worker CPU cap** | RLIMIT_CPU / Job CPU hard-cap kill | `ParserError::ParseFailed("worker cpu-exceeded")` → same arm |

**No new schema table is required for the floor** (map 3 constraint: "No new persistent schema tables unless strongly motivated; failure contract should map to existing ParserError codepaths"). The crash/timeout/OOM cases all funnel into the **one existing `Err` arm** — the file is skipped, a `warn!` is logged with the structured reason, indexing continues. The `parse_errors`-table ask in the original issue is **optional and deferred**: if per-file error visibility proves needed, add it as a separate enrichment (a `parse_errors` ledger keyed by origin), but it is *not* a blocker for isolation. The map 3 baseline already notes failures are "non-fatal per-file"; we are extending that guarantee to crash/OOM, which previously were *fatal to the daemon*.

### Worker restart policy

- **Crash/OOM/timeout kills the worker process.** The pool **respawns** it (the daemon owns the pool; a dead worker is replaced, never left as a gap). This is the inverse of today's failure mode where "a worker crash = daemon death" (map 3) — now a worker crash = one respawn.
- **Backoff on repeated crashes:** if a single worker crashes K times within a window (the same poisoned grammar/file shape recurring), log at `warn`/`error` with the offending grammar and back off respawn to avoid a fork-storm. The file that killed it is already skipped, so the loop is not infinite.
- **Daemon shutdown / idle eviction** (the `serve_idle_minutes` 30-min pattern — `limits.rs:494`) tears down the pool cleanly. On Windows, **kill-on-job-close** guarantees a dead daemon reaps its workers (map 1); on Unix the parent must kill the worker group on its own exit (don't leak orphans).

---

## 6. Composition with the Landed In-Process Rails

This layer is **additive**; it does not remove the RT-PARSE inner rails (map 3 constraint: "Subprocess isolation adds another layer"). The two layers nest:

```
Daemon (parent)
└─ Parse worker (child process)  ◄── OUTER: process isolation (this doc)
   • RLIMIT_AS / RLIMIT_CPU (unix) | Job Object mem+cpu cap (win)  [hard, kernel-enforced]
   • [Phase C] seccomp-BPF deny-by-default (Linux only)
   └─ parse_file_all_with_chunk_calls   ◄── INNER: RT-PARSE (landed)
      • parse-timeout (wall-clock progress callback, ~5 s default)  [soft, in-process]
      • PARSER_MAX_WALK_DEPTH (~800 frames, guards Pass-2 walks)    [soft, in-process]
      • PARSER_MAX_FILE_SIZE (50 MiB), PARSER_MAX_CHUNK_BYTES (100 KiB)  [limits.rs]
```

**Why both, not either:**
- The **inner** rails are *fast and precise* — they catch the *common* pathologies (deep nesting, slow error-recovery) cheaply, in-process, without paying a single byte of IPC. They keep the worker from *needing* to be killed in the normal pathological case (a depth-overflow is caught by the depth rail before it overflows the stack).
- The **outer** layer is the *backstop for the cases the inner rails structurally cannot catch*: a C-scanner `SIGSEGV` on line 1, memory corruption, an OOM faster than the timeout fires, a stack overflow that the depth rail mis-estimated. The inner rail is a soft, cooperative budget; the outer boundary is a hard, non-cooperative kill. A scanner that ignores cancellation (map 3: "no internal cancellation") is exactly what the process boundary exists for.
- **The limits flow through unchanged.** The parent resolves the `limits.rs` env-driven caps (they're read per-call, cheap — `limits.rs:9-11`) and passes them to the worker, so a single source of truth still governs both layers. The worker runs the *same* `parse_file_all_with_chunk_calls` with the *same* inner rails active; it is just doing so behind a wall.

Net: inner rails reduce *how often* the outer kill fires; the outer boundary guarantees that *when* something slips the inner net, the daemon lives.

---

## 7. Phased Plan (each phase independently landable)

### Phase A — Portable subprocess crash + RLIMIT isolation (the 80%)
The must-ship floor. Independently landable and already retires the daemon-death headline.
- Add a `cqs __parse-worker` hidden subcommand: reads a file path + limit set, runs `parse_file_all_with_chunk_calls`, writes length-framed serde bundle to stdout (reuse the `umap.rs` bounded-I/O discipline).
- **Pre-forked worker pool** in the daemon/pipeline (§3): bounded set, work-stealing per-file dispatch, file-aligned reassembly on recv (§3 constraints). Staleness pre-filter stays in the parent.
- Per-platform hard caps applied to each worker: `rlimit` `RLIMIT_AS`+`RLIMIT_CPU` on Linux; `RLIMIT_AS`+`RLIMIT_CPU` only on macOS (skip the EINVAL-prone limits); Job Object mem+cpu-hard-cap + kill-on-close on Windows. **Degrade gracefully:** if a platform can't set a given limit, log once and proceed crash-isolated (the subprocess boundary alone is the floor).
- Failure contract (§5): crash/OOM/timeout → existing skip-with-warn `Err` arm; pool respawns; backoff on repeated crash.
- **Gate:** `evil.rs` and the 40 MB hang file index to "1 file skipped, daemon alive" instead of daemon death. Bulk reindex time regression within budget (target: pool overhead ≪ the 25–50 s per-file would cost; embedding still dominates).

### Phase B — Worker-pool perf tuning, only if perf demands
Land only if Phase A's bulk numbers warrant it.
- Tune pool size (env knob in the `limits.rs` style, default tracking `max_concurrent_daemon_clients`).
- **Sandbox-only-risky-grammars fast-path** (the §3 runner-up, as a dispatch knob): after the C-scanner audit (open-q below), route scanner-free grammars to an in-process fast path, scanner-bearing grammars to the pool. Blanket isolation remains the default and the fallback.
- Warm-pool persistence across watch ticks so incremental parses reuse warm workers.

### Phase C — seccomp Linux defense-in-depth
Lowest priority; not the floor.
- `seccompiler` deny-by-default BPF filter on the Linux worker, allowing only the parse worker's real syscall set (read/write/mmap/exit-group/futex). Applied after grammar load, before reading the first untrusted byte.
- Optional Landlock FS confinement via `extrasafe` **iff** the multiarch/maintenance question resolves; otherwise raw `seccompiler` only.
- **Gate:** a synthetic scanner that attempts an out-of-policy syscall is `SIGSYS`-killed; the kill maps to the same skip-with-warn arm. No effect on macOS/Windows (graceful no-op).

---

## 8. Open Questions (genuine forks to weigh before implementation)

1. **C-scanner inventory (gates Phase B).** Exactly how many of the ~35 scanner-bearing grammars are in the *default* feature set vs optional (map 3 open-q 1; Cargo.toml `[features]`)? And what fraction of a real cqs corpus actually exercises a C scanner vs pure-Rust paths (map 3 open-q 2)? This decides whether the §3 "sandbox-only-risky-grammars" fast-path is worth the dispatch complexity, or whether blanket isolation is simply fine because the pool is cheap.

2. **MSRV verification on the new crates (blocks Phase A/C crate selection).** Does `rlimit` 0.10.2 build on MSRV 1.96 (map 1 open-q; Cargo.toml not exposed)? Does `seccompiler` declare an MSRV (map 1 open-q)? If `rlimit` violates 1.96, fall back to raw `libc::setrlimit` behind `cfg(unix)` (we already use `libc` patterns — map 1). Decide before adding the dependency.

3. **Worker pool warmth vs the daemon lifecycle.** The daemon idle-evicts at 30 min (`limits.rs:494`). Do parse workers live for the daemon's whole life (warm, but holding N× grammar-loaded memory the whole time), or spin up per index/watch pass and tear down after (cold-start each pass, lower idle footprint)? The bulk path is one long pass (favors per-pass); the watch path is many tiny passes (favors persistent). This is the real perf fork — measure typical watch-tick file-change cardinality (map 2 open-q c) before deciding.

4. **`parse_errors` ledger — ship now or defer?** The issue asks for `parse_errors` alongside skip-with-warn. The floor needs only the existing `Err` arm (no schema change — §5). Is per-file structured error *visibility* worth a new table now (so operators can see *which* files/grammars are poisoning), or is the `warn!` log sufficient until demand appears (map 3 open-q 3)? Recommendation: defer; revisit if Phase A logs show recurring poison files.

5. **Windows Job Object + tokio interaction.** The watch daemon uses `spawn_blocking` / async threads (map 1 open-q). Is the Job-Object-assigned child managed cleanly under `spawn_blocking`, or only under plain `Command::spawn`? Needs a Windows smoke test before Phase A is declared cross-platform-done (map 1 open-q: "Job Objects + rlimit combo tested for spawn_blocking?").

6. **Trust-aware vs blanket isolation.** Should isolation be trust-aware — isolate only reference/untrusted content, parse first-party worktree code in-process for speed — or blanket-isolate everything (map 3 open-q 6)? Blanket is simpler and there is **no per-file trust label at parse time today** (map 3: "No per-file origin/trust label at parse time; all grammars treated equally"). Trust-aware would require plumbing a trust signal *into* the parse stage that doesn't currently exist. Recommendation: **blanket** for Phase A; trust-awareness is a separate, larger feature.

---

Grounding: `src/parser/mod.rs:70-76` (`ParseAllWithChunkCallsResult`), `src/parser/types.rs:14-27` (`ParserError`), `src/limits.rs` (env-rail pattern, `serve_idle_minutes`/`small_file_max_bytes`/`parser_max_file_size`), `src/cli/pipeline/parsing.rs:118-243` (staleness pre-filter), `:301-499` (rayon parse), `:580-651` (file-aligned batching), `:1350/1418/1611/1663` (shared `ParseFailed`/IO `Err` arm), `src/cli/watch/reindex.rs:505-997` (incremental path), `src/cli/watch/reconcile.rs:39` (~17k-chunk corpus), plus maps 1–3 for crate availability, perf budget, and threat model.