# 20-Category Audit Design

## Overview

**Goal:** Bring cqs to a thoroughly clean state - production-ready AND maintainable. Not just documented findings, but actually fixed code.

**Stopping criteria:** Diminishing returns. Fix until the effort-to-benefit ratio gets bad. This means easy wins in high-impact areas first; stop when only hard, low-impact work remains.

**Execution model:** 4 batches of 5 parallel agents each. Batches run sequentially to avoid context overflow. All findings collected before any fixing begins - this gives full visibility to make smart prioritization decisions.

**Tracking:** Markdown file during collection phase (`docs/audit-findings.md`). Findings promoted to GitHub issues when we enter fix phase. This keeps collection lightweight while ensuring work gets tracked formally once committed.

## Batch Composition

Categories ordered by impact, with maintainability/readability prioritized:

### Batch 1: Readability foundation
- Code Hygiene - dead code, duplication, complexity
- Module Boundaries - coupling, cohesion, dependency direction
- Documentation - accuracy, completeness, staleness
- API Design - consistency, ergonomics, stability
- Error Propagation - Result chains, context, recovery

### Batch 2: Understandable behavior
- Observability - logging coverage, tracing, debuggability
- Test Coverage - gaps, meaningful assertions, integration
- Panic Paths - unwrap/expect usage, unwind safety
- Algorithm Correctness - off-by-one, boundary conditions, logic
- Extensibility - adding features without surgery

### Batch 3: Data & platform correctness
- Data Integrity - corruption detection, validation, migrations
- Edge Cases - empty, huge, unicode, malformed inputs
- Platform Behavior - OS differences, path handling, WSL
- Memory Management - leaks, OOM protection, resource limits
- Concurrency Safety - races, deadlocks, thread safety

### Batch 4: Security & performance
- Input Security - injection, path traversal, untrusted data
- Data Security - file permissions, secrets, access control
- Algorithmic Complexity - O(n²), unnecessary iterations
- I/O Efficiency - batching, caching, disk/network patterns
- Resource Footprint - memory usage, startup time, idle cost

## Agent Behavior and Findings Format

**Each audit agent:**
1. Enables audit mode (`cqs_audit_mode(true)`) to force fresh examination
2. Explores the codebase looking for issues in its category
3. Produces structured findings with difficulty estimates
4. Does NOT fix anything - collection phase only

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

## Fix Prioritization

After all 4 batches complete, findings sorted by impact × effort:

| Tier | Criteria | Action |
|------|----------|--------|
| P1 | Easy + Batch 1-2 (high impact) | Fix immediately |
| P2 | Easy + Batch 3-4, or Medium + Batch 1 | Fix next |
| P3 | Medium + Batch 2-3 | Fix if time permits |
| P4 | Medium + Batch 4, or Hard + any | Create issue, defer |

**Diminishing returns trigger:** When we've cleared P1 and P2, assess remaining work. If P3 items are mostly "meh" improvements, stop fixing and bank the rest as issues.

**Fix workflow:**
1. Promote current tier's findings to GitHub issues
2. Work through issues, closing as fixed
3. Run tests after each fix
4. When tier complete, reassess: continue to next tier or stop?

**Commit cadence:** One commit per logical fix (may cover multiple related findings). Don't batch unrelated fixes.

## Execution Workflow

### Phase 1: Collection (4 sequential rounds)

```
For each batch (1-4):
  1. Enable audit mode
  2. Dispatch 5 agents in parallel
  3. Wait for all agents to complete
  4. Aggregate findings to docs/audit-findings.md
  5. Clear context if needed before next batch
```

### Phase 2: Triage

1. Review complete findings list
2. De-duplicate (some issues span categories)
3. Assign priority tiers (P1-P4)
4. Check against existing GitHub issues
5. Decide: proceed with fixing or stop here?

### Phase 3: Fixing

1. Create GitHub issues for current tier
2. Fix issues, one logical commit each
3. Run `cargo test` and `cargo clippy` after each fix
4. When tier complete: continue or hit diminishing returns?

## Artifacts

- `docs/audit-findings.md` - raw findings from collection
- `docs/plans/2026-02-04-20-category-audit-design.md` - this design doc
- GitHub issues for findings being fixed
- Clean code
