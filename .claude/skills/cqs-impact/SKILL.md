---
name: cqs-impact
description: What breaks if you change X? Callers with snippets + affected tests.
disable-model-invocation: false
argument-hint: "<function_name> [--depth 1]"
---

# Impact

Parse arguments:
- First positional arg = function name to analyze (required)
- `--depth <n>` — caller depth, 1 = direct only (default 1)

Run via Bash: `cqs impact "<name>" [--depth N] --json -q`

Present the results to the user. Returns callers with call-site snippets and affected tests via reverse BFS.
