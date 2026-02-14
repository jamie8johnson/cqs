---
name: cqs-review
description: Comprehensive diff review — impact + notes + risk scoring + staleness.
disable-model-invocation: false
argument-hint: "[--base HEAD~1] [--stdin] [--format text|json|mermaid]"
---

# Review

Parse arguments:
- `--base <ref>` — git ref to diff against (default: unstaged changes). Examples: `HEAD`, `HEAD~1`, `main`
- `--stdin` — read diff from stdin instead of running git diff
- `--format <fmt>` — output format: `text` (default), `json`, `mermaid`
- `--json` — alias for `--format json`
- `--tokens <N>` — token budget (truncates callers/tests lists)

Run via Bash: `cqs review [--base <ref>] [--stdin] [--json] [--tokens N]`

If no arguments, reviews unstaged changes.

Composes impact analysis, risk scoring, note matching, and staleness into a single structured review. More detailed than `cqs ci` (no gate logic, no dead code — pure review).

Present the results: changed functions with risk levels, affected callers, tests to re-run, relevant notes, staleness warnings.
