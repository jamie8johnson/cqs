---
name: cqs-suggest
description: Auto-detect note-worthy patterns — dead code clusters, untested hotspots, high-risk functions.
---

# cqs suggest

Scans the index for anti-patterns and suggests notes to add.

## Usage

```bash
cqs suggest              # dry-run: show suggestions
cqs suggest --apply      # add suggested notes to docs/notes.toml
cqs suggest --json       # JSON output
```

## Detectors

1. **Dead code clusters** — files with 5+ dead functions (sentiment: -0.5)
2. **Untested hotspots** — functions with 5+ callers and 0 tests (sentiment: -0.5)
3. **High-risk functions** — risk_level == High (sentiment: -1.0)

## Deduplication

Suggestions are filtered against existing notes — substring match prevents duplicates.

## When to use

- After indexing a new codebase to surface quality issues
- During periodic grooming (`/groom-notes`)
- After major refactoring to flag new hotspots

## Example

```bash
# See what patterns exist
cqs suggest

# Auto-add all suggestions
cqs suggest --apply

# Check how many suggestions
cqs suggest --json | jq 'length'
```
