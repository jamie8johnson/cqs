-- cq index schema v1

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
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file);
CREATE INDEX IF NOT EXISTS idx_chunks_content_hash ON chunks(content_hash);
CREATE INDEX IF NOT EXISTS idx_chunks_name ON chunks(name);
CREATE INDEX IF NOT EXISTS idx_chunks_language ON chunks(language);
