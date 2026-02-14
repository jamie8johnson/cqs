---
name: cqs-health
description: Codebase quality snapshot — dead code, staleness, hotspots, test coverage gaps in one call.
---

# cqs health

Composite quality report. Runs stats + dead code + staleness + hotspot analysis in one command.

## Usage

```bash
cqs health              # dashboard text output
cqs health --json       # structured JSON
```

## Output includes

- Index stats (chunks, files, schema, model)
- HNSW index status
- Staleness (stale + missing files)
- Dead code counts (confident + possible)
- Top 5 hotspots (most-called functions)
- Untested hotspots (high-caller, zero tests)
- Note stats (total, warnings)

## When to use

- Quick project health check before starting work
- After major refactoring to assess impact
- Periodic quality monitoring

## Example

```bash
# Quick health check
cqs health

# Parse JSON programmatically
cqs health --json | jq '.dead_code.confident'
```
