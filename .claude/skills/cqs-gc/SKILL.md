---
name: cqs-gc
description: Reports staleness (CLI version prunes + rebuilds).
disable-model-invocation: false
argument-hint: ""
---

# GC

Call `cqs_gc` MCP tool. No arguments.

Reports index staleness: stale file count and missing file count. The CLI version (`cqs gc`) prunes chunks for deleted files, cleans orphan call graph entries, and rebuilds HNSW. The MCP version is read-only.
