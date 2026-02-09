---
name: cqs-diff
description: Semantic diff between indexed snapshots. Compare project vs reference.
disable-model-invocation: false
argument-hint: "<source_ref> [--target project|<ref>] [--threshold 0.95] [--lang rust]"
---

# Diff

Parse arguments:
- First positional arg = `source` reference name (required)
- Second positional arg or `--target <name>` = target (default: "project")
- `--threshold <n>` — similarity threshold for "modified" classification (default 0.95)
- `--lang <language>` — filter by language

Run via Bash: `cqs diff "<source>" [target] [--threshold F] [--lang L] --json -q`

Present the results to the user. Shows added, removed, and modified functions between two indexed snapshots. Requires references set up via `cqs ref add`.
