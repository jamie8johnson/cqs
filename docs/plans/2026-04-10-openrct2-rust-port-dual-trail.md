# OpenRCT2 Rust Port — Dual-Trail Experiment

## Thesis

A faithful Rust port of OpenRCT2 (a 25-year-old C/C++ codebase reverse-engineered from x86 assembly) is the substrate. **The publishable result is the dual-trail experiment**: two parallel translation efforts on the same upstream commit using the same agent stack, with one trail using cqs for code intelligence augmentation and the other using only the agent's built-in tools. Pre-registered metrics. Result published regardless of direction.

The full port is the largest container the experiment fits inside. **The experiment is publishable after a single shared module completes in both trails**, not after the full port. If the full port never finishes, the experiment still produces an answer to the question that motivates building cqs in the first place: does code intelligence augmentation measurably improve agent-directed code translation?

## Why this question matters

cqs's design rests on the bet that structural code intelligence — typed chunks, call graphs, impact tracing, per-category retrieval — produces meaningfully better outcomes for agents working on code than unstructured search. That bet has been validated indirectly (telemetry, eval R@1, internal use) but never head-to-head against an unaugmented agent on a sustained, observable, multi-thousand-decision task. OpenRCT2 → Rust is the first task large enough, mechanical enough, and verifiable enough to make the comparison rigorous.

If the cqs trail wins on the pre-registered metrics, that validates the entire research lane and produces a paper that justifies further investment in code intelligence as a category. If the unaugmented trail wins or ties, that's also a finding worth publishing — cqs's actual leverage is narrower than claimed and the architecture should be re-examined.

## Pre-registered hypothesis

**H1 (cqs trail produces fewer regression bugs per ported function.)** Code intelligence makes the agent aware of caller/callee context before editing, so cross-module breakage is caught earlier. Predicted effect: ≥30% reduction in regression bugs in the cqs trail vs the control trail, measured per merged module.

**H2 (cqs trail consumes fewer agent tokens per validated module.)** Structural retrieval reduces the amount of file content the agent has to read into context to understand a function. Predicted effect: ≥20% token reduction per validated module.

**H3 (cqs trail completes modules in less wall-clock time.)** Faster context assembly + fewer regression cycles. Predicted effect: ≥25% wall-clock reduction (measured per-module from "module started" to "module passes validation").

**H0 (no significant difference.)** Both trails perform within ±10% on all three metrics. This would be a meaningful negative result — the structural advantages don't manifest at translation-task scale.

The metrics are tracked from Phase 1 module 1 onward. The experiment publishes after **at least three modules** have completed in both trails, regardless of whether the full Phase 1 ever finishes. Three modules is the minimum for a non-trivial sample.

## Source

- Upstream: https://github.com/OpenRCT2/OpenRCT2
- Pin target: latest tagged release at Phase 0 start. Both trails use the **identical pinned commit**. Upstream is treated as immutable spec for the duration of the experiment.
- Required runtime assets: original RCT2 or RCT Classic data files from a legal copy (GOG sells RCT2 for ~$10).

## Trail definitions

### Trail A (cqs-augmented)

- Agent stack: Claude Code with cqs MCP server enabled
- Pre-edit hook: cqs impact runs before every Edit, injecting caller/test/risk context
- Translation workflow uses: `cqs scout`, `cqs gather`, `cqs impact`, `cqs callers`, `cqs test-map`, `cqs context` for cross-language tracing across the C++/Rust boundary
- Indexes both upstream OpenRCT2 (C++) and the in-progress Rust port simultaneously
- Cross-language call tracing identifies untouched-but-affected Rust code when an upstream C++ function is re-translated

### Trail B (control)

- Agent stack: Claude Code with default tools only (Read, Edit, Glob, Grep, Bash)
- No cqs, no MCP code intelligence server, no pre-edit hook
- Translation workflow uses standard exploration: grep, file reads, manual context assembly
- Indexes nothing — context comes from agent's own search tools session-by-session

### Identical between trails

- Same Anthropic model (whatever is current at Phase 0 start, then frozen for the duration)
- Same upstream pinned commit
- Same module ordering
- Same validation harness (Phase 0c output)
- Same RNG seeds in test fixtures
- Same operator (single human directing both trails, alternating sessions)
- Same time-of-day distribution (agent state may vary across the day; alternating prevents lopsided sampling)

### Tracked per session

- Tokens consumed (input + output, separated)
- Wall-clock duration
- Number of agent tool calls, broken down by tool
- Number of validation iterations (cycles of port → harness fail → fix → retry)
- Lines of Rust produced (added) and modified
- Regression bugs introduced (count of validation failures in already-passing modules after a new module's port begins)

## Validation harness

The experiment cannot run without a working harness. Phase 0c is the gate.

**Strategy 1 (preferred): state-dump diff.** Patch upstream OpenRCT2 to emit a deterministic JSON dump of simulation state every N ticks. Run the same save through upstream and the Rust port for N ticks. Diff the dumps. If they match for the slice owned by the module under test, the module passes.

**Strategy 2 (fallback): behavioral equivalence.** If state-dump determinism cannot be achieved (because of float ordering, uninitialized memory, OS-specific behavior, or RNG re-seeding), validation switches to invariant-checking with numeric thresholds. Phase 1 metrics adjust accordingly:

- Park rating tracks within ±5% over 10,000 ticks
- Guest count converges within ±2% by tick 10,000
- Total revenue tracks within ±5%
- No ride breakdowns occur in the port that didn't occur in upstream
- No guest type appears in the port that didn't exist in upstream

The fallback is weaker but still rigorous enough to detect translation errors that produce qualitatively different behavior.

**Strategy 3 (hard fallback): user-acceptance only.** If neither state-dump nor behavioral equivalence can be made to work, validation degrades to "the port produces a playable scenario indistinguishable from upstream by an experienced player." This is the weakest bar and changes the experiment's scope substantially. Document and re-spec if reached.

The choice between Strategy 1 and Strategy 2 is decided in Phase 0c. **Do not begin Phase 1 without a working strategy.**

## Phase 0 — Reconnaissance and validation gate

Phase 0 deliverables are the go/no-go gate. Do not write any Rust until all five are complete.

### 0a — Prior art check

Search GitHub, crates.io, GitLab, Codeberg, and the web for existing Rust ports of OpenRCT2 or RCT2. Document findings:
- Any prior attempt at any completion level
- Active maintainers, last commit, license
- Whether the prior attempt is joinable or fork-able
- Whether the prior attempt's design decisions match this spec's

If a viable prior attempt exists at meaningful completion, **stop** and re-evaluate: join, fork, or proceed knowing the comparison's external validity is reduced.

### 0b — Codebase recon

- Pin upstream to the latest tagged release. Record the commit hash.
- Build OpenRCT2 from the pinned commit, verify it runs with legal asset files.
- Index the pinned C++ codebase with cqs (Trail A's first use of the tool).
- Generate `RECON.md` containing:
  - File counts and line counts per module
  - Module dependency graph (cqs gather output)
  - Inventory of every file-scope variable, extern, singleton, and global state holder
  - Inventory of every RNG call site
  - Save file format reference
  - Game action system reference
  - Multiplayer sync hash extraction location (used in 0c)

### 0c — Validation harness verification

- **First** verify Strategy 1 (state-dump diff). Patch upstream to emit deterministic state dumps. Run two upstream instances from the same save for 10,000 ticks. Diff the dumps. **They must be byte-identical.** If yes, Strategy 1 is the validation strategy and Phase 1 can begin.
- If Strategy 1 fails, identify why (float ordering? uninitialized memory? OS scheduler?). Document. Verify Strategy 2 (behavioral equivalence) by running two upstream instances and confirming the invariant thresholds hold. If yes, Phase 1 begins with Strategy 2 validation.
- If Strategy 2 fails, escalate to Strategy 3 and re-evaluate whether the project should continue at all.

### 0d — RNG verification

- Locate the upstream RNG. Document algorithm and seeding.
- Implement the RNG in Rust (the smallest possible standalone test).
- Run both with identical seeds for 10,000 iterations.
- Sequences must match exactly (Strategy 1) or produce statistically equivalent distributions (Strategy 2).

### 0e — Architectural decision: global state

OpenRCT2's heritage from x86 assembly means it's full of file-scope and global mutable state. Choose the Rust mapping based on the inventory from 0b:

- **Single `World` struct passed to every function** — most idiomatic, biggest API surface change, easiest to reason about
- **Thread-locals matching C++ globals** — least idiomatic, smallest API surface change, easiest to keep validation harness comparing apples-to-apples
- **`Arc<Mutex<…>>` per subsystem** — middle ground, allows future parallelism, adds locking overhead and complexity

Document the decision and rationale in `RECON.md`. Both trails must use the same mapping (otherwise they're not comparing the same task).

### Phase 0 deliverable

`RECON.md` containing all five outputs, the validation strategy choice, the global state decision, the pinned commit hash, and a sign-off section the operator initials before Phase 1 begins.

## Phase 1 — Simulation core port

Port modules in dependency order. Each module must pass the validation harness for its state slice before the next module begins.

### Module sizing — not equal

Modules 1.5 and 1.6 are dramatically larger than the others. Acknowledged up front so the spec doesn't read like eight equal milestones.

| Module | Description | Estimated effort share |
|---|---|---|
| 1.1 | Foundational types: coordinates, fixed-point math, RNG, tile data, entity IDs | ~5% |
| 1.2 | Map and terrain: grid, tile types, height data, footpath network | ~10% |
| 1.3 | Minimum viable ride: one stationary flat ride (e.g. observation tower) | ~5% |
| 1.4 | Minimum viable guest: one guest spawned, walking on a path | ~5% |
| **1.5** | **Vehicles and tracked rides** (coasters, gentle rides, thrill rides, trains, station logic, track pieces) | **~30%** |
| **1.6** | **Full guest AI** (needs system, decision making, pathfinding at scale) | **~30%** |
| 1.7 | Staff (mechanics, handymen, security, entertainers) | ~10% |
| 1.8 | Park economics, ratings, loans, scenarios, win conditions | ~5% |

Modules 1.5 and 1.6 together are roughly 60% of Phase 1's total work. Each will likely require its own internal milestone breakdown. The spec deliberately does not break them into sub-modules at this stage because the right decomposition depends on what 0b's recon reveals about upstream's actual structure.

### Per-module workflow (both trails)

1. Read the module's upstream C++ scope (Trail A uses cqs scout/gather/context; Trail B uses Read/Glob/Grep).
2. Identify the state slice this module owns (Trail A reads from RECON.md's inventory; Trail B builds the slice list manually).
3. Build a fixture: load a known save in upstream, run N ticks, dump the state slice using the Phase 0c harness.
4. Translate the C++ to Rust function-by-function. Reproduce behavior, including bugs that the pinned commit has. **The pinned commit is the spec, not "what the code should do."**
5. Run the same fixture through the Rust port. Dump the same slice.
6. Diff the two dumps (Strategy 1) or check invariant thresholds (Strategy 2).
7. If the diff is empty / invariants hold: module passes. Otherwise iterate.
8. Record session metrics (tokens, wall clock, tool calls, iteration count, lines, regressions) in `metrics.tsv`.
9. After both trails complete the module, snapshot metrics for the experiment writeup.

### Module exit criteria

A module is "done" when:
- Validation harness reports zero divergence (Strategy 1) or all invariants hold (Strategy 2)
- Both trails have ported the same module with the same pinned upstream
- Per-trail metrics are recorded
- The Rust crate compiles cleanly with no warnings
- All previously-passing modules still pass (regression check)
- A snapshot tag is created in both trail repos for reproducibility

## Phase 2 — Rendering, input, audio, UI

**Phase 2 has effort parity with Phase 1.** Do not scope it down to "bolt rendering on top of the simulation." Rendering, asset loading, audio, and UI together represent a body of work comparable to Phase 1 in scope.

Sub-components (each is its own substantial sub-project):

- **DAT format asset loader**: Port from upstream. The DAT format is a 25-year-old binary asset bundle that took the OpenRCT2 community years to fully reverse-engineer. The port reuses upstream's loader logic, not the original Sawyer asset format work.
- **Sprite renderer (wgpu)**: Match upstream's software renderer pixel-for-pixel where possible; document any GPU-specific divergence.
- **Audio playback**: Original sound effects and music from RCT2 asset files.
- **Input handling (winit)**: Mouse, keyboard, gamepad if upstream supports it.
- **In-game UI**: Menus, ride construction interface, park management dialogs, finance reports, scenario editor. Decision deferred to Phase 2 start: pixel-exact reproduction (much more work) vs egui-based reimplementation (more work to design, less work to implement, breaks the "faithful port" thesis).

Phase 2 begins only after Phase 1 is complete. The experiment publishes after Phase 1 if Phase 2 is too large a commitment to make.

## Phase 3 — Library extraction

Refactor Phase 1's simulation core into a standalone crate `rct-sim` exposing:

```rust
fn load(path: &Path) -> Result<Simulation>
impl Simulation {
    fn step(&mut self, n_ticks: u32);
    fn state(&self) -> StateSnapshot;
    fn inject(&mut self, action: GameAction) -> Result<()>;
    fn serialize(&self) -> Vec<u8>;
}
```

This is a refactor, not a rewrite. The validation harness must continue to pass after the refactor with no behavioral change.

## Phase 4 — Optional follow-ups

Each is a separate project gated on Phase 3 completion, with its own spec when reached:

- **ML testbed**: gym-style RL environment wrapping `rct-sim`. Park management is a constrained economic simulation with clear metrics — perfect for RL benchmarking.
- **WASM mod API**: user-written plugins in any WASM-targeting language for rides, scenarios, and AI guests.
- **Replay and diff tooling**: record inputs, replay deterministically, diff simulation states between versions. Useful for regression testing the rewrite, also useful for speedrunning and competitive play.
- **Parallel simulation harness**: run thousands of parks concurrently for optimization studies.

None of these are part of the core experiment. They exist to make the spec's "this is worth doing" argument bigger, and to attract contributors after publication.

## Phase ordering and exit criteria

The experiment is gated on deliverables, not durations. A phase is "done" when its exit criteria are satisfied, not when a time budget elapses.

- **Phase 0**: All five 0a–0e deliverables complete. Validation strategy verified (Strategy 1, 2, or 3 chosen and proven to work on a real diff). Gate to Phase 1.
- **Phase 1**: All eight modules pass validation in both trails. An interim writeup based on the first three modules is a valid milestone (see "First publishable milestone" below) but is not a substitute for completing Phase 1.
- **Phase 2**: Playable scenario indistinguishable from upstream by an experienced player.
- **Phase 3**: `rct-sim` crate compiles cleanly, validation harness still passes after the refactor.
- **Phase 4**: Per-project specs.

## Calibration as a deliverable

The dual-trail experiment produces real per-module measurements: tokens consumed, agent invocations, validation iterations, regression bugs, lines of Rust generated. These measurements are the primary outputs of the experiment, and they're also the only credible answer to "how long does agent-directed translation actually take."

Treat the first three modules as the calibration run. Whatever throughput numbers come out of those modules are the basis for any future estimate of the work remaining. Do not write estimates into this spec; record measured numbers in `metrics.tsv` and update the writeup when the data exists.

## First publishable milestone

The dual-trail comparison is meaningful as soon as three modules are complete in both trails. After Phase 0 + modules 1.1–1.3, the operator can publish an interim writeup containing:

- A working subset of the Rust port (foundational types, map, one stationary ride)
- A pre-registered head-to-head comparison of cqs-augmented vs unaugmented agent-directed translation
- Per-module numbers on tokens, agent invocations, validation iterations, regression bugs
- The first calibration-grade evidence on whether code intelligence augmentation measurably helps agents on a sustained, real-world translation task

This is a milestone, not a stopping point. The full Phase 1, Phase 2, and Phase 3 are still the deliverables. The interim writeup exists to start gathering external feedback on the methodology while the rest of the work continues.

## Risks

1. **Validation strategy fails** (0c). Mitigation: three-tier fallback chain (state-dump → behavioral → user-acceptance). If all three fail, the project is not feasible as specified and must be rescoped or abandoned.

2. **RNG cannot be reproduced bit-exact**. Same as above, mitigated by Strategy 2 fallback.

3. **Global state translation intractable**. Decided in 0e. Worst case: the chosen mapping doesn't fit the upstream architecture and the Rust port has to refactor partway through. Mitigation: revisit the 0e decision after module 1.4 completes, when enough Rust exists to test the assumption.

4. **Upstream changes invalidate the harness**. Mitigation: pin to a tagged release and treat as immutable. Do not chase upstream changes during Phase 1 or Phase 2.

5. **Single-operator confound**. The same human directs both trails. Operator priors, time of day, and recent context affect both trails. Acknowledged as a limitation; alternating between trails reduces but doesn't eliminate the bias. Independent replication would strengthen external validity.

6. **Trail B advantage from cqs design lessons**. The operator built cqs and has strong priors about what code intelligence should provide. Trail B might benefit from those priors even without the tool. Mitigation: pre-commit to Trail B's tool list before Phase 1 and do not modify it during the experiment.

7. **Trail A disadvantage from cqs maintenance overhead**. Time spent debugging cqs itself counts as Trail A overhead even though it's not translation work. Mitigation: track cqs maintenance time separately so it can be excluded or included in the analysis as appropriate.

8. **Token measurement noise**. Anthropic API token counts include cache reads, which behave differently across sessions. Mitigation: report both raw and cache-adjusted token counts. The cache-adjusted number is the better proxy for agent work done.

9. **Metric tracking lapses**. The dual-trail comparison requires `metrics.tsv` updates after every module in both trails. If tracking lapses, the experiment loses its publishable result even though the port keeps progressing. Mitigation: sessions without a corresponding metrics row do not count toward the experiment, period. This is a hard rule, not a soft preference.

## Repository layout

```
openrct2-rust-port/
├── SPEC.md                      # this file
├── RECON.md                     # Phase 0 output
├── trail-a-cqs/                 # cqs-augmented Rust port
│   ├── Cargo.toml
│   ├── src/
│   ├── tests/
│   └── metrics.tsv              # per-session metrics for Trail A
├── trail-b-control/             # control Rust port
│   ├── Cargo.toml
│   ├── src/
│   ├── tests/
│   └── metrics.tsv              # per-session metrics for Trail B
├── upstream/                    # pinned OpenRCT2 fork (immutable spec)
├── harness/                     # validation harness (state-dump tool, diff scripts)
└── analysis/                    # comparison scripts, plots, writeup
```

## First concrete step

Execute Phase 0a. One command:

```sh
mkdir -p ~/openrct2-rust-port && cd ~/openrct2-rust-port && \
  echo "# Phase 0a: Prior Art Check" > RECON-0a.md && \
  date >> RECON-0a.md
```

Then web search for existing Rust ports of OpenRCT2 or RCT2. Document findings in `RECON-0a.md`. **Do not proceed to 0b until 0a is complete and the findings have been reviewed.**
