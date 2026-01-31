//! SQLite storage for chunks and embeddings

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, params_from_iter};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::embedder::Embedding;
use crate::parser::{Chunk, ChunkType, Language};

// Schema version for migrations
const CURRENT_SCHEMA_VERSION: i32 = 1;
const MODEL_NAME: &str = "nomic-embed-text-v1.5";

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Connection pool error: {0}")]
    Pool(#[from] r2d2::Error),
    #[error("Schema version mismatch: index is v{0}, cq expects v{1}. Run 'cq index --force' to rebuild.")]
    SchemaMismatch(i32, i32),
    #[error("Index created by newer cq version (schema v{0}). Please upgrade cq.")]
    SchemaNewerThanCq(i32),
    #[error(
        "Model mismatch: index uses '{0}', current is '{1}'. Run 'cq index --force' to re-embed."
    )]
    ModelMismatch(String, String),
}

/// Thread-safe SQLite store using connection pooling
pub struct Store {
    pool: Pool<SqliteConnectionManager>,
}

/// Raw row from chunks table (for internal use)
#[derive(Clone)]
struct ChunkRow {
    id: String,
    file: String,
    language: String,
    chunk_type: String,
    name: String,
    signature: String,
    content: String,
    doc: Option<String>,
    line_start: u32,
    line_end: u32,
}

/// Minimal struct for scoring phase - ID, embedding, and optionally name
struct ChunkScore {
    id: String,
    embedding: Vec<u8>,
    name: Option<String>,
}

/// Lightweight chunk info for search results
#[derive(Debug, Clone)]
pub struct ChunkSummary {
    pub id: String,
    pub file: PathBuf,
    pub language: Language,
    pub chunk_type: ChunkType,
    pub name: String,
    pub signature: String,
    pub content: String,
    pub doc: Option<String>,
    pub line_start: u32,
    pub line_end: u32,
}

impl From<ChunkRow> for ChunkSummary {
    fn from(row: ChunkRow) -> Self {
        ChunkSummary {
            id: row.id,
            file: PathBuf::from(row.file),
            language: row.language.parse().unwrap_or(Language::Rust),
            chunk_type: row.chunk_type.parse().unwrap_or(ChunkType::Function),
            name: row.name,
            signature: row.signature,
            content: row.content,
            doc: row.doc,
            line_start: row.line_start,
            line_end: row.line_end,
        }
    }
}

#[derive(Debug)]
pub struct SearchResult {
    pub chunk: ChunkSummary,
    pub score: f32,
}

/// Filter options for search
#[derive(Default)]
pub struct SearchFilter {
    pub languages: Option<Vec<Language>>,
    pub path_pattern: Option<String>,
    /// Weight for name matching (0.0-1.0, default 0.0 = pure embedding)
    pub name_boost: f32,
    /// Original query text for name matching
    pub query_text: String,
}

/// Model metadata for index initialization
pub struct ModelInfo {
    pub name: String,
    pub dimensions: u32,
    pub version: String,
}

impl Default for ModelInfo {
    fn default() -> Self {
        ModelInfo {
            name: MODEL_NAME.to_string(),
            dimensions: 768,
            version: "1.5".to_string(),
        }
    }
}

/// Index statistics
#[derive(Debug)]
pub struct IndexStats {
    pub total_chunks: u64,
    pub total_files: u64,
    pub chunks_by_language: HashMap<Language, u64>,
    pub chunks_by_type: HashMap<ChunkType, u64>,
    pub index_size_bytes: u64,
    pub created_at: String,
    pub updated_at: String,
    pub model_name: String,
    pub schema_version: i32,
}

// Helper functions for embedding serialization
pub fn embedding_to_bytes(embedding: &Embedding) -> Vec<u8> {
    embedding.0.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

/// Cosine similarity for L2-normalized vectors (just dot product)
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Compute name match score for hybrid search
pub fn name_match_score(query: &str, name: &str) -> f32 {
    let query_lower = query.to_lowercase();
    let name_lower = name.to_lowercase();

    // Exact match
    if name_lower == query_lower {
        return 1.0;
    }

    // Name contains query as substring
    if name_lower.contains(&query_lower) {
        return 0.8;
    }

    // Query contains name as substring
    if query_lower.contains(&name_lower) {
        return 0.6;
    }

    // Word overlap (split on camelCase, snake_case)
    let query_words = tokenize_identifier(&query_lower);
    let name_words = tokenize_identifier(&name_lower);

    if query_words.is_empty() || name_words.is_empty() {
        return 0.0;
    }

    let overlap = query_words
        .iter()
        .filter(|w| {
            name_words
                .iter()
                .any(|nw| nw.contains(w.as_str()) || w.contains(nw.as_str()))
        })
        .count() as f32;
    let total = query_words.len().max(1) as f32;

    (overlap / total) * 0.5 // Max 0.5 for partial word overlap
}

/// Split identifier on snake_case and camelCase boundaries
fn tokenize_identifier(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();

    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            if !current.is_empty() {
                words.push(current.clone());
                current.clear();
            }
        } else if c.is_uppercase() && !current.is_empty() {
            words.push(current.clone());
            current.clear();
            current.push(c.to_lowercase().next().unwrap_or(c));
        } else {
            current.push(c.to_lowercase().next().unwrap_or(c));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

impl Store {
    /// Open an existing index with connection pooling
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let manager = SqliteConnectionManager::file(path)
            .with_init(|conn| {
                // Enable WAL mode for better concurrent read performance
                conn.pragma_update(None, "journal_mode", "WAL")?;
                // Wait up to 5s if database is locked
                conn.pragma_update(None, "busy_timeout", 5000)?;
                // NORMAL sync is safe with WAL and faster than FULL
                conn.pragma_update(None, "synchronous", "NORMAL")?;
                Ok(())
            });

        let pool = Pool::builder()
            .max_size(4) // Allow up to 4 concurrent connections
            .build(manager)?;

        let store = Self { pool };

        // Check schema version compatibility
        store.check_schema_version()?;
        // Check model version compatibility
        store.check_model_version()?;

        Ok(store)
    }

    /// Create a new index
    pub fn init(&self, model_info: &ModelInfo) -> Result<(), StoreError> {
        let conn = self.pool.get()?;

        // Create tables
        conn.execute_batch(include_str!("schema.sql"))?;

        // Store metadata
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
            ["schema_version", &CURRENT_SCHEMA_VERSION.to_string()],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
            ["model_name", &model_info.name],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
            ["dimensions", &model_info.dimensions.to_string()],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
            ["created_at", &now],
        )?;
        conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
            ["cq_version", env!("CARGO_PKG_VERSION")],
        )?;

        Ok(())
    }

    fn check_schema_version(&self) -> Result<(), StoreError> {
        let conn = self.pool.get()?;
        let version: i32 = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if version > CURRENT_SCHEMA_VERSION {
            return Err(StoreError::SchemaNewerThanCq(version));
        }
        if version < CURRENT_SCHEMA_VERSION && version > 0 {
            return Err(StoreError::SchemaMismatch(version, CURRENT_SCHEMA_VERSION));
        }
        Ok(())
    }

    fn check_model_version(&self) -> Result<(), StoreError> {
        let conn = self.pool.get()?;
        let stored_model: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'model_name'",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();

        if !stored_model.is_empty() && stored_model != MODEL_NAME {
            return Err(StoreError::ModelMismatch(
                stored_model,
                MODEL_NAME.to_string(),
            ));
        }
        Ok(())
    }

    /// Insert or update chunks in batch (10x faster than individual inserts)
    pub fn upsert_chunks_batch(
        &self,
        chunks: &[(Chunk, Embedding)],
        file_mtime: i64,
    ) -> Result<usize, StoreError> {
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;

        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO chunks (id, file, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, file_mtime, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;

            let now = chrono::Utc::now().to_rfc3339();
            for (chunk, embedding) in chunks {
                stmt.execute(params![
                    chunk.id,
                    chunk.file.to_string_lossy(),
                    chunk.language.to_string(),
                    chunk.chunk_type.to_string(),
                    chunk.name,
                    chunk.signature,
                    chunk.content,
                    chunk.content_hash,
                    chunk.doc,
                    chunk.line_start,
                    chunk.line_end,
                    embedding_to_bytes(embedding),
                    file_mtime,
                    &now,
                    &now,
                ])?;
            }
        }

        tx.commit()?;
        Ok(chunks.len())
    }

    /// Insert or update a single chunk
    pub fn upsert_chunk(
        &self,
        chunk: &Chunk,
        embedding: &Embedding,
        file_mtime: i64,
    ) -> Result<(), StoreError> {
        self.upsert_chunks_batch(&[(chunk.clone(), embedding.clone())], file_mtime)?;
        Ok(())
    }

    /// Check if a file needs reindexing based on mtime
    pub fn needs_reindex(&self, path: &Path) -> Result<bool, StoreError> {
        let current_mtime = path
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| std::io::Error::other("time error"))?
            .as_secs() as i64;

        let conn = self.pool.get()?;
        let stored_mtime: Option<i64> = conn
            .query_row(
                "SELECT file_mtime FROM chunks WHERE file = ?1 LIMIT 1",
                [path.to_string_lossy()],
                |r| r.get(0),
            )
            .ok();

        match stored_mtime {
            Some(mtime) if mtime >= current_mtime => Ok(false),
            _ => Ok(true),
        }
    }

    /// Search for similar chunks (two-phase for memory efficiency)
    pub fn search(
        &self,
        query: &Embedding,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>, StoreError> {
        self.search_filtered(query, &SearchFilter::default(), limit, threshold)
    }

    /// Search with filters
    pub fn search_filtered(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>, StoreError> {
        let conn = self.pool.get()?;

        // Build WHERE clause from filter with parameterized query
        let mut conditions = Vec::new();
        let mut params_vec: Vec<String> = Vec::new();

        if let Some(ref langs) = filter.languages {
            // Build placeholders: ?,?,?
            let placeholders: Vec<_> = langs.iter().map(|_| "?").collect();
            conditions.push(format!("language IN ({})", placeholders.join(",")));
            // Collect param values
            for lang in langs {
                params_vec.push(lang.to_string());
            }
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        // Check if we need hybrid scoring
        let use_hybrid = filter.name_boost > 0.0 && !filter.query_text.is_empty();

        // Phase 1: Score matching chunks (load id + embedding, optionally name)
        let sql = if use_hybrid {
            format!("SELECT id, embedding, name FROM chunks{}", where_clause)
        } else {
            format!("SELECT id, embedding FROM chunks{}", where_clause)
        };
        let mut stmt = conn.prepare(&sql)?;

        let mut scored: Vec<(String, f32)> = stmt
            .query_map(params_from_iter(params_vec.iter()), |row| {
                Ok(ChunkScore {
                    id: row.get(0)?,
                    embedding: row.get(1)?,
                    name: if use_hybrid { row.get(2).ok() } else { None },
                })
            })?
            .filter_map(|r| r.ok())
            .filter_map(|chunk| {
                let embedding = bytes_to_embedding(&chunk.embedding);
                let embedding_score = cosine_similarity(&query.0, &embedding);

                // Compute hybrid score if enabled
                let score = if use_hybrid {
                    let name = chunk.name.as_deref().unwrap_or("");
                    let name_score = name_match_score(&filter.query_text, name);
                    (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
                } else {
                    embedding_score
                };

                // Apply path filter in Rust (glob matching)
                if let Some(ref pattern) = filter.path_pattern {
                    if let Ok(glob_pattern) =
                        globset::Glob::new(pattern).map(|g| g.compile_matcher())
                    {
                        // Extract file path from chunk id (format: path:line:hash)
                        let file_part = chunk.id.split(':').next().unwrap_or("");
                        if !glob_pattern.is_match(file_part) {
                            return None;
                        }
                    }
                }

                if score >= threshold {
                    Some((chunk.id, score))
                } else {
                    None
                }
            })
            .collect();

        // Sort and take top-N
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        if scored.is_empty() {
            return Ok(vec![]);
        }

        // Phase 2: Fetch full content only for top-N results
        let ids: Vec<&str> = scored.iter().map(|(id, _)| id.as_str()).collect();
        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, file, language, chunk_type, name, signature, content, doc, line_start, line_end
             FROM chunks WHERE id IN ({})",
            placeholders
        );

        let mut stmt = conn.prepare(&sql)?;
        let rows: HashMap<String, ChunkRow> = stmt
            .query_map(rusqlite::params_from_iter(&ids), |row| {
                Ok(ChunkRow {
                    id: row.get(0)?,
                    file: row.get(1)?,
                    language: row.get(2)?,
                    chunk_type: row.get(3)?,
                    name: row.get(4)?,
                    signature: row.get(5)?,
                    content: row.get(6)?,
                    doc: row.get(7)?,
                    line_start: row.get(8)?,
                    line_end: row.get(9)?,
                })
            })?
            .filter_map(|r| r.ok())
            .map(|row| (row.id.clone(), row))
            .collect();

        // Reassemble results in score order
        let results: Vec<SearchResult> = scored
            .into_iter()
            .filter_map(|(id, score)| {
                rows.get(&id).map(|row| SearchResult {
                    chunk: ChunkSummary::from(row.clone()),
                    score,
                })
            })
            .collect();

        Ok(results)
    }

    /// Delete all chunks for a file
    pub fn delete_by_file(&self, file: &Path) -> Result<u32, StoreError> {
        let conn = self.pool.get()?;
        let deleted = conn.execute(
            "DELETE FROM chunks WHERE file = ?1",
            [file.to_string_lossy()],
        )?;
        Ok(deleted as u32)
    }

    /// Delete chunks for files that no longer exist
    pub fn prune_missing(&self, existing_files: &HashSet<PathBuf>) -> Result<u32, StoreError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare("SELECT DISTINCT file FROM chunks")?;
        let indexed_files: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        let mut deleted = 0u32;
        for file in indexed_files {
            let path = PathBuf::from(&file);
            if !existing_files.contains(&path) {
                deleted += conn
                    .execute("DELETE FROM chunks WHERE file = ?1", [&file])?
                    as u32;
            }
        }
        Ok(deleted)
    }

    /// Get embedding by content hash (for reuse when content unchanged)
    pub fn get_by_content_hash(&self, hash: &str) -> Option<Embedding> {
        let conn = self.pool.get().ok()?;
        conn.query_row(
            "SELECT embedding FROM chunks WHERE content_hash = ?1 LIMIT 1",
            [hash],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .ok()
        .map(|bytes| Embedding(bytes_to_embedding(&bytes)))
    }

    /// Get index statistics
    pub fn stats(&self) -> Result<IndexStats, StoreError> {
        let conn = self.pool.get()?;

        let total_chunks: u64 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;

        let total_files: u64 =
            conn.query_row("SELECT COUNT(DISTINCT file) FROM chunks", [], |r| r.get(0))?;

        // Chunks by language
        let mut stmt = conn
            .prepare("SELECT language, COUNT(*) FROM chunks GROUP BY language")?;
        let chunks_by_language: HashMap<Language, u64> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(lang, count)| lang.parse().ok().map(|l| (l, count)))
            .collect();

        // Chunks by type
        let mut stmt = conn
            .prepare("SELECT chunk_type, COUNT(*) FROM chunks GROUP BY chunk_type")?;
        let chunks_by_type: HashMap<ChunkType, u64> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(ct, count)| ct.parse().ok().map(|c| (c, count)))
            .collect();

        // Metadata
        let model_name: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'model_name'",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let created_at: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'created_at'",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let updated_at: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'updated_at'",
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| created_at.clone());
        let schema_version: i32 = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Index file size - handled by caller since we don't know the path here
        let index_size_bytes = 0;

        Ok(IndexStats {
            total_chunks,
            total_files,
            chunks_by_language,
            chunks_by_type,
            index_size_bytes,
            created_at,
            updated_at,
            model_name,
            schema_version,
        })
    }
}
