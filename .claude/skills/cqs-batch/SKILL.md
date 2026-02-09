---
name: cqs-batch
description: Execute multiple cqs queries in parallel via Bash.
disable-model-invocation: false
argument-hint: "<query1> <query2> ..."
---

# Batch

No single CLI command for batch queries. Instead, run multiple `cqs` commands in parallel via Bash tool calls.

Parse the user's arguments into individual commands. Supported operations:

- `search "<query>"` → `cqs "<query>" --json -q`
- `callers <name>` → `cqs callers <name> --json -q`
- `callees <name>` → `cqs callees <name> --json -q`
- `explain <name>` → `cqs explain <name> --json -q`
- `similar <name>` → `cqs similar <name> --json -q`
- `stats` → `cqs stats --json -q`

Example: if user says `/cqs-batch search "retry logic" callers search_filtered`, run two parallel Bash calls:
1. `cqs "retry logic" --json -q`
2. `cqs callers search_filtered --json -q`

Present all results together.
