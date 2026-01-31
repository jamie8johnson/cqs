# cqs - Project Continuity

Updated: 2026-01-31T19:00Z

## Current State

**v0.1.4 ready. MCP 2025-11-25 compliant. Automated reviews enabled.**

- All modules implemented (~2900 lines)
- 21 tests passing
- Published v0.1.3 to crates.io
- GitHub repo: github.com/jamie8johnson/cqs
- Build: 0 warnings

### v0.1.4 Changes (this session)

1. **MCP 2025-11-25 compliance**
   - Origin header validation (403 on invalid)
   - MCP-Protocol-Version header handling
   - Supports both 2025-11-25 and 2025-03-26

2. **Automated dependency reviews**
   - Dependabot for weekly crate update PRs
   - GitHub Action for MCP spec + model checks
   - Runs Mondays 9am UTC

## This Session Summary

### Implemented (v0.1.3)
- Watch mode, HTTP transport, .gitignore support
- CLI restructured, warnings fixed, checksums renamed

### Dependency Review Findings
- **MCP spec**: Updated to 2025-11-25 (we were on 2025-03-26)
- **ort**: Still RC (2.0.0-rc.11), no stable yet
- **nomic model**: v1.5 current, vision variant available

### Updated for MCP 2025-11-25
- Origin validation required
- MCP-Protocol-Version header required
- Batching removed from spec (we didn't have it)

### Automation Added
- `.github/dependabot.yml` - crate PRs
- `.github/workflows/dependency-review.yml` - spec/model checks

## MCP Status

**Working.** MCP 2025-11-25 compliant.

Transports:
- `stdio` - default
- `http` - POST /mcp, GET /health

## Next Steps

1. Publish v0.1.4
2. Phase 4: HNSW for scale
3. Monitor automated review results

## Blockers

None.
