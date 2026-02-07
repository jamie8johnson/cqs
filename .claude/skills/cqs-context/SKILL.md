---
name: cqs-context
description: Module-level understanding — chunks, callers, callees, notes for a file.
disable-model-invocation: false
argument-hint: "<file_path>"
---

# Context

Call `cqs_context` MCP tool with `path` set to the user's argument.

Returns a module overview for the given file: all chunks (signatures), external callers, external callees, dependent files, and related notes. Useful for understanding a file's role before making changes.
