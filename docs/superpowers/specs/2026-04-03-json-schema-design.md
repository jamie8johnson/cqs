# Explicit JSON Output Schema Design

## Problem

312 `serde_json::json!` calls (226 CLI + 86 batch) with implicit schemas. Each command hand-builds its JSON, meaning the schema is defined by whatever fields the handler decides to include. Batch and CLI outputs can diverge silently. No compile-time guarantee that two code paths produce the same schema.

## Solution

Define explicit result types with `#[derive(Serialize)]` for every command. Replace `json!{}` calls with `serde_json::to_value(&result)`.

## Design decision needed

For each command, the current `json!{}` output may not match the struct that holds the data. Three approaches:

**A: Reshape structs to match output.** Change existing types so their Serialize output matches what `json!{}` currently produces. Risk: existing code that reads these structs may break. Highest correctness.

**B: Serde annotations.** Keep structs as-is, add `#[serde(rename)]`, `#[serde(skip)]`, `#[serde(flatten)]` to bridge. Less disruptive but the struct and JSON are coupled via annotations rather than structure.

**C: Intermediate output types.** Define separate `*Output` types for JSON serialization, convert from internal types. Most flexible, most boilerplate.

**Recommendation:** A for new code, B for existing types where reshaping would cascade. C only when A and B don't work (e.g., the output combines data from multiple sources that don't have a natural shared type).

## Scope

~50 commands × text + JSON output = ~50 result types to define. Many commands share common patterns (list of search results, single function card, impact report). Shared base types would reduce the count.

## Effort

Multi-day. Start with the most-used commands (search, callers, impact, explain) and expand.

## Dependencies

- Batch/CLI unification should complete first — reduces the number of divergent JSON construction sites
- No code changes needed until design is approved
