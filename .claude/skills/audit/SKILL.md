---
name: audit
description: Run a multi-category code audit on the cqs codebase. Spawns parallel agents per batch.
disable-model-invocation: true
argument-hint: "[batch-number|all]"
---

# Audit

Run the 16-category code audit.

## Arguments

- `$ARGUMENTS` — batch number (1-2) or `all` for full audit

**`all` means run BOTH batches** — all 16 categories. Run batch 1, then batch 2, then triage all findings together. Do NOT stop after batch 1.

## Batches

There are 16 categories split into 2 batches of 8. Each batch spawns 8 parallel agents (one per category). `all` runs both batches sequentially.

| Batch | Categories |
|-------|-----------|
| 1 | Code Quality, Documentation, API Design, Error Handling, Observability, Test Coverage (adversarial), Robustness, Scaling & Hardcoded Limits |
| 2 | Algorithm Correctness, Extensibility, Platform Behavior, Security, Data Safety, Performance, Resource Management, Test Coverage (happy path) |

## Process

### Setup

1. **Archive previous audit**: If `docs/audit-findings.md` or `docs/audit-triage.md` exist, rename both with the version suffix (e.g., `audit-findings-v0.9.1.md`, `audit-triage-v0.9.1.md`). Each audit starts with fresh files.

2. **Enable audit mode**: `cqs audit-mode on --expires 2h -q` — prevents stale notes from biasing review

### Per-Batch

3. **Create team**: One team per batch (`audit-batch-N`)

4. **Spawn teammates**: One per category. Use `model: "opus"` for every auditor — sonnet produces lower-quality judgments and haiku misses subtle findings. The per-category scope keeps each agent focused enough that opus cost is reasonable.

5. **Each teammate prompt must include**:
   - Their category scope (from table below)
   - Instruction to read archived triage files (e.g., `docs/audit-triage-v*.md`) — skip anything already triaged in prior audits
   - Instruction to read `docs/audit-findings.md` first — skip anything already reported by earlier batches in this audit
   - Instruction to append findings to `docs/audit-findings.md`
   - Format: `## [Category]\n\n#### [Finding title]\n- **Difficulty:** easy | medium | hard\n- **Location:** ...\n- **Description:** ...\n- **Suggested fix:** ...`
   - Use `subagent_type: "auditor"` when spawning — the auditor agent definition (`.claude/agents/auditor.md`) has cqs tools built in

6. **Shutdown team** after all agents complete

### After All Batches

7. **Triage**: Read `docs/audit-findings.md` in full, then classify:
   - P1: Easy + high impact → fix immediately
   - P2: Medium effort + high impact → fix in batch
   - P3: Easy + low impact → fix if time
   - P4: Hard or low impact → create issues for hard items; fix trivial ones inline (doc comments, one-liners, undocumented edge cases)
   - **Write triage to `docs/audit-triage.md`** — fresh file with P1-P4 tables (include Status column). This survives context compaction.

8. **Generate fix prompts**: For each P1, P2, and P3 finding, spawn opus agents to (P4 trivials get prompts too; hard P4s get issue descriptions):
   - Read the actual source file at the stated line numbers
   - Write a self-contained fix prompt with: exact file path, current code verbatim, replacement code, one-line "why"
   - Group related findings (e.g., stale doc references) into a single prompt
   - Save to `docs/audit-fix-prompts.md`

9. **Review fix prompts**: Spawn a second opus agent to verify each prompt against the actual source:
   - Does the "current code" match what's really in the file? (catches line drift)
   - Does the fix compile? (check types, imports, API existence)
   - Are there any missing edge cases?
   - Report: "VERIFIED" or "NEEDS FIX — [specific issue]"
   - This step catches ~20% of prompt errors (wrong field names, nonexistent APIs, moved code)

10. **Execute fixes**: P1 first, then P2. Mark each item in triage as fixed.

11. **Disable audit mode**: `cqs audit-mode off`

## Category Scopes

| Category | Covers (merged from) |
|----------|---------------------|
| Code Quality | Dead code, duplication, complexity, coupling, cohesion, module boundaries, **convenience wrappers that hardcode defaults** |
| Documentation | Accuracy, completeness, staleness of docs and comments |
| API Design | Consistency, ergonomics, naming, type design |
| Error Handling | Result chains, context, recovery, swallowed errors |
| Observability | Logging coverage, tracing, debuggability |
| Test Coverage (adversarial) | Edge-case/sad-path gaps: malformed input, NaN/Inf embeddings, concurrent access, empty queries, huge inputs, error paths not tested |
| Test Coverage (happy path) | Missing tests for high-caller public functions, untested modules, integration test gaps, meaningful assertion quality |
| Scaling & Hardcoded Limits | Constants that should scale with model config, corpus size, or hardware. Magic numbers without rationale. |
| Robustness | unwrap/expect, edge cases (empty/huge/unicode/malformed), panic paths |
| Algorithm Correctness | Off-by-one, boundary conditions, logic errors |
| Extensibility | Adding features without surgery, hardcoded values |
| Platform Behavior | OS differences, path handling, WSL quirks |
| Security | Injection, path traversal, file permissions, secrets, access control |
| Data Safety | Corruption, validation, migrations, races, deadlocks, thread safety |
| Performance | O(n²), unnecessary iterations, batching, caching, I/O patterns |
| Resource Management | Memory usage, startup time, idle cost, OOM protection, leaks |

## Mandatory First Steps per Category

Run these cqs commands **before** manual exploration — they surface the highest-value data in a single call.

**Batch 1:**
- **Code Quality**: Run `cqs dead --json` + `cqs health --json` first. Also grep for convenience wrappers that hardcode defaults (e.g., `fn foo()` that calls `foo_with_dim(HARDCODED)`) — these mask incorrect wiring when the default changes.
- **Documentation**: Run `cqs health --json` for staleness counts.
- **API Design**: No mandatory command.
- **Error Handling**: No mandatory command — grep-driven.
- **Observability**: No mandatory command — grep for `tracing::` patterns.

**Batch 2:**
- **Test Coverage**: Run `cqs health --json` first (includes untested hotspots). Also check for **adversarial test gaps**: functions that accept user input, external data, or embeddings should have tests for malformed/adversarial inputs (empty, NaN, truncated, wrong-type, concurrent).
- **Robustness**: No mandatory command — grep for `.unwrap()`, `.expect(`, `panic!`.
- **Algorithm Correctness**: Use `cqs explain <fn> --json` on algorithmic functions.
- **Extensibility**: Run `cqs health --json` for hotspot overview.
- **Platform Behavior**: No mandatory command.

**Batch 3:**
- **Security**: No mandatory command.
- **Data Safety**: No mandatory command.
- **Performance**: Run `cqs health --json` first (identifies hotspots).
- **Resource Management**: No mandatory command.

## Rules

- Collect ALL findings before fixing ANY
- One batch at a time (context limits)
- Clean up teams between batches
- Stop at diminishing returns during discovery
- Once triaged, complete the tier — don't suggest stopping mid-priority
- Cross-check findings against open GitHub issues — note overlaps as "existing #NNN"
- **Mark items in `docs/audit-triage.md` as fixed when done** — update the Status column (e.g., `✅ PR #N` or `✅ fixed`). This is the source of truth for what's been addressed.
