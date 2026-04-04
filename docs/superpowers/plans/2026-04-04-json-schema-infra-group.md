# JSON Schema: Infra Group Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `json!{}` calls in infra commands (telemetry, audit_mode, project, reference) with typed `#[derive(Serialize)]` output structs.

**Architecture:** Same pattern. These are simple key-value outputs — straightforward struct definitions.

**Tech Stack:** Rust, serde, serde_json

---

### Task 1: TelemetryOutput

**Files:**
- Modify: `src/cli/commands/infra/telemetry_cmd.rs`

Telemetry has 6 json! calls — the dashboard output and the empty-state output.

- [ ] **Step 1: Define output types**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct TopQuery {
    pub query: String,
    pub count: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TelemetryOutput {
    pub events: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_range: Option<DateRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<usize>,
    pub commands: std::collections::HashMap<String, usize>,
    pub categories: std::collections::HashMap<String, usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_queries: Vec<TopQuery>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DateRange {
    pub from: u64,
    pub to: u64,
}
```

- [ ] **Step 2: Replace json! blocks in cmd_telemetry with typed struct. Add tests. Commit.**

---

### Task 2: AuditModeOutput

**Files:**
- Modify: `src/cli/commands/infra/audit_mode.rs`

- [ ] **Step 1: Define output type**

```rust
#[derive(Debug, serde::Serialize)]
pub(crate) struct AuditModeOutput {
    pub audit_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}
```

- [ ] **Step 2: Replace json! blocks. Add test. Commit.**

---

### Task 3: Project and Reference — minimal

Project and reference have 1 json! call each. Define simple structs or leave as-is if the output is trivial (single-field).

- [ ] **Step 1: Read both files. If json! is a simple list/status, define struct. If trivial, add TODO and skip.**
- [ ] **Step 2: Commit.**

---

### Task 4: Final verification

- [ ] **Step 1: Build, clippy, full test suite**
- [ ] **Step 2: PR**
