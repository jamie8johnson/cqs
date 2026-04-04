# JSON Schema: Graph Group Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `json!{}` calls in graph commands (callers, callees, deps, explain, test_map, trace) with typed `#[derive(Serialize)]` output structs. Normalize field names. Impact/impact_diff use lib-level `impact_to_json` — wrap rather than modify.

**Architecture:** Same pattern as index group. Define `*Output` structs in command files. Replace `*_to_json()` with `build_*()` → struct. Normalize `"line"` → `"line_start"`, `"function"` → `"name"`.

**Tech Stack:** Rust, serde, serde_json

---

### Task 1: CallerOutput / CalleeOutput

**Files:**
- Modify: `src/cli/commands/graph/callers.rs`
- Modify: `src/cli/commands/graph/mod.rs`
- Modify: `src/cli/batch/handlers/graph.rs`

- [ ] **Step 1: Define output types**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct CallerEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,  // was "line"
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleeEntry {
    pub name: String,
    pub line_start: u32,  // was "line"
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleesOutput {
    pub name: String,  // was "function"
    pub calls: Vec<CalleeEntry>,
    pub count: usize,
}
```

- [ ] **Step 2: Replace callers_to_json with build_callers**

```rust
pub(crate) fn build_callers(callers: &[CallerInfo]) -> Vec<CallerEntry> {
    let _span = tracing::info_span!("build_callers", count = callers.len()).entered();
    callers.iter().map(|c| CallerEntry {
        name: c.name.clone(),
        file: normalize_path(&c.file).to_string(),
        line_start: c.line,
    }).collect()
}
```

- [ ] **Step 3: Replace callees_to_json with build_callees**

```rust
pub(crate) fn build_callees(name: &str, callees: &[(String, u32)]) -> CalleesOutput {
    let _span = tracing::info_span!("build_callees", name, count = callees.len()).entered();
    CalleesOutput {
        name: name.to_string(),
        calls: callees.iter().map(|(n, line)| CalleeEntry {
            name: n.clone(),
            line_start: *line,
        }).collect(),
        count: callees.len(),
    }
}
```

- [ ] **Step 4: Update cmd_callers and cmd_callees JSON branches**

```rust
// callers
if json {
    let output = build_callers(&callers);
    println!("{}", serde_json::to_string_pretty(&output)?);
}

// callees
if json {
    let output = build_callees(name, &callees);
    println!("{}", serde_json::to_string_pretty(&output)?);
}
```

- [ ] **Step 5: Update batch handlers**

In `src/cli/batch/handlers/graph.rs`, replace `callers_to_json` and `callees_to_json` calls with `build_callers` / `build_callees` + `serde_json::to_value()`.

- [ ] **Step 6: Update mod.rs exports**

Replace `callers_to_json`/`callees_to_json` with `build_callers`/`build_callees` + output types.

- [ ] **Step 7: Add tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_caller_entry_field_names() {
        let entry = CallerEntry { name: "foo".into(), file: "src/lib.rs".into(), line_start: 42 };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none());  // normalized away
    }

    #[test]
    fn test_build_callers_empty() {
        let output = build_callers(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn test_build_callees_empty() {
        let output = build_callees("foo", &[]);
        assert_eq!(output.count, 0);
        assert!(output.calls.is_empty());
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "foo");
    }

    #[test]
    fn test_callees_output_field_names() {
        let output = build_callees("bar", &[("baz".into(), 10)]);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "bar");  // was "function"
        assert!(json.get("function").is_none());
        assert_eq!(json["calls"][0]["line_start"], 10);
    }
}
```

- [ ] **Step 8: Build, clippy, test, commit**

---

### Task 2: DepsOutput

**Files:**
- Modify: `src/cli/commands/graph/deps.rs`
- Modify: `src/cli/commands/graph/mod.rs`

- [ ] **Step 1: Define output types**

Reverse deps returns type usage (type_name + edge_kind). Forward deps returns chunk summaries (name + file + line_start + chunk_type). Different shapes.

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct TypeUsageEntry {
    pub type_name: String,
    pub edge_kind: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DepsReverseOutput {
    pub name: String,  // was "function"
    pub types: Vec<TypeUsageEntry>,
    pub count: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DepsUserEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub chunk_type: String,
}
```

Note: forward deps already uses correct field names (`name`, `file`, `line_start`, `chunk_type`). Only reverse needs `"function"` → `"name"`.

- [ ] **Step 2: Replace deps_reverse_to_json with build_deps_reverse**

```rust
pub(crate) fn build_deps_reverse(name: &str, types: &[TypeUsage]) -> DepsReverseOutput {
    let _span = tracing::info_span!("build_deps_reverse", name).entered();
    DepsReverseOutput {
        name: name.to_string(),
        types: types.iter().map(|t| TypeUsageEntry {
            type_name: t.type_name.clone(),
            edge_kind: t.edge_kind.clone(),
        }).collect(),
        count: types.len(),
    }
}
```

- [ ] **Step 3: Replace deps_forward_to_json with build_deps_forward**

```rust
pub(crate) fn build_deps_forward(users: &[ChunkSummary], root: &Path) -> Vec<DepsUserEntry> {
    let _span = tracing::info_span!("build_deps_forward", count = users.len()).entered();
    users.iter().map(|c| DepsUserEntry {
        name: c.name.clone(),
        file: cqs::rel_display(&c.file, root).to_string(),
        line_start: c.line_start,
        chunk_type: c.chunk_type.to_string(),
    }).collect()
}
```

- [ ] **Step 4: Update CLI, batch, mod.rs exports**

- [ ] **Step 5: Add tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deps_reverse_field_names() {
        let output = DepsReverseOutput {
            name: "my_func".into(),
            types: vec![TypeUsageEntry { type_name: "Config".into(), edge_kind: "Param".into() }],
            count: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "my_func");  // was "function"
        assert!(json.get("function").is_none());
    }

    #[test]
    fn test_deps_reverse_empty() {
        let output = DepsReverseOutput { name: "foo".into(), types: vec![], count: 0 };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["count"], 0);
        assert!(json["types"].as_array().unwrap().is_empty());
    }
}
```

- [ ] **Step 6: Build, clippy, test, commit**

---

### Task 3: ExplainOutput

**Files:**
- Modify: `src/cli/commands/graph/explain.rs`
- Modify: `src/cli/commands/graph/mod.rs`

The `explain_to_json` function is the largest (9 `json!` calls). It constructs a rich object with chunk info, callers, callees, similar functions, type deps.

- [ ] **Step 1: Define output types**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct ExplainOutput {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub chunk_type: String,
    pub language: String,
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub callers: Vec<CallerEntry>,  // reuse from Task 1
    pub callees: Vec<CalleeEntry>,  // reuse from Task 1
    pub caller_count: usize,
    pub callee_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub similar: Vec<SimilarEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub type_deps: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct SimilarEntry {
    pub name: String,
    pub file: String,
    pub score: f32,
}
```

- [ ] **Step 2: Import CallerEntry and CalleeEntry from callers.rs**

`ExplainOutput` reuses types from Task 1. Import via `super::callers::{CallerEntry, CalleeEntry}` or re-export through `graph/mod.rs`.

- [ ] **Step 3: Replace explain_to_json with build_explain_output**

```rust
pub(crate) fn build_explain_output(data: &ExplainData, root: &Path) -> ExplainOutput {
    let _span = tracing::info_span!("build_explain_output", name = %data.chunk.name).entered();
    let chunk = &data.chunk;
    ExplainOutput {
        name: chunk.name.clone(),
        file: cqs::rel_display(&chunk.file, root).to_string(),
        line_start: chunk.line_start,
        line_end: chunk.line_end,
        chunk_type: chunk.chunk_type.to_string(),
        language: chunk.language.to_string(),
        signature: chunk.signature.clone(),
        content: data.content.clone(),
        callers: data.callers.iter().map(|c| CallerEntry {
            name: c.name.clone(),
            file: normalize_path(&c.file).to_string(),
            line_start: c.line,
        }).collect(),
        callees: data.callees.iter().map(|(n, line)| CalleeEntry {
            name: n.clone(),
            line_start: *line,
        }).collect(),
        caller_count: data.caller_count,
        callee_count: data.callee_count,
        similar: data.similar.iter().map(|(idx, score)| SimilarEntry {
            name: data.similar_chunks.get(*idx).map(|c| c.chunk.name.clone()).unwrap_or_default(),
            file: data.similar_chunks.get(*idx).map(|c| cqs::rel_display(&c.chunk.file, root).to_string()).unwrap_or_default(),
            score: *score,
        }).collect(),
        type_deps: data.type_deps.clone(),
    }
}
```

Note: The exact field access depends on `ExplainData`'s structure. Read the file — `similar` may be stored differently (as `Vec<(usize, f32)>` with a separate chunks vec). Adapt the mapping to match.

- [ ] **Step 4: Update CLI, batch, add tests**

```rust
#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn test_explain_output_field_names() {
        let output = ExplainOutput {
            name: "foo".into(), file: "src/lib.rs".into(),
            line_start: 10, line_end: 20,
            chunk_type: "function".into(), language: "rust".into(),
            signature: "fn foo()".into(), content: None,
            callers: vec![], callees: vec![],
            caller_count: 0, callee_count: 0,
            similar: vec![], type_deps: vec![],
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none());
        // Empty vecs should be omitted
        assert!(json.get("similar").is_none());
        assert!(json.get("type_deps").is_none());
        // None content should be omitted
        assert!(json.get("content").is_none());
    }
}
```

- [ ] **Step 5: Build, clippy, test, commit**

---

### Task 4: TestMapOutput

**Files:**
- Modify: `src/cli/commands/graph/test_map.rs`
- Modify: `src/cli/commands/graph/mod.rs`

- [ ] **Step 1: Define output types**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct TestMapEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,  // was "line"
    pub call_depth: usize,
    pub call_chain: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TestMapOutput {
    pub name: String,  // was "function"
    pub tests: Vec<TestMapEntry>,
    pub count: usize,
}
```

- [ ] **Step 2: Replace test_map_to_json with build_test_map_output**

```rust
pub(crate) fn build_test_map_output(target_name: &str, matches: &[TestMatch]) -> TestMapOutput {
    let _span = tracing::info_span!("build_test_map_output", target_name, count = matches.len()).entered();
    TestMapOutput {
        name: target_name.to_string(),
        tests: matches.iter().map(|m| TestMapEntry {
            name: m.name.clone(),
            file: m.file.clone(),
            line_start: m.line,
            call_depth: m.depth,
            call_chain: m.chain.clone(),
        }).collect(),
        count: matches.len(),
    }
}
```

- [ ] **Step 3: Update CLI and batch handlers to use build_test_map_output + serde_json::to_value**

- [ ] **Step 4: Add tests**

```rust
#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn test_test_map_output_field_names() {
        let output = TestMapOutput {
            name: "my_func".into(),
            tests: vec![TestMapEntry {
                name: "test_it".into(), file: "tests/foo.rs".into(),
                line_start: 10, call_depth: 1, call_chain: vec!["my_func".into()],
            }],
            count: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "my_func");  // was "function"
        assert!(json.get("function").is_none());
        assert_eq!(json["tests"][0]["line_start"], 10);  // was "line"
    }

    #[test]
    fn test_test_map_output_empty() {
        let output = build_test_map_output("no_tests", &[]);
        assert_eq!(output.count, 0);
    }
}
```

- [ ] **Step 5: Build, clippy, test, commit**

---

### Task 5: TraceOutput

**Files:**
- Modify: `src/cli/commands/graph/trace.rs`
- Modify: `src/cli/commands/graph/mod.rs`

- [ ] **Step 1: Define output types**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct TraceHop {
    pub name: String,
    pub file: String,
    pub line_start: u32,  // was "line"
    pub signature: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TraceOutput {
    pub source: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<TraceHop>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    pub found: bool,
}
```

- [ ] **Step 2: Replace trace_to_json with build_trace_output**

`trace_to_json` currently takes `&Store` to do batch name lookup. The new function should take the same params and return `TraceOutput`:

```rust
pub(crate) fn build_trace_output(
    store: &cqs::Store,
    source_name: &str,
    target_name: &str,
    path: Option<&[String]>,
    root: &std::path::Path,
    _max_depth: usize,
) -> Result<TraceOutput> {
    let _span = tracing::info_span!("build_trace_output", source_name, target_name).entered();

    match path {
        Some(names) => {
            let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            let batch_results = store.search_by_names_batch(&name_refs, 1)?;

            let hops: Vec<TraceHop> = names.iter().map(|name| {
                match batch_results.get(name.as_str()).and_then(|v| v.first()) {
                    Some(r) => TraceHop {
                        name: name.clone(),
                        file: cqs::rel_display(&r.chunk.file, root).to_string(),
                        line_start: r.chunk.line_start,
                        signature: r.chunk.signature.clone(),
                    },
                    None => {
                        tracing::warn!(name, "Trace hop not found in index");
                        TraceHop {
                            name: name.clone(),
                            file: String::new(),
                            line_start: 0,
                            signature: String::new(),
                        }
                    }
                }
            }).collect();

            Ok(TraceOutput {
                source: source_name.to_string(),
                target: target_name.to_string(),
                depth: Some(hops.len().saturating_sub(1)),
                path: Some(hops),
                found: true,
            })
        }
        None => Ok(TraceOutput {
            source: source_name.to_string(),
            target: target_name.to_string(),
            path: None,
            depth: None,
            found: false,
        }),
    }
}
```

- [ ] **Step 3: Update CLI and batch handlers**

- [ ] **Step 4: Add tests**

```rust
#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn test_trace_output_not_found() {
        let output = TraceOutput {
            source: "a".into(), target: "b".into(),
            path: None, depth: None, found: false,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["found"], false);
        assert!(json.get("path").is_none());
        assert!(json.get("depth").is_none());
    }

    #[test]
    fn test_trace_output_found() {
        let output = TraceOutput {
            source: "a".into(), target: "c".into(),
            path: Some(vec![
                TraceHop { name: "a".into(), file: "src/a.rs".into(), line_start: 1, signature: "fn a()".into() },
                TraceHop { name: "b".into(), file: "src/b.rs".into(), line_start: 10, signature: "fn b()".into() },
                TraceHop { name: "c".into(), file: "src/c.rs".into(), line_start: 20, signature: "fn c()".into() },
            ]),
            depth: Some(2),
            found: true,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["found"], true);
        assert_eq!(json["depth"], 2);
        assert_eq!(json["path"][0]["line_start"], 1);  // was "line"
        assert!(json["path"][0].get("line").is_none());
    }
}
```

- [ ] **Step 5: Build, clippy, test, commit**

---

### Task 6: Impact — wrap lib output (no lib changes)

**Files:**
- Modify: `src/cli/commands/graph/impact.rs`

Impact uses `cqs::impact_to_json()` from the lib crate. Don't modify the lib. Instead, the CLI already uses the lib output directly. No struct needed for this phase — the lib's `impact_to_json` is the schema. Flag `"line"` normalization as a future lib-level change.

- [ ] **Step 1: Audit impact JSON for field naming violations**

Grep `impact_to_json` in the lib for `"line"`, `"function"`, other non-normalized names. Document what needs changing when we do the lib-level migration.

- [ ] **Step 2: Add a comment in impact.rs noting the lib dependency**

```rust
// TODO: impact_to_json lives in lib crate — normalize "line" → "line_start" there
```

- [ ] **Step 3: Commit**

---

### Task 7: Final verification

- [ ] **Step 1: Full test suite**
- [ ] **Step 2: Manual JSON output check for all 7 commands**
- [ ] **Step 3: Verify all `"line"` normalized to `"line_start"` (grep)**
- [ ] **Step 4: Verify all `"function"` normalized to `"name"` (grep)**
- [ ] **Step 5: PR**
