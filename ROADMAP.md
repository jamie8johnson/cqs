# Roadmap

## Current: v0.12.2

All agent experience features shipped. CLI-only (MCP removed in v0.10.0).

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

### Next — RAG Strengthening

- [x] Web help ingestion — multi-page HTML sites (AuthorIT, MadCap Flare) auto-detected and merged (PR #397)
- [ ] Token-budgeted gather — `cqs gather` with `--tokens N` budget. Pack most relevant chunks into a token limit instead of fixed count.
- [ ] Re-ranking — cross-encoder or second-pass scoring on top-N retrieval results. Biggest retrieval quality win.

### Next — Infrastructure

- [ ] Pre-built release binaries (GitHub Actions)
- [ ] Skill grouping / organization (30+ skills)
- [x] `cqs plan` — skill with 5 task-type templates (v0.12.2)
- [x] `cqs convert` — document-to-Markdown conversion (PR #397)

### Parked

- **VB.NET** — `tree-sitter-vb-dotnet` (git dep). VS2005 project delayed.
- **Pre-built reference packages** (#255) — `cqs ref install tokio`
- **Index encryption** — SQLCipher behind cargo feature flag

### Open Issues

- #270: HNSW LoadedHnsw uses unsafe transmute (upstream hnsw_rs)
- #255: Pre-built reference packages
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

## 1.0 Release Criteria

- [ ] Schema stable for 1+ week of daily use (currently v10)
- [ ] Used on 2+ different codebases without issues
- [ ] No known correctness bugs

1.0 means: API stable, semver enforced, breaking changes = major bump.

---

*Completed phase history archived in `docs/roadmap-archive.md`.*
