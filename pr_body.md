Informational: GC runs 4 prune operations (`prune_missing`, `prune_stale_calls`, `prune_stale_type_edges`, `prune_orphan_summaries`) in separate transactions. Between `prune_missing` (which commits) and `prune_stale_calls` (separate commit), concurrent queries could see stale `function_calls` entries.

In practice this is harmless: FK cascades on `calls`/`type_edges` handle cleanup atomically within `prune_missing`. The `function_calls` table has no FK by design (performance). Stale entries are silently dropped by call graph construction (JOIN with chunks) and cleaned milliseconds later by `prune_stale_calls`.

No code change needed. Documenting for awareness.

**Location:** `src/cli/commands/gc.rs:44-59`
**Audit:** v1.4.0 audit findings DS-17, DS-18
