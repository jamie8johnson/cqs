---
name: cqs-scout
description: Pre-investigation dashboard for task planning. Search, group by file, show caller/test counts, staleness, and relevant notes.
disable-model-invocation: false
argument-hint: "<task description>"
---

# Scout

Parse arguments:
- First positional arg = task description (required)
- `-n/--limit <n>` → max file groups to return (default 5)
- `--tokens <N>` → token budget (includes chunk content within budget)

Run via Bash: `cqs scout "<task description>" [-n N] [--tokens N] --json -q`

Returns a compact planning dashboard with:
- **File groups**: files ranked by relevance, each with chunks showing:
  - Signature, caller count, test count
  - Role: `modify_target` (score >= 0.5), `test_to_update`, or `dependency`
  - Staleness status
- **Relevant notes**: project notes whose mentions overlap with result files
- **Summary**: total files, functions, untested count, stale count

Use this as the first step when starting a new task. Replaces the typical search -> read -> callers -> tests -> notes workflow with a single command.
