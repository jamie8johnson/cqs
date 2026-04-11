# OpenRCT2 Rust Port — Dual-Trail Experiment

## Thesis

Two parallel Rust translation efforts of OpenRCT2, same pinned upstream commit, same agent stack, same operator. Trail A uses cqs for code intelligence augmentation. Trail B uses the agent's built-in tools only. Pre-registered hypotheses, pre-committed metrics, result published in whichever direction the numbers point.

The full port is the container the experiment fits inside. The publishable dual-trail comparison is a function of the metrics recorded per module, not a function of the port ever shipping. If the port ships, the paper has a bigger story. If only Phase 1 ships, the paper still has its result.

## Pre-registered hypotheses

**H1 — Regression bugs per merged module.** Trail A produces ≥30% fewer regression bugs (validation failures in previously-passing modules after a new module merges) than Trail B.

**H2 — Tokens per validated module.** Trail A consumes ≥20% fewer agent tokens per validated module than Trail B, measured cache-adjusted.

**H3 — Wall-clock per validated module.** Trail A completes modules in ≥25% less wall-clock time than Trail B, measured from "module started" to "module passes validation."

**H0 — Null.** Both trails land within ±10% on all three metrics.

Metrics are tracked from Phase 1 module 1 onward. Both trails must complete each module before the next begins (lockstep comparison, not racing).

## Source

- Upstream: https://github.com/OpenRCT2/OpenRCT2
- Pin target: latest tagged release at Phase 0 start. Both trails use the identical pinned commit. Upstream is treated as immutable spec for the duration.
- Required runtime assets: original RCT2 or RCT Classic data files from a legal copy.

## Trail definitions

### Trail A — cqs-augmented

- Claude Code with cqs MCP server enabled
- Pre-edit hook runs `cqs impact` before every Edit, injecting caller/test/risk context
- Uses `cqs scout`, `cqs gather`, `cqs impact`, `cqs callers`, `cqs test-map`, `cqs context`
- Indexes upstream OpenRCT2 (C++) and the in-progress Rust port in parallel so cross-language call tracing works across the translation boundary

### Trail B — control

- Claude Code with default tools only (Read, Edit, Glob, Grep, Bash)
- No cqs, no MCP code intelligence server, no pre-edit hook
- Standard exploration: grep, file reads, manual context assembly
- Tool list is frozen at Phase 0 end and does not change during the experiment

### Identical between trails

- Same Anthropic model, frozen at Phase 0 start for the duration
- Same pinned upstream commit
- Same module ordering (defined by Phase 1)
- Same validation harness (produced by Phase 0c)
- Same RNG seeds in test fixtures
- Same operator, alternating sessions between trails to avoid time-of-day bias
- Same definition of "module done"

### Tracked per session (both trails)

Written to `metrics.tsv` after every session. A session without a metrics row does not count toward the experiment. This is a hard rule.

- Tokens consumed, input and output separate, cache-adjusted and raw
- Wall-clock duration
- Agent tool call count, broken down by tool
- Validation iteration count (port → harness fail → fix → retry cycles)
- Lines of Rust added and modified
- Regression bugs introduced (validation failures in already-passing modules after this session)

## Validation harness

Phase 0c produces the harness. Phase 1 cannot begin without one.

**Strategy 1 — state-dump diff.** Patch upstream OpenRCT2 to emit a deterministic JSON dump of simulation state every N ticks. Run the same save through upstream and the Rust port for N ticks. Diff the dumps. If they match for the state slice owned by the module under test, the module passes.

**Strategy 2 — behavioral equivalence.** If state-dump determinism cannot be achieved (float ordering, uninitialized memory, OS-specific scheduling, RNG re-seeding), validation falls back to invariant checking with numeric thresholds:

- Park rating tracks within ±5% over 10,000 ticks
- Guest count converges within ±2% by tick 10,000
- Total revenue tracks within ±5%
- No ride breakdowns occur in the port that didn't occur in upstream
- No guest type appears in the port that didn't exist in upstream

**Strategy 3 — user-acceptance.** If neither state-dump nor behavioral equivalence works, validation is "the port produces a playable scenario indistinguishable from upstream by an experienced player." This weakens the experiment's rigor significantly; document what made 1 and 2 unreachable and proceed.

The fallback chain is technical contingency, not a permission slip. Strategy 1 is the target. Strategy 2 is what gets used when 1 provably can't be made to work. Strategy 3 is what gets used when 2 also can't. Which strategy is in use is fixed at Phase 0c and does not change mid-experiment.

## Phase 0 — Recon and validation gate

Phase 0 deliverables are the technical gate to Phase 1. No Rust gets written until all five are complete.

### 0a — Prior art check

Search GitHub, crates.io, GitLab, Codeberg, and the web for existing Rust ports of OpenRCT2 or RCT2. Document:
- Any prior attempt at any completion level
- Active maintainers, last commit, license
- Whether the prior attempt is joinable or fork-able
- Whether its design decisions match this spec

A viable prior attempt at meaningful completion changes the experiment's external validity. Decide whether to join, fork, or proceed with the comparison noted as a limitation.

### 0b — Codebase recon

- Pin upstream to the latest tagged release, record commit hash
- Build from the pinned commit, verify it runs with legal assets
- Index the pinned C++ codebase with cqs (Trail A's first use of the tool)
- Generate `RECON.md`:
  - File and line counts per module
  - Module dependency graph (from `cqs gather`)
  - Inventory of every file-scope variable, extern, singleton, global state holder
  - Inventory of every RNG call site
  - Save file format reference
  - Game action system reference
  - Multiplayer sync hash extraction location (used by 0c)

### 0c — Validation harness verification

- Verify Strategy 1 first. Patch upstream to emit deterministic state dumps. Run two upstream instances from the same save for 10,000 ticks. Diff the dumps. They must be byte-identical. If yes, Strategy 1 is the validation strategy.
- If Strategy 1 fails, identify why (float ordering, uninitialized memory, OS scheduler) and document. Then verify Strategy 2 by running two upstream instances and confirming the invariant thresholds hold. If yes, Phase 1 begins with Strategy 2.
- If Strategy 2 also fails, use Strategy 3 and document which failure modes forced it.

### 0d — RNG verification

- Locate the upstream RNG, document algorithm and seeding
- Implement the RNG in Rust as the smallest possible standalone test
- Run both with identical seeds for 10,000 iterations
- Sequences match exactly (Strategy 1) or produce statistically equivalent distributions (Strategy 2)

### 0e — Global state architectural decision

OpenRCT2's heritage from x86 assembly means it's full of file-scope and global mutable state. Choose the Rust mapping based on the 0b inventory:

- **Single `World` struct passed to every function.** Most idiomatic, largest API surface change, easiest to reason about
- **Thread-locals matching C++ globals.** Least idiomatic, smallest API surface change, easiest to keep validation comparing apples-to-apples
- **`Arc<Mutex<…>>` per subsystem.** Middle ground, allows future parallelism, adds locking overhead

Both trails must use the same mapping. Document the decision and rationale in `RECON.md`. Revisit after module 1.4 if the chosen mapping isn't fitting upstream's actual structure; a mid-Phase-1 refactor is preferable to cementing a wrong choice.

### Phase 0 exit

`RECON.md` contains all five outputs, the validation strategy, the global state decision, the pinned commit hash, and an operator sign-off section. Phase 1 begins when `RECON.md` is complete and signed off.

## Phase 1 — Simulation core

Modules port in dependency order. Each module must pass validation for its state slice before the next begins. Both trails port the same module in parallel before advancing to the next.

| Module | Description |
|---|---|
| 1.1 | Foundational types: coordinates, fixed-point math, RNG, tile data, entity IDs |
| 1.2 | Map and terrain: grid, tile types, height data, footpath network |
| 1.3 | Minimum viable ride: one stationary flat ride (e.g. observation tower) |
| 1.4 | Minimum viable guest: one guest spawned, walking on a path |
| 1.5 | Vehicles and tracked rides: coasters, gentle rides, thrill rides, trains, station logic, track pieces |
| 1.6 | Full guest AI: needs system, decision making, pathfinding at scale |
| 1.7 | Staff: mechanics, handymen, security, entertainers |
| 1.8 | Park economics, ratings, loans, scenarios, win conditions |

Modules 1.5 and 1.6 are substantially larger than the others and will each require an internal breakdown before they start. The right decomposition depends on what 0b's recon reveals about upstream's actual structure, so the sub-module list is defined when the module begins, not now.

### Per-module workflow (both trails)

1. Read the module's upstream C++ scope. Trail A uses `cqs scout`, `cqs gather`, `cqs context`. Trail B uses Read, Glob, Grep.
2. Identify the state slice this module owns. Trail A reads from `RECON.md`. Trail B builds the slice list manually.
3. Build a fixture: load a known save in upstream, run N ticks, dump the state slice using the Phase 0c harness.
4. Translate the C++ to Rust function by function. Reproduce behavior, including bugs the pinned commit has. The pinned commit is the spec, not "what the code should do."
5. Run the same fixture through the Rust port and dump the same slice.
6. Diff the dumps (Strategy 1) or check invariant thresholds (Strategy 2).
7. If the diff is empty or invariants hold, the module passes. Otherwise iterate.
8. Record session metrics (tokens, wall clock, tool calls, iterations, lines, regressions) to `metrics.tsv`.
9. Once both trails complete the module, snapshot metrics and advance to the next module.

### Module exit criteria

A module is done when:

- Validation reports zero divergence (Strategy 1) or all invariants hold (Strategy 2)
- Both trails have ported the same module against the same pinned upstream
- Per-trail metrics are recorded in each trail's `metrics.tsv`
- The Rust crate compiles cleanly with no warnings
- All previously-passing modules still pass (regression check)
- A snapshot tag is created in both trail repos

## Phase 2 — Rendering, input, audio, UI

Phase 2 has effort parity with Phase 1. Rendering, asset loading, audio, and UI together are a body of work comparable in scope to the simulation core. Do not treat it as a thin layer on top.

Sub-components, each substantial on its own:

- **DAT format asset loader.** Port from upstream. The DAT format is a 25-year-old binary asset bundle that took the community years to reverse-engineer. Reuse upstream's loader logic.
- **Sprite renderer (wgpu).** Match upstream's software renderer pixel-for-pixel where possible; document any GPU-specific divergence.
- **Audio playback.** Original sound effects and music from RCT2 asset files.
- **Input handling (winit).** Mouse, keyboard, gamepad if upstream supports it.
- **In-game UI.** Menus, ride construction interface, park management dialogs, finance reports, scenario editor. Decision at Phase 2 start: pixel-exact reproduction (more implementation work) vs egui-based reimplementation (more design work, but diverges from "faithful port").

Phase 2 begins when Phase 1 is complete.

## Phase 3 — Library extraction

Refactor Phase 1's simulation core into a standalone crate `rct-sim`:

```rust
fn load(path: &Path) -> Result<Simulation>
impl Simulation {
    fn step(&mut self, n_ticks: u32);
    fn state(&self) -> StateSnapshot;
    fn inject(&mut self, action: GameAction) -> Result<()>;
    fn serialize(&self) -> Vec<u8>;
}
```

This is a refactor, not a rewrite. The validation harness must continue to pass with no behavioral change.

## Phase 4 — Downstream projects

Each is a separate project gated on Phase 3, specced when reached:

- **ML testbed.** Gym-style RL environment wrapping `rct-sim`. Park management is a constrained economic simulation with clear metrics.
- **WASM mod API.** User plugins in any WASM-targeting language for rides, scenarios, AI guests.
- **Replay and diff tooling.** Record inputs, replay deterministically, diff simulation states between versions.
- **Parallel simulation harness.** Run thousands of parks concurrently for optimization studies.

## Phase ordering and exit criteria

The experiment is gated on deliverables, not durations.

- **Phase 0.** All five 0a–0e deliverables complete, validation strategy verified on a real diff, `RECON.md` signed off. Gate to Phase 1.
- **Phase 1.** All eight modules pass validation in both trails, regression checks clean, metrics recorded.
- **Phase 2.** Playable scenario indistinguishable from upstream by an experienced player.
- **Phase 3.** `rct-sim` crate compiles cleanly, validation passes after the refactor.
- **Phase 4.** Per-project specs.

## Metrics are the deliverable

The dual-trail comparison produces per-module measurements: tokens consumed, agent invocations, validation iterations, regression bugs, lines of Rust generated. These measurements are the primary output of the experiment. Every session produces a `metrics.tsv` row or the session does not count. Every module produces a snapshot or the module is not done. Gaps in the metrics invalidate the corresponding comparison, which is the single thing the experiment exists to produce.

No predicted throughput numbers appear in this spec. Actual throughput comes from running the work and measuring it.

## Risks

1. **Validation strategy cannot be built.** Technical contingency: three-tier fallback (state-dump → behavioral → user-acceptance). If Strategy 3 also fails, the harness has a deeper problem than the fallback chain addresses and Phase 0c needs a different approach before Phase 1 can begin.

2. **RNG cannot be reproduced bit-exact.** Handled by Strategy 2 fallback. Documented in 0d.

3. **Global state mapping doesn't fit upstream.** The 0e decision is revisited after module 1.4, when enough Rust exists to test the assumption. A mid-Phase-1 refactor is the expected response if the original mapping is wrong.

4. **Upstream changes invalidate the harness.** Pin to a tagged release and treat as immutable. Do not chase upstream changes during Phase 1 or Phase 2.

5. **Single-operator confound.** The same human directs both trails. Operator priors and recent context affect both. Alternating between trails reduces but doesn't eliminate the bias. Independent replication would strengthen external validity and is noted as a limitation in the writeup.

6. **Trail B advantage from cqs design priors.** The operator built cqs and has strong priors about what code intelligence should provide. Trail B may benefit from those priors even without the tool. Trail B's tool list and workflow are frozen at Phase 0 end and not modified during the experiment.

7. **Trail A overhead from cqs maintenance.** Time spent debugging cqs counts against Trail A. Track cqs maintenance time in a separate column so it can be analyzed with and without it.

8. **Token measurement noise.** Anthropic API token counts include cache reads which behave differently across sessions. Report both raw and cache-adjusted numbers. The cache-adjusted number is the primary metric.

9. **Metrics tracking lapses.** Sessions without `metrics.tsv` rows do not count. This is the experiment's hard rule.

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

Execute Phase 0a.

```sh
mkdir -p ~/openrct2-rust-port && cd ~/openrct2-rust-port && \
  echo "# Phase 0a: Prior Art Check" > RECON-0a.md && \
  date >> RECON-0a.md
```

Web search for existing Rust ports of OpenRCT2 or RCT2. Document findings in `RECON-0a.md`. Phase 0b begins when 0a is complete.
