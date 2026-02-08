# Markdown Indexing for cqs

**Date:** 2026-02-08
**Status:** Design

## Problem

cqs indexes code but not documentation. Large markdown doc sets (product guides, API references, tutorials) contain answers to conceptual and cross-product engineering questions that semantic code search can't reach. The AI consumer asks questions like "how do I combine historian data retrieval with InTouch scripting to display tag values?" and needs chunks from multiple docs assembled into a coherent answer.

## Design

### Chunking Strategy

**Heading-based with windowing.** Headings are the author's own topic segmentation — they carry semantic meaning that arbitrary fixed-size windows destroy.

- **Primary split: H2 headings.** H1 is the document title (becomes metadata). H3+ headings stay inside their parent chunk as structural context.
- **Overflow split: H3 boundaries** when an H2 section exceeds 150 lines. If an H3 subsection itself exceeds 150 lines, apply the existing overlapping-window logic.
- **Merge small sections:** Adjacent H2 sections under 30 lines get merged with the next H2. Prevents orphan parameter docs with no surrounding context.
- **Code blocks stay inline.** Most code examples in these docs are unlabeled or in proprietary syntax (QuickScript). They're more valuable in context with surrounding prose than isolated. Dual-indexing of labeled code blocks is a future enhancement.

### Chunk Metadata

| Field | Markdown Value |
|-------|---------------|
| `name` | Heading text (e.g., "Cyclic Retrieval") |
| `signature` | Full breadcrumb path: `Doc Title > H2 > H3 > ...` |
| `chunk_type` | `Section` (new variant) |
| `content` | Section body — prose, lists, tables, code blocks |
| `doc` | `None` (the chunk IS the doc) |
| `language` | `Markdown` (new variant) |

Full-depth breadcrumb signatures are critical. The AI consumer uses signatures to assess relevance at a glance. "Historian Data Retrieval > Retrieval Modes > Cyclic Retrieval" is immediately useful; bare "Cyclic Retrieval" is not.

### Cross-Reference Extraction

Cross-references between doc sections (and between docs and code) are stored as regular `CallSite` entries. This means all existing graph tools — callers, callees, gather, impact, trace — work across markdown↔markdown and markdown↔code boundaries without modification.

Three extraction sources:

1. **Markdown links** — `[text](target.md#anchor)` → `CallSite` to resolved target chunk. Trivial regex.
2. **Backtick function references** — `` `TagRead()` `` → `CallSite` to the chunk defining that function. Regex: backtick text ending in `()`.
3. **Known chunk name mentions** — After all chunks exist, scan each chunk's content for exact substring matches of other chunk names. Minimum 20 chars to avoid false positives. Second pass.

### Parser Implementation

No tree-sitter. Markdown structure is simple enough for a line-by-line parser. tree-sitter-markdown is a heavy dependency for what amounts to heading detection + regex.

**`src/language/markdown.rs`** — Language definition. Same `LanguageDef` pattern but with `grammar: None`, extensions `["md", "mdx"]`, and a new `SignatureStyle::Breadcrumb`.

**`src/parser/markdown.rs`** — Custom parse logic:

1. **Pass 1 — Chunking:** Walk lines, track heading stack. Emit chunk on each H2 (or H3 if H2 section > 150 lines). Build breadcrumb from heading stack. Content = everything between this heading and next split point.
2. **Pass 2 — Reference extraction:** For each chunk, scan content for markdown links, backtick function references, and known chunk name matches. Emit `CallSite` entries.
3. **Pass 3 — Windowing:** Chunks over 150 lines get overlapping windows (same logic as code chunks).

### Integration

Markdown enters the existing pipeline. No changes to embedding, HNSW, search, gather, callers/callees, store, or MCP tools.

- `cqs index` picks up `.md`/`.mdx` files via the language registry extension map.
- Embedder processes chunk content as text (same as code — E5-base-v2 doesn't distinguish).
- RRF hybrid search extracts keywords from prose (already works — stopword filtering applies).
- Gather BFS follows cross-reference edges across doc↔code boundaries.
- `cqs_dead` skips `Section` chunks (a doc section with no incoming references isn't "dead code").

### What Changes

| Change | Where | Scope |
|--------|-------|-------|
| Add `Markdown` to `Language` enum | `src/language/mod.rs` | ~5 lines in macro |
| New markdown language def | `src/language/markdown.rs` | New file, ~200 lines |
| Custom `parse_markdown()` | `src/parser/markdown.rs` | New file, ~300 lines |
| Add `Section` to `ChunkType` | `src/parser/types.rs` | ~3 lines |
| Wire markdown parse dispatch | `src/parser/mod.rs` | ~10 lines |
| `SignatureStyle::Breadcrumb` | `src/parser/chunk.rs` | ~10 lines |
| Skip `Section` in dead code analysis | dead code command | ~3 lines |
| Feature flag `lang-markdown` | `Cargo.toml` | ~3 lines |
| Tests | `tests/` | ~150 lines |

~700 lines new code. No architectural changes.

### Not in Scope (Future)

- Dual-indexing labeled code blocks as separate language-specific chunks
- Table-aware chunking (tables as structured data, not prose)
- Image/diagram reference extraction
- Markdown-specific keyword boosting (heading text weighted higher)

## Validation

Tested chunking strategy against 5 sample files (43KB–1.7MB). Heading-based split works for all of them. Key observations:

- historian-concepts (43KB): clean H1-H3, uniform sections. Straightforward.
- historian-data-retrieval (535KB): H1-H6, 280 code blocks, 5–2000 line sections. H2 primary + H3 overflow handles the variance.
- aveva-scripting (375KB): 982 code blocks, function-level H4-H6. H2/H3 split keeps function docs intact with their examples.
- aveva-sql-script-library (140KB): consistent method docs. H3 aligns with method boundaries.
- intouch-hmi-app-development (1.7MB, 60K lines): H1-H4, 2180 code blocks. H2 primary + H3 overflow keeps task-level docs together.
