# cqs - Project Continuity

Updated: 2026-01-31T23:00Z

## Current State

**v0.1.5 published. Audit Phase A fixes in PR #7. 29 tests passing.**

- ~3200 lines across 7 modules
- 29 tests passing (13 parser + 8 store + 8 MCP)
- Published v0.1.3, v0.1.4, v0.1.5 to crates.io
- GitHub repo: github.com/jamie8johnson/cqs
- Automated dependency reviews active
- CI workflow running (build, test, clippy, fmt) - all passing
- GitHub release v0.1.5 created
- Branch ruleset active (main requires CI, blocks force push)
- 16-category audit completed (74 findings documented)

### Version History This Session

| Version | Changes |
|---------|---------|
| v0.1.3 | Watch mode, HTTP transport, .gitignore, CLI restructure |
| v0.1.4 | MCP 2025-11-25 compliance (Origin, Protocol-Version headers) |
| v0.1.5 | GET /mcp SSE stream support, full spec compliance |
| post-v0.1.5 | CI workflow, issue templates, GitHub release |

## Features Complete

### Core
- Semantic code search (5 languages)
- GPU acceleration (CUDA) with CPU fallback
- .gitignore support
- Watch mode with debounce

### MCP
- stdio transport (default)
- HTTP transport (Streamable HTTP 2025-11-25)
  - POST /mcp - JSON-RPC requests
  - GET /mcp - SSE stream for server messages
  - Origin validation
  - MCP-Protocol-Version header
- Tools: cqs_search, cqs_stats

### Automation
- Dependabot for weekly crate PRs
- GitHub Action for MCP spec + model checks
- CI workflow (build, test, clippy, fmt on push/PR)
- Issue templates (bug report, feature request)

## This Session Summary

1. Reviewed hunches, filled checksums
2. Implemented v0.1.3 features (watch, HTTP, gitignore, CLI)
3. Did dependency review - found MCP spec at 2025-11-25
4. Updated to MCP 2025-11-25 (v0.1.4)
5. Added SSE stream support (v0.1.5)
6. Added automated dependency reviews
7. Published v0.1.5 to crates.io
8. Added CI workflow, issue templates, GitHub release
9. **16-category audit** - 74 findings (0 critical, 6 high, 29 medium, 39 low)
10. **CI fixes** - dtolnay/rust-toolchain action, clippy warnings, .cargo/config.toml excluded
11. **Branch ruleset** - main protection via GitHub API (require PR, require CI, block force push)
12. Full MD file review and updates
13. **Audit Phase A fixes** (PR #7):
    - A1: SQL parameterized queries (S1.1 HIGH)
    - A2: Replace glob with globset (D10.2 MEDIUM)
    - A3: Replace fs2 with fs4 (D10.3 MEDIUM)
    - A4: 8 MCP protocol integration tests (T8.1 HIGH)
    - D5: CodeQL badge added to README
    - D6: Community standards (CODE_OF_CONDUCT, CONTRIBUTING, PR template)
14. **Phase 5 (Security)** added to roadmap - index encryption planned
15. Enabled CodeQL analysis and Secret Protection

## Next Steps

1. Merge PR #7 (Phase A audit fixes)
2. Continue with Phase B (RwLock, UUID, rate limiting, query cache)
3. Monitor Dependabot PRs (5 open)
4. Phase 4: HNSW for scale (>50k chunks)

## Blockers

None.
