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
// v3: NL-based embeddings (code->NL translation before embedding)
// v4: Call graph (function call relationships)
// v5: Full call graph (captures calls from large functions)
const CURRENT_SCHEMA_VERSION: i32 = 5;
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

/// Thread-safe SQLite store for chunks and embeddings
///
/// Uses r2d2 connection pooling for concurrent reads and WAL mode
/// for crash safety. All methods take `&self` and are safe to call
/// from multiple threads.
///
/// # Example
///
/// ```no_run
/// use cqs::Store;
/// use std::path::Path;
///
/// let store = Store::open(Path::new(".cq/index.db"))?;
/// let stats = store.stats()?;
/// println!("Indexed {} chunks", stats.total_chunks);
/// # Ok::<(), anyhow::Error>(())
/// ```
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

/// Chunk metadata returned from search results
///
/// Contains all chunk information except the embedding vector.
#[derive(Debug, Clone)]
pub struct ChunkSummary {
    /// Unique identifier
    pub id: String,
    /// Source file path (relative to project root)
    pub file: PathBuf,
    /// Programming language
    pub language: Language,
    /// Type of code element
    pub chunk_type: ChunkType,
    /// Name of the function/class/etc.
    pub name: String,
    /// Function signature or declaration
    pub signature: String,
    /// Full source code
    pub content: String,
    /// Documentation comment if present
    pub doc: Option<String>,
    /// Starting line number (1-indexed)
    pub line_start: u32,
    /// Ending line number (1-indexed)
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

/// A search result with similarity score
#[derive(Debug)]
pub struct SearchResult {
    /// The matching chunk
    pub chunk: ChunkSummary,
    /// Similarity score (0.0 to 1.0, higher is better)
    pub score: f32,
}

/// Caller information from the full call graph
///
/// Unlike ChunkSummary, this doesn't require a chunk to exist -
/// it captures callers from large functions that exceed chunk size limits.
#[derive(Debug, Clone)]
pub struct CallerInfo {
    /// Function name
    pub name: String,
    /// Source file path
    pub file: PathBuf,
    /// Line where function starts
    pub line: u32,
}

/// Filter and scoring options for search
///
/// All fields are optional. Unset filters match all chunks.
#[derive(Default)]
pub struct SearchFilter {
    /// Filter by programming language(s)
    pub languages: Option<Vec<Language>>,
    /// Filter by file path glob pattern (e.g., `src/**/*.rs`)
    pub path_pattern: Option<String>,
    /// Weight for name matching in hybrid search (0.0-1.0)
    ///
    /// 0.0 = pure embedding similarity (default)
    /// 1.0 = pure name matching
    /// 0.2 = recommended for balanced results
    pub name_boost: f32,
    /// Query text for name matching (required if name_boost > 0 or enable_rrf)
    pub query_text: String,
    /// Enable RRF (Reciprocal Rank Fusion) hybrid search
    ///
    /// When enabled, combines semantic search results with FTS5 keyword search
    /// using the formula: score = Σ 1/(k + rank), where k=60.
    /// This typically improves recall for identifier-heavy queries.
    pub enable_rrf: bool,
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
    // Expected: 768 dimensions * 4 bytes = 3072 bytes
    const EXPECTED_BYTES: usize = 768 * 4;
    if bytes.len() != EXPECTED_BYTES {
        tracing::warn!(
            "Embedding byte length mismatch: expected {}, got {} (possible corruption)",
            EXPECTED_BYTES,
            bytes.len()
        );
    }
    bytes
        .chunks_exact(4)
        .map(|chunk| {
            // SAFETY: chunks_exact(4) guarantees exactly 4 bytes per chunk
            f32::from_le_bytes(chunk.try_into().expect("chunks_exact guarantees 4 bytes"))
        })
        .collect()
}

/// Cosine similarity for L2-normalized vectors (just dot product)
/// Uses SIMD acceleration when available (2-4x faster on AVX2/NEON)
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    use simsimd::SpatialSimilarity;
    f32::dot(a, b).unwrap_or_else(|| {
        // Fallback for unsupported architectures or mismatched lengths
        a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() as f64
    }) as f32
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

// Re-export tokenize_identifier from nl module for backwards compatibility
pub use crate::nl::tokenize_identifier;

/// Normalize code text for FTS5 indexing.
/// Splits identifiers on camelCase/snake_case boundaries and joins with spaces.
/// Example: "parseConfigFile" -> "parse config file"
pub fn normalize_for_fts(text: &str) -> String {
    // Split on word boundaries (spaces, punctuation, operators)
    let mut result = String::new();
    let mut current_word = String::new();

    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            current_word.push(c);
        } else if !current_word.is_empty() {
            // Tokenize this identifier
            let tokens = tokenize_identifier(&current_word);
            if !result.is_empty() && !tokens.is_empty() {
                result.push(' ');
            }
            result.push_str(&tokens.join(" "));
            current_word.clear();
        }
        // Skip punctuation/whitespace - we only want spaces between words
    }
    // Handle trailing word
    if !current_word.is_empty() {
        let tokens = tokenize_identifier(&current_word);
        if !result.is_empty() && !tokens.is_empty() {
            result.push(' ');
        }
        result.push_str(&tokens.join(" "));
    }
    result
}

impl Store {
    /// Open an existing index with connection pooling
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let manager = SqliteConnectionManager::file(path).with_init(|conn| {
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

            // FTS5: delete old entry, insert new (UPSERT not supported in FTS5)
            let mut fts_delete = tx.prepare_cached("DELETE FROM chunks_fts WHERE id = ?1")?;
            let mut fts_insert = tx.prepare_cached(
                "INSERT INTO chunks_fts (id, name, signature, content, doc) VALUES (?1, ?2, ?3, ?4, ?5)",
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

                // Update FTS5 index with normalized text
                let _ = fts_delete.execute(params![chunk.id]);
                fts_insert.execute(params![
                    chunk.id,
                    normalize_for_fts(&chunk.name),
                    normalize_for_fts(&chunk.signature),
                    normalize_for_fts(&chunk.content),
                    chunk
                        .doc
                        .as_ref()
                        .map(|d| normalize_for_fts(d))
                        .unwrap_or_default(),
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

    /// Search FTS5 index for keyword matches.
    /// Returns chunk IDs ranked by FTS5 relevance (BM25).
    /// Query is normalized before searching.
    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<String>, StoreError> {
        let conn = self.pool.get()?;

        // Normalize query for FTS matching
        let normalized_query = normalize_for_fts(query);
        if normalized_query.is_empty() {
            return Ok(vec![]);
        }

        // FTS5 MATCH query - search across all indexed columns
        // bm25() returns negative scores (more negative = better match)
        let sql = "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2";

        let mut stmt = conn.prepare(sql)?;
        let results: Vec<String> = stmt
            .query_map(params![normalized_query, limit as i64], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Compute RRF (Reciprocal Rank Fusion) scores for combining two ranked lists.
    /// Formula: score = Σ 1/(k + rank), where k=60 (standard constant).
    /// Returns IDs sorted by fused score (descending).
    fn rrf_fuse(semantic_ids: &[String], fts_ids: &[String], limit: usize) -> Vec<(String, f32)> {
        const K: f32 = 60.0;

        let mut scores: HashMap<String, f32> = HashMap::new();

        // Add semantic search contributions
        for (rank, id) in semantic_ids.iter().enumerate() {
            let contribution = 1.0 / (K + rank as f32 + 1.0);
            *scores.entry(id.clone()).or_insert(0.0) += contribution;
        }

        // Add FTS search contributions
        for (rank, id) in fts_ids.iter().enumerate() {
            let contribution = 1.0 / (K + rank as f32 + 1.0);
            *scores.entry(id.clone()).or_insert(0.0) += contribution;
        }

        // Sort by score descending
        let mut sorted: Vec<(String, f32)> = scores.into_iter().collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(limit);
        sorted
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

        // Check if we need hybrid scoring (weighted combination)
        let use_hybrid = filter.name_boost > 0.0 && !filter.query_text.is_empty();

        // Check if RRF is enabled
        let use_rrf = filter.enable_rrf && !filter.query_text.is_empty();

        // For RRF, we need more candidates to fuse
        let semantic_limit = if use_rrf { limit * 3 } else { limit };

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
            .filter_map(|r| match r {
                Ok(chunk) => Some(chunk),
                Err(e) => {
                    tracing::warn!("Skipped chunk due to DB error: {}", e);
                    None
                }
            })
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

        // Sort and take top-N (or more for RRF)
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(semantic_limit);

        // Apply RRF if enabled
        let final_scored: Vec<(String, f32)> = if use_rrf {
            // Get FTS5 results
            let fts_ids = self.search_fts(&filter.query_text, semantic_limit)?;
            let semantic_ids: Vec<String> = scored.iter().map(|(id, _)| id.clone()).collect();

            // Fuse rankings
            Self::rrf_fuse(&semantic_ids, &fts_ids, limit)
        } else {
            scored.truncate(limit);
            scored
        };

        if final_scored.is_empty() {
            return Ok(vec![]);
        }

        // Phase 2: Fetch full content only for top-N results
        let ids: Vec<&str> = final_scored.iter().map(|(id, _)| id.as_str()).collect();
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
        let results: Vec<SearchResult> = final_scored
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
        // Delete from FTS5 first (need the IDs)
        conn.execute(
            "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE file = ?1)",
            [file.to_string_lossy()],
        )?;
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
                // Delete from FTS5 first
                conn.execute(
                    "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE file = ?1)",
                    [&file],
                )?;
                deleted += conn.execute("DELETE FROM chunks WHERE file = ?1", [&file])? as u32;
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

    /// Get embeddings for chunks with matching content hashes (batch lookup).
    /// Returns a map of content_hash -> Embedding for hashes that exist in the index.
    /// Used to skip re-embedding unchanged chunks during incremental indexing.
    pub fn get_embeddings_by_hashes(&self, hashes: &[&str]) -> HashMap<String, Embedding> {
        if hashes.is_empty() {
            return HashMap::new();
        }
        let conn = match self.pool.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to get connection for hash lookup: {}", e);
                return HashMap::new();
            }
        };

        // Build IN clause: WHERE content_hash IN (?, ?, ...)
        let placeholders: Vec<&str> = (0..hashes.len()).map(|_| "?").collect();
        let sql = format!(
            "SELECT content_hash, embedding FROM chunks WHERE content_hash IN ({})",
            placeholders.join(", ")
        );

        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to prepare hash lookup query: {}", e);
                return HashMap::new();
            }
        };

        let mut result = HashMap::new();
        if let Ok(rows) = stmt.query_map(params_from_iter(hashes.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        }) {
            for row in rows.flatten() {
                result.insert(row.0, Embedding(bytes_to_embedding(&row.1)));
            }
        }
        result
    }

    /// Get index statistics
    pub fn stats(&self) -> Result<IndexStats, StoreError> {
        let conn = self.pool.get()?;

        let total_chunks: u64 = conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;

        let total_files: u64 =
            conn.query_row("SELECT COUNT(DISTINCT file) FROM chunks", [], |r| r.get(0))?;

        // Chunks by language
        let mut stmt = conn.prepare("SELECT language, COUNT(*) FROM chunks GROUP BY language")?;
        let chunks_by_language: HashMap<Language, u64> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(lang, count)| lang.parse().ok().map(|l| (l, count)))
            .collect();

        // Chunks by type
        let mut stmt =
            conn.prepare("SELECT chunk_type, COUNT(*) FROM chunks GROUP BY chunk_type")?;
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

    /// Get a single chunk by its ID
    pub fn get_chunk_by_id(&self, id: &str) -> Result<Option<ChunkSummary>, StoreError> {
        let conn = self.pool.get()?;
        let row = conn.query_row(
            "SELECT id, file, language, chunk_type, name, signature, content, doc, line_start, line_end
             FROM chunks WHERE id = ?1",
            [id],
            |row| {
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
            },
        );

        match row {
            Ok(r) => Ok(Some(ChunkSummary::from(r))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Search within a set of candidate IDs (for HNSW-guided filtered search)
    ///
    /// Instead of scanning all chunks, only fetches and scores the given candidates.
    /// This is 10-100x faster than brute-force when filters are combined with HNSW.
    pub fn search_by_candidate_ids(
        &self,
        candidate_ids: &[&str],
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>, StoreError> {
        if candidate_ids.is_empty() {
            return Ok(vec![]);
        }

        let conn = self.pool.get()?;

        // Check if we need hybrid scoring
        let use_hybrid = filter.name_boost > 0.0 && !filter.query_text.is_empty();

        // Fetch candidate chunks with embedding and metadata
        let placeholders: String = candidate_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, file, language, chunk_type, name, signature, content, doc, line_start, line_end, embedding
             FROM chunks WHERE id IN ({})",
            placeholders
        );

        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<(ChunkRow, Vec<u8>)> = stmt
            .query_map(rusqlite::params_from_iter(candidate_ids), |row| {
                Ok((
                    ChunkRow {
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
                    },
                    row.get::<_, Vec<u8>>(10)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        // Score and filter candidates
        let mut scored: Vec<(ChunkRow, f32)> = rows
            .into_iter()
            .filter_map(|(row, embedding_bytes)| {
                // Apply language filter
                if let Some(ref langs) = filter.languages {
                    let row_lang: Result<Language, _> = row.language.parse();
                    if let Ok(lang) = row_lang {
                        if !langs.contains(&lang) {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }

                // Apply path pattern filter
                if let Some(ref pattern) = filter.path_pattern {
                    if let Ok(glob_pattern) =
                        globset::Glob::new(pattern).map(|g| g.compile_matcher())
                    {
                        if !glob_pattern.is_match(&row.file) {
                            return None;
                        }
                    }
                }

                // Compute similarity score
                let embedding = bytes_to_embedding(&embedding_bytes);
                let embedding_score = cosine_similarity(&query.0, &embedding);

                // Compute hybrid score if enabled
                let score = if use_hybrid {
                    let name_score = name_match_score(&filter.query_text, &row.name);
                    (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
                } else {
                    embedding_score
                };

                if score >= threshold {
                    Some((row, score))
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending and take top-N
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // Convert to SearchResult
        let results: Vec<SearchResult> = scored
            .into_iter()
            .map(|(row, score)| SearchResult {
                chunk: ChunkSummary::from(row),
                score,
            })
            .collect();

        Ok(results)
    }

    /// Get all chunk IDs and embeddings (for HNSW index building)
    pub fn all_embeddings(&self) -> Result<Vec<(String, Embedding)>, StoreError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare("SELECT id, embedding FROM chunks")?;

        let results: Vec<(String, Embedding)> = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                Ok((id, Embedding(bytes_to_embedding(&bytes))))
            })?
            .filter_map(|r| match r {
                Ok(pair) => Some(pair),
                Err(e) => {
                    tracing::warn!("Skipped chunk due to DB error: {}", e);
                    None
                }
            })
            .collect();

        Ok(results)
    }

    // ============ Call Graph Methods ============

    /// Insert or replace call sites for a chunk
    ///
    /// Deletes existing calls for the chunk, then inserts new ones.
    pub fn upsert_calls(
        &self,
        chunk_id: &str,
        calls: &[crate::parser::CallSite],
    ) -> Result<(), StoreError> {
        let conn = self.pool.get()?;

        // Delete existing calls for this chunk
        conn.execute("DELETE FROM calls WHERE caller_id = ?1", [chunk_id])?;

        // Insert new calls
        let mut stmt = conn.prepare_cached(
            "INSERT INTO calls (caller_id, callee_name, line_number) VALUES (?1, ?2, ?3)",
        )?;

        for call in calls {
            stmt.execute(params![chunk_id, call.callee_name, call.line_number])?;
        }

        Ok(())
    }

    /// Find all chunks that call a given function name
    pub fn get_callers(&self, callee_name: &str) -> Result<Vec<ChunkSummary>, StoreError> {
        let conn = self.pool.get()?;

        let mut stmt = conn.prepare(
            "SELECT DISTINCT c.id, c.file, c.language, c.chunk_type, c.name, c.signature,
                    c.content, c.doc, c.line_start, c.line_end
             FROM chunks c
             JOIN calls ca ON c.id = ca.caller_id
             WHERE ca.callee_name = ?1
             ORDER BY c.file, c.line_start",
        )?;

        let rows: Vec<ChunkSummary> = stmt
            .query_map([callee_name], |row| {
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
            .map(ChunkSummary::from)
            .collect();

        Ok(rows)
    }

    /// Get all function names called by a given chunk
    pub fn get_callees(&self, chunk_id: &str) -> Result<Vec<String>, StoreError> {
        let conn = self.pool.get()?;

        let mut stmt = conn.prepare(
            "SELECT DISTINCT callee_name FROM calls WHERE caller_id = ?1 ORDER BY line_number",
        )?;

        let callees: Vec<String> = stmt
            .query_map([chunk_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(callees)
    }

    /// Get call graph statistics
    pub fn call_stats(&self) -> Result<(u64, u64), StoreError> {
        let conn = self.pool.get()?;

        let total_calls: u64 = conn.query_row("SELECT COUNT(*) FROM calls", [], |r| r.get(0))?;
        let unique_callees: u64 =
            conn.query_row("SELECT COUNT(DISTINCT callee_name) FROM calls", [], |r| {
                r.get(0)
            })?;

        Ok((total_calls, unique_callees))
    }

    // ============ Full Call Graph Methods (v5) ============

    /// Insert function calls for a file (full call graph, no size limits)
    pub fn upsert_function_calls(
        &self,
        file: &Path,
        function_calls: &[crate::parser::FunctionCalls],
    ) -> Result<(), StoreError> {
        let conn = self.pool.get()?;
        let file_str = file.to_string_lossy();

        // Delete existing calls for this file
        conn.execute("DELETE FROM function_calls WHERE file = ?1", [&file_str])?;

        // Insert new calls
        let mut stmt = conn.prepare_cached(
            "INSERT INTO function_calls (file, caller_name, caller_line, callee_name, call_line)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;

        for fc in function_calls {
            for call in &fc.calls {
                stmt.execute(params![
                    &file_str,
                    &fc.name,
                    fc.line_start,
                    &call.callee_name,
                    call.line_number,
                ])?;
            }
        }

        Ok(())
    }

    /// Find all callers of a function (from full call graph)
    ///
    /// This searches the function_calls table, which includes callers from
    /// large functions that exceed the 100-line chunk limit.
    pub fn get_callers_full(&self, callee_name: &str) -> Result<Vec<CallerInfo>, StoreError> {
        let conn = self.pool.get()?;

        let mut stmt = conn.prepare(
            "SELECT DISTINCT file, caller_name, caller_line
             FROM function_calls
             WHERE callee_name = ?1
             ORDER BY file, caller_line",
        )?;

        let rows: Vec<CallerInfo> = stmt
            .query_map([callee_name], |row| {
                Ok(CallerInfo {
                    file: PathBuf::from(row.get::<_, String>(0)?),
                    name: row.get(1)?,
                    line: row.get(2)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    }

    /// Get all callees of a function (from full call graph)
    pub fn get_callees_full(&self, caller_name: &str) -> Result<Vec<(String, u32)>, StoreError> {
        let conn = self.pool.get()?;

        let mut stmt = conn.prepare(
            "SELECT DISTINCT callee_name, call_line
             FROM function_calls
             WHERE caller_name = ?1
             ORDER BY call_line",
        )?;

        let callees: Vec<(String, u32)> = stmt
            .query_map([caller_name], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(callees)
    }

    /// Get full call graph statistics
    pub fn function_call_stats(&self) -> Result<(u64, u64, u64), StoreError> {
        let conn = self.pool.get()?;

        let total_calls: u64 =
            conn.query_row("SELECT COUNT(*) FROM function_calls", [], |r| r.get(0))?;
        let unique_callers: u64 = conn.query_row(
            "SELECT COUNT(DISTINCT caller_name) FROM function_calls",
            [],
            |r| r.get(0),
        )?;
        let unique_callees: u64 = conn.query_row(
            "SELECT COUNT(DISTINCT callee_name) FROM function_calls",
            [],
            |r| r.get(0),
        )?;

        Ok((total_calls, unique_callers, unique_callees))
    }
}
