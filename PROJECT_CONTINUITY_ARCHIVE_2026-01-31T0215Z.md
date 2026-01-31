# cq - Archive

Session log and detailed notes.

---

## Session: 2026-01-31

### Bootstrap

Ran bootstrap per CLAUDE.md instructions:
- Created docs/ directory
- Created SESSION_CONTEXT.md, HUNCHES.md from templates
- Created ROADMAP.md from template
- Created tear files (this file and PROJECT_CONTINUITY)
- Scaffolded Cargo.toml per DESIGN.md dependencies section
- Created GitHub repo

Design doc version: 0.6.1-draft

Key architecture decisions from design doc:
- tree-sitter for parsing (not syn) - multi-language support
- ort + tokenizers for embeddings (not fastembed-rs) - GPU control
- nomic-embed-text-v1.5 model (768-dim, 8192 context)
- SQLite with WAL mode for storage
- Brute-force search initially, HNSW in Phase 4

---
