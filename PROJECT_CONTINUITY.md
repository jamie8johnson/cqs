# Project Continuity

## Right Now

**Implementing unified notes + 769-dim sentiment + cuVS**

Branch: `feat/cuvs-gpu-search`

### Done this session:
- Merged PR #45
- Created notes.toml (migrated scars + hunches)
- Created src/note.rs with sentiment field
- Updated embedder.rs: with_sentiment(), EMBEDDING_DIM=769
- Updated store.rs + hnsw.rs: 768→769 everywhere
- Code compiles clean

### Key architecture decisions:
- `sentiment` field (-1.0 to +1.0) replaces pain/gain/type fields
- Natural language carries valence, sentiment makes it explicit/searchable
- 769th embedding dimension = sentiment baked into similarity
- cuVS CAGRA feature-flagged as `gpu-search`

### Fleet architecture (not implemented):
- Multiple Claudes share notes.toml via Git
- Any Claude can append notes (cqs_add_note MCP tool)
- "Curator" role is an impulse, not dedicated agent
- Append-only, truth emerges from pile
- cqs is MCP server first, CLI second
- Consumer is Claude, not human

## Schema

```toml
[[note]]
sentiment = -0.8
text = "tree-sitter 0.26 breaks with 0.23 grammars"
mentions = ["tree-sitter"]
```

## Files Modified

- src/note.rs (NEW)
- src/embedder.rs (with_sentiment, 769 dim)
- src/store.rs (769 dim)
- src/hnsw.rs (769 dim)
- src/lib.rs (exports note module)
- docs/notes.toml (NEW - migrated content)
- Cargo.toml (gpu-search feature)

## Next

1. Add cqs_add_note MCP tool
2. Update cli.rs for note display
3. Wire notes into search results
4. Test reindex with 769-dim
5. Delete old scar.rs, hunch.rs after verification

## Parked

- cuVS CAGRA implementation (needs CUDA environment)
- Curator agent architecture (design done, impl later)
- Fleet coordination (Git append-only model)
