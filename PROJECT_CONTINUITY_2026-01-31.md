# cqs - Project Continuity

Updated: 2026-01-31T08:45Z

## Current State

**Phase 2 complete. Published v0.1.2.**

- All modules implemented (~2300 lines)
- 21 tests passing (13 parser, 8 store)
- Published to crates.io as `cqs` v0.1.2
- GitHub repo public at github.com/jamie8johnson/cqs
- Index has 296 chunks with doc comments embedded
- MCP integration working

### Index Stats

```
Total chunks: 296
Total files:  20

By language:
  rust: 181, python: 31, go: 31, typescript: 28, javascript: 25

By type:
  function: 173, method: 65, struct: 33, constant: 15, enum: 8, class: 2
```

## This Session

### Phase 2 Implementation Complete

All 6 features implemented and committed:

1. **New chunk types** - Class, Struct, Enum, Trait, Interface, Constant
2. **Hybrid search** - `--name-boost` flag for name matching
3. **Context display** - `-C N` for surrounding lines
4. **Doc comments in embeddings** - Better semantic matching

### Published v0.1.2

- Committed Phase 2 changes
- Bumped version to 0.1.2
- Published to crates.io
- Reindexed with `--force` to pick up doc comment embeddings

## MCP Status

**Working.** Tools available:
- `cqs_search` - semantic code search with filters, name_boost
- `cqs_stats` - index statistics

## Next Steps

1. Test hybrid search effectiveness in real usage
2. Phase 3: VS Code extension, SSE transport
3. Phase 4: HNSW for scale (>50k chunks)

## Blockers

None.

## Decisions Made

- **Relative paths in index**: Makes indexes portable, fixes glob matching
- **Error on invalid language**: Fail fast, don't silently default
- **MCP project path required**: Working directory unpredictable for MCP servers
- **Scale warning at 50k**: Inform users before search becomes slow
- **Signature-aware search deferred**: Name boost covers most cases
- **Flags before query**: Due to clap trailing_var_arg behavior
