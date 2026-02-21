---
name: cqs-drift
description: Detect semantic drift between a reference snapshot and the current project.
disable-model-invocation: false
argument-hint: "<reference> [--threshold 0.95] [--min-drift 0.0] [--lang rust] [--limit 20] [--json]"
---

# Drift Detection

Compares embeddings of same-named functions between a reference snapshot and the current project. Surfaces functions that changed semantically.

## Usage

```bash
# Basic drift detection against a reference
cqs drift v1.0

# Show only significant drift (≥10% change)
cqs drift v1.0 --min-drift 0.1

# Filter by language, limit results
cqs drift v1.0 --lang rust --limit 20

# JSON output
cqs drift v1.0 --json

# Batch mode
echo 'drift v1.0' | cqs batch
```

## Output

Sorted by drift magnitude (most changed first). Each entry shows:
- **drift**: 1.0 - cosine_similarity (0 = unchanged, 1 = completely different)
- **similarity**: cosine similarity between embeddings
- **name**: function/method name
- **file**: source file
- **chunk_type**: Function, Method, Struct, etc.

## Prerequisites

Requires a reference snapshot of the same codebase at a different point in time:
```bash
cqs ref add v1.0 .    # snapshot current state
# ... make changes ...
cqs index             # re-index
cqs drift v1.0        # compare
```
