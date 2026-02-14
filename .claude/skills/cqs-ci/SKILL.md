---
name: cqs-ci
description: CI pipeline analysis — impact + risk + dead code + gate logic with exit codes.
disable-model-invocation: false
argument-hint: "[--base HEAD~1] [--stdin] [--gate high|medium|off]"
---

# CI Analysis

Parse arguments:
- `--base <ref>` — git ref to diff against (default: unstaged changes). Examples: `HEAD`, `HEAD~1`, `main`
- `--stdin` — read diff from stdin instead of running git diff
- `--format <fmt>` — output format: `text` (default), `json`, `mermaid`
- `--json` — alias for `--format json`
- `--gate <level>` — gate threshold: `high` (default), `medium`, `off`
- `--tokens <N>` — token budget for output

Run via Bash: `cqs ci [--base <ref>] [--stdin] [--gate high] [--json] [--tokens N]`

If no arguments, runs `git diff` on unstaged changes.

Composes review_diff (impact + risk + notes + staleness), dead code filtered to diff files, and gate evaluation.

Exit codes:
- 0: gate passed (or `--gate off`)
- 3: gate failed (risk threshold exceeded)

Present the results: gate status, risk summary, changed functions with risk levels, dead code in diff files, tests to re-run, affected callers.
