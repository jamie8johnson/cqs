# JSON Schema: Search Group Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `json!{}` calls in search commands (query, gather, scout, onboard, where, similar, related, neighbors) with typed `#[derive(Serialize)]` output structs. Normalize field names.

**Architecture:** Same pattern as other groups. Most search commands already use lib-level `*_to_json` functions or the shared token-packing utilities. The typed structs replace the remaining inline JSON.

**Tech Stack:** Rust, serde, serde_json

**Complexity notes:**
- `query.rs` (704 lines) has only 1 `json!` call (empty results). The main output path uses `SearchResult` from the lib which already has fields. Minimal change.
- `onboard.rs` has 7 `json!` calls but most are content injection (`entry["content"] = json!("")`). The base output comes from `cqs::onboard_to_json`. Wrap it.
- `gather.rs` has 3 `json!` calls — the output struct + token info injection.
- `scout.rs` has 2 `json!` calls — token info injection on existing lib output.

---

### Task 1: query.rs — TODO only

**Files:**
- Modify: `src/cli/commands/search/query.rs`

Query has 1 `json!` call (empty results) and the main output path uses `token_pack_results` which builds JSON from lib-level `SearchResult`. Both depend on lib types. Add TODO comment, no struct change.

- [ ] **Step 1: Add TODO comment at top of file**

```rust
// TODO: main search output uses lib-level SearchResult via token_pack_results.
// Full typed output requires adding #[derive(Serialize)] to SearchResult in lib crate.
// Empty-results json! is trivial — defer until lib migration.
```

- [ ] **Step 2: Commit**

---

### Task 2: GatherOutput

**Files:**
- Modify: `src/cli/commands/search/gather.rs`

- [ ] **Step 1: Define GatherChunkEntry and GatherOutput**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct GatherChunkEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub chunk_type: String,
    pub language: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct GatherOutput {
    pub query: String,
    pub chunks: Vec<GatherChunkEntry>,
    pub expansion_capped: bool,
    pub search_degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<usize>,
}
```

- [ ] **Step 2: Build GatherOutput from GatheredChunks, replace json! block**

```rust
pub(crate) fn build_gather_output(
    query: &str,
    result: &cqs::GatheredChunks,
    packed_chunks: &[&cqs::GatheredChunk],
    root: &Path,
    token_info: Option<(usize, usize)>,
) -> GatherOutput {
    let _span = tracing::info_span!("build_gather_output", query, chunks = packed_chunks.len()).entered();
    GatherOutput {
        query: query.to_string(),
        chunks: packed_chunks.iter().map(|gc| {
            let rel = cqs::rel_display(&gc.chunk.file, root);
            GatherChunkEntry {
                name: gc.chunk.name.clone(),
                file: rel.to_string(),
                line_start: gc.chunk.line_start,
                line_end: gc.chunk.line_end,
                chunk_type: gc.chunk.chunk_type.to_string(),
                language: gc.chunk.language.to_string(),
                score: gc.score,
                content: Some(gc.chunk.content.clone()),
            }
        }).collect(),
        expansion_capped: result.expansion_capped,
        search_degraded: result.search_degraded,
        token_count: token_info.map(|(used, _)| used),
        token_budget: token_info.map(|(_, budget)| budget),
    }
}
```

- [ ] **Step 3: Update cmd_gather JSON branch to use build_gather_output**

Note: Read the actual gather.rs first. The `packed_chunks` parameter type may differ — `pack_gather_chunks` from shared utilities returns a specific type. Adapt the builder signature to match.

- [ ] **Step 4: Update batch handler (dispatch_gather in misc.rs) to use the typed struct**

- [ ] **Step 5: Add tests**

```rust
#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn test_gather_output_with_token_info() {
        let output = GatherOutput {
            query: "test query".into(),
            chunks: vec![],
            expansion_capped: false,
            search_degraded: false,
            token_count: Some(100),
            token_budget: Some(500),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["token_count"], 100);
        assert_eq!(json["token_budget"], 500);
    }

    #[test]
    fn test_gather_output_without_token_info() {
        let output = GatherOutput {
            query: "q".into(),
            chunks: vec![],
            expansion_capped: false,
            search_degraded: false,
            token_count: None,
            token_budget: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("token_count").is_none());
        assert!(json.get("token_budget").is_none());
    }
}
```

- [ ] **Step 6: Commit**

---

### Task 3: ScoutOutput (scout.rs)

**Files:**
- Modify: `src/cli/commands/search/scout.rs`

Scout has 2 `json!` calls — both injecting token info into existing lib output (`cqs::scout_to_json`). The base output is lib-level.

- [ ] **Step 1: Wrap lib output — add token fields to the existing JSON**

Since `scout_to_json` returns `serde_json::Value` from the lib, we can't fully type it without lib changes. Instead, define a thin wrapper that adds token info:

```rust
// TODO: scout_to_json lives in lib — full typed output requires lib migration
fn inject_token_info(output: &mut serde_json::Value, token_info: Option<(usize, usize)>) {
    if let Some((used, budget)) = token_info {
        output["token_count"] = serde_json::json!(used);
        output["token_budget"] = serde_json::json!(budget);
    }
}
```

This is already what the code does — just extract the pattern into a named function for clarity. Add TODO comment.

- [ ] **Step 2: Commit**

---

### Task 4: OnboardOutput (onboard.rs)

**Files:**
- Modify: `src/cli/commands/search/onboard.rs`

Onboard has 7 `json!` calls. Most are content injection on lib output (`cqs::onboard_to_json`). Same situation as scout — lib-level output, CLI adds content.

- [ ] **Step 1: Extract content injection into named function, add TODO**

The `json!("")` calls are clearing content for chunks excluded from token budget. This is manipulation of lib output, not a schema issue. Add TODO for lib migration.

- [ ] **Step 2: Commit**

---

### Task 5: similar.rs — TODO only

**Files:**
- Modify: `src/cli/commands/search/similar.rs`

Similar has 1 `json!` call (empty results). Main path uses `SearchResult` from lib. Same situation as query — defer to lib migration.

- [ ] **Step 1: Add TODO comment, commit**

---

### Task 6: RelatedOutput

**Files:**
- Modify: `src/cli/commands/search/related.rs`

Related already has `related_items_to_json` and `related_result_to_json`. Normalize `"line"` → `"line_start"`.

- [ ] **Step 1: Define RelatedItemEntry and RelatedOutput**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct RelatedItemEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,  // was "line"
    pub overlap_count: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct RelatedOutput {
    pub target: String,
    pub shared_callers: Vec<RelatedItemEntry>,
    pub shared_callees: Vec<RelatedItemEntry>,
    pub shared_types: Vec<RelatedItemEntry>,
}
```

- [ ] **Step 2: Replace related_items_to_json and related_result_to_json with typed builders**

```rust
pub(crate) fn build_related_items(items: &[cqs::RelatedItem], root: &Path) -> Vec<RelatedItemEntry> {
    let _span = tracing::info_span!("build_related_items", count = items.len()).entered();
    items.iter().map(|r| RelatedItemEntry {
        name: r.name.clone(),
        file: cqs::rel_display(&r.file, root).to_string(),
        line_start: r.line,
        overlap_count: r.overlap_count,
    }).collect()
}

pub(crate) fn build_related_output(result: &cqs::RelatedResult, root: &Path) -> RelatedOutput {
    let _span = tracing::info_span!("build_related_output", target = %result.target).entered();
    RelatedOutput {
        target: result.target.clone(),
        shared_callers: build_related_items(&result.shared_callers, root),
        shared_callees: build_related_items(&result.shared_callees, root),
        shared_types: build_related_items(&result.shared_types, root),
    }
}
```

- [ ] **Step 3: Update CLI, batch, mod.rs exports**

- [ ] **Step 4: Add tests**

```rust
#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn test_related_item_normalized_fields() {
        let entry = RelatedItemEntry {
            name: "foo".into(), file: "src/lib.rs".into(),
            line_start: 42, overlap_count: 3,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none());  // normalized
    }

    #[test]
    fn test_related_output_empty() {
        let output = RelatedOutput {
            target: "bar".into(),
            shared_callers: vec![],
            shared_callees: vec![],
            shared_types: vec![],
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["target"], "bar");
        assert!(json["shared_callers"].as_array().unwrap().is_empty());
    }
}
```

Note: Check if `RelatedResult` actually has `shared_types`. If not, remove from the struct.

- [ ] **Step 5: Commit**

---

### Task 7: NeighborsOutput

**Files:**
- Modify: `src/cli/commands/search/neighbors.rs`

Read `neighbors.rs` first. The `entries` variable is built from HNSW nearest-neighbor results. Check what type it is — if it's `Vec<serde_json::Value>` already, define a `NeighborEntry` struct to replace it. If it's typed data, derive Serialize on that type.

- [ ] **Step 1: Read neighbors.rs and define NeighborEntry**

Expected fields (based on the json! shape): `target` (string), `neighbors` (array of entries with name/file/score/distance), `count`.

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct NeighborEntry {
    pub name: String,
    pub file: String,
    pub score: f32,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct NeighborsOutput {
    pub target: String,
    pub neighbors: Vec<NeighborEntry>,
    pub count: usize,
}
```

Adapt fields to match what the actual code builds. The agent must read the file.

- [ ] **Step 2: Replace json! block with typed struct**

- [ ] **Step 3: Add tests**

```rust
#[test]
fn test_neighbors_output_empty() {
    let output = NeighborsOutput { target: "foo".into(), neighbors: vec![], count: 0 };
    let json = serde_json::to_value(&output).unwrap();
    assert_eq!(json["count"], 0);
    assert!(json["neighbors"].as_array().unwrap().is_empty());
}
```

- [ ] **Step 4: Commit**

---

### Task 8: WhereOutput

**Files:**
- Modify: `src/cli/commands/search/where_cmd.rs`

Already has `where_to_json`. Replace with typed struct.

- [ ] **Step 1: Define WhereSuggestionEntry and WhereOutput**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct WhereSuggestionEntry {
    pub file: String,
    pub score: f32,
    pub insertion_line: u32,
    pub near_function: String,
    pub reason: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct WhereOutput {
    pub description: String,
    pub suggestions: Vec<WhereSuggestionEntry>,
}
```

- [ ] **Step 2: Replace where_to_json with build_where_output**

```rust
pub(crate) fn build_where_output(
    description: &str,
    suggestions: &[cqs::PlacementSuggestion],
    root: &Path,
) -> WhereOutput {
    let _span = tracing::info_span!("build_where_output", description, count = suggestions.len()).entered();
    WhereOutput {
        description: description.to_string(),
        suggestions: suggestions.iter().map(|s| {
            let rel = cqs::rel_display(&s.file, root);
            WhereSuggestionEntry {
                file: rel.to_string(),
                score: s.score,
                insertion_line: s.insertion_line,
                near_function: s.near_function.clone(),
                reason: s.reason.clone(),
            }
        }).collect(),
    }
}
```

- [ ] **Step 3: Update CLI, batch, mod.rs. Add tests. Commit.**

---

### Task 9: Final verification

- [ ] **Step 1: Build, clippy, full test suite**
- [ ] **Step 2: Grep for remaining `"line"` in search/ json output (should be `"line_start"` everywhere)**
- [ ] **Step 3: Grep for remaining `"function"` in search/ json output (should be `"name"`)**
- [ ] **Step 4: PR**
