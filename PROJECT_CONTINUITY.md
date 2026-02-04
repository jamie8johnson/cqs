# Project Continuity

## Right Now

**P1 audit complete** (2026-02-04) - PR #151 merged

Triage: `docs/audit-triage.md` | Findings: `docs/audit-findings.md`

### P1 Status: 59 of 64 Closed

| # | Issue | Resolution |
|---|-------|------------|
| 1 | Model name mismatches | Fixed: lib.rs, embedder.rs |
| 2 | Line number clamping | Fixed: helpers.rs + 10 call sites |
| 3 | Dead code markers | Closed: serde requires them |
| 4 | Magic numbers | Fixed: documented in store/mod.rs, hnsw.rs |
| 5 | Commented debug code | Closed: doc examples, not debug |
| 6 | CLI imports internal types | Closed: uses public API correctly |
| 7 | ChunkRow exposed publicly | Fixed: pub(crate) |
| 11 | Missing doc comments | Fixed: parser.rs, note.rs, nl.rs |
| 12 | Redundant HnswResult | Fixed: use IndexResult directly |
| 13 | serve_http/stdio docs | Fixed: mcp.rs |
| 14 | SearchFilter validation | Fixed: validate() + 7 tests |
| 15 | MCP tool naming | Closed: cqs_ prefix intentional |
| 16 | Language::FromStr error | Closed: works correctly |
| 17 | Config::merge naming | Closed: well-documented |
| 18 | Note index-based IDs | Fixed: content-hash IDs |
| 20 | VectorIndex &Embedding | Closed: type-safe design |
| 21 | Swallowed .ok() patterns | Closed: intentional (mtime, etc) |
| 22 | FTS delete errors | Fixed: logging added |
| 23 | parse_duration unwrap | Fixed: strict parsing + tests |
| 24 | HNSW checksum warns | Closed: correct behavior |
| 26 | Tracing log levels | Fixed: EnvFilter in main.rs |
| 27 | Embedder batch timing | Fixed: tracing spans |
| 28 | MCP tool calls logged | Fixed: mcp.rs |
| 29 | Note indexing silent | Fixed: store/notes.rs tracing |
| 30 | Call graph no progress | Fixed: store/calls.rs tracing |
| 31 | Watch mode print | Closed: CLI should use print |
| 32 | Parser errors detailed | Closed: already detailed |
| 33 | HNSW checksum logging | Closed: appropriate levels |
| 34 | Config loading silent | Fixed: debug logging |
| 36 | cosine_similarity tests | Fixed: 4 tests |
| 37 | name_match_score tests | Fixed: 5 tests |
| 38 | delete_by_origin test | Fixed: store_test.rs |
| 39 | needs_reindex test | Fixed: store_test.rs |
| 40 | parse_duration tests | Fixed: 10 tests |
| 41 | AuditMode tests | Fixed: 4 tests |
| 42 | Source error paths | Fixed: 5 tests |
| 43 | parse_file_calls tests | Fixed: 3 tests |
| 44 | ChunkType::FromStr tests | Fixed: 4 tests |
| 45 | Unicode proptest | Closed: adequate coverage |
| 46 | display.rs bounds | Closed: already saturating |
| 47-51 | Panic paths | Closed: verified appropriate |
| 52 | Call extraction underflow | Closed: saturating_sub |
| 53 | RRF test max bound | Closed: 0.5 is correct |
| 54 | Context line edge case | Closed: saturating_sub |
| 55 | TypeScript return type | Closed: documented limitation |
| 56 | Go return type | Fixed: paren depth tracking |
| 58 | Chunk/token limits | Fixed: documented rationale |
| 59 | HNSW params | Fixed: documented tuning |
| 60 | Project root markers | Fixed: documented |
| 63 | Callee skip list | Fixed: documented |
| 64 | Sentiment thresholds | Fixed: documented |

### Remaining P1: 5 items

| # | Issue | Status |
|---|-------|--------|
| 8 | nl coupled to parser | Deferred: architectural (P4) |
| 9 | normalize_for_fts location | Deferred: architectural (P4) |
| 10 | CAGRA/HNSW scattered | Deferred: architectural (P4) |
| 19 | &Path vs PathBuf | Deferred: minor ergonomic |
| 35 | token_count test | Deferred: needs integration test |
| 57 | Hardcoded language list | Closed: has sync comment |
| 61 | SignatureStyle closed | Closed: intentional design |
| 62 | RRF K constant | Closed: documented |

### Remaining Tiers
| Tier | Count | Status |
|------|-------|--------|
| P1 | 5 deferred | Move to P4 |
| P2 | 58 | Pending |
| P3 | 43 | Pending |
| P4 | 19 + 5 = 24 | Create GitHub issues |

## Previous Session

**P1 audit fixes** - PR #151 merged with 59/64 items closed

## Open Issues

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
- 267 tests
