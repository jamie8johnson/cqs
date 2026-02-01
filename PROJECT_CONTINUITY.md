# Project Continuity

## Right Now

**BLOCKER: Path traversal in cqs_read** (mcp.rs:563)
- `cqs_read` tool allows `../../../etc/passwd` reads
- Must fix before PR #45 merges

PR #45: https://github.com/jamie8johnson/cqs/pull/45

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
