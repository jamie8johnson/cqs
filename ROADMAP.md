# Roadmap

## Current: v0.21.0

All agent experience features shipped. CLI-only (MCP removed in v0.10.0). 31 languages. Two full audits complete (v0.12.3 + v0.19.2).

### Next — Commands

- [x] `cqs blame` — semantic git blame. Given a function, show who last changed it, when, and the commit message. Combines call graph with git log.
- [x] `cqs chat` — interactive REPL for chained queries. Readline, history, tab completion. Wraps batch mode.

### Next — Performance

- [x] PF-5: Lightweight HNSW candidate fetch (#510) — fetch only `(id, embedding)` for scoring, load full content only for top-k survivors.

### Next — Expansion

- [ ] Pre-built release binaries (GitHub Actions) — adoption friction

### Future Languages — Priority Order

- [x] **Elixir** — Module + Macro exist. defprotocol → Interface, defimpl → Object. Clean mapping.
- [x] **Erlang** — FP + modules, behaviour → Interface, record → Struct.
- [x] **Haskell** — data → Enum, newtype → Struct, type synonym → TypeAlias, class → Trait, instance → Object.
- [ ] **Clojure** — Blocked: `tree-sitter-clojure` 0.1.0 requires tree-sitter ^0.25, incompatible with 0.26.
- [ ] **OCaml** — FP + modules. Uses `LANGUAGE_OCAML` export.
- [ ] **Julia** — Scientific + types.
- [ ] **Gleam** — FP + types.
- [ ] **CSS** — Selectors + rules. Rule sets → Section.
- [ ] **Perl** — Subs + packages. OOP via bless.
- [ ] **Dart** — Blocked: old tree-sitter API (pre-0.24). Property covers properties, mixin → Trait.
- [ ] **ArchestrA QuickScript** — No tree-sitter grammar exists. Needs custom grammar from scratch (VB-like syntax).

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
