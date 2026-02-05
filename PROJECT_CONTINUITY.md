# Project Continuity

## Right Now

**Ready to resume P2 audit fixes** (2026-02-04)

Definition search feature complete (PR #165). Use `name_only=true` in cqs_search for "where is X defined?" queries.

### Next: Continue P2 Fixes

P2 audit at 23/58. Resume from item #24 onward.

Reference: `docs/plans/2026-02-04-20-category-audit-design.md` has the full P2 list.

### Recent Merges

| PR | Description |
|----|-------------|
| #165 | `name_only` definition search mode |
| #163 | HNSW checksum efficiency, TOML injection fix |
| #162 | Memory caps for watch/notes |
| #161 | P2 performance and platform fixes |

### P2 Progress: 23 of 58 Fixed

| # | Issue | Resolution |
|---|-------|------------|
| 1 | Unicode string slicing panic | Fixed: char_indices for text_preview |
| 2 | Inconsistent error handling | Fixed: StoreError::SystemTime |
| 11 | Parse failures default silently | Fixed: log warnings on parse failures |
| 15 | Non-atomic note append | Fixed: sync_all after write |
| 17 | HNSW id_map size validation | Fixed: check count on load |
| 19 | Empty query no feedback | Fixed: debug log when normalized empty |
| 20 | No max query length | Already had: validate_query_length (8192) |
| 21 | Content hash slicing | Fixed: .get(..8).unwrap_or() |
| 22 | Parser capture index bounds | Fixed: .get().copied() |
| 28 | libc unconditional dep | Fixed: cfg(unix) |
| 31 | Unbounded note parsing | Fixed: MAX_NOTES 10k cap |
| 32 | Watch pending_files unbounded | Fixed: MAX_PENDING_FILES 10k cap |
| 38 | TOML injection in mentions | Fixed: escape newlines/tabs/etc |
| 39 | Glob pattern validation | Fixed: SearchFilter.validate() |
| 40 | FTS normalization unbounded | Fixed: 16KB output cap |
| 47 | prune_missing individual deletes | Fixed: batch 100 at a time |
| 48 | stats() multiple queries | Fixed: batched metadata query |
| 49 | HashSet per function | Fixed: reuse across iterations |
| 50 | HNSW checksum I/O | Fixed: hash ids from memory |
| 52 | Stats loads HNSW for length | Fixed: count_vectors() reads ids only |
| - | Glob pattern tests | Fixed: 3 new tests + FTS bounds tests |
| - | CLI file split | watch.rs extracted (274 lines) |

### P1 Status: 62 of 64 Closed

2 deferred to P4 (architectural: nl/parser coupling, CAGRA/HNSW scattered).

### Remaining Tiers
| Tier | Count | Status |
|------|-------|--------|
| P2 | 35 remaining | 23 fixed |
| P3 | 43 | Pending |
| P4 | 21 | Pending (includes 2 deferred P1) |

## Previous Session

- Definition search feature implemented and merged
- CLI split: watch.rs extracted from mod.rs
- P2 fixes batched into PRs #161-163

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
- CLI: mod.rs + display.rs + watch.rs
- Schema v10, WAL mode
- tests/common/mod.rs for test fixtures
- 172 tests
