-- cq index schema v9

CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chunks (
    id TEXT PRIMARY KEY,
    file TEXT NOT NULL,
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
    file_mtime INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    parent_id TEXT,           -- if windowed: ID of the logical parent chunk
    window_idx INTEGER        -- if windowed: 0, 1, 2... for each window
);

CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file);
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

-- Hunches: soft observations and latent risks that surface in search
CREATE TABLE IF NOT EXISTS hunches (
    id TEXT PRIMARY KEY,           -- "hunch:2026-01-31-title-slug"
    date TEXT NOT NULL,            -- ISO date (YYYY-MM-DD)
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    severity TEXT NOT NULL,        -- high, med, low
    confidence TEXT NOT NULL,      -- high, med, low
    resolution TEXT NOT NULL,      -- open, resolved, accepted
    mentions TEXT,                 -- JSON array of mentioned paths/functions
    embedding BLOB NOT NULL,
    source_file TEXT NOT NULL,     -- path to HUNCHES.md
    file_mtime INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_hunches_resolution ON hunches(resolution);
CREATE INDEX IF NOT EXISTS idx_hunches_severity ON hunches(severity);

-- FTS5 for hunch keyword search
CREATE VIRTUAL TABLE IF NOT EXISTS hunches_fts USING fts5(
    id UNINDEXED,
    title,
    description,
    tokenize='unicode61'
);

-- Scars: failed approaches - limbic memory that surfaces when relevant
CREATE TABLE IF NOT EXISTS scars (
    id TEXT PRIMARY KEY,           -- "scar:2026-01-31-title-slug"
    date TEXT NOT NULL,            -- ISO date (YYYY-MM-DD)
    title TEXT NOT NULL,
    tried TEXT NOT NULL,           -- what was attempted
    pain TEXT NOT NULL,            -- what hurt
    learned TEXT NOT NULL,         -- what to do instead
    mentions TEXT,                 -- JSON array of mentioned paths/functions
    embedding BLOB NOT NULL,
    source_file TEXT NOT NULL,     -- path to scars.toml
    file_mtime INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_scars_date ON scars(date);

-- FTS5 for scar keyword search
CREATE VIRTUAL TABLE IF NOT EXISTS scars_fts USING fts5(
    id UNINDEXED,
    title,
    tried,
    pain,
    learned,
    tokenize='unicode61'
);

-- Notes: unified memory entries (replaces separate scars/hunches in v9+)
-- Sentiment field bakes valence into similarity search via 769th embedding dimension
CREATE TABLE IF NOT EXISTS notes (
    id TEXT PRIMARY KEY,           -- "note:0", "note:1", etc.
    text TEXT NOT NULL,            -- the note content
    sentiment REAL NOT NULL,       -- -1.0 to +1.0 (negative=warning, positive=pattern)
    mentions TEXT,                 -- JSON array of mentioned paths/functions
    embedding BLOB NOT NULL,       -- 769-dim (768 model + sentiment)
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
