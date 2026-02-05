# Project Continuity

## Right Now

**v0.4.1 released** (2026-02-05)

P2 audit fixes continuing. Most easy items addressed.

### Recent Fixes (PR #168, #169, #171)
- GPU failures counter and index visibility
- VectorIndex::name() for HNSW/CAGRA identification
- Doc comments for IndexStats, UnifiedResult, CURRENT_SCHEMA_VERSION
- Config::merge renamed to override_with for clarity
- Config TOML example in doc comment
- check_cq_version logs errors instead of silent discard
- NameMatcher for efficient query tokenization
- Note indexing logic deduplicated (shared cqs::index_notes function)
- Language::FromStr now returns ParserError::UnknownLanguage (thiserror)
- --verbose flag wired to tracing subscriber (sets debug level)

### Next: Remaining P2 Items

Remaining P2 items are mostly medium difficulty:
- Search function naming consolidation
- Schema migration paths

### P2 Progress: ~48 of 58 Fixed

| # | Issue | Resolution |
|---|-------|------------|
| 1 | Unicode string slicing panic | Fixed: char_indices for text_preview |
| 2 | Inconsistent error handling | Fixed: StoreError::SystemTime |
| 6 | Config docs missing README | Fixed: added Configuration section |
| 11 | Parse failures default silently | Fixed: log warnings on parse failures |
| 12 | Missing .context() on ? | Fixed: added context to thread init errors |
| 13 | CAGRA failure not surfaced | Fixed: active_index field in cqs_stats |
| 14 | GPU failures no metrics | Fixed: gpu_failures counter in index summary |
| 27 | No line ending normalization | Fixed: CRLF -> LF in parser/filesystem |
| 15 | Non-atomic note append | Fixed: sync_all after write |
| 17 | HNSW id_map size validation | Fixed: check count on load |
| 19 | Empty query no feedback | Fixed: debug log when normalized empty |
| 20 | No max query length | Already had: validate_query_length (8192) |
| 21 | Content hash slicing | Fixed: .get(..8).unwrap_or() |
| 22 | Parser capture index bounds | Fixed: .get().copied() |
| 24 | Embedding dim validation | Fixed: bytes_to_embedding returns Option |
| 25 | Model dims not validated | Fixed: DimensionMismatch error at load |
| 27 | File metadata read twice | Fixed: needs_reindex returns mtime |
| 28 | libc unconditional dep | Fixed: cfg(unix) |
| 30 | name_match O(n*m) | Fixed: HashSet fast path for exact match |
| 31 | Unbounded note parsing | Fixed: MAX_NOTES 10k cap |
| 32 | Watch pending_files unbounded | Fixed: MAX_PENDING_FILES 10k cap |
| 33 | Context line edge case | Fixed: validate line_start, line_end |
| 34 | Watch embedder per reindex | Fixed: OnceCell lazy init |
| 35 | CAGRA thread not tracked | Fixed: documented as intentional |
| 36 | INTERRUPTED memory ordering | Fixed: AcqRel/Acquire instead of SeqCst |
| 37 | HTTP RwLock unnecessary | Fixed: removed outer RwLock, uses interior mutability |
| 38 | TOML injection in mentions | Fixed: escape newlines/tabs/etc |
| 39 | Glob pattern validation | Fixed: SearchFilter.validate() |
| 40 | FTS normalization unbounded | Fixed: 16KB output cap |
| 46 | tokenize_identifier repeated | Fixed: NameMatcher pre-tokenizes query |
| 47 | prune_missing individual deletes | Fixed: batch 100 at a time |
| 48 | stats() multiple queries | Fixed: batched metadata query |
| 49 | HashSet per function | Fixed: reuse across iterations |
| 50 | HNSW checksum I/O | Fixed: hash ids from memory |
| 52 | Stats loads HNSW for length | Fixed: count_vectors() reads ids only |

Also fixed: Flaky HNSW test (robust assertion), documented embedder cache + HTTP runtime tradeoffs.
Config logging now shows loaded values (P1 #34).

Additional fixes (PR #168, #169, #171):
| # | Issue | Resolution |
|---|-------|------------|
| 51 | GPU failures invisible | Fixed: counter + summary line |
| 53 | CAGRA status invisible | Fixed: VectorIndex::name(), active_index in stats |
| 54 | Config merge naming | Fixed: renamed to override_with |
| 55 | check_cq_version silent | Fixed: logs errors at debug level |
| 56 | Missing doc comments | Fixed: IndexStats, UnifiedResult, CURRENT_SCHEMA_VERSION |
| 57 | Duplicate note indexing logic | Fixed: shared cqs::index_notes() |
| 58 | Language::FromStr uses anyhow | Fixed: ParserError::UnknownLanguage |
| 59 | --verbose not wired to tracing | Fixed: sets debug level filter |

### Remaining Tiers
| Tier | Count | Status |
|------|-------|--------|
| P2 | ~10 remaining | ~48 fixed |
| P3 | 43 | Pending |
| P4 | 21 | Pending |

## Previous Session

- v0.4.0 released (GitHub + crates.io)
- Definition search implemented (name_only mode)
- CLI refactored (watch.rs extracted)

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
