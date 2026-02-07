---
name: cqs-batch
description: Execute multiple cqs queries in a single tool call. MCP-only, max 10.
disable-model-invocation: false
argument-hint: "<query1> <query2> ..."
---

# Batch

Call `cqs_batch` MCP tool. Build a `queries` array from the user's arguments.

Each query is `{"tool": "<tool_name>", "arguments": {...}}`. Supported tools: search, callers, callees, explain, similar, stats.

Example: if user says `/cqs-batch search "retry logic" callers search_filtered`, build:
```json
{"queries": [
  {"tool": "search", "arguments": {"query": "retry logic"}},
  {"tool": "callers", "arguments": {"name": "search_filtered"}}
]}
```

Max 10 queries per batch. Returns per-query results or errors.
