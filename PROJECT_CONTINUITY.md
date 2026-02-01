# Project Continuity

## Right Now

**Notes are the unified memory system. Scars/hunches removed.**

Branch: main

### Done this session:
- Removed scar.rs, hunch.rs (source files)
- Removed Scar/Hunch imports, structs, methods from store.rs
- Removed UnifiedResult::Scar and UnifiedResult::Hunch variants
- Simplified search_unified to only return Code + Note
- Updated cli.rs: removed hunch/scar indexing, display, CLI flags
- Updated mcp.rs: removed hunch/scar from search results, tool_read uses notes
- Updated tests to use notes instead of hunches
- 769-dim embeddings working (768 model + 1 sentiment)
- Schema v8 with notes table

### Remaining cleanup (optional):
- Delete docs/scars.toml, docs/hunches.toml (content already in notes.toml)
- Remove hunch.rs, scar.rs from lib.rs if not already done

## Key Architecture

- **769-dim embeddings**: 768 from nomic-embed-text + 1 sentiment
- **Notes**: unified memory (text + sentiment + mentions)
- **Sentiment**: -1.0 to +1.0, baked into similarity search
- **Fleet architecture**: Multiple Claudes share notes.toml via Git

## Parked

- cuVS CAGRA implementation (needs conda or RAPIDS build)
- Curator agent architecture (design done)
- Fleet coordination (Git append-only model)
