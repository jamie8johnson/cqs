# Roadmap Archive

Completed phases, moved from ROADMAP.md to reduce token cost on resume.

## Phase 1: MVP (v0.1.0–v0.1.1)

- Parser (tree-sitter, 5 languages), Embedder (ort + CUDA/CPU), Store (SQLite WAL)
- CLI: init, doctor, index, query, stats, serve
- MCP server (stdio), eval suite (98% Recall@5)

## Phase 2: Polish (v0.1.2)

- GPU acceleration (6ms query), more chunk types, hybrid search, doc comments in embeddings

## Phase 3: Integration (v0.1.3–v0.1.5)

- .gitignore support, watch mode, HTTP transport, MCP compliance, CI, Dependabot

## Phase 4: Multi-index (v0.1.8–v0.1.9)

- HNSW index (O(log n) search), P0 audit fixes, SIMD cosine similarity
- Shell completions, config file, lock file, CHANGELOG

## Phase 5: Quality (v0.1.10–v0.4.6)

- Incremental indexing, RRF hybrid search, Code→NL→Embed pipeline
- Call graph analysis, E5-base-v2 model, GPU query embedding
- Three 20-category audits (~520 findings total), schema migration framework
- C and Java languages, parser consolidation, 50+ new tests, template experiments
- Note management tools, note grooming, multi-index (references)

## Phase 6: Discovery & UX (v0.7.0)

- `cqs similar`, `cqs explain`, `cqs diff`, workspace-aware indexing

## Phase 7: Token Efficiency (v0.8.0)

- `cqs trace`, `cqs impact`, `cqs test-map`, `cqs batch` (MCP), `cqs context`
- Focused `cqs read`, shared call graph infrastructure

## Post-v0.8.0: Uncharted Features (v0.8.x)

- `--chunk-type` filter, `cqs dead`, `cqs gc`, `cqs gather`, `cqs project`
- `--format mermaid`, `--pattern` filter, index staleness warnings

## Refactoring (v0.9.x)

- Split parser.rs and hnsw.rs into directories
- 14-category audit v4: 125 fixes across 9 PRs

## New Languages

- SQL (T-SQL, PostgreSQL) with forked tree-sitter-sql
- Markdown (.md, .mdx) with heading-based chunking

## Agent Experience (v0.10.0–v0.12.0)

- MCP server removed (v0.10.0), CLI-only
- `note_only` search, `context --summary`, Mermaid impact output
- Table-aware Markdown chunking, parent retrieval (small-to-big)
- Proactive hints, `impact-diff`, `stale`, `context --compact`
- `related`, `impact --suggest-tests`, `where`, `scout`

## Phase 8: Security (partial)

- Rate limiting (1MB body limit). Index encryption deferred.
