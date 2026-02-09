---
name: cqs-gc
description: Reports staleness (CLI version prunes + rebuilds).
disable-model-invocation: false
argument-hint: ""
---

# GC

Run via Bash: `cqs gc --json -q`

Present the results to the user. Reports index staleness: stale file count and missing file count. The CLI `gc` command also prunes chunks for deleted files, cleans orphan call graph entries, and rebuilds HNSW.
