# cqs - Project Continuity

Updated: 2026-01-31T22:30Z

## Current State

**v0.1.5 published. Full MCP 2025-11-25 compliance. CI green. Branch protected.**

- ~3000 lines across 7 modules
- 21 tests passing
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

## Next Steps

1. Phase 4: HNSW for scale (>50k chunks)
2. Monitor automated review results (weekly)
3. Address audit HIGH findings (S1.1, C5.1, T8.1, D10.1-3)

## Blockers

None.
