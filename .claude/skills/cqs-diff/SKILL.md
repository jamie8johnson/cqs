---
name: cqs-diff
description: Semantic diff between indexed snapshots. Compare project vs reference.
disable-model-invocation: false
argument-hint: "<source_ref> [--target project|<ref>] [--threshold 0.95] [--lang rust]"
---

# Diff

Call `cqs_diff` MCP tool. Parse arguments:

- First positional arg = `source` reference name (required)
- `--target <name>` → `target` (default: "project")
- `--threshold <n>` → similarity threshold for "modified" classification (default: 0.95)
- `--lang <language>` → filter by language

Shows added, removed, and modified functions between two indexed snapshots. Requires references to be set up via `cqs ref add`.
