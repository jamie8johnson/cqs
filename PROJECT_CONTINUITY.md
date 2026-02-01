# Project Continuity

## Right Now

**Hunches feature COMPLETE. All code compiles, all tests pass.**

What was built this session:
- `src/hunch.rs` - TOML parser for `docs/hunches.toml`
- Schema v6 with `hunches` table + FTS5
- `store.rs` - hunch storage, unified search (code + hunches together)
- `cli.rs` - hunches indexed during `cqs index`, `--no-hunches`, `--include-resolved` flags
- `mcp.rs` - `cqs_search` now returns hunches, new `cqs_read` tool injects context
- `docs/hunches.toml` - sample entries created
- README updated with `<claude>` section for AI instances
- Audit expanded to 18 categories (added Community + Promotion)

**Ready for PR.** Uncommitted files:
- src/hunch.rs (new)
- src/lib.rs, src/schema.sql, src/store.rs, src/cli.rs, src/mcp.rs
- tests/store_test.rs (schema version bump)
- docs/hunches.toml (new), docs/HUNCHES.md (keep for reference)
- docs/AUDIT_2026-01-31_16CAT.md (now 18 categories)
- CLAUDE.md, README.md

## Key Insight

cqs is not "semantic code search". It's **Tears** - context persistence for AI collaborators. Code search was just the first entity type. Hunches are the second. Scars and tears proper are next.

## MCP Tools Now

- `cqs_search` - returns code + hunches unified
- `cqs_read` - reads file with relevant hunches injected as header comments
- `cqs_stats`, `cqs_callers`, `cqs_callees` - unchanged

## Hunch Format (TOML)

```toml
[[hunch]]
date = "2026-01-31"
title = "Example"
severity = "high"  # high, med, low
confidence = "med"
resolution = "open"  # open, resolved, accepted
mentions = ["file.rs"]
description = """
Multi-line description.
"""
```

## Parked

- C/Java language support
- `/tears` command for auto state capture
- Scars indexing
- Pre-compaction hook

## Blockers

None.
