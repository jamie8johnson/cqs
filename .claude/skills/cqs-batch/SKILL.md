---
name: cqs-batch
description: Execute multiple cqs queries in a single persistent session via stdin batch mode.
disable-model-invocation: false
argument-hint: "<commands>"
---

# Batch Mode

`cqs batch` reads commands from stdin, outputs compact JSONL. Persistent Store + lazy Embedder — amortizes startup cost across N commands.

## Usage

```bash
# Single command
echo 'callers search_filtered' | cqs batch

# Multiple commands
printf 'callers gather\nexplain gather\nstats\n' | cqs batch

# From file
cqs batch < commands.txt
```

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

## Output

Compact JSONL — one JSON object per line, flushed after each command. Errors: `{"error":"message"}`.
