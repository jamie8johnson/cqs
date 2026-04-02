# CommandContext Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate 32 copies of `open_project_store_readonly()` by introducing a shared `CommandContext` struct created once in dispatch and passed to all store-using handlers.

**Architecture:** Define `CommandContext` in `cli/mod.rs` holding `&Cli`, `Store`, `root`, `cqs_dir`. Construct it once in `dispatch::run_with()` for commands that need a store. Handlers change from `fn cmd_foo(... lots of params)` to `fn cmd_foo(ctx: &CommandContext, ... command-specific params)`. Embedder creation stays in handlers that need it (only 10 of 33 — not worth lazy-init complexity).

**Tech Stack:** Rust, existing cqs CLI architecture

---

### Task 1: Define CommandContext and constructor

**Files:**
- Modify: `src/cli/mod.rs`

- [ ] **Step 1: Add CommandContext struct**

Add after the existing `open_project_store_readonly` function:

```rust
/// Shared context for CLI commands that need an open store.
/// Created once in dispatch, passed to all store-using handlers.
pub(crate) struct CommandContext<'a> {
    pub cli: &'a Cli,
    pub store: cqs::Store,
    pub root: std::path::PathBuf,
    pub cqs_dir: std::path::PathBuf,
}

impl<'a> CommandContext<'a> {
    /// Open the project store in read-only mode and build a command context.
    pub fn open_readonly(cli: &'a Cli) -> anyhow::Result<Self> {
        let (store, root, cqs_dir) = open_project_store_readonly()?;
        Ok(Self { cli, store, root, cqs_dir })
    }

    /// Get the resolved model config from the CLI.
    pub fn model_config(&self) -> &cqs::embedder::ModelConfig {
        self.cli.model_config()
    }
}
```

- [ ] **Step 2: Build and verify**

Run: `cargo build --features gpu-index`
Expected: compiles with no errors (CommandContext is defined but not yet used)

- [ ] **Step 3: Commit**

```bash
git add src/cli/mod.rs
git commit -m "refactor: add CommandContext struct for shared CLI state"
```

### Task 2: Restructure dispatch — no-store commands early-return

**Files:**
- Modify: `src/cli/dispatch.rs`

- [ ] **Step 1: Split dispatch into no-store and store sections**

Restructure `run_with()` to handle no-store commands first (returning early), then construct `CommandContext` for all remaining commands. The no-store commands are: `Init`, `Doctor`, `Index`, `Watch`, `Batch`, `Chat`, `Completions`, `TrainData`, `ExportModel`, `Convert`, `Telemetry`.

After the early returns, add:
```rust
    // All remaining commands need an open store
    let ctx = crate::cli::CommandContext::open_readonly(&cli)?;
```

Then convert each remaining match arm from:
```rust
Some(Commands::Health { ref output }) => cmd_health(output.json),
```
to:
```rust
Some(Commands::Health { ref output }) => cmd_health(&ctx, output.json),
```

Do this for ALL store-using commands in a single pass. The handlers won't compile yet — that's expected. This commit captures the dispatch restructure.

- [ ] **Step 2: Commit (won't compile yet — that's intentional)**

```bash
git add src/cli/dispatch.rs
git commit -m "refactor: restructure dispatch to construct CommandContext once

WIP: handler signatures not yet updated"
```

### Task 3: Update store-only handlers (batch 1 — simple handlers)

**Files:**
- Modify: 16 files in `src/cli/commands/`: `affected.rs`, `blame.rs`, `brief.rs`, `dead.rs`, `deps.rs`, `gc.rs`, `graph.rs` (callers+callees), `health.rs`, `impact.rs`, `neighbors.rs`, `read.rs`, `reconstruct.rs`, `stale.rs`, `stats.rs`, `suggest.rs`, `test_map.rs`

Each handler follows the same mechanical pattern. Replace:
```rust
pub(crate) fn cmd_foo(arg1: T1, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_foo").entered();
    let (store, root, cqs_dir) = crate::cli::open_project_store_readonly()?;
    // ... use store, root, cqs_dir
```
With:
```rust
pub(crate) fn cmd_foo(ctx: &crate::cli::CommandContext, arg1: T1, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_foo").entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;  // only if used
    // ... rest unchanged
```

- [ ] **Step 1: Update all 16 handlers**

For handlers that take `&Cli` (like `cmd_dead`, `cmd_stats`, `cmd_stale`), replace `cli: &Cli` with `ctx: &CommandContext` and use `ctx.cli` where `cli` was used, `&ctx.store` where store was used.

For `cmd_gc` which uses read-write `open_project_store()`, keep it special-cased — it opens its own store inside and takes `ctx: &CommandContext` but ignores `ctx.store`. Actually, simpler: leave `gc` unchanged with no CommandContext. Handle it in dispatch as an early-return that opens its own read-write store.

- [ ] **Step 2: Build**

Run: `cargo build --features gpu-index`
Expected: may still fail if batch 2 handlers aren't updated yet

- [ ] **Step 3: Commit**

```bash
git add src/cli/commands/
git commit -m "refactor: update 16 store-only handlers to use CommandContext"
```

### Task 4: Update store+embedder handlers (batch 2)

**Files:**
- Modify: 10 files in `src/cli/commands/`: `context.rs`, `explain.rs`, `gather.rs`, `onboard.rs`, `plan.rs`, `query.rs`, `scout.rs`, `similar.rs`, `task.rs`, `where_cmd.rs`

Same pattern as Task 3, but these also create embedders. Replace:
```rust
pub(crate) fn cmd_foo(cli: &Cli, query: &str, json: bool) -> Result<()> {
    let (store, root, _) = crate::cli::open_project_store_readonly()?;
    let embedder = Embedder::new(cli.model_config().clone())?;
```
With:
```rust
pub(crate) fn cmd_foo(ctx: &CommandContext, query: &str, json: bool) -> Result<()> {
    let store = &ctx.store;
    let root = &ctx.root;
    let embedder = Embedder::new(ctx.model_config().clone())?;
```

Special case — `gather.rs` has `GatherContext` which holds `&Cli`. Change it to hold `&CommandContext` instead:
```rust
pub(crate) struct GatherContext<'a> {
    pub ctx: &'a CommandContext<'a>,  // was: pub cli: &'a Cli
    // ... other fields unchanged
}
```
And update `cmd_gather` to use `gctx.ctx.store` etc.

- [ ] **Step 1: Update all 10 handlers**

- [ ] **Step 2: Update dispatch match arms for these handlers**

Make sure dispatch passes `&ctx` instead of `&cli` for all updated handlers.

- [ ] **Step 3: Build and fix any remaining compile errors**

Run: `cargo build --features gpu-index`
Expected: clean compile

- [ ] **Step 4: Commit**

```bash
git add src/cli/commands/ src/cli/dispatch.rs
git commit -m "refactor: update 10 store+embedder handlers to use CommandContext"
```

### Task 5: Update remaining handlers that take &Cli

**Files:**
- Modify: `src/cli/commands/notes.rs`, `src/cli/commands/impact_diff.rs`, `src/cli/commands/review.rs`, `src/cli/commands/ci.rs`, `src/cli/commands/related.rs`, `src/cli/commands/trace.rs`, `src/cli/commands/train_pairs.rs`

These handlers open stores and/or take `&Cli`. Apply the same mechanical transformation.

Handlers that open stores for diff comparison (`diff.rs`, `drift.rs`, `reference.rs`) are **excluded** — they open stores on arbitrary paths and cannot use CommandContext.

- [ ] **Step 1: Update remaining handlers**

- [ ] **Step 2: Build**

Run: `cargo build --features gpu-index`
Expected: clean compile with zero warnings from our code

- [ ] **Step 3: Commit**

```bash
git add src/cli/commands/ src/cli/dispatch.rs
git commit -m "refactor: update remaining handlers to use CommandContext"
```

### Task 6: Update commands/mod.rs exports and clean up

**Files:**
- Modify: `src/cli/commands/mod.rs`
- Modify: `src/cli/mod.rs`

- [ ] **Step 1: Export CommandContext from cli module**

Ensure `CommandContext` is accessible as `crate::cli::CommandContext` (it should be already since it's in `cli/mod.rs`).

- [ ] **Step 2: Remove open_project_store_readonly if no longer called directly**

Check if any code outside dispatch still calls `open_project_store_readonly()`. If only `CommandContext::open_readonly` calls it, consider making it private. If `diff.rs`/`drift.rs`/`reference.rs` still use it, keep it `pub(crate)`.

Run: `grep -r "open_project_store_readonly" src/cli/`

- [ ] **Step 3: cargo fmt && cargo clippy**

Run: `cargo fmt && cargo clippy --features gpu-index -- -D warnings`
Expected: clean

- [ ] **Step 4: Run full test suite**

Run: `cargo test --features gpu-index 2>&1 | grep "^test result:" | awk '{p+=$4; f+=$6} END {printf "%d pass, %d fail\n", p, f}'`
Expected: ~1567 pass, 0 fail

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: clean up exports and verify full test suite"
```

### Task 7: Update batch handlers

**Files:**
- Modify: files in `src/cli/batch/handlers/` that call `open_project_store_readonly()`

Batch handlers in `src/cli/batch/` may also open stores independently. Check and update if applicable. Batch mode creates its own store context in `cmd_batch()`, so these may not need changes. Verify.

- [ ] **Step 1: Grep for store opens in batch**

Run: `grep -r "open_project_store" src/cli/batch/`

- [ ] **Step 2: Update if needed, or skip**

If batch handlers open their own stores, they likely need to stay that way (batch mode has its own lifecycle). Document the decision.

- [ ] **Step 3: Final full build + test**

Run: `cargo build --release --features gpu-index && cargo test --features gpu-index`
Expected: clean build, all tests pass

- [ ] **Step 4: Commit and PR**

```bash
git add -A
git commit -m "refactor: CommandContext — eliminate 32 redundant store opens"
```
