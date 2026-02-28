# Roadmap

## Current: v0.19.3

All agent experience features shipped. CLI-only (MCP removed in v0.10.0). 20 languages. Two full audits complete (v0.12.3 + v0.19.2).

### Next — Commands

- [ ] `cqs blame` — semantic git blame. Given a function, show who last changed it, when, and the commit message. Combines call graph with git log.
- [ ] `cqs chat` — interactive REPL for chained queries. Batch mode + pipeline syntax done; REPL deferred (agents use batch).

### Next — Performance

- [ ] PF-5: Lightweight HNSW candidate fetch (#510) — fetch only `(id, embedding)` for scoring, load full content only for top-k survivors.

### Next — Expansion

- [ ] Pre-built release binaries (GitHub Actions) — adoption friction

### Future Languages — Priority Order

**Structured schemas (better RAG, not just code search):**
- [ ] **Protobuf** — message → Struct, service → Interface, rpc → Function, enum → Enum. Every microservices shop has `.proto` files.
- [ ] **GraphQL** — type/input → Struct, query/mutation/subscription → Function, interface → Interface, enum → Enum. Every web API shop has these.

**Programming languages with clean mappings:**
- [ ] **Elixir** — Module + Macro exist. defprotocol → Trait, defrecord → Struct. Clean mapping.
- [ ] **Lua** — Function-only. Game dev niche (Roblox, Neovim). Easy.
- [ ] **Haskell** — TypeAlias exists. data → Enum, class → Trait. Niche but loved.
- [ ] **PHP** — Property covers properties, trait → Trait
- [ ] **Dart** — Property covers properties, mixin → Trait
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

- #510: PF-5 — lightweight HNSW candidate fetch
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
