# Code Audit Design

## Overview

**Goal:** Bring cqs to a thoroughly clean state - production-ready AND maintainable. Not just documented findings, but actually fixed code.

**Stopping criteria:** Diminishing returns. Fix until the effort-to-benefit ratio gets bad. This means easy wins in high-impact areas first; stop when only hard, low-impact work remains.

**Execution model:** 3 batches of 5 parallel agents each. Batches run sequentially to avoid context overflow. All findings collected before any fixing begins - this gives full visibility to make smart prioritization decisions.

**Tracking:** Markdown file during collection phase (`docs/audit-findings.md`). Findings promoted to GitHub issues when we enter fix phase. This keeps collection lightweight while ensuring work gets tracked formally once committed.

## Category Design (14 categories, 3 batches)

### v0.5.3 Lesson: Consolidation

The original 20-category / 4-batch design produced 193 raw findings with 38% duplication. Root cause: categories in batches 3-4 overlapped heavily:

| Overlap cluster | Duplicated across |
|----------------|-------------------|
| Path traversal / untrusted input | Input Security, Data Security, Edge Cases |
| Memory / resource use | Memory Management, Resource Footprint |
| Performance / I/O | Algorithmic Complexity, I/O Efficiency |
| Robustness / edge cases | Panic Paths, Edge Cases |
| Data safety / concurrency | Data Integrity, Concurrency Safety |

Consolidating these into broader categories eliminates duplication without losing coverage. Each agent gets a wider scope but produces fewer redundant findings.

### Batch 1: Code Quality (5 agents)
- **Code Quality** — dead code, duplication, complexity, coupling, cohesion, dependency direction (was: Code Hygiene + Module Boundaries)
- **Documentation** — accuracy, completeness, staleness
- **API Design** — consistency, ergonomics, naming, type design
- **Error Handling** — Result chains, context, recovery, swallowed errors (was: Error Propagation)
- **Observability** — logging coverage, tracing, debuggability

### Batch 2: Behavior (5 agents)
- **Test Coverage** — gaps, meaningful assertions, integration
- **Robustness** — unwrap/expect, edge cases (empty, huge, unicode, malformed), panic paths (was: Panic Paths + Edge Cases)
- **Algorithm Correctness** — off-by-one, boundary conditions, logic errors
- **Extensibility** — adding features without surgery, hardcoded values
- **Platform Behavior** — OS differences, path handling, WSL

### Batch 3: Infrastructure (4 agents)
- **Security** — injection, path traversal, file permissions, secrets, access control (was: Input Security + Data Security)
- **Data Safety** — corruption, validation, migrations, races, deadlocks, thread safety (was: Data Integrity + Concurrency Safety)
- **Performance** — O(n²), unnecessary iterations, batching, caching, I/O patterns (was: Algorithmic Complexity + I/O Efficiency)
- **Resource Management** — memory usage, startup time, idle cost, OOM protection, leaks (was: Memory Management + Resource Footprint)

## Agent Behavior and Findings Format

**Each audit agent:**
1. Explores the codebase looking for issues in its category
2. Produces structured findings with difficulty estimates
3. Does NOT fix anything - collection phase only

**Finding structure:**
```markdown
### [Category Name]

#### [Finding title]
- **Difficulty:** easy | medium | hard
- **Location:** `path/to/file.rs:123`
- **Description:** What's wrong and why it matters
- **Suggested fix:** Brief approach (not implementation)
```

**Difficulty definitions:**
- **Easy:** < 30 min, localized change, low risk of breakage
- **Medium:** 30 min - 2 hours, may touch multiple files, needs testing
- **Hard:** 2+ hours, architectural change, or high risk of breakage

**Aggregation:** After each batch completes, findings appended to `docs/audit-findings.md` under a batch header. Cross-reference against existing GitHub issues to avoid duplicates.

**Agent prompts must include:**
- List of open issues from previous audits (to note overlaps, not re-report)
- Key files and line counts for the category
- Explicit "stop after ~10 findings" to prevent diminishing returns
- Convention reminders (thiserror vs anyhow, no unwrap in prod, etc.)

## Fix Prioritization

After all batches complete, findings sorted by impact × effort:

| Tier | Criteria | Action |
|------|----------|--------|
| P1 | Easy + Batch 1-2 (high impact) | Fix immediately |
| P2 | Easy + Batch 3, or Medium + Batch 1 | Fix next |
| P3 | Medium + Batch 2 | Fix if time permits |
| P4 | Medium + Batch 3, or Hard + any | Create issue, defer |

**Diminishing returns trigger:** When we've cleared P1 and P2, assess remaining work. If P3 items are mostly "meh" improvements, stop fixing and bank the rest as issues.

**Fix workflow:**
1. Promote current tier's findings to GitHub issues
2. Work through issues, closing as fixed
3. Run tests after each fix
4. When tier complete, reassess: continue to next tier or stop?

**Commit cadence:** One commit per logical fix (may cover multiple related findings). Don't batch unrelated fixes.

## Execution Workflow

### Phase 1: Collection (3 sequential rounds)

```
For each batch (1-3):
  1. Enable audit mode: cqs_audit_mode(true, expires_in="2h")
  2. Create team: audit-batch-N
  3. Dispatch agents in parallel (5 for batch 1-2, 4 for batch 3)
  4. Wait for all agents to complete
  5. Shutdown team, cleanup
  6. Clear context if needed before next batch
```

### Phase 2: Triage

1. Review complete findings list
2. De-duplicate (should be minimal with consolidated categories)
3. Assign priority tiers (P1-P4)
4. Check against existing GitHub issues
5. Decide: proceed with fixing or stop here?

### Phase 3: Fixing

1. Create GitHub issues for current tier
2. Fix issues, one logical commit each
3. Run `cargo test` and `cargo clippy` after each fix
4. When tier complete: continue or hit diminishing returns?

## Artifacts

- `docs/audit-findings.md` - raw findings from collection (fresh each audit)
- `docs/audit-triage.md` - prioritized findings with status tracking (fresh each audit)
- `docs/audit-findings-v{VERSION}.md` - archived findings from prior audits
- `docs/audit-triage-v{VERSION}.md` - archived triage from prior audits
- `docs/plans/2026-02-04-20-category-audit-design.md` - this design doc
- GitHub issues for findings being fixed

## History

- **v0.5.1**: First audit. 20 categories, 4 batches. ~85 actionable findings from ~120 raw. P1-P3 fixed in 3 PRs, 13 P4 deferred as issues (#231-241).
- **v0.5.3**: Second audit. 20 categories, 4 batches. 193 raw findings, ~120 unique (38% duplication). Identified consolidation opportunity → redesigned to 14 categories / 3 batches for next run.
- **v0.9.1**: Third audit. 14 categories, 3 batches. 96 fixes across 3 PRs (#293, #295, #296). 17 P4 deferred (issues #300-#303). Adopted archive workflow — each audit starts fresh with `audit-findings.md` and `audit-triage.md`.
