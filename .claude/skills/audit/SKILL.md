---
name: audit
description: Run a multi-category code audit on the cqs codebase. Spawns parallel agents per batch.
disable-model-invocation: true
argument-hint: "[batch-number|all]"
---

# Audit

Run the 14-category code audit. Full design: `docs/plans/2026-02-04-20-category-audit-design.md`

## Arguments

- `$ARGUMENTS` — batch number (1-3) or `all` for full audit

## Batches

| Batch | Categories |
|-------|-----------|
| 1 | Code Quality, Documentation, API Design, Error Handling, Observability |
| 2 | Test Coverage, Robustness, Algorithm Correctness, Extensibility, Platform Behavior |
| 3 | Security, Data Safety, Performance, Resource Management |

## Process

### Setup

1. **Archive previous findings**: Rename `docs/audit-findings.md` to `docs/audit-findings-vPREVIOUS.md` (e.g., `audit-findings-v0.5.3.md`). Start fresh — old findings live in the triage file.

2. **Enable audit mode**: `cqs_audit_mode(true, expires_in="2h")` — prevents stale notes from biasing review

### Per-Batch

3. **Create team**: One team per batch (`audit-batch-N`)

4. **Spawn teammates**: One per category (use `sonnet` for judgment-heavy categories, `haiku` for mechanical ones)

5. **Each teammate prompt must include**:
   - Their category scope (from table below)
   - Instruction to read `docs/audit-triage.md` first — skip anything already triaged
   - Instruction to read `docs/audit-findings.md` first — skip anything already reported by earlier batches in this audit
   - Instruction to append findings to `docs/audit-findings.md`
   - Format: `## [Category]\n\n#### [Finding title]\n- **Difficulty:** easy | medium | hard\n- **Location:** ...\n- **Description:** ...\n- **Suggested fix:** ...`

6. **Shutdown team** after all agents complete

### After All Batches

7. **Triage**: Read `docs/audit-findings.md` in full, then classify:
   - P1: Easy + high impact → fix immediately
   - P2: Medium effort + high impact → fix in batch
   - P3: Easy + low impact → fix if time
   - P4: Hard or low impact → create issues
   - **Write triage to `docs/audit-triage.md`** — append a new version section with P1-P4 tables (include Status column). This survives context compaction.

8. **Disable audit mode**: `cqs_audit_mode(false)`

## Category Scopes

| Category | Covers (merged from) |
|----------|---------------------|
| Code Quality | Dead code, duplication, complexity, coupling, cohesion, module boundaries |
| Documentation | Accuracy, completeness, staleness of docs and comments |
| API Design | Consistency, ergonomics, naming, type design |
| Error Handling | Result chains, context, recovery, swallowed errors |
| Observability | Logging coverage, tracing, debuggability |
| Test Coverage | Gaps, meaningful assertions, integration tests |
| Robustness | unwrap/expect, edge cases (empty/huge/unicode/malformed), panic paths |
| Algorithm Correctness | Off-by-one, boundary conditions, logic errors |
| Extensibility | Adding features without surgery, hardcoded values |
| Platform Behavior | OS differences, path handling, WSL quirks |
| Security | Injection, path traversal, file permissions, secrets, access control |
| Data Safety | Corruption, validation, migrations, races, deadlocks, thread safety |
| Performance | O(n²), unnecessary iterations, batching, caching, I/O patterns |
| Resource Management | Memory usage, startup time, idle cost, OOM protection, leaks |

## Rules

- Collect ALL findings before fixing ANY
- One batch at a time (context limits)
- Clean up teams between batches
- Stop at diminishing returns during discovery
- Once triaged, complete the tier — don't suggest stopping mid-priority
- Cross-check findings against open GitHub issues — note overlaps as "existing #NNN"
- **Mark items in `docs/audit-triage.md` as fixed when done** — update the Status column (e.g., `✅ PR #N` or `✅ fixed`). This is the source of truth for what's been addressed.
