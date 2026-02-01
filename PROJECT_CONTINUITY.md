# Project Continuity

## Right Now

**PR #45** - Hunches + Scars + Security

https://github.com/jamie8johnson/cqs/pull/45

Session work:
- Hunches as indexed entities (entity type 2)
- Scars as indexed entities (entity type 3)
- Path traversal fix in cqs_read
- 12 MCP integration tests
- 18-category audit (verified clean)
- blake3 checksums for HNSW (mitigates bincode RUSTSEC-2025-0141)
- Schema v7 with scars table + FTS

## Entity Types

| Type | Purpose | Source | Display |
|------|---------|--------|---------|
| 1. Code | Functions, methods, structs | Source files | Standard |
| 2. Hunch | Soft observations | docs/hunches.toml | Yellow `[hunch]` |
| 3. Scar | Failed approaches | docs/scars.toml | Red `[scar]` |

Hunches = optional (--no-hunches flag)
Scars = always included (limbic memory, protective reflex)

## Scar Format (TOML)

```toml
[[scar]]
date = "2026-01-15"
title = "tree-sitter grammar version mismatch"
mentions = ["tree-sitter", "parser.rs"]
tried = "Using tree-sitter 0.26 with grammar crates pinned to 0.23.x"
pain = "Mysterious parsing failures, no clear error messages."
learned = "Keep grammar versions as close to core as possible."
```

## Parked

- C/Java language support
- `/tears` command for auto state capture
- Pre-compaction hook
- Session state persistence (entity type 4?)

## Blockers

None.
