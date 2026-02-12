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
- Web help ingestion — multi-page HTML sites (AuthorIT, MadCap Flare) (PR #397)
- Token-budgeted output — `--tokens N` on query, gather, context, explain, scout (PR #398)
- Cross-index gather — `cqs gather --ref` (PR #399)
- `cqs plan` — skill with 5 task-type templates (v0.12.2)
- `cqs convert` — document-to-Markdown conversion (PR #397)

### Next — New Commands

Priority order based on competitive gap analysis (Feb 2026).

- [ ] `cqs review` — structured review context from a diff. Combines impact-diff + gather + notes + risk score. JSON for agent consumption.
- [ ] `cqs ci` — CI pipeline mode. Impact analysis on PR diff, suggested test targets, dead code introduced, risk score. Exit codes for CI gates.
- [ ] `cqs health` — codebase quality snapshot. Dead code count, stale files, untested high-impact functions, hotspots, note warnings.
- [ ] `cqs onboard "concept"` — guided codebase tour. Entry point → call chain → key types → tests. Ordered reading list from gather + trace + explain.

### Next — Retrieval Quality

- [ ] Re-ranking — cross-encoder or second-pass scoring on top-N retrieval results. Biggest retrieval quality win.
- [ ] Change risk scoring — `risk = caller_count * (1 - test_coverage) * centrality`. Feeds into review/ci/health.
- [ ] Embedding model eval — benchmark current E5-base-v2 against CodeSage, UniXcoder, Nomic Code on existing eval harness. Quantify gap before committing to upgrade.

### Next — Expansion

- [ ] C# language support — biggest missing language by market size
- [ ] Pre-built release binaries (GitHub Actions) — adoption friction
- [ ] Skill grouping / organization (30+ skills)

### Parked

- **MCP server** — re-add as slim read-only wrapper when CLI features are rock solid. Architecture proven clean (removed in v0.10.0 with zero core changes).
- **VB.NET** — `tree-sitter-vb-dotnet` (git dep). VS2005 project delayed.
- **Pre-built reference packages** (#255) — `cqs ref install tokio`
- **Index encryption** — SQLCipher behind cargo feature flag
- **Query-intent routing** — auto-boost ref weight when query mentions product names
- **Pattern mining** (`cqs patterns`) — recurring code conventions. Large effort, defer.
- **Post-index name matching** — fuzzy cross-doc references

### Open Issues

- #270: HNSW LoadedHnsw uses unsafe transmute (upstream hnsw_rs)
- #255: Pre-built reference packages
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)
- #389: CAGRA GPU memory — needs disk persistence layer

## 1.0 Release Criteria

- [ ] Schema stable for 1+ week of daily use (currently v10)
- [ ] Used on 2+ different codebases without issues
- [ ] No known correctness bugs

1.0 means: API stable, semver enforced, breaking changes = major bump.

---

*Completed phase history archived in `docs/roadmap-archive.md`.*
