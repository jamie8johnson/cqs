# JSON Schema: Index Group Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `json!{}` calls in stats, stale, and gc commands with typed `#[derive(Serialize)]` output structs. Normalize field names per the schema spec.

**Architecture:** Define `*Output` structs in each command file. Replace `*_to_json()` functions with `build_*()` → struct. CLI serializes with `serde_json::to_string_pretty()`. Batch serializes with `serde_json::to_value()`. Text output reads struct fields.

**Tech Stack:** Rust, serde, serde_json (already dependencies)

---

### Task 1: StatsOutput struct

**Files:**
- Modify: `src/cli/commands/index/stats.rs`
- Modify: `src/cli/commands/index/mod.rs`

- [ ] **Step 1: Define StatsOutput and nested types**

Add to `src/cli/commands/index/stats.rs` above `stats_to_json`:

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct CallGraphStats {
    pub total_calls: usize,
    pub unique_callers: usize,
    pub unique_callees: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TypeGraphStats {
    pub total_edges: usize,
    pub unique_types: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct StatsOutput {
    pub total_chunks: usize,
    pub total_files: usize,
    pub notes: usize,
    pub call_graph: CallGraphStats,
    pub type_graph: TypeGraphStats,
    pub by_language: std::collections::HashMap<String, usize>,
    pub by_type: std::collections::HashMap<String, usize>,
    pub model: String,
    pub schema_version: u32,
    // CLI-specific (batch omits these via Option)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hnsw_vectors: Option<usize>,
    // Batch-specific
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<usize>,
}
```

- [ ] **Step 2: Add build_stats function**

Replace `stats_to_json` with:

```rust
pub(crate) fn build_stats(store: &cqs::Store) -> Result<StatsOutput> {
    let _span = tracing::info_span!("build_stats").entered();
    let stats = store.stats().context("Failed to read index statistics")?;
    let note_count = store.note_count()?;
    let fc_stats = store.function_call_stats()?;
    let te_stats = store.type_edge_stats()?;

    Ok(StatsOutput {
        total_chunks: stats.total_chunks,
        total_files: stats.total_files,
        notes: note_count,
        call_graph: CallGraphStats {
            total_calls: fc_stats.total_calls,
            unique_callers: fc_stats.unique_callers,
            unique_callees: fc_stats.unique_callees,
        },
        type_graph: TypeGraphStats {
            total_edges: te_stats.total_edges,
            unique_types: te_stats.unique_types,
        },
        by_language: stats.chunks_by_language.iter()
            .map(|(l, c)| (l.to_string(), *c))
            .collect(),
        by_type: stats.chunks_by_type.iter()
            .map(|(t, c)| (t.to_string(), *c))
            .collect(),
        model: stats.model_name.clone(),
        schema_version: stats.schema_version,
        stale_files: None,
        missing_files: None,
        created_at: None,
        hnsw_vectors: None,
        errors: None,
    })
}
```

- [ ] **Step 3: Update cmd_stats to use StatsOutput**

Refactor `cmd_stats` to call `build_stats` once and use the struct for both JSON and text output:

```rust
    let mut output = build_stats(store)?;
    output.stale_files = Some(stale_count);
    output.missing_files = Some(missing_count);
    output.created_at = Some(stats.created_at.clone());
    output.hnsw_vectors = hnsw_vectors;  // Option<usize> directly

    if json || ctx.cli.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_stats_text(&output);
    }
```

Extract the text output into a `print_stats_text(output: &StatsOutput)` function that reads fields from the struct. This eliminates the duplicate `store.stats()` call and ensures text and JSON use the same data.

- [ ] **Step 4: Update mod.rs exports**

In `src/cli/commands/index/mod.rs`, change `stats_to_json` export to `build_stats`:

```rust
pub(crate) use stats::{build_stats, cmd_stats, StatsOutput};
```

- [ ] **Step 5: Update batch handler**

In `src/cli/batch/handlers/info.rs`, replace:

```rust
pub(in crate::cli::batch) fn dispatch_stats(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stats").entered();
    let errors = ctx.error_count.load(std::sync::atomic::Ordering::Relaxed);
    let mut output = crate::cli::commands::build_stats(&ctx.store())?;
    output.errors = Some(errors);
    Ok(serde_json::to_value(&output)?)
}
```

- [ ] **Step 6: Build and test**

Run: `cargo build --features gpu-index`
Run: `cargo clippy --features gpu-index -- -D warnings`
Run: `cargo test --features gpu-index --bin cqs`

- [ ] **Step 7: Add serialization test**

Add to the bottom of `stats.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_output_serialization() {
        let output = StatsOutput {
            total_chunks: 100,
            total_files: 10,
            notes: 5,
            call_graph: CallGraphStats { total_calls: 50, unique_callers: 20, unique_callees: 30 },
            type_graph: TypeGraphStats { total_edges: 40, unique_types: 15 },
            by_language: [("rust".into(), 80), ("python".into(), 20)].into(),
            by_type: [("function".into(), 60), ("struct".into(), 40)].into(),
            model: "bge-large".into(),
            schema_version: 16,
            stale_files: None,
            missing_files: None,
            created_at: None,
            hnsw_vectors: None,
            errors: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        // Verify normalized field names
        assert!(json.get("total_chunks").is_some());
        assert!(json.get("call_graph").is_some());
        assert!(json.get("by_language").is_some());
        // Verify None fields are omitted
        assert!(json.get("stale_files").is_none());
        assert!(json.get("errors").is_none());
    }
}
```

- [ ] **Step 8: Commit**

```bash
git add src/cli/commands/index/stats.rs src/cli/commands/index/mod.rs src/cli/batch/handlers/info.rs
git commit -m "refactor: typed StatsOutput replaces stats_to_json"
```

---

### Task 2: StaleOutput struct

**Files:**
- Modify: `src/cli/commands/index/stale.rs`
- Modify: `src/cli/commands/index/mod.rs`
- Modify: `src/cli/batch/handlers/analysis.rs`

- [ ] **Step 1: Define StaleOutput and StaleEntry**

Add to `src/cli/commands/index/stale.rs` above `stale_to_json`:

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct StaleEntry {
    pub file: String,
    pub stored_mtime: i64,
    pub current_mtime: i64,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct StaleOutput {
    pub stale: Vec<StaleEntry>,
    pub missing: Vec<String>,
    pub stale_count: usize,
    pub missing_count: usize,
    pub total_indexed: usize,
}
```

- [ ] **Step 2: Replace stale_to_json with build_stale**

```rust
pub(crate) fn build_stale(report: &StaleReport) -> StaleOutput {
    let _span = tracing::info_span!("build_stale").entered();

    let stale = report.stale.iter().map(|f| StaleEntry {
        file: cqs::normalize_path(&f.file).to_string(),
        stored_mtime: f.stored_mtime,
        current_mtime: f.current_mtime,
    }).collect();

    let missing = report.missing.iter()
        .map(|f| cqs::normalize_path(f).to_string())
        .collect();

    StaleOutput {
        stale_count: report.stale.len(),
        missing_count: report.missing.len(),
        total_indexed: report.total_indexed,
        stale,
        missing,
    }
}
```

- [ ] **Step 3: Update cmd_stale JSON branch**

Replace:
```rust
    if json {
        let output = build_stale(&report);
        println!("{}", serde_json::to_string_pretty(&output)?);
    }
```

- [ ] **Step 4: Update mod.rs exports**

```rust
pub(crate) use stale::{build_stale, cmd_stale, StaleOutput};
```

- [ ] **Step 5: Update batch handler**

In `src/cli/batch/handlers/analysis.rs`, replace `stale_to_json` call:

```rust
    let output = crate::cli::commands::build_stale(&report);
    Ok(serde_json::to_value(&output)?)
```

- [ ] **Step 6: Add test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stale_output_empty() {
        let output = StaleOutput {
            stale: vec![],
            missing: vec![],
            stale_count: 0,
            missing_count: 0,
            total_indexed: 50,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["stale_count"], 0);
        assert!(json["stale"].as_array().unwrap().is_empty());
        assert!(json["missing"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_stale_output_serialization() {
        let output = StaleOutput {
            stale: vec![StaleEntry {
                file: "src/main.rs".into(),
                stored_mtime: 1000,
                current_mtime: 2000,
            }],
            missing: vec!["src/deleted.rs".into()],
            stale_count: 1,
            missing_count: 1,
            total_indexed: 50,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["stale_count"], 1);
        assert_eq!(json["stale"][0]["file"], "src/main.rs");
        assert!(json.get("missing").is_some());
    }
}
```

- [ ] **Step 7: Build, test, commit**

```bash
cargo fmt && cargo build --features gpu-index && cargo clippy --features gpu-index -- -D warnings && cargo test --features gpu-index --bin cqs
git add src/cli/commands/index/stale.rs src/cli/commands/index/mod.rs src/cli/batch/handlers/analysis.rs
git commit -m "refactor: typed StaleOutput replaces stale_to_json"
```

---

### Task 3: GcOutput struct

**Files:**
- Modify: `src/cli/commands/index/gc.rs`
- Modify: `src/cli/commands/index/mod.rs`
- Modify: `src/cli/batch/handlers/misc.rs`

- [ ] **Step 1: Define GcOutput**

Add to `src/cli/commands/index/gc.rs`:

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct GcOutput {
    pub stale_files: usize,
    pub missing_files: usize,
    pub pruned_chunks: usize,
    pub pruned_calls: usize,
    pub pruned_type_edges: usize,
    pub pruned_summaries: usize,
    pub hnsw_rebuilt: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hnsw_vectors: Option<usize>,
}
```

- [ ] **Step 2: Replace inline json! in cmd_gc**

Replace the JSON branch (around line 94):

```rust
    if json {
        let output = GcOutput {
            stale_files: stale_count,
            missing_files: missing_count,
            pruned_chunks,
            pruned_calls,
            pruned_type_edges,
            pruned_summaries,
            hnsw_rebuilt: pruned_chunks > 0,
            hnsw_vectors,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    }
```

- [ ] **Step 3: Update mod.rs exports**

```rust
pub(crate) use gc::{cmd_gc, GcOutput};
```

- [ ] **Step 4: Update batch handler**

In `src/cli/batch/handlers/misc.rs`, replace the `json!` block in `dispatch_gc` with:

```rust
    let output = crate::cli::commands::GcOutput {
        stale_files: stale_count,
        missing_files: missing_count,
        pruned_chunks: prune.pruned_chunks,
        pruned_calls: prune.pruned_calls,
        pruned_type_edges: prune.pruned_type_edges,
        pruned_summaries: prune.pruned_summaries,
        hnsw_rebuilt: false,  // batch GC doesn't rebuild HNSW
        hnsw_vectors: None,
    };
    Ok(serde_json::to_value(&output)?)
```

Note: batch GC does not rebuild HNSW (no index lock, no `build_hnsw_index` call). `hnsw_rebuilt` is always false and `hnsw_vectors` is omitted from batch output.

- [ ] **Step 5: Add test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gc_output_serialization() {
        let output = GcOutput {
            stale_files: 2,
            missing_files: 1,
            pruned_chunks: 15,
            pruned_calls: 30,
            pruned_type_edges: 5,
            pruned_summaries: 3,
            hnsw_rebuilt: true,
            hnsw_vectors: Some(500),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["pruned_chunks"], 15);
        assert_eq!(json["hnsw_rebuilt"], true);
        assert_eq!(json["hnsw_vectors"], 500);
    }

    #[test]
    fn test_gc_output_no_hnsw() {
        let output = GcOutput {
            stale_files: 0,
            missing_files: 0,
            pruned_chunks: 0,
            pruned_calls: 0,
            pruned_type_edges: 0,
            pruned_summaries: 0,
            hnsw_rebuilt: false,
            hnsw_vectors: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("hnsw_vectors").is_none());
    }
}
```

- [ ] **Step 6: Build, test, commit**

```bash
cargo fmt && cargo build --features gpu-index && cargo clippy --features gpu-index -- -D warnings && cargo test --features gpu-index --bin cqs
git add src/cli/commands/index/gc.rs src/cli/commands/index/mod.rs src/cli/batch/handlers/misc.rs
git commit -m "refactor: typed GcOutput replaces inline json!"
```

---

### Task 4: Final verification and PR

- [ ] **Step 1: Run full test suite**

```bash
cargo test --features gpu-index
```
Expected: all pass, zero failures

- [ ] **Step 2: Verify JSON output manually**

```bash
cqs stats --json | head -20
cqs stale --json
cqs gc --json
```

Check field names match the normalized convention.

- [ ] **Step 3: Squash commits and create PR**

```bash
git rebase -i HEAD~3  # squash into single commit
# Or leave as 3 commits — one per command
```
