# Project Continuity

## Right Now

**20-category audit COMPLETE** (2026-02-04)

- Design: `docs/plans/2026-02-04-20-category-audit-design.md`
- Findings: `docs/audit-findings.md`

**202 findings** across 20 categories:
- ~130 easy, ~55 medium, ~17 hard
- Ready for Phase 2: Triage and prioritize
- Then Phase 3: Fix by priority tier

### Next Steps
1. Review findings, de-duplicate overlaps
2. Assign P1-P4 tiers
3. Start fixing P1 (easy wins in Batch 1-2)

Reconciled overlapping categories into clean taxonomy:

### Security (2)
1. Input Security - injection, path traversal, untrusted data
2. Data Security - file permissions, secrets, access control

### Reliability (4)
3. Memory Management - leaks, OOM protection, resource limits
4. Concurrency Safety - races, deadlocks, thread safety
5. Panic Paths - unwrap/expect usage, unwind safety
6. Error Propagation - Result chains, context, recovery

### Correctness (4)
7. Algorithm Correctness - off-by-one, boundary conditions, logic
8. Data Integrity - corruption detection, validation, migrations
9. Edge Cases - empty, huge, unicode, malformed inputs
10. Platform Behavior - OS differences, path handling, WSL

### Performance (3)
11. Algorithmic Complexity - O(n²), unnecessary iterations
12. I/O Efficiency - batching, caching, disk/network patterns
13. Resource Footprint - memory usage, startup time, idle cost

### Architecture (3)
14. Module Boundaries - coupling, cohesion, dependency direction
15. API Design - consistency, ergonomics, stability
16. Extensibility - adding features without surgery

### Maintainability (4)
17. Code Hygiene - dead code, TODOs, duplication, complexity
18. Documentation - accuracy, completeness, staleness
19. Test Coverage - gaps, meaningful assertions, integration
20. Observability - logging coverage, tracing, debuggability

## Previous Session

**Error path tests complete** - Added 16 tests, closed 6 stale issues (#142-146, #148)

## Open Issues (8)

### Medium (1-4 hr)
- #126: Error path tests (IN PROGRESS - tests added, needs PR)

### Hard (1+ day)
- #147: Duplicate types
- #103: O(n) note search
- #107: Memory OOM
- #139: Deferred housekeeping

### External/Waiting
- #106: ort stable
- #63: paste dep
- #130: Tracking issue

## Architecture

- 769-dim embeddings (768 + sentiment)
- Store: split into focused modules (6 files)
- Schema v10, WAL mode
- tests/common/mod.rs for test fixtures
