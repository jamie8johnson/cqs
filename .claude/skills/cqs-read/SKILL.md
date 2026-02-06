---
name: cqs-read
description: Read a file with contextual notes injected as comments. Richer than raw Read.
disable-model-invocation: false
argument-hint: "<path>"
---

# CQS Read

Call `cqs_read` MCP tool with `path` set to the user's argument (relative to project root).

Returns the file contents with relevant notes and observations injected as comments at the appropriate locations. Use instead of raw `Read` tool when you want contextual awareness from prior sessions.
