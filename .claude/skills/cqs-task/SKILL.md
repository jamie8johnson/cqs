---
name: cqs-task
description: Single-call implementation brief — scout + gather + impact + placement + notes with shared resources and waterfall token budgeting.
disable-model-invocation: false
argument-hint: "<task description>"
---

# Task

Parse arguments:
- First positional arg = task description (required)
- `-n/--limit <n>` → max file groups from scout phase (default 5)
- `--tokens <N>` → token budget (waterfall across sections)

Run via Bash: `cqs task "<task description>" [-n N] [--tokens N] --json 2>/dev/null`

Returns a complete implementation brief in one call:
- **Scout**: file groups ranked by relevance with chunks showing signature, caller count, test count, role, staleness
- **Code**: BFS-expanded source from modify targets (full content)
- **Impact**: per-target risk assessment (score, callers, coverage, blast radius)
- **Tests**: affected tests across all modify targets (deduped)
- **Placement**: where to add new code with local patterns
- **Notes**: relevant project notes matching result files
- **Summary**: total files, functions, modify targets, high-risk count, test count, stale count

Replaces the manual scout → gather → impact → where → notes workflow. Loads call graph and test chunks once instead of per-phase.

Use `--tokens` for waterfall budgeting: scout 15%, code 50%, impact 15%, placement 10%, notes 10% — unused budget flows to next section.
