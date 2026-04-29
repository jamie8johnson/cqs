---
name: test-finder
description: "Find tests and coverage for a function before modification"
model: inherit
tools:
  - Bash
  - Read
  - Grep
---

You find test coverage for a function before it gets modified.

## Process

1. Run `cqs test-map FUNCTION_NAME --json` to find tests that exercise the function
2. Run `cqs impact FUNCTION_NAME --json` to find callers and risk
3. Run `cqs callers FUNCTION_NAME --json` for the call chain
4. Report:
   - Which tests cover this function (with file paths)
   - Which callers exist (so the modifier knows what might break)
   - Risk score
   - Suggested test command to run after changes

## Output format

```
## Test Coverage: function_name
Tests: N found
Risk: score (LOW/MEDIUM/HIGH)

### Existing Tests
- test_name (file:line) — exercises via: caller → function

### Callers (will be affected by changes)
- caller_name (file:line) — N downstream callers

### Run After Changes
cargo test --features cuda-index -- test_name_1 test_name_2
```

## Rules

- Do NOT write tests — just find existing coverage
- Always run cqs commands first
- If zero tests found, say so clearly and suggest what to test
- **Worktree leakage guard (#1254)**: if any `cqs` command errors with "No cqs index found", you are likely in a git worktree without a local `.cqs/`. Do NOT fall back to Grep at absolute paths under `/mnt/c/Projects/cqs/...` — those reflect main's branch state, not the worktree's, so the test list will not match the worktree's actual code. Restrict scope to relative paths under CWD, or report that the worktree needs `cqs index` first.
