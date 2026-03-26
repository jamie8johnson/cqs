# Check My Work

Run after making changes, before committing. Reviews your diff for impact and risk.

## Arguments

None — operates on the current git diff.

## Process

1. Run `cqs review --json` via Bash (analyzes current diff)
2. If no diff, say "No changes to review" and stop
3. Present results as a review checklist

## Output Format

```
## Diff Review

### Changed Functions
<from review: list of modified functions with risk scores>

### Affected Callers
<from review: callers of changed functions that may need updates>

### Tests to Re-run
<from review: tests that exercise the changed functions>

### Risk Assessment
<from review: overall risk level and reasoning>

### Checklist
- [ ] All affected callers still work with your changes
- [ ] Tests listed above pass: `cargo test --features gpu-index -- <test_names>`
- [ ] No new warnings from `cargo clippy --features gpu-index`
- [ ] Changes formatted: `cargo fmt`
```

## Rules

- If `cqs review` returns no changed functions, the changes may be in non-indexed files (docs, config) — note this and skip the impact analysis
- If risk is High, recommend the user review each affected caller manually before committing
- If no tests cover the changed functions, flag prominently
