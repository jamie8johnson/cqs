-- cq index schema v29 (see src/store/helpers/mod.rs::CURRENT_SCHEMA_VERSION; v22+v23+v24+v25+v26+v27+v28+v29 columns annotated inline below)
-- v29: file_registry table (origin → source_mtime/size/content_hash) persists
--      the reconcile fingerprint for files that parse to ZERO chunks, where the
--      chunk-row fingerprint columns have no row to live on. Without it, a
--      zero-chunk file (comment-only source, parser-empty) is reclassified
--      "not indexed" every run and re-parsed forever. The staleness readers
--      (fingerprints_for_origins / indexed_file_origins / check_origins_stale /
--      list_stale_files) UNION this table so a zero-chunk origin still surfaces
--      a stored fingerprint and skips the parse like any unchanged file. Rows
--      are pruned alongside chunk deletes (delete_by_origin / prune paths).
--      v29 also adds a CHECK on notes.sentiment pinning it to the five discrete
--      values (-1, -0.5, 0, 0.5, 1) the docs promise; parse_notes_str snaps to
--      the nearest discrete value so continuous TOML inputs satisfy the CHECK.
-- v28: chunks.canonical_hash TEXT (nullable) + idx_chunks_canonical_hash. blake3
--      of a comment-/whitespace-normalized form of the chunk content; keys the
--      embedding-reuse cache so comment-only / formatting-only edits reuse the
--      prior embedding instead of re-embedding. NULL on pre-v28 rows = clean
--      cache miss until the next reindex writes it.
-- v27: chunks.needs_embedding INTEGER NOT NULL DEFAULT 0 + partial index on
--      needs_embedding=1. Set on chunks written by the parser stage during a
--      `--llm-summaries` reindex without a first-pass embed (#1452); cleared
--      by `enrichment_pass` once a real embedding is written. The
--      `embedding` column stays BLOB NOT NULL (zero-vec sentinel for
--      unembedded chunks); HNSW build + search hydration filter
--      `WHERE needs_embedding = 0` so partial-state chunks are invisible
--      until enrichment lands their real vector. The visibility gate is
--      local — LLM summary failure does not block it.
-- v22: umap_x / umap_y REAL columns on chunks for the cqs serve cluster view.
--      Both nullable; populated only when `cqs index --umap` runs the
--      umap-learn projection. /api/embed/2d skips chunks where coords are NULL.
-- v21: parser_version column on chunks so incremental UPSERT can refresh rows
--      whose content_hash hasn't changed but whose parser-emitted fields (e.g.
--      `doc` from extract_doc_fallback_for_short_chunk) would now differ. The
--      WHERE clause in batch_insert_chunks's ON CONFLICT path additionally
--      checks `OR parser_version != excluded.parser_version`, and
--      upsert_fts_conditional uses the same OR filter when comparing the
--      pre-INSERT snapshot.
-- v20: AFTER DELETE trigger on chunks bumps splade_generation so any persisted
--      splade.index.bin is invalidated when underlying chunks are removed
--      (catches the race `delete_by_origin` left behind when CASCADE alone
--      wasn't enough to drive rebuild scheduling).
-- v19: FK(chunk_id) ON DELETE CASCADE on sparse_vectors → chunks(id) so SPLADE
--      rows can't outlive the chunks they describe; splade_generation is also
--      bumped on migration to invalidate any pre-v19 splade.index.bin.
-- v18: embedding_base column for dual embeddings (adaptive retrieval Phase 5)
-- v17: sparse_vectors table + enrichment_version column
-- v16: composite PK on llm_summaries
-- v10: Generalized for multiple sources (filesystem, SQL Server, etc.)
--   file → origin (unique identifier like "file:src/main.rs" or "mssql:server/db/dbo.MyProc")
--   file_mtime → source_mtime (nullable for sources without mtime)
--   + source_type for fast filtering

CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chunks (
    id TEXT PRIMARY KEY,
    origin TEXT NOT NULL,           -- unique source identifier
    source_type TEXT NOT NULL,      -- "file", "mssql", etc.
    language TEXT NOT NULL,
    chunk_type TEXT NOT NULL,
    name TEXT NOT NULL,
    signature TEXT NOT NULL,
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    doc TEXT,
    line_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    embedding BLOB NOT NULL,
    embedding_base BLOB,            -- v18 dual embeddings — NL only, no enrichment, NULL until re-indexed
    source_mtime INTEGER,           -- nullable: not all sources have mtime
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    parent_id TEXT,           -- if windowed: ID of the logical parent chunk
    window_idx INTEGER,       -- if windowed: 0, 1, 2... for each window
    parent_type_name TEXT,    -- for methods: name of enclosing class/struct/impl
    enrichment_hash TEXT,     -- blake3 hash of call context used for enrichment (NULL = not enriched)
    enrichment_version INTEGER NOT NULL DEFAULT 0,  -- RT-DATA-2: idempotency marker for enrichment passes
    parser_version INTEGER NOT NULL DEFAULT 0,  -- v21: parser stamp for content-hash-stable doc enrichment refresh (P2 #29)
    umap_x REAL,                              -- v22: 2D projection X coord (NULL until `cqs index --umap` runs)
    umap_y REAL,                              -- v22: 2D projection Y coord (NULL until `cqs index --umap` runs)
    source_size INTEGER,                      -- v23: file size in bytes for reconcile fingerprint (#1219); nullable on pre-migration rows
    source_content_hash BLOB,                 -- v23: BLAKE3 hash of file bytes for reconcile fingerprint (#1219); nullable on pre-migration rows
    vendored INTEGER NOT NULL DEFAULT 0,      -- v24: 1 if origin matches a vendored-path prefix at index time (#1221); search emits trust_level="vendored-code" for these
    needs_embedding INTEGER NOT NULL DEFAULT 0, -- v27: 1 when chunk was written without a real embedding (#1452 first-pass-skip); cleared by enrichment_pass
    canonical_hash TEXT                       -- v28: blake3 of comment-/whitespace-normalized content; embedding-reuse cache key so comment-only edits reuse the prior embedding. Nullable: NULL = not computed (clean cache miss)
);

CREATE INDEX IF NOT EXISTS idx_chunks_needs_embedding
    ON chunks(needs_embedding) WHERE needs_embedding = 1;

-- v28: powers the canonical-hash embedding-reuse lookup in
-- get_embeddings_by_canonical_hashes (WHERE canonical_hash IN (...)).
CREATE INDEX IF NOT EXISTS idx_chunks_canonical_hash ON chunks(canonical_hash);

CREATE INDEX IF NOT EXISTS idx_chunks_origin ON chunks(origin);
CREATE INDEX IF NOT EXISTS idx_chunks_source_type ON chunks(source_type);
-- v26 / #1371 / PERF-V1.33-10: composite index covering the
-- `WHERE source_type = ? + DISTINCT origin` pattern used by
-- `list_stale_files` (every reconcile + `cqs status --watch-fresh`)
-- and `prune_missing_files` (GC). With the single-column indexes
-- above, SQLite probes one then row-visits the other; with the
-- composite, both the filter and the DISTINCT walk satisfy from a
-- single index pass. Expected ~50× speedup at 50k+ chunk corpora;
-- index size ~5-15% of the chunks table.
CREATE INDEX IF NOT EXISTS idx_chunks_source_type_origin
    ON chunks(source_type, origin);
CREATE INDEX IF NOT EXISTS idx_chunks_content_hash ON chunks(content_hash);
CREATE INDEX IF NOT EXISTS idx_chunks_name ON chunks(name);
CREATE INDEX IF NOT EXISTS idx_chunks_language ON chunks(language);
CREATE INDEX IF NOT EXISTS idx_chunks_parent ON chunks(parent_id);

-- v29 (#1774): per-origin reconcile fingerprint that persists INDEPENDENT of
-- chunk rows. The reconcile fingerprint normally lives on chunk rows
-- (chunks.source_mtime/source_size/source_content_hash), but a file that parses
-- to zero chunks (comment-only source, parser-emitted-empty) has no chunk row to
-- carry it. Before v29 such a file was re-parsed on EVERY `cqs index` run because
-- the staleness pre-filter saw no rows and treated it as un-indexed. This table
-- gives those origins somewhere to stash the fingerprint so the next run skips
-- the parse. Columns mirror the chunk-row fingerprint shape exactly. Rows are
-- written inside the same transaction as the chunk writes / prune (crash-safety
-- convention, #1772) and pruned alongside chunk deletes (delete_by_origin /
-- prune_missing / prune_all / prune_gitignored, #1759).
CREATE TABLE IF NOT EXISTS file_registry (
    origin TEXT PRIMARY KEY,              -- slash-normalized source identifier (matches chunks.origin)
    source_mtime INTEGER,                 -- nullable: not all sources have mtime
    source_size INTEGER,                  -- file size in bytes; NULL when unread
    source_content_hash BLOB              -- BLAKE3 of file bytes (32 bytes); NULL when unhashed
);

-- FTS5 virtual table for keyword search (RRF hybrid search)
-- Normalized text (camelCase/snake_case split to words) populated by application
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    id UNINDEXED,  -- chunk ID for joining (not searchable)
    name,          -- normalized function/method name
    signature,     -- normalized signature
    content,       -- normalized code content
    doc,           -- documentation text
    tokenize='unicode61'
);

-- Call graph: function call relationships (within-file resolution)
CREATE TABLE IF NOT EXISTS calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    caller_id TEXT NOT NULL,      -- chunk ID of the calling function
    callee_name TEXT NOT NULL,    -- name of the called function
    line_number INTEGER NOT NULL, -- line where call occurs
    FOREIGN KEY (caller_id) REFERENCES chunks(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_calls_caller ON calls(caller_id);
CREATE INDEX IF NOT EXISTS idx_calls_callee ON calls(callee_name);

-- Full call graph: captures ALL function calls, including from large functions
-- that are skipped during chunk extraction (>100 lines)
CREATE TABLE IF NOT EXISTS function_calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file TEXT NOT NULL,           -- source file path
    caller_name TEXT NOT NULL,    -- name of the calling function
    caller_line INTEGER NOT NULL, -- line where function starts
    callee_name TEXT NOT NULL,    -- name of the called function
    call_line INTEGER NOT NULL,   -- line where call occurs
    edge_kind TEXT NOT NULL DEFAULT 'call'  -- provenance: call|serde_callback|macro_heuristic|fn_pointer (v30)
);
CREATE INDEX IF NOT EXISTS idx_fcalls_file ON function_calls(file);
CREATE INDEX IF NOT EXISTS idx_fcalls_caller ON function_calls(caller_name);
CREATE INDEX IF NOT EXISTS idx_fcalls_callee ON function_calls(callee_name);

-- Type dependency edges: which chunks reference which types (Phase 2b)
-- Source is chunk-level for precise dependency tracking.
-- edge_kind stores TypeEdgeKind classification (Param, Return, Field, Impl, Bound, Alias)
-- or empty string '' for catch-all types (inside generics, etc.).
-- Empty string used instead of NULL to simplify WHERE clause filtering.
CREATE TABLE IF NOT EXISTS type_edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_chunk_id TEXT NOT NULL,    -- chunk ID of the referencing code
    target_type_name TEXT NOT NULL,   -- name of the referenced type
    edge_kind TEXT NOT NULL DEFAULT '',-- TypeEdgeKind or '' for catch-all
    line_number INTEGER NOT NULL,     -- line where type reference occurs
    FOREIGN KEY (source_chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_type_edges_source ON type_edges(source_chunk_id);
CREATE INDEX IF NOT EXISTS idx_type_edges_target ON type_edges(target_type_name);

-- Notes: unified memory entries (sentiment-based, replaces deprecated hunches/scars)
-- Embedding column retained for schema compatibility, new notes store empty blobs (SQ-9)
CREATE TABLE IF NOT EXISTS notes (
    id TEXT PRIMARY KEY,           -- "note:0", "note:1", etc.
    text TEXT NOT NULL,            -- the note content
    -- v29 (#1774 ride-along): sentiment is DISCRETE — only the five documented
    -- values are valid. The CHECK enforces the invariant at the schema layer so
    -- it can't be bypassed by a future call site; `parse_notes_str` snaps
    -- continuous TOML inputs to the nearest discrete value before write, and the
    -- v28→v29 migration clamp-rewrites any pre-existing off-grid rows.
    sentiment REAL NOT NULL CHECK (sentiment IN (-1.0, -0.5, 0.0, 0.5, 1.0)),  -- negative=warning, positive=pattern
    mentions TEXT,                 -- JSON array of mentioned paths/functions
    embedding BLOB NOT NULL,       -- legacy: was 769-dim, now empty blob (SQ-9)
    source_file TEXT NOT NULL,     -- path to notes.toml
    file_mtime INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    kind TEXT                      -- v25 / #1133: optional structured kind tag (`todo`, `design-decision`, …); NULL on pre-v25 rows + sentiment-only notes
);

CREATE INDEX IF NOT EXISTS idx_notes_sentiment ON notes(sentiment);
CREATE INDEX IF NOT EXISTS idx_notes_kind ON notes(kind);

-- FTS5 for note keyword search
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    id UNINDEXED,
    text,
    tokenize='unicode61'
);

-- SPLADE sparse vectors for hybrid search (v17, FK cascade added v19)
-- Each chunk gets a set of (token_id, weight) pairs from the learned sparse encoder.
-- FK + ON DELETE CASCADE added in v19 (v1.22.0 audit DS-W3) so every delete
-- from `chunks` automatically removes the matching sparse rows. Before v19,
-- three delete paths in src/store/chunks/crud.rs leaked orphan sparse rows.
CREATE TABLE IF NOT EXISTS sparse_vectors (
    chunk_id TEXT NOT NULL,
    token_id INTEGER NOT NULL,
    weight REAL NOT NULL,
    PRIMARY KEY (chunk_id, token_id),
    FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_sparse_token ON sparse_vectors(token_id);

-- v20 (v1.22.0 audit DS-W2 / OB-22 / PB-NEW-6): trigger that bumps the
-- SPLADE generation counter whenever a chunk is deleted. Fires per-row on
-- explicit DELETE FROM chunks AND on CASCADE delete (which actually comes
-- from type_edges.source_chunk_id, calls.source_chunk_id, and now
-- sparse_vectors.chunk_id since v19). This makes the persisted
-- `splade.index.bin` file invalidation structural instead of instrumented —
-- `cqs watch` no longer needs to call `bump_splade_generation` explicitly
-- because every delete_phantom_chunks / delete_by_origin statement bumps
-- the counter automatically. Scoped to deletion specifically because
-- chunks INSERT doesn't invalidate existing sparse data (new chunks have
-- no sparse rows yet) and UPDATE-without-ID-change doesn't affect
-- sparse_vectors at all.
CREATE TRIGGER IF NOT EXISTS bump_splade_on_chunks_delete
AFTER DELETE ON chunks
BEGIN
    INSERT INTO metadata (key, value) VALUES ('splade_generation', '1')
    ON CONFLICT(key) DO UPDATE SET
        value = CAST((CAST(value AS INTEGER) + 1) AS TEXT);
END;

-- LLM-generated summaries cache (SQ-6, v16: composite PK)
-- Keyed by (content_hash, purpose) so the same code can have multiple summary types
-- (e.g., 'summary', 'doc-comment'). Summaries survive chunk deletion and --force rebuilds.
CREATE TABLE IF NOT EXISTS llm_summaries (
    content_hash TEXT NOT NULL,
    purpose TEXT NOT NULL DEFAULT 'summary',
    summary TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (content_hash, purpose)
);
