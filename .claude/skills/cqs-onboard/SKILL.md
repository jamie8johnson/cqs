---
name: cqs-onboard
description: Guided codebase tour — given a concept, produces an ordered reading list with entry point, call chain, callers, key types, and tests.
disable-model-invocation: false
argument-hint: "<concept>"
---

# Onboard

Parse arguments:
- First positional arg = concept to explore (required)
- `-d/--depth <n>` → callee BFS expansion depth (default 3, max 5)
- `--tokens <N>` → token budget (packs content within budget by priority)

Run via Bash: `cqs onboard "<concept>" [-d N] [--tokens N] --json 2>/dev/null`

Returns an ordered reading list for understanding a concept:
- **entry_point**: Best function/method matching the concept (prefers callable types with call graph connections)
- **call_chain**: Callees via BFS, sorted by depth then file/line
- **callers**: Who calls the entry point (1 level)
- **key_types**: Type dependencies of the entry point (common types like String, Vec filtered)
- **tests**: Tests exercising the entry point via reverse call graph
- **summary**: Total items, files covered, callee depth, tests found

Use this when an agent needs to understand a concept from scratch. Replaces the manual workflow of scout -> read -> callers -> callees -> test-map -> explain with a single call.

Token budgeting with `--tokens N` strips content from lower-priority entries to fit within budget. Entry point always included. Higher depth = lower priority for call chain.
