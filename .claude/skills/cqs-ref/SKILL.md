---
name: cqs-ref
description: Manage reference indexes — add, remove, update, list external codebases for multi-index search.
disable-model-invocation: false
argument-hint: "<add|remove|update|list> [name] [source_path] [--weight 0.8]"
---

# Reference Management

Manage reference indexes for multi-index search. References are read-only indexes of external codebases that surface in search results alongside the primary project.

## Subcommands

### `/cqs-ref add <name> <source_path> [--weight 0.8]`

Run: `cqs ref add <name> <source_path> --weight <weight>`

Steps:
1. Validates the source path exists
2. Creates reference storage directory (`~/.local/share/cqs/refs/<name>/`)
3. Indexes all supported source files
4. Builds HNSW index for the reference
5. Adds reference config to `.cqs.toml`

**Weight** (0.0–1.0, default 0.8): Score multiplier for reference results. Lower = reference results rank below project results. Never exceed 1.0.

### `/cqs-ref list`

Run: `cqs ref list`

Shows all configured references with name, weight, chunk count, and source path.

### `/cqs-ref update <name>`

Run: `cqs ref update <name>`

Re-indexes a reference from its original source path. Incremental — only re-embeds changed files, prunes deleted ones, rebuilds HNSW.

### `/cqs-ref remove <name>`

Run: `cqs ref remove <name>`

Removes reference from config and deletes its storage directory.

## Tips

- Use `/cqs-diff` after adding a reference to compare it against your project
- Use `/cqs-search --sources <name>` to search only within a specific reference
- References are stored in `~/.local/share/cqs/refs/`, not in the project directory
- Config lives in `.cqs.toml` at project root (checked into git)
