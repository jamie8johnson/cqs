---
name: audit
description: Run a multi-category code audit on the cqs codebase. Spawns parallel agents per batch.
disable-model-invocation: true
argument-hint: "[batch-number|all]"
---

# Audit

Run the 20-category code audit. Full design: `docs/plans/2026-02-04-20-category-audit-design.md`

## Arguments

- `$ARGUMENTS` — batch number (1-4) or `all` for full audit

## Batches

| Batch | Categories |
|-------|-----------|
| 1 | Code Hygiene, Module Boundaries, Documentation, API Design, Error Propagation |
| 2 | Observability, Test Coverage, Panic Paths, Algorithm Correctness, Extensibility |
| 3 | Data Integrity, Edge Cases, Platform Behavior, Memory Management, Concurrency Safety |
| 4 | Input Security, Data Security, Algorithmic Complexity, I/O Efficiency, Resource Footprint |

## Process

1. **Enable audit mode**: `cqs_audit_mode(true)` — prevents stale notes from biasing review

2. **Create team**: One team per batch (`audit-batch-N`)

3. **Spawn teammates**: 5 agents, one per category (use `sonnet` for judgment-heavy categories, `haiku` for mechanical ones)

4. **Each teammate**:
   - Reviews code for their category
   - Writes findings to `docs/audit-findings.md` (append, don't overwrite)
   - Format: `## [Category] - [File]\n- Finding\n- Severity: P1-P4\n`

5. **Shutdown team** after all agents complete

6. **Triage**: After ALL batches complete:
   - Sort findings by impact x effort
   - P1: Easy + high impact → fix immediately
   - P2: Medium effort + high impact → fix in batch
   - P3: Easy + low impact → fix if time
   - P4: Hard or low impact → create issues

7. **Disable audit mode**: `cqs_audit_mode(false)`

## Rules

- Collect ALL findings before fixing ANY
- One batch at a time (context limits)
- Clean up teams between batches
- Stop at diminishing returns during discovery
- Once triaged, complete the tier — don't suggest stopping mid-priority
