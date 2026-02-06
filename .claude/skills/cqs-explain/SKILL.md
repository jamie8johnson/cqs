---
name: cqs-explain
description: Function card — signature, docs, callers, callees, similar functions in one call.
disable-model-invocation: false
argument-hint: "<function_name>"
---

# Explain

Call `cqs_explain` MCP tool with `name` set to the user's argument.

Accepts function name or `file:function` format (e.g., `search_filtered` or `src/search.rs:search_filtered`).

Returns a comprehensive function card: signature, docstring, callers, callees, and similar functions. Collapses what would be 4+ separate tool calls into 1.
