# Roadmap

## Current: v0.18.0

All agent experience features shipped. CLI-only (MCP removed in v0.10.0). 15 languages.

### Recently Completed

- `cqs stale` + proactive staleness warnings (PR #365)
- `cqs context --compact` (PR #366)
- `cqs related` (PR #367)
- `cqs impact --suggest-tests` (PR #368)
- `cqs where` (PR #369)
- `cqs scout` (PR #370)
- Proactive hints in `cqs explain` / `cqs read --focus` (PR #362)
- `cqs impact-diff` — diff-aware impact analysis (PR #362)
- Table-aware Markdown chunking + parent retrieval (PR #361)
- Delete `type_map` dead code from LanguageDef (v0.12.1)
- Scout note matching precision — path-component boundary matching (v0.12.1)
- Web help ingestion — multi-page HTML sites (AuthorIT, MadCap Flare) (PR #397)
- Token-budgeted output — `--tokens N` on query, gather, context, explain, scout (PR #398)
- Cross-index gather — `cqs gather --ref` (PR #399)
- `cqs plan` — skill with 5 task-type templates (v0.12.2)
- `cqs convert` — document-to-Markdown conversion (PR #397)
- `cqs review` — structured diff review with risk scoring (PR #400)
- Change risk scoring — `compute_risk_batch()` + `find_hotspots()` (PR #400)
- Split `impact.rs` monolith → `src/impact/` directory (PR #402)
- Eliminate unsafe transmute in HNSW load + `--ref` integration tests (PR #405, v0.12.5)
- v0.12.3 audit: 73/76 findings fixed (P1-P3 complete, 11/14 P4 fixed) (PR #421, v0.12.6)
- `cqs ci` — CI pipeline mode with gate logic and exit codes
- Cross-encoder re-ranking — `--rerank` flag on query, ms-marco-MiniLM-L-6-v2

### Next — New Commands

Priority order based on competitive gap analysis (Feb 2026).

- [x] `cqs ci` — CI pipeline mode. Impact analysis on PR diff, suggested test targets, dead code introduced, risk score. Exit codes for CI gates.
- [x] `cqs health` — codebase quality snapshot. Dead code count, stale files, untested high-impact functions, hotspots, note warnings.
- [x] `cqs onboard "concept"` — guided codebase tour. Entry point → call chain → key types → tests. Ordered reading list from gather + trace + explain.
- [ ] `cqs blame` — semantic git blame. Given a function, show who last changed it, when, and the commit message. Combines call graph with git log.
- [x] `cqs drift` — detect semantic drift between reference snapshots. Embedding distance, not just text diff. Surface functions that changed behavior.
- [x] `cqs suggest` — auto-generate notes from code patterns. Scan for anti-patterns (unwrap in non-test code, high-caller untested functions, dead code clusters).
- [x] `cqs deps` — type-level dependency impact. Trace struct/enum usage through functions and tests (PR #442). Wired into related, impact, read, dead (PR #447).
- [ ] `cqs chat` — interactive REPL for chained queries. Build order: (1) ~~`ChunkSummary` unification~~, (2) ~~batch mode `cqs batch`~~, (3) ~~REPL~~ (deferred — agents use batch), (4) ~~pipeline syntax~~ (`search | callers | test-map` in `cqs batch`).

### Next — Retrieval Quality

- [x] Re-ranking — cross-encoder `--rerank` flag. Second-pass scoring on top-N retrieval results. Biggest retrieval quality win remaining.
- [x] Embedding model eval — E5-base-v2 confirmed (90.9% R@1, 0.941 MRR on 55-query hard eval). Beats jina-v2-base-code (80.0% R@1). Parent type context enrichment (PR #455).

### Next — Code Quality

- [x] `store.search()` safety — renamed to `search_embedding_only()` to prevent direct use. All user-facing paths should use `search_filtered()`.
- [x] `DocFormat` registry table (#412) — static FORMAT_TABLE replaces 4 match blocks, 6→3 changes per new variant.
- [x] `ChunkSummary` type consistency — `ChunkIdentity`, `LightChunk`, `GatheredChunk` now use `Language`/`ChunkType` enums. Parse boundary at SQL read.
- [x] `reverse_bfs_multi` depth accuracy (#407) — fixed with stale-entry detection + shorter-path updates in multi-source BFS.
- [x] Convert filename TOCTOU race (#410) — atomic `create_new` instead of check-then-write.
- [x] `gather_cross_index` tests (#414) — 4 integration tests added.

### Next — Expansion

- [x] C# language support — 10th language. Property, Delegate, Event chunk types. Per-language common_types. Data-driven container extraction.
- [x] F# language support — 11th language. Module ChunkType. Functions, records, discriminated unions, classes, interfaces, modules, members.
- [x] PowerShell language support — 12th language. Functions, classes, methods, properties, enums, command/method calls.
- [x] Scala language support — 13th language. Object, TypeAlias ChunkTypes. Functions, classes, objects, traits, enums, type aliases, vals/vars. Type dependency extraction.
- [x] Ruby language support — 14th language. SignatureStyle::FirstLine. Functions, classes, modules, singleton methods. Call graph extraction.
- [ ] Pre-built release binaries (GitHub Actions) — adoption friction
- [x] Skill grouping — consolidated 35 thin cqs-* wrappers into unified `/cqs` dispatcher (48→14 skills)

### Future Languages — Priority Order

**Tier 1 — High value, easy mapping:**
- [ ] **Shell/Bash** — Function only. Every project has scripts, nobody indexes them semantically. `tree-sitter-bash` mature. Easiest win.
- [x] **C++** — Biggest gap by dev population. All variants mapped: namespace → Module, concept → Trait, `#define` → Macro/Constant, union → Struct, typedef/using → TypeAlias. `tree-sitter-cpp` mature.

**Tier 2 — Structured schemas (better RAG, not just code search):**
- [ ] **Terraform/HCL** — resource/data → Struct, module → Module, variable/output → Constant. Huge market. People search Terraform the same way they search docs.
- [ ] **Protobuf** — message → Struct, service → Interface, rpc → Function, enum → Enum. Every microservices shop has `.proto` files.
- [ ] **GraphQL** — type/input → Struct, query/mutation/subscription → Function, interface → Interface, enum → Enum. Every web API shop has these.

**Tier 3 — Programming languages with clean mappings:**
- [ ] **Kotlin** — Object (companion/singleton), TypeAlias, Property; data class → Struct, sealed class → Class
- [ ] **Swift** — protocol → Trait, actor → Class, TypeAlias, Property. May need `Extension` variant (primary code org).
- [ ] **Elixir** — Module + Macro exist. defprotocol → Trait, defrecord → Struct. Clean mapping.
- [ ] **Lua** — Function-only. Game dev niche (Roblox, Neovim). Easy.
- [ ] **Haskell** — TypeAlias exists. data → Enum, class → Trait. Niche but loved.
- [ ] **PHP** — Property covers properties, trait → Trait
- [ ] **Dart** — Property covers properties, mixin → Trait
- [ ] **Objective-C** — Property covers `@property`, `@protocol` → Interface
- [ ] **Zig** — maps cleanly

### ChunkType Variant Status

All 16 variants shipped and used across languages. Only one potential new variant remains: `Extension` for Swift.

| Variant | Shipped in | Used by |
|---------|-----------|---------|
| `Module` | v0.16.0 | F#, Ruby, TS (namespace) |
| `Macro` | v0.17.0 | Rust, C (`#define(...)`) |
| `TypeAlias` | v0.17.0 | Scala, Rust, TypeScript, Go, C, F#, SQL |
| `Object` | v0.17.0 | Scala |

Infrastructure for adding variants is now cheap: per-language LanguageDef fields, data-driven container extraction, dynamic callable SQL. New variant = enum arm + Display/FromStr + is_callable decision + nl.rs + capture_types.

### Parked

- **MCP server** — re-add as slim read-only wrapper when CLI features are rock solid. Architecture proven clean (removed in v0.10.0 with zero core changes).
- **VB.NET** — `tree-sitter-vb-dotnet` (git dep). VS2005 project delayed.
- **Pre-built reference packages** (#255) — `cqs ref install tokio`
- **Index encryption** — SQLCipher behind cargo feature flag
- **Query-intent routing** — auto-boost ref weight when query mentions product names
- **Pattern mining** (`cqs patterns`) — recurring code conventions. Large effort, defer.
- **Post-index name matching** — fuzzy cross-doc references

### Open Issues

- #389: CAGRA GPU memory — needs disk persistence layer
- #255: Pre-built reference packages
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

## 1.0 Release Criteria

- [ ] Schema stable for 1+ week of daily use (currently v11)
- [ ] Used on 2+ different codebases without issues
- [ ] No known correctness bugs

1.0 means: API stable, semver enforced, breaking changes = major bump.

---

*Completed phase history archived in `docs/roadmap-archive.md`.*
