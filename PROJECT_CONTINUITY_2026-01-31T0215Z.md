# cq - Project Continuity

Updated: 2026-01-31

## Current State

Project bootstrapped. Repo exists with design doc (v6.1), docs created. Cargo.toml set up per spec. GitHub repo created. No code written yet.

## Recent Changes

- Created docs/ directory with SESSION_CONTEXT.md and HUNCHES.md
- Created ROADMAP.md
- Created tear files
- Scaffolded Cargo.toml with all Phase 1 dependencies
- Created GitHub repo: cq (public)

## Blockers / Open Questions

- **Push to GitHub blocked**: `gh` CLI not installed in WSL, git credential helper not configured
- crates.io name claim requires publishing a placeholder—defer until we have something runnable
- Need to verify tree-sitter grammar versions match tree-sitter 0.26
- WSL permission issues require using `CARGO_TARGET_DIR=~/cq-target` for builds

## Next Steps

1. Create src/main.rs stub
2. Create lib.rs with module structure
3. Implement Parser module first (most foundational)
4. Test parser with sample files in each language

## Decisions Made

- Using tree-sitter over syn for multi-language support
- Using ort + tokenizers directly for GPU control
- SQLite for storage (simple, reliable, good enough for MVP)
