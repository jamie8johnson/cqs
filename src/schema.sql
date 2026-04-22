-- cq index schema v22
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
    umap_y REAL                               -- v22: 2D projection Y coord (NULL until `cqs index --umap` runs)
);

CREATE INDEX IF NOT EXISTS idx_chunks_origin ON chunks(origin);
CREATE INDEX IF NOT EXISTS idx_chunks_source_type ON chunks(source_type);
CREATE INDEX IF NOT EXISTS idx_chunks_content_hash ON chunks(content_hash);
CREATE INDEX IF NOT EXISTS idx_chunks_name ON chunks(name);
CREATE INDEX IF NOT EXISTS idx_chunks_language ON chunks(language);
CREATE INDEX IF NOT EXISTS idx_chunks_parent ON chunks(parent_id);

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
    call_line INTEGER NOT NULL    -- line where call occurs
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
    sentiment REAL NOT NULL,       -- -1.0 to +1.0 (negative=warning, positive=pattern)
    mentions TEXT,                 -- JSON array of mentioned paths/functions
    embedding BLOB NOT NULL,       -- legacy: was 769-dim, now empty blob (SQ-9)
    source_file TEXT NOT NULL,     -- path to notes.toml
    file_mtime INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_notes_sentiment ON notes(sentiment);

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
