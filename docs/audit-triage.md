# Audit Triage

Generated: 2026-02-04

Source: `docs/audit-findings.md` (202 raw findings)

## Duplicate Clusters

These findings describe the same underlying issue across multiple categories:

### Cluster A: Language/Query Duplication (5 findings → 1 issue)
- Code Hygiene #1: Duplicated Tree-Sitter Query Constants (hard)
- Code Hygiene #4: Unused LanguageRegistry Infrastructure (hard)
- Module Boundaries #1: Duplicate Language Enum Definitions (medium)
- Module Boundaries #8: Parser Module Contains Query Constants (medium)
- Extensibility #1: Duplicate Language Enum (medium)

**Root issue:** Dual language systems - parser.rs has own Language enum + queries while language/ module exists but is unused.
**Tier:** P4 (hard architectural)

### Cluster B: Model Name Mismatch (5 findings → 1 issue)
- Documentation #1: lib.rs says nomic (easy)
- Documentation #2: embedder.rs says nomic (easy)
- Documentation #3: CHANGELOG says nomic (easy)
- Documentation #15: DESIGN_SPEC says nomic (easy)
- Documentation #17: CI workflow checks wrong model (medium)

**Root issue:** Model changed from nomic-embed-text-v1.5 to E5-base-v2 but docs not updated.
**Tier:** P1 (easy Batch 1, high visibility)

### Cluster C: Note Search O(n) (3 findings → 1 issue)
- Algorithmic Complexity #2: O(n) Brute-Force Note Search (medium)
- I/O Efficiency #1: Note Search Full Table Scan (medium)
- Edge Cases #8: No Limit on Notes in Memory (medium)

**Root issue:** Notes not indexed in HNSW, always brute-force searched.
**Tier:** P3 (medium Batch 3-4)

### Cluster D: Embedder Cold Start (3 findings → 1 issue)
- I/O Efficiency #5: No Embedder Caching Across CLI (hard)
- Resource Footprint #3: ONNX Session Cold Start (hard)
- Resource Footprint #11: Watch Creates Embedder Per Reindex (easy)

**Root issue:** ONNX session initialized per-command instead of cached.
**Tier:** P4 (hard)

### Cluster E: HNSW File I/O (2 findings → 1 issue)
- I/O Efficiency #3: Checksum reads files twice (easy)
- I/O Efficiency #4: Save re-reads files for checksum (easy)

**Root issue:** Checksum computed by re-reading files instead of hashing during write/load.
**Tier:** P2 (easy Batch 4)

### Cluster F: Runtime/Store Duplication (3 findings → 1 issue)
- Resource Footprint #1: Tokio Runtime Per-Store (medium)
- Resource Footprint #2: Separate HTTP Runtime (easy)
- Resource Footprint #7: 4 Connections Per Store Pool (easy)

**Root issue:** No shared runtime/store across components.
**Tier:** P3 (medium Batch 4)

---

## De-duplicated Count

- Raw findings: 202
- Duplicate findings removed: 17
- **Unique issues: 185**

---

## P1: Fix Immediately (Easy + Batch 1-2)

High-impact maintainability/readability fixes.

| # | Issue | Location | Category |
|---|-------|----------|----------|
| 1 | **Model name mismatches** (Cluster B) | lib.rs, embedder.rs, CHANGELOG, DESIGN_SPEC, CI | Documentation |
| 2 | Line number clamping helper | 10 locations in store/, search.rs | Code Hygiene |
| 3 | Dead code markers (#[allow(dead_code)]) | mcp.rs, cli/mod.rs | Code Hygiene |
| 4 | Magic numbers in configuration | store/mod.rs, hnsw.rs | Code Hygiene |
| 5 | Commented debug code | search.rs | Code Hygiene |
| 6 | CLI imports internal types directly | cli/mod.rs:22-27 | Module Boundaries |
| 7 | ChunkRow exposed publicly | store/helpers.rs | Module Boundaries |
| 8 | nl module coupled to parser | nl.rs:6 | Module Boundaries |
| 9 | normalize_for_fts in wrong module | store/mod.rs | Module Boundaries |
| 10 | CAGRA/HNSW selection scattered | cli/mod.rs, cagra.rs | Module Boundaries |
| 11 | Missing doc comments (8 items) | nl.rs, parser.rs, note.rs, config.rs, helpers.rs | Documentation |
| 12 | Redundant HnswResult vs IndexResult | hnsw.rs, index.rs | API Design |
| 13 | serve_http/stdio doc mismatch | mcp.rs | API Design |
| 14 | SearchFilter validation missing | store/helpers.rs | API Design |
| 15 | MCP tool naming convention | mcp.rs | API Design |
| 16 | Language::FromStr error type | parser.rs | API Design |
| 17 | Config::merge naming confusing | config.rs | API Design |
| 18 | Note index-based IDs fragile | note.rs | API Design |
| 19 | &Path vs PathBuf inconsistent | multiple files | API Design |
| 20 | VectorIndex takes &Embedding not &[f32] | index.rs | API Design |
| 21 | Swallowed .ok() patterns (9 items) | cli/mod.rs, store/*.rs, source/*.rs | Error Propagation |
| 22 | FTS delete errors ignored | store/chunks.rs, notes.rs | Error Propagation |
| 23 | parse_duration .unwrap_or(0) | mcp.rs | Error Propagation |
| 24 | HNSW checksum warns not fails | hnsw.rs | Error Propagation |
| 25 | Thread panic loses payload | cli/mod.rs | Error Propagation |
| 26 | Tracing subscriber no log levels | main.rs | Observability |
| 27 | Embedder batch timing missing | embedder.rs | Observability |
| 28 | MCP tool calls not logged | mcp.rs | Observability |
| 29 | Note indexing silent | store/notes.rs | Observability |
| 30 | Call graph no progress | store/calls.rs | Observability |
| 31 | Watch mode uses print not trace | cli/mod.rs | Observability |
| 32 | Parser errors not detailed | parser.rs | Observability |
| 33 | HNSW checksum asymmetric logging | hnsw.rs | Observability |
| 34 | Config loading silent | config.rs | Observability |
| 35 | Missing tests: token_count | embedder.rs | Test Coverage |
| 36 | Missing tests: cosine_similarity | search.rs | Test Coverage |
| 37 | Missing tests: name_match_score | search.rs | Test Coverage |
| 38 | Missing tests: delete_by_origin | store/chunks.rs | Test Coverage |
| 39 | Missing tests: needs_reindex | store/chunks.rs | Test Coverage |
| 40 | Missing tests: parse_duration | mcp.rs | Test Coverage |
| 41 | Missing tests: AuditMode | mcp.rs | Test Coverage |
| 42 | Missing tests: Source error paths | source/filesystem.rs | Test Coverage |
| 43 | Missing tests: parse_file_calls | parser.rs | Test Coverage |
| 44 | Missing tests: ChunkType.from_str | language/mod.rs | Test Coverage |
| 45 | Incomplete unicode proptest | store/mod.rs | Test Coverage |
| 46 | display.rs bounds unchecked | cli/display.rs | Panic Paths |
| 47 | Ctrl+C handler .expect() | cli/mod.rs | Panic Paths |
| 48 | Progress bar .expect() | cli/mod.rs | Panic Paths |
| 49 | Static regex .expect() | nl.rs | Panic Paths |
| 50 | embed_batch .expect() single item | embedder.rs | Panic Paths |
| 51 | NonZeroUsize .expect() on literals | embedder.rs | Panic Paths |
| 52 | Call extraction line underflow | parser.rs | Algorithm Correctness |
| 53 | RRF property test wrong max | store/mod.rs | Algorithm Correctness |
| 54 | Context line edge case | cli/display.rs | Algorithm Correctness |
| 55 | TypeScript return type parens | nl.rs | Algorithm Correctness |
| 56 | **Go return type broken** | nl.rs | Algorithm Correctness |
| 57 | Hardcoded language list in MCP | mcp.rs | Extensibility |
| 58 | Hardcoded chunk/token limits | parser.rs, cli/mod.rs | Extensibility |
| 59 | HNSW params not configurable | hnsw.rs | Extensibility |
| 60 | Project root markers hardcoded | cli/mod.rs | Extensibility |
| 61 | SignatureStyle enum closed | language/mod.rs | Extensibility |
| 62 | RRF K constant hardcoded | store/mod.rs | Extensibility |
| 63 | Callee skip list hardcoded | parser.rs | Extensibility |
| 64 | Sentiment thresholds hardcoded | note.rs | Extensibility |

**Total P1: 64 unique issues**

---

## P2: Fix Next (Easy + Batch 3-4, Medium + Batch 1)

| # | Issue | Location | Category |
|---|-------|----------|----------|
| 1 | **Unicode string slicing panic** | mcp.rs:991 | Edge Cases |
| 2 | Inconsistent error handling style | store/mod.rs, notes.rs | Code Hygiene |
| 3 | Feature flag query duplication | parser.rs | Code Hygiene |
| 4 | MCP/CLI duplicate note indexing | mcp.rs, cli/mod.rs | Module Boundaries |
| 5 | Embedder has ONNX setup logic | embedder.rs | Module Boundaries |
| 6 | Config documentation missing README | README.md | Documentation |
| 7 | CLI command docs incomplete | cli/mod.rs | Documentation |
| 8 | Inconsistent search naming | search.rs, store/mod.rs | API Design |
| 9 | Embedding dimension validation | embedder.rs | API Design |
| 10 | Chunk ID format exposed | parser.rs | API Design |
| 11 | Parse failures default silently | store/helpers.rs | Error Propagation |
| 12 | Missing .context() on ? | cli/mod.rs | Error Propagation |
| 13 | CAGRA failure not surfaced | mcp.rs | Error Propagation |
| 14 | GPU failures no metrics | cli/mod.rs | Error Propagation |
| 15 | Non-atomic note append | mcp.rs | Data Integrity |
| 16 | Embedding dim not validated on load | store/helpers.rs | Data Integrity |
| 17 | HNSW id_map size not validated | hnsw.rs | Data Integrity |
| 18 | Model dimensions not validated | store/mod.rs | Data Integrity |
| 19 | Empty query no feedback | store/mod.rs | Edge Cases |
| 20 | No max query length | embedder.rs | Edge Cases |
| 21 | Content hash slicing assumes hex | parser.rs, cli/mod.rs | Edge Cases |
| 22 | Parser capture index bounds | parser.rs | Edge Cases |
| 23 | Empty result no distinction | search.rs | Edge Cases |
| 24 | Window calculation overflow | embedder.rs | Edge Cases |
| 25 | Windows process_exists shell | cli/mod.rs | Platform Behavior |
| 26 | Hardcoded forward slash | mcp.rs, cli/mod.rs | Platform Behavior |
| 27 | No line ending normalization | parser.rs, filesystem.rs | Platform Behavior |
| 28 | libc unconditional dependency | Cargo.toml | Platform Behavior |
| 29 | Intermediate Vec allocations | embedder.rs | Memory Management |
| 30 | Clone-heavy search results | search.rs | Memory Management |
| 31 | Unbounded note parsing | note.rs | Memory Management |
| 32 | Watch pending_files unbounded | cli/mod.rs | Memory Management |
| 33 | CAGRA index not restored on error | cagra.rs | Concurrency Safety |
| 34 | Embedder cache race | embedder.rs | Concurrency Safety |
| 35 | MCP CAGRA opens separate Store | mcp.rs | Concurrency Safety |
| 36 | INTERRUPTED memory ordering | cli/mod.rs | Concurrency Safety |
| 37 | HTTP RwLock unnecessary | mcp.rs | Concurrency Safety |
| 38 | TOML injection in mentions | mcp.rs | Input Security |
| 39 | Glob pattern validation | search.rs | Input Security |
| 40 | FTS normalization unbounded | store/mod.rs | Input Security |
| 41 | Config from user path | config.rs | Input Security |
| 42 | Notes path not validated | mcp.rs | Input Security |
| 43 | No file permission controls | hnsw.rs, mcp.rs, cli/mod.rs | Data Security |
| 44 | Database path in errors | store/mod.rs | Data Security |
| 45 | name_match_score O(n*m) | search.rs | Algorithmic Complexity |
| 46 | tokenize_identifier repeated | store/mod.rs | Algorithmic Complexity |
| 47 | prune_missing individual deletes | store/chunks.rs | Algorithmic Complexity |
| 48 | stats() multiple queries | store/chunks.rs | Algorithmic Complexity |
| 49 | HashSet per function | parser.rs | Algorithmic Complexity |
| 50 | **HNSW checksum I/O** (Cluster E) | hnsw.rs | I/O Efficiency |
| 51 | File metadata read twice | cli/mod.rs, store/chunks.rs | I/O Efficiency |
| 52 | Stats loads HNSW for length | cli/mod.rs | I/O Efficiency |
| 53 | Metadata queries not batched | store/chunks.rs | I/O Efficiency |
| 54 | Separate HTTP runtime | mcp.rs | Resource Footprint |
| 55 | CAGRA thread not tracked | mcp.rs | Resource Footprint |
| 56 | SQLite mmap 256MB | store/mod.rs | Resource Footprint |
| 57 | 4 connections per Store | store/mod.rs | Resource Footprint |
| 58 | Query cache size hardcoded | embedder.rs | Resource Footprint |

**Total P2: 58 unique issues**

---

## P3: Fix If Time Permits (Medium + Batch 2-3)

| # | Issue | Location | Category |
|---|-------|----------|----------|
| 1 | Store database ops no spans | store/*.rs | Observability |
| 2 | HTTP request no tracing | mcp.rs | Observability |
| 3 | RRF fusion no visibility | store/mod.rs | Observability |
| 4 | Missing tests: split_into_windows | embedder.rs | Test Coverage |
| 5 | Missing tests: note CRUD | store/notes.rs | Test Coverage |
| 6 | Missing tests: call graph ops | store/calls.rs | Test Coverage |
| 7 | Missing tests: gitignore | source/filesystem.rs | Test Coverage |
| 8 | Missing tests: malformed files | parser.rs | Test Coverage |
| 9 | OnceCell embedder race | mcp.rs | Panic Paths |
| 10 | SQLx row.get() panics | search.rs, store/calls.rs | Panic Paths |
| 11 | Unified search slot allocation | search.rs | Algorithm Correctness |
| 12 | ChunkType enum manual updates | language/mod.rs, parser.rs | Extensibility |
| 13 | Embedding model hardcoded | embedder.rs, store/helpers.rs | Extensibility |
| 14 | VectorIndex trait minimal | index.rs | Extensibility |
| 15 | HNSW/SQLite sync risk | cli/mod.rs | Data Integrity |
| 16 | Non-atomic HNSW writes | hnsw.rs | Data Integrity |
| 17 | Delete without transaction | store/chunks.rs, notes.rs | Data Integrity |
| 18 | FTS table orphaned | store/chunks.rs | Data Integrity |
| 19 | Debug-only dimension assert | search.rs | Edge Cases |
| 20 | Signature extraction slicing | parser.rs | Edge Cases |
| 21 | HNSW load no bounds check | hnsw.rs | Edge Cases |
| 22 | **Note search O(n)** (Cluster C) | store/notes.rs | Edge Cases |
| 23 | CAGRA neighbor cast | cagra.rs | Edge Cases |
| 24 | FTS injection risk | store/mod.rs | Edge Cases |
| 25 | Unix-only ONNX provider | embedder.rs | Platform Behavior |
| 26 | Path separator in storage | store/chunks.rs, calls.rs | Platform Behavior |
| 27 | SQLite URL Windows | store/mod.rs | Platform Behavior |
| 28 | Unbounded file enumeration | cli/mod.rs | Memory Management |
| 29 | Unbounded search results | search.rs | Memory Management |
| 30 | File content in memory | parser.rs, filesystem.rs | Memory Management |
| 31 | Pipeline channel buffers | cli/mod.rs | Memory Management |
| 32 | Lock ordering CAGRA | cagra.rs | Concurrency Safety |
| 33 | CLI pipeline producers | cli/mod.rs | Concurrency Safety |
| 34 | HNSW ID map size unlimited | hnsw.rs | Input Security |
| 35 | Windows tasklist injection | cli/mod.rs | Input Security |
| 36 | API key plaintext memory | mcp.rs, cli/mod.rs | Data Security |
| 37 | CORS allows any | mcp.rs | Data Security |
| 38 | Call graph re-parses files | cli/mod.rs | I/O Efficiency |
| 39 | upsert_calls per chunk | cli/mod.rs | I/O Efficiency |
| 40 | FileSystemSource eager read | source/filesystem.rs | I/O Efficiency |
| 41 | **Runtime/Store duplication** (Cluster F) | store/mod.rs, mcp.rs | Resource Footprint |
| 42 | Three threads indexing | cli/mod.rs | Resource Footprint |
| 43 | HNSW loaded per CLI query | cli/mod.rs | Resource Footprint |

**Total P3: 43 unique issues**

---

## P4: Create Issue, Defer (Medium Batch 4, Hard any)

| # | Issue | Location | Category |
|---|-------|----------|----------|
| 1 | **Language/Query duplication** (Cluster A) | parser.rs, language/*.rs | Code Hygiene |
| 2 | Search logic split Store/search | search.rs, store/mod.rs | Module Boundaries |
| 3 | Source trait unused | source/mod.rs | Module Boundaries |
| 4 | HTTP API documentation | README.md, mcp.rs | Documentation |
| 5 | Missing tests: embed_documents | embedder.rs | Test Coverage |
| 6 | Missing tests: CLI integration | cli/mod.rs | Test Coverage |
| 7 | HNSW LoadedHnsw unsafe Send+Sync | hnsw.rs | Concurrency Safety |
| 8 | MCP tool registration not plugin | mcp.rs | Extensibility |
| 9 | No indexing extension hooks | cli/mod.rs | Extensibility |
| 10 | SQLite synchronous = NORMAL | store/mod.rs | Data Integrity |
| 11 | **No schema migration path** | store/mod.rs | Data Integrity |
| 12 | Deep directory trees | source/filesystem.rs | Edge Cases |
| 13 | Case sensitivity assumptions | search.rs, cli/mod.rs | Platform Behavior |
| 14 | GPU features assume Linux | embedder.rs, cagra.rs | Platform Behavior |
| 15 | All embeddings in memory for HNSW | store/chunks.rs, cli/mod.rs | Memory Management |
| 16 | CAGRA dataset duplication | cagra.rs | Memory Management |
| 17 | Lock file race | cli/mod.rs | Data Security |
| 18 | **Embedder cold start** (Cluster D) | embedder.rs, cli/mod.rs | Resource Footprint |
| 19 | Large binary size (34MB) | Cargo.toml | Resource Footprint |

**Total P4: 19 unique issues**

---

## Summary

| Tier | Count | Criteria |
|------|-------|----------|
| P1 | 64 | Easy + Batch 1-2 |
| P2 | 58 | Easy + Batch 3-4, Medium + Batch 1 |
| P3 | 43 | Medium + Batch 2-3 |
| P4 | 19 | Medium + Batch 4, Hard |
| **Total** | **184** | (after de-duplication) |

---

## Recommended Fix Order

### Phase 1: Quick Wins (P1)
1. **Model name fixes** - 5 locations, all easy, high visibility
2. **Go return type** - one-liner, actual bug
3. **display.rs bounds** - one-liner, prevents panic
4. **Line number helper** - extract once, use 10 places
5. **Doc comments batch** - 8 missing, easy adds

### Phase 2: Medium Value (P2)
1. **Unicode slicing panic** - user-facing crash
2. **HNSW checksum I/O** - double file reads
3. **Error swallowing patterns** - visibility improvements

### Diminishing Returns Check
After P1+P2, assess: are P3 items worth the effort for maintainability goal?
