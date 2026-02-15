---
name: cqs-batch
description: Execute multiple cqs queries in a single persistent session via stdin batch mode.
disable-model-invocation: false
argument-hint: "<commands>"
---

# Batch Mode

`cqs batch` reads commands from stdin, outputs compact JSONL. Persistent Store + lazy Embedder — amortizes startup cost across N commands. Supports pipeline syntax for chaining commands.

## Usage

```bash
# Single command
echo 'callers search_filtered' | cqs batch

# Multiple commands
printf 'callers gather\nexplain gather\nstats\n' | cqs batch

# Pipeline — chain commands via |
echo 'callees dispatch | callers | test-map' | cqs batch

# From file
cqs batch < commands.txt
```

## Pipeline Syntax

Chain commands with `|` — upstream names feed downstream commands via fan-out:

```bash
# Search → get callers of each result
echo 'search "error handling" --limit 5 | callers' | cqs batch

# 3-stage: callees → callers of each → test coverage
echo 'callees main | callers | test-map' | cqs batch

# Dead code → explain each
echo 'dead --min-confidence high | explain' | cqs batch
```

**Pipeable downstream commands:** callers, callees, explain, similar, impact, test-map, related.

Pipeline output is a JSON envelope:
```json
{"pipeline": "...", "stages": N, "results": [{"_input": "name", "data": ...}], "errors": [...], "total_inputs": N, "truncated": false}
```

**Limits:** Max 50 names per stage (prevents fan-out explosion). Quoted pipes (`search "foo | bar"`) are not treated as separators.

## Supported Commands

| Command | Example |
|---------|---------|
| `search <query>` | `search "error handling" --limit 3` |
| `callers <name>` | `callers search_filtered` |
| `callees <name>` | `callees gather` |
| `explain <name>` | `explain search_filtered` |
| `similar <name>` | `similar gather --limit 3` |
| `gather <query>` | `gather "retry logic" --expand 2` |
| `impact <name>` | `impact search_filtered --depth 2` |
| `test-map <name>` | `test-map search_filtered` |
| `trace <src> <tgt>` | `trace main search_filtered` |
| `dead` | `dead --min-confidence high` |
| `related <name>` | `related gather --limit 3` |
| `context <path>` | `context src/lib.rs --compact` |
| `stats` | `stats` |

## Input Format

- One command per line
- `#` comments and empty lines are skipped
- `quit` or `exit` stops processing
- Quoted strings supported: `search "multi word query"`
- `|` chains commands (pipeline syntax)

## Output

Compact JSONL — one JSON object per line, flushed after each command. Pipelines produce a single envelope per pipeline. Errors: `{"error":"message"}`.
