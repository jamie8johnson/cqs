# Project Continuity

## Right Now

**20-category audit TRIAGED** (2026-02-04)

- Design: `docs/plans/2026-02-04-20-category-audit-design.md`
- Findings: `docs/audit-findings.md`
- **Triage: `docs/audit-triage.md`**

**184 unique issues** (after de-duplication from 202 raw):

| Tier | Count | Criteria |
|------|-------|----------|
| P1 | 64 | Easy + Batch 1-2 (fix immediately) |
| P2 | 58 | Easy + Batch 3-4, Medium + Batch 1 |
| P3 | 43 | Medium + Batch 2-3 |
| P4 | 19 | Hard, defer to issues |

### Duplicate Clusters Identified
- A: Language/Query duplication (5→1)
- B: Model name mismatch (5→1)
- C: Note search O(n) (3→1)
- D: Embedder cold start (3→1)
- E: HNSW file I/O (2→1)
- F: Runtime/Store duplication (3→1)

### Next Steps
1. **Start fixing P1** - model names, Go return type, display.rs bounds
2. After P1+P2: assess diminishing returns
3. Create GitHub issues for P4

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
