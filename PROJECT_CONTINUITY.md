# Project Continuity

## Right Now

**P1 fixes IN PROGRESS** (2026-02-04)

Triage: `docs/audit-triage.md` | Findings: `docs/audit-findings.md`

### P1 Completed (~18 of 64)

| Fix | Location |
|-----|----------|
| Line number clamping helper | helpers.rs + 10 call sites |
| Model name mismatches | lib.rs, embedder.rs, CI workflow |
| DESIGN_SPEC model note | docs/DESIGN_SPEC_27k_tokens.md |
| Go return type extraction | nl.rs |
| display.rs bounds checking | cli/display.rs |
| SQLite magic numbers | store/mod.rs (comments) |
| get_by_content_hash error | store/chunks.rs |
| FTS delete error logging | chunks.rs, notes.rs |
| parse_duration strictness | mcp.rs |
| parse_duration tests | mcp.rs (10 new tests) |
| Call extraction underflow | parser.rs |
| RRF test max bound | store/mod.rs |
| TypeScript return type | nl.rs (documented) |
| Dead code markers | verified correct |
| Tracing RUST_LOG support | main.rs (EnvFilter) |
| embed_batch empty check | embedder.rs |
| Panic paths | verified appropriate use of .expect()

### Next P1 Items
- Observability: tracing subscriber, embedder timing, MCP logging
- Test coverage: token_count, cosine_similarity, delete_by_origin
- API design: redundant HnswResult, SearchFilter validation

### Remaining Tiers
| Tier | Count | Status |
|------|-------|--------|
| P1 | ~50 remaining | In progress |
| P2 | 58 | Pending |
| P3 | 43 | Pending |
| P4 | 19 | Create GitHub issues |

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
