---
name: cqs-impact-diff
description: Impact analysis from a git diff — affected callers + tests to re-run.
disable-model-invocation: false
argument-hint: "[--base HEAD~1] [--stdin]"
---

# Impact Diff

Parse arguments:
- `--base <ref>` — git ref to diff against (default: unstaged changes). Examples: `HEAD`, `HEAD~1`, `main`
- `--stdin` — read diff from stdin instead of running git diff

Run via Bash: `cqs impact-diff [--base <ref>] [--stdin] --json`

If no arguments, runs `git diff` on unstaged changes.

Present the results: changed functions, affected callers, and tests that need re-running. Summary includes counts.
