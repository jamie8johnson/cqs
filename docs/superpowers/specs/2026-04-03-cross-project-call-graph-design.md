# Cross-Project Call Graph Design

## Problem

Call graph commands (`callers`, `callees`, `impact`, `trace`, `test-map`) only query the local project's SQLite database. In multi-project setups (PLC plant with 20 controllers, monorepo with shared libraries, microservice architecture), function calls cross project boundaries. There's no way to trace from project A's function into project B's callers.

## Solution

A cross-project call graph layer that wraps the existing single-store call graph API. When `--cross-project` is passed, the command queries all configured reference stores plus the local store, merges results with source attribution.

## Architecture

### New module: `src/impact/cross_project.rs`

Orchestration layer — does not modify existing call graph code.

```rust
pub struct CrossProjectContext {
    local: Store,
    references: Vec<(String, Store)>,  // (name, store) pairs
}

impl CrossProjectContext {
    /// Load from .cqs.toml reference config
    pub fn from_config(local: Store, root: &Path) -> Result<Self>;

    /// Query callers across all stores
    pub fn get_callers_cross(&self, name: &str) -> Result<Vec<CrossProjectCaller>>;

    /// Query callees across all stores  
    pub fn get_callees_cross(&self, name: &str) -> Result<Vec<CrossProjectCallee>>;

    /// Transitive impact across all stores
    pub fn analyze_impact_cross(&self, name: &str, depth: usize) -> Result<CrossProjectImpact>;

    /// Shortest path that may cross project boundaries
    pub fn trace_cross(&self, source: &str, target: &str, max_depth: usize) -> Result<Option<Vec<CrossProjectHop>>>;
}
```

### Result types

Every result includes source attribution:

```rust
pub struct CrossProjectCaller {
    pub caller: CallerInfo,     // existing type from store/calls/
    pub source: String,         // reference name or "local"
}

pub struct CrossProjectHop {
    pub function: String,
    pub source: String,         // which project this hop is in
}
```

### CLI integration

Five commands get `--cross-project` flag:

- `cqs callers <fn> --cross-project`
- `cqs callees <fn> --cross-project`
- `cqs impact <fn> --cross-project`
- `cqs trace <source> <target> --cross-project`
- `cqs test-map <fn> --cross-project`

Flag is in `definitions.rs`. Dispatch checks the flag and calls the cross-project wrapper instead of the direct store function.

### Text output format

```
Callers of MyAOI:
  MainRoutine (controller_3)    line 42
  StartupSeq (controller_7)     line 18
  ManualMode (local)            line 95
```

JSON output adds `"source": "controller_3"` to each result object.

### Reference store loading

Reuses existing `ReferenceConfig` from `.cqs.toml`. Each reference already has a name and path. `CrossProjectContext::from_config` opens each reference store in read-only mode (same as `cqs ref` search does).

### Edge cases

- **Same function name in multiple projects:** Show all results, disambiguated by source name.
- **Cross-boundary tracing:** BFS alternates between stores when a callee in store A matches a function defined in store B. Existing depth limit prevents infinite traversal.
- **Missing references:** If a configured reference can't be opened, warn and continue with available stores.
- **No references configured:** `--cross-project` with no references configured = same as without the flag (local only), with a warning.

## What stays unchanged

- `store/calls/` — single-store call graph API untouched
- `cqs ref` search infrastructure — reused, not modified  
- Index schema — no migration needed
- Default behavior — without `--cross-project`, everything works as before

## Testing

- Unit tests in `cross_project.rs` with mock stores (two temp stores with known call graphs)
- Integration test: create two projects, add cross-project calls, verify `--cross-project` finds them
- Edge case tests: missing reference, same-name functions, circular calls across projects

## Files to create/modify

- **Create:** `src/impact/cross_project.rs`
- **Modify:** `src/impact/mod.rs` (add module)
- **Modify:** `src/cli/definitions.rs` (add `--cross-project` flag to 5 commands)
- **Modify:** `src/cli/commands/graph/callers.rs`, `impact.rs`, `trace.rs`, `test_map.rs`, `deps.rs`
- **Modify:** `src/cli/dispatch.rs` (pass flag through)
