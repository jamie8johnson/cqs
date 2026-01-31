# Scars

Limbic memory. Things that hurt, so we don't touch the stove twice.

---

## tree-sitter grammar version mismatch

**Tried:** Using tree-sitter 0.26 with grammar crates pinned to 0.23.x

**Pain:** Mysterious parsing failures, no clear error messages. Worked sometimes, broke others.

**Learned:** Grammar crates have dev-dep on specific tree-sitter versions. The `tree-sitter-language` abstraction papers over it but it's fragile. Keep grammar versions as close to core as possible.

---

## Storing absolute paths in index

**Tried:** Storing full absolute paths like `/home/user/project/src/lib.rs` in chunk IDs

**Pain:** Path pattern filtering broke. Indexes weren't portable. Glob patterns never matched.

**Learned:** Store relative paths. Join with project root for filesystem ops. Test glob patterns early - they're a canary for path issues.

---

## MCP tools/call returning raw JSON

**Tried:** Returning search results as plain JSON from MCP tool calls

**Pain:** Tool ran successfully but results appeared empty in Claude Code. No error messages. Silent failure.

**Learned:** MCP `tools/call` responses MUST wrap in `{"content":[{"type":"text","text":"..."}]}`. The spec is strict.

---

## trailing_var_arg in clap

**Tried:** Using `#[arg(trailing_var_arg = true)]` for multi-word queries without quotes

**Pain:** Flags after the query got eaten as query text. `cqs "foo" -n 5` parsed as query "foo -n 5".

**Learned:** Removed trailing_var_arg. Users quote multi-word queries. Flags work anywhere.

---

## .mcp.json in project root

**Tried:** Putting MCP server config in `.mcp.json` at project root

**Pain:** Claude Code ignored it completely. Server never started. No error.

**Learned:** Claude Code config lives in `~/.claude.json` under `projects["/path"].mcpServers`. Use `claude mcp add`. The `.mcp.json` convention is for other tools.

---

## gh pr checks exit code

**Tried:** Using exit code from `gh pr checks` to determine CI status

**Pain:** Returns exit code 1 if ANY check is pending or skipped, even if critical ones passed. Looked like failure when it wasn't.

**Learned:** Don't trust the exit code. Parse the output or use `--watch` and wait for completion.

---
