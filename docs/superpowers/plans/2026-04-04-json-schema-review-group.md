# JSON Schema: Review Group Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `json!{}` calls in review commands (dead, suggest, affected, review, ci, health) with typed `#[derive(Serialize)]` output structs where possible. Commands that use lib-level JSON builders (affected, review, ci) get TODO comments like impact.

**Architecture:** Same pattern as index/graph groups. Self-contained commands (dead, suggest) get full struct migration. Lib-dependent commands (affected, review, ci) get audited and TODO'd. Health already has no json! calls.

**Tech Stack:** Rust, serde, serde_json

---

### Task 1: DeadOutput

**Files:**
- Modify: `src/cli/commands/review/dead.rs`
- Modify: `src/cli/commands/review/mod.rs`
- Modify: `src/cli/commands/mod.rs`
- Modify: `src/cli/batch/handlers/analysis.rs`

Dead already uses normalized field names (`line_start`, `line_end`, `name`). Just replace `dead_to_json` with a typed struct.

- [ ] **Step 1: Define DeadFunctionEntry and DeadOutput structs**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct DeadFunctionEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub chunk_type: String,
    pub signature: String,
    pub language: String,
    pub confidence: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DeadOutput {
    pub dead: Vec<DeadFunctionEntry>,
    pub possibly_dead_pub: Vec<DeadFunctionEntry>,
    pub count: usize,
    pub possibly_pub_count: usize,
}
```

- [ ] **Step 2: Replace dead_to_json with build_dead_output**

```rust
pub(crate) fn build_dead_output(
    confident: &[DeadFunction],
    possibly_pub: &[DeadFunction],
    root: &Path,
) -> DeadOutput {
    let _span = tracing::info_span!("build_dead_output", confident = confident.len(), possibly = possibly_pub.len()).entered();
    let format = |d: &DeadFunction| DeadFunctionEntry {
        name: d.chunk.name.clone(),
        file: cqs::rel_display(&d.chunk.file, root).to_string(),
        line_start: d.chunk.line_start,
        line_end: d.chunk.line_end,
        chunk_type: d.chunk.chunk_type.to_string(),
        signature: d.chunk.signature.clone(),
        language: d.chunk.language.to_string(),
        confidence: confidence_label(d.confidence).to_string(),
    };
    DeadOutput {
        count: confident.len(),
        possibly_pub_count: possibly_pub.len(),
        dead: confident.iter().map(&format).collect(),
        possibly_dead_pub: possibly_pub.iter().map(&format).collect(),
    }
}
```

- [ ] **Step 3: Update CLI, batch, mod.rs exports. Add tests. Commit.**

Tests: serialization with normalized field names, empty case.

---

### Task 2: SuggestOutput

**Files:**
- Modify: `src/cli/commands/review/suggest.rs`

- [ ] **Step 1: Define SuggestEntry and replace inline json!**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct SuggestEntry {
    pub text: String,
    pub sentiment: f64,
    pub mentions: Vec<String>,
    pub reason: String,
}
```

- [ ] **Step 2: Replace the json! block in cmd_suggest with typed serialization. Add test. Commit.**

---

### Task 3: Affected, Review, CI — audit only

**Files:**
- Modify: `src/cli/commands/review/affected.rs`
- Modify: `src/cli/commands/review/diff_review.rs`
- Modify: `src/cli/commands/review/ci.rs`

These use lib-level `diff_impact_to_json`, `review_to_json`, and mutate the result with additional fields (`overall_risk`, `token_count`). Same situation as impact.

- [ ] **Step 1: Audit each file for field naming violations. Add TODO comments noting the lib dependency.**

- [ ] **Step 2: Commit.**

---

### Task 4: Health — no changes needed

Health already has 0 json! calls. Uses `serde_json::to_value(&report)` where `report` is a lib type. Already correct pattern.

- [ ] **Step 1: Verify. Note in commit message.**

---

### Task 5: Final verification

- [ ] **Step 1: Build, clippy, full test suite**
- [ ] **Step 2: PR**
