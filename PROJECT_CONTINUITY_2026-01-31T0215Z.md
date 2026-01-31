# cq - Project Continuity

Updated: 2026-01-31T07:30Z

## Current State

**Design complete (v0.13.0), implementation ready.**

- DESIGN.md has full implementation code for all components
- 7 audit rounds completed, 0 critical/high issues
- MCP moved to Phase 1
- Testing strategy defined (unit, integration, eval)
- Name `cq` confirmed available on crates.io

## Recent Changes (This Session)

- Ran comprehensive 7-category audits (security, performance, edge cases, code correctness, types/API, UX, dependencies)
- Fixed all critical/high issues:
  - Compilation errors (mutability, type mismatches)
  - Missing types (ChunkSummary, ChunkRow, ModelInfo, IndexStats)
  - Missing implementations (Store methods, Embedder::new, etc.)
  - Two-phase search for memory efficiency
  - Parser query caching
- Added comprehensive MCP Integration section (~300 lines)
- Added Testing Strategy section (~250 lines)
- Moved MCP to Phase 1 per user request
- Updated CLAUDE.md to include HUNCHES.md and ROADMAP.md in tears

## Blockers / Open Questions

None. Ready to implement.

## Next Steps

1. **Plan implementation** - Break Phase 1 into tasks
2. **Implement Parser** - Most foundational module
3. **Implement Embedder** - Needs model download first
4. **Implement Store** - Depends on Chunk type from Parser
5. **Implement CLI** - Ties it all together
6. **Implement MCP** - cq serve with stdio
7. **Write tests** - Unit, integration, eval

## Decisions Made

- **MCP in Phase 1**: User wants Claude Code integration from the start
- **Two-phase search**: Load only id+embedding for scoring, fetch content for top-N
- **Parser caches queries**: Pre-compile tree-sitter queries once
- **Testing**: All three tiers (unit, integration, eval) with 80% recall@5 target
- **Name**: `cq` confirmed (available on crates.io)
- **WSL workaround**: Use `powershell.exe` for git push (Windows has credentials)
