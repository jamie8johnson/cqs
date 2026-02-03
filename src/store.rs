//! SQLite storage for chunks and embeddings (sqlx async with sync wrappers)
//!
//! Provides sync methods that internally use tokio runtime to execute async sqlx operations.
//! This allows callers to use the Store synchronously while benefiting from sqlx's async features.

use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::runtime::Runtime;

use crate::embedder::Embedding;
use crate::index::VectorIndex;
use crate::note::Note;
use crate::parser::{Chunk, ChunkType, Language};

// Schema version for migrations
// v3: NL-based embeddings (code->NL translation before embedding)
// v4: Call graph (function call relationships)
// v5: Full call graph (captures calls from large functions)
// v6-7: (deprecated hunches/scars, replaced by notes in v8)
// v8: Notes (unified memory with sentiment, 769-dim embeddings)
// v9: Windowing (parent_id, window_idx for chunking long functions)
// v10: Multi-source support (file -> origin, file_mtime -> source_mtime, + source_type)
const CURRENT_SCHEMA_VERSION: i32 = 10;
const MODEL_NAME: &str = "intfloat/e5-base-v2";

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Runtime error: {0}")]
    Runtime(String),
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
/// Uses sqlx connection pooling for concurrent reads and WAL mode
/// for crash safety. All methods are synchronous but internally use
/// an async runtime to execute sqlx operations.
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
    pool: SqlitePool,
    rt: Runtime,
}

/// Raw row from chunks table (for internal use)
#[derive(Clone)]
struct ChunkRow {
    id: String,
    origin: String,
    language: String,
    chunk_type: String,
    name: String,
    signature: String,
    content: String,
    doc: Option<String>,
    line_start: u32,
    line_end: u32,
    parent_id: Option<String>,
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
            file: PathBuf::from(row.origin),
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

/// Note metadata returned from search results
#[derive(Debug, Clone)]
pub struct NoteSummary {
    /// Unique identifier
    pub id: String,
    /// Note content
    pub text: String,
    /// Sentiment: -1.0 to +1.0
    pub sentiment: f32,
    /// Mentioned code paths/functions
    pub mentions: Vec<String>,
}

/// A note search result with similarity score
#[derive(Debug)]
pub struct NoteSearchResult {
    /// The matching note
    pub note: NoteSummary,
    /// Similarity score (0.0 to 1.0)
    pub score: f32,
}

/// Unified search result (code chunk or note)
#[derive(Debug)]
pub enum UnifiedResult {
    Code(SearchResult),
    Note(NoteSearchResult),
}

impl UnifiedResult {
    /// Get the similarity score
    pub fn score(&self) -> f32 {
        match self {
            UnifiedResult::Code(r) => r.score,
            UnifiedResult::Note(r) => r.score,
        }
    }
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
            dimensions: 769, // 768 from model + 1 sentiment
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
    embedding
        .as_slice()
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

/// Zero-copy view of embedding bytes as f32 slice (for hot paths)
pub fn embedding_slice(bytes: &[u8]) -> Option<&[f32]> {
    const EXPECTED_BYTES: usize = 769 * 4; // 768 model + 1 sentiment
    if bytes.len() != EXPECTED_BYTES {
        return None;
    }
    Some(bytemuck::cast_slice(bytes))
}

/// Convert embedding bytes to owned Vec (when ownership needed)
pub fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    embedding_slice(bytes)
        .map(|s| s.to_vec())
        .unwrap_or_else(|| {
            tracing::warn!(
                "Embedding byte length mismatch: expected {}, got {} (possible corruption)",
                769 * 4,
                bytes.len()
            );
            bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("4 bytes")))
                .collect()
        })
}

/// Cosine similarity for L2-normalized vectors (just dot product)
/// Uses SIMD acceleration when available (2-4x faster on AVX2/NEON)
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "Embedding dimension mismatch");
    debug_assert_eq!(a.len(), 769, "Expected 769-dim embeddings");
    use simsimd::SpatialSimilarity;
    f32::dot(a, b).unwrap_or_else(|| {
        // Fallback for unsupported architectures
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
        let rt = Runtime::new().map_err(|e| StoreError::Runtime(e.to_string()))?;

        let db_url = format!("sqlite://{}?mode=rwc", path.display());

        let pool = rt.block_on(async {
            SqlitePoolOptions::new()
                .max_connections(4)
                .after_connect(|conn, _meta| {
                    Box::pin(async move {
                        sqlx::query("PRAGMA journal_mode = WAL")
                            .execute(&mut *conn)
                            .await?;
                        sqlx::query("PRAGMA busy_timeout = 5000")
                            .execute(&mut *conn)
                            .await?;
                        sqlx::query("PRAGMA synchronous = NORMAL")
                            .execute(&mut *conn)
                            .await?;
                        sqlx::query("PRAGMA cache_size = -65536")
                            .execute(&mut *conn)
                            .await?;
                        sqlx::query("PRAGMA temp_store = MEMORY")
                            .execute(&mut *conn)
                            .await?;
                        sqlx::query("PRAGMA mmap_size = 268435456")
                            .execute(&mut *conn)
                            .await?;
                        Ok(())
                    })
                })
                .connect(&db_url)
                .await
        })?;

        let store = Self { pool, rt };

        // Check schema version compatibility
        store.check_schema_version()?;
        // Check model version compatibility
        store.check_model_version()?;
        // Warn if index was created by different cq version
        store.check_cq_version();

        Ok(store)
    }

    /// Create a new index
    pub fn init(&self, model_info: &ModelInfo) -> Result<(), StoreError> {
        self.rt.block_on(async {
            // Create tables - execute each statement separately
            // sqlx::query() only handles single statements
            let schema = include_str!("schema.sql");
            for statement in schema.split(';') {
                // Strip leading comment-only lines, keep the SQL
                let stmt: String = statement
                    .lines()
                    .skip_while(|line| {
                        let trimmed = line.trim();
                        trimmed.is_empty() || trimmed.starts_with("--")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let stmt = stmt.trim();
                if stmt.is_empty() {
                    continue;
                }
                sqlx::query(stmt).execute(&self.pool).await?;
            }

            // Store metadata
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query("INSERT INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("schema_version")
                .bind(CURRENT_SCHEMA_VERSION.to_string())
                .execute(&self.pool)
                .await?;
            sqlx::query("INSERT INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("model_name")
                .bind(&model_info.name)
                .execute(&self.pool)
                .await?;
            sqlx::query("INSERT INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("dimensions")
                .bind(model_info.dimensions.to_string())
                .execute(&self.pool)
                .await?;
            sqlx::query("INSERT INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("created_at")
                .bind(&now)
                .execute(&self.pool)
                .await?;
            sqlx::query("INSERT INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("cq_version")
                .bind(env!("CARGO_PKG_VERSION"))
                .execute(&self.pool)
                .await?;

            Ok(())
        })
    }

    fn check_schema_version(&self) -> Result<(), StoreError> {
        self.rt.block_on(async {
            // Try to read schema version - if table doesn't exist, it's a new database
            let row: Option<(String,)> =
                match sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_optional(&self.pool)
                    .await
                {
                    Ok(r) => r,
                    Err(sqlx::Error::Database(e)) if e.message().contains("no such table") => {
                        // New database, no tables yet - that's fine
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                };

            let version: i32 = row.and_then(|(s,)| s.parse().ok()).unwrap_or(0);

            if version > CURRENT_SCHEMA_VERSION {
                return Err(StoreError::SchemaNewerThanCq(version));
            }
            if version < CURRENT_SCHEMA_VERSION && version > 0 {
                return Err(StoreError::SchemaMismatch(version, CURRENT_SCHEMA_VERSION));
            }
            Ok(())
        })
    }

    fn check_model_version(&self) -> Result<(), StoreError> {
        self.rt.block_on(async {
            // Try to read model name - if table doesn't exist, it's a new database
            let row: Option<(String,)> =
                match sqlx::query_as("SELECT value FROM metadata WHERE key = 'model_name'")
                    .fetch_optional(&self.pool)
                    .await
                {
                    Ok(r) => r,
                    Err(sqlx::Error::Database(e)) if e.message().contains("no such table") => {
                        // New database, no tables yet - that's fine
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                };

            let stored_model = row.map(|(s,)| s).unwrap_or_default();

            if !stored_model.is_empty() && stored_model != MODEL_NAME {
                return Err(StoreError::ModelMismatch(
                    stored_model,
                    MODEL_NAME.to_string(),
                ));
            }
            Ok(())
        })
    }

    /// Warn if index was created by a different version of cqs (informational only)
    fn check_cq_version(&self) {
        let _ = self.rt.block_on(async {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'cq_version'")
                    .fetch_optional(&self.pool)
                    .await
                    .ok()
                    .flatten();

            let stored_version = row.map(|(s,)| s).unwrap_or_default();
            let current_version = env!("CARGO_PKG_VERSION");

            if !stored_version.is_empty() && stored_version != current_version {
                tracing::info!(
                    "Index created by cqs v{}, running v{}",
                    stored_version,
                    current_version
                );
            }
            Ok::<_, StoreError>(())
        });
    }

    /// Insert or update chunks in batch (10x faster than individual inserts)
    pub fn upsert_chunks_batch(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
    ) -> Result<usize, StoreError> {
        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            let now = chrono::Utc::now().to_rfc3339();
            for (chunk, embedding) in chunks {
                sqlx::query(
                    "INSERT OR REPLACE INTO chunks (id, origin, source_type, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, source_mtime, created_at, updated_at, parent_id, window_idx)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                )
                .bind(&chunk.id)
                .bind(chunk.file.to_string_lossy().to_string())
                .bind("file")
                .bind(chunk.language.to_string())
                .bind(chunk.chunk_type.to_string())
                .bind(&chunk.name)
                .bind(&chunk.signature)
                .bind(&chunk.content)
                .bind(&chunk.content_hash)
                .bind(&chunk.doc)
                .bind(chunk.line_start as i64)
                .bind(chunk.line_end as i64)
                .bind(embedding_to_bytes(embedding))
                .bind(source_mtime)
                .bind(&now)
                .bind(&now)
                .bind(&chunk.parent_id)
                .bind(chunk.window_idx.map(|i| i as i64))
                .execute(&mut *tx)
                .await?;

                let _ = sqlx::query("DELETE FROM chunks_fts WHERE id = ?1")
                    .bind(&chunk.id)
                    .execute(&mut *tx)
                    .await;

                sqlx::query(
                    "INSERT INTO chunks_fts (id, name, signature, content, doc) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .bind(&chunk.id)
                .bind(normalize_for_fts(&chunk.name))
                .bind(normalize_for_fts(&chunk.signature))
                .bind(normalize_for_fts(&chunk.content))
                .bind(chunk.doc.as_ref().map(|d| normalize_for_fts(d)).unwrap_or_default())
                .execute(&mut *tx)
                .await?;
            }

            tx.commit().await?;
            Ok(chunks.len())
        })
    }

    /// Insert or update a single chunk
    pub fn upsert_chunk(
        &self,
        chunk: &Chunk,
        embedding: &Embedding,
        source_mtime: Option<i64>,
    ) -> Result<(), StoreError> {
        self.upsert_chunks_batch(&[(chunk.clone(), embedding.clone())], source_mtime)?;
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

        self.rt.block_on(async {
            let row: Option<(Option<i64>,)> =
                sqlx::query_as("SELECT source_mtime FROM chunks WHERE origin = ?1 LIMIT 1")
                    .bind(path.to_string_lossy().to_string())
                    .fetch_optional(&self.pool)
                    .await?;

            match row {
                Some((Some(mtime),)) if mtime >= current_mtime => Ok(false),
                _ => Ok(true),
            }
        })
    }

    /// Search FTS5 index for keyword matches.
    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<String>, StoreError> {
        let normalized_query = normalize_for_fts(query);
        if normalized_query.is_empty() {
            return Ok(vec![]);
        }

        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2",
            )
            .bind(&normalized_query)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows.into_iter().map(|(id,)| id).collect())
        })
    }

    /// Compute RRF (Reciprocal Rank Fusion) scores for combining two ranked lists.
    fn rrf_fuse(semantic_ids: &[String], fts_ids: &[String], limit: usize) -> Vec<(String, f32)> {
        const K: f32 = 60.0;

        let mut scores: HashMap<&str, f32> = HashMap::new();

        for (rank, id) in semantic_ids.iter().enumerate() {
            let contribution = 1.0 / (K + rank as f32 + 1.0);
            *scores.entry(id.as_str()).or_insert(0.0) += contribution;
        }

        for (rank, id) in fts_ids.iter().enumerate() {
            let contribution = 1.0 / (K + rank as f32 + 1.0);
            *scores.entry(id.as_str()).or_insert(0.0) += contribution;
        }

        let mut sorted: Vec<(String, f32)> = scores
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
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
        let _span = tracing::info_span!("search_filtered", limit = limit, rrf = filter.enable_rrf)
            .entered();

        self.rt.block_on(async {
            // Build WHERE clause from filter
            let mut conditions = Vec::new();
            let mut bind_values: Vec<String> = Vec::new();

            if let Some(ref langs) = filter.languages {
                let placeholders: Vec<_> = (0..langs.len())
                    .map(|i| format!("?{}", bind_values.len() + i + 1))
                    .collect();
                conditions.push(format!("language IN ({})", placeholders.join(",")));
                for lang in langs {
                    bind_values.push(lang.to_string());
                }
            }

            let where_clause = if conditions.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", conditions.join(" AND "))
            };

            let use_hybrid = filter.name_boost > 0.0 && !filter.query_text.is_empty();
            let use_rrf = filter.enable_rrf && !filter.query_text.is_empty();
            let semantic_limit = if use_rrf { limit * 3 } else { limit };

            let sql = if use_hybrid {
                format!("SELECT id, embedding, name FROM chunks{}", where_clause)
            } else {
                format!("SELECT id, embedding FROM chunks{}", where_clause)
            };

            let rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for val in &bind_values {
                    q = q.bind(val);
                }
                q.fetch_all(&self.pool).await?
            };

            let mut scored: Vec<(String, f32)> = rows
                .iter()
                .filter_map(|row| {
                    let id: String = row.get(0);
                    let embedding_bytes: Vec<u8> = row.get(1);
                    let name: Option<String> = if use_hybrid { row.get(2) } else { None };

                    let embedding = embedding_slice(&embedding_bytes)?;
                    let embedding_score = cosine_similarity(query.as_slice(), embedding);

                    let score = if use_hybrid {
                        let n = name.as_deref().unwrap_or("");
                        let name_score = name_match_score(&filter.query_text, n);
                        (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
                    } else {
                        embedding_score
                    };

                    if let Some(ref pattern) = filter.path_pattern {
                        if let Ok(glob_pattern) =
                            globset::Glob::new(pattern).map(|g| g.compile_matcher())
                        {
                            let file_part = id.split(':').next().unwrap_or("");
                            if !glob_pattern.is_match(file_part) {
                                return None;
                            }
                        }
                    }

                    if score >= threshold {
                        Some((id, score))
                    } else {
                        None
                    }
                })
                .collect();

            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(semantic_limit);

            let final_scored: Vec<(String, f32)> = if use_rrf {
                let fts_ids = {
                    let normalized_query = normalize_for_fts(&filter.query_text);
                    if normalized_query.is_empty() {
                        vec![]
                    } else {
                        let fts_rows: Vec<(String,)> = sqlx::query_as(
                            "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2",
                        )
                        .bind(&normalized_query)
                        .bind(semantic_limit as i64)
                        .fetch_all(&self.pool)
                        .await?;
                        fts_rows.into_iter().map(|(id,)| id).collect()
                    }
                };
                let semantic_ids: Vec<String> = scored.iter().map(|(id, _)| id.clone()).collect();
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
            let placeholders: String = (1..=ids.len()).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id
                 FROM chunks WHERE id IN ({})",
                placeholders
            );

            let detail_rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for id in &ids {
                    q = q.bind(*id);
                }
                q.fetch_all(&self.pool).await?
            };

            let rows_map: HashMap<String, ChunkRow> = detail_rows
                .into_iter()
                .map(|row| {
                    let chunk_row = ChunkRow {
                        id: row.get(0),
                        origin: row.get(1),
                        language: row.get(2),
                        chunk_type: row.get(3),
                        name: row.get(4),
                        signature: row.get(5),
                        content: row.get(6),
                        doc: row.get(7),
                        line_start: row.get::<i64, _>(8) as u32,
                        line_end: row.get::<i64, _>(9) as u32,
                        parent_id: row.get(10),
                    };
                    (chunk_row.id.clone(), chunk_row)
                })
                .collect();

            let mut seen_parents: HashSet<String> = HashSet::new();
            let results: Vec<SearchResult> = final_scored
                .into_iter()
                .filter_map(|(id, score)| {
                    rows_map.get(&id).and_then(|row| {
                        let dedup_key = row.parent_id.clone().unwrap_or_else(|| row.id.clone());
                        if seen_parents.insert(dedup_key) {
                            Some(SearchResult {
                                chunk: ChunkSummary::from(row.clone()),
                                score,
                            })
                        } else {
                            None
                        }
                    })
                })
                .collect();

            Ok(results)
        })
    }

    /// Search with optional vector index for O(log n) candidate retrieval
    pub fn search_filtered_with_index(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
        index: Option<&dyn VectorIndex>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        if let Some(idx) = index {
            let _span = tracing::info_span!("search_index_guided", limit = limit).entered();

            let candidate_count = (limit * 5).max(100);
            let index_results = idx.search(query, candidate_count);

            if index_results.is_empty() {
                tracing::debug!("Index returned no candidates, falling back to brute-force");
                return self.search_filtered(query, filter, limit, threshold);
            }

            tracing::debug!("Index returned {} candidates", index_results.len());

            let candidate_ids: Vec<&str> = index_results.iter().map(|r| r.id.as_str()).collect();
            return self.search_by_candidate_ids(&candidate_ids, query, filter, limit, threshold);
        }

        self.search_filtered(query, filter, limit, threshold)
    }

    /// Delete all chunks for an origin (file path or source identifier)
    pub fn delete_by_origin(&self, origin: &Path) -> Result<u32, StoreError> {
        let origin_str = origin.to_string_lossy().to_string();

        self.rt.block_on(async {
            sqlx::query(
                "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin = ?1)",
            )
            .bind(&origin_str)
            .execute(&self.pool)
            .await?;

            let result = sqlx::query("DELETE FROM chunks WHERE origin = ?1")
                .bind(&origin_str)
                .execute(&self.pool)
                .await?;

            Ok(result.rows_affected() as u32)
        })
    }

    /// Delete chunks for files that no longer exist
    pub fn prune_missing(&self, existing_files: &HashSet<PathBuf>) -> Result<u32, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&self.pool)
            .await?;

            let mut deleted = 0u32;
            for (origin,) in rows {
                let path = PathBuf::from(&origin);
                if !existing_files.contains(&path) {
                    sqlx::query(
                        "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin = ?1)",
                    )
                    .bind(&origin)
                    .execute(&self.pool)
                    .await?;

                    let result = sqlx::query("DELETE FROM chunks WHERE origin = ?1")
                        .bind(&origin)
                        .execute(&self.pool)
                        .await?;

                    deleted += result.rows_affected() as u32;
                }
            }
            Ok(deleted)
        })
    }

    /// Get embedding by content hash (for reuse when content unchanged)
    pub fn get_by_content_hash(&self, hash: &str) -> Option<Embedding> {
        self.rt.block_on(async {
            let row: Option<(Vec<u8>,)> =
                sqlx::query_as("SELECT embedding FROM chunks WHERE content_hash = ?1 LIMIT 1")
                    .bind(hash)
                    .fetch_optional(&self.pool)
                    .await
                    .ok()?;

            row.map(|(bytes,)| Embedding::new(bytes_to_embedding(&bytes)))
        })
    }

    /// Get embeddings for chunks with matching content hashes (batch lookup).
    pub fn get_embeddings_by_hashes(&self, hashes: &[&str]) -> HashMap<String, Embedding> {
        if hashes.is_empty() {
            return HashMap::new();
        }

        self.rt.block_on(async {
            let placeholders: String = (1..=hashes.len())
                .map(|i| format!("?{}", i))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT content_hash, embedding FROM chunks WHERE content_hash IN ({})",
                placeholders
            );

            let rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for hash in hashes {
                    q = q.bind(*hash);
                }
                match q.fetch_all(&self.pool).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Failed to fetch embeddings by hash: {}", e);
                        return HashMap::new();
                    }
                }
            };

            let mut result = HashMap::new();
            for row in rows {
                let hash: String = row.get(0);
                let bytes: Vec<u8> = row.get(1);
                result.insert(hash, Embedding::new(bytes_to_embedding(&bytes)));
            }
            result
        })
    }

    /// Get the number of chunks in the index
    pub fn chunk_count(&self) -> Result<usize, StoreError> {
        self.rt.block_on(async {
            let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
                .fetch_one(&self.pool)
                .await?;
            Ok(row.0 as usize)
        })
    }

    /// Get index statistics
    pub fn stats(&self) -> Result<IndexStats, StoreError> {
        self.rt.block_on(async {
            let (total_chunks,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
                .fetch_one(&self.pool)
                .await?;

            let (total_files,): (i64,) =
                sqlx::query_as("SELECT COUNT(DISTINCT origin) FROM chunks")
                    .fetch_one(&self.pool)
                    .await?;

            let lang_rows: Vec<(String, i64)> =
                sqlx::query_as("SELECT language, COUNT(*) FROM chunks GROUP BY language")
                    .fetch_all(&self.pool)
                    .await?;

            let chunks_by_language: HashMap<Language, u64> = lang_rows
                .into_iter()
                .filter_map(|(lang, count)| lang.parse().ok().map(|l| (l, count as u64)))
                .collect();

            let type_rows: Vec<(String, i64)> =
                sqlx::query_as("SELECT chunk_type, COUNT(*) FROM chunks GROUP BY chunk_type")
                    .fetch_all(&self.pool)
                    .await?;

            let chunks_by_type: HashMap<ChunkType, u64> = type_rows
                .into_iter()
                .filter_map(|(ct, count)| ct.parse().ok().map(|c| (c, count as u64)))
                .collect();

            let model_name: String = sqlx::query_as::<_, (String,)>(
                "SELECT value FROM metadata WHERE key = 'model_name'",
            )
            .fetch_optional(&self.pool)
            .await?
            .map(|(s,)| s)
            .unwrap_or_default();

            let created_at: String = sqlx::query_as::<_, (String,)>(
                "SELECT value FROM metadata WHERE key = 'created_at'",
            )
            .fetch_optional(&self.pool)
            .await?
            .map(|(s,)| s)
            .unwrap_or_default();

            let updated_at: String = sqlx::query_as::<_, (String,)>(
                "SELECT value FROM metadata WHERE key = 'updated_at'",
            )
            .fetch_optional(&self.pool)
            .await?
            .map(|(s,)| s)
            .unwrap_or_else(|| created_at.clone());

            let schema_version: i32 = sqlx::query_as::<_, (String,)>(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
            )
            .fetch_optional(&self.pool)
            .await?
            .and_then(|(s,)| s.parse().ok())
            .unwrap_or(0);

            Ok(IndexStats {
                total_chunks: total_chunks as u64,
                total_files: total_files as u64,
                chunks_by_language,
                chunks_by_type,
                index_size_bytes: 0,
                created_at,
                updated_at,
                model_name,
                schema_version,
            })
        })
    }

    /// Get a single chunk by its ID
    pub fn get_chunk_by_id(&self, id: &str) -> Result<Option<ChunkSummary>, StoreError> {
        self.rt.block_on(async {
            let row: Option<_> = sqlx::query(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id
                 FROM chunks WHERE id = ?1",
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

            Ok(row.map(|r| {
                ChunkSummary::from(ChunkRow {
                    id: r.get(0),
                    origin: r.get(1),
                    language: r.get(2),
                    chunk_type: r.get(3),
                    name: r.get(4),
                    signature: r.get(5),
                    content: r.get(6),
                    doc: r.get(7),
                    line_start: r.get::<i64, _>(8) as u32,
                    line_end: r.get::<i64, _>(9) as u32,
                    parent_id: r.get(10),
                })
            }))
        })
    }

    /// Search within a set of candidate IDs (for HNSW-guided filtered search)
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

        let use_hybrid = filter.name_boost > 0.0 && !filter.query_text.is_empty();

        self.rt.block_on(async {
            let placeholders: String = (1..=candidate_ids.len())
                .map(|i| format!("?{}", i))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, embedding, parent_id
                 FROM chunks WHERE id IN ({})",
                placeholders
            );

            let rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for id in candidate_ids {
                    q = q.bind(*id);
                }
                q.fetch_all(&self.pool).await?
            };

            let mut scored: Vec<(ChunkRow, f32)> = rows
                .into_iter()
                .filter_map(|row| {
                    let chunk_row = ChunkRow {
                        id: row.get(0),
                        origin: row.get(1),
                        language: row.get(2),
                        chunk_type: row.get(3),
                        name: row.get(4),
                        signature: row.get(5),
                        content: row.get(6),
                        doc: row.get(7),
                        line_start: row.get::<i64, _>(8) as u32,
                        line_end: row.get::<i64, _>(9) as u32,
                        parent_id: row.get(11),
                    };
                    let embedding_bytes: Vec<u8> = row.get(10);

                    if let Some(ref langs) = filter.languages {
                        let row_lang: Result<Language, _> = chunk_row.language.parse();
                        if let Ok(lang) = row_lang {
                            if !langs.contains(&lang) {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    }

                    if let Some(ref pattern) = filter.path_pattern {
                        if let Ok(glob_pattern) =
                            globset::Glob::new(pattern).map(|g| g.compile_matcher())
                        {
                            if !glob_pattern.is_match(&chunk_row.origin) {
                                return None;
                            }
                        }
                    }

                    let embedding = match embedding_slice(&embedding_bytes) {
                        Some(e) => e,
                        None => return None,
                    };
                    let embedding_score = cosine_similarity(query.as_slice(), embedding);

                    let score = if use_hybrid {
                        let name_score = name_match_score(&filter.query_text, &chunk_row.name);
                        (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
                    } else {
                        embedding_score
                    };

                    if score >= threshold {
                        Some((chunk_row, score))
                    } else {
                        None
                    }
                })
                .collect();

            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut seen_parents: HashSet<String> = HashSet::new();
            let results: Vec<SearchResult> = scored
                .into_iter()
                .filter_map(|(row, score)| {
                    let dedup_key = row.parent_id.clone().unwrap_or_else(|| row.id.clone());
                    if seen_parents.insert(dedup_key) {
                        Some(SearchResult {
                            chunk: ChunkSummary::from(row),
                            score,
                        })
                    } else {
                        None
                    }
                })
                .take(limit)
                .collect();

            Ok(results)
        })
    }

    /// Get all chunk IDs and embeddings (for HNSW index building)
    pub fn all_embeddings(&self) -> Result<Vec<(String, Embedding)>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query("SELECT id, embedding FROM chunks")
                .fetch_all(&self.pool)
                .await?;

            let results: Vec<(String, Embedding)> = rows
                .into_iter()
                .map(|row| {
                    let id: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    (id, Embedding::new(bytes_to_embedding(&bytes)))
                })
                .collect();

            Ok(results)
        })
    }

    // ============ Call Graph Methods ============

    /// Insert or replace call sites for a chunk
    pub fn upsert_calls(
        &self,
        chunk_id: &str,
        calls: &[crate::parser::CallSite],
    ) -> Result<(), StoreError> {
        self.rt.block_on(async {
            sqlx::query("DELETE FROM calls WHERE caller_id = ?1")
                .bind(chunk_id)
                .execute(&self.pool)
                .await?;

            for call in calls {
                sqlx::query(
                    "INSERT INTO calls (caller_id, callee_name, line_number) VALUES (?1, ?2, ?3)",
                )
                .bind(chunk_id)
                .bind(&call.callee_name)
                .bind(call.line_number as i64)
                .execute(&self.pool)
                .await?;
            }

            Ok(())
        })
    }

    /// Find all chunks that call a given function name
    pub fn get_callers(&self, callee_name: &str) -> Result<Vec<ChunkSummary>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT DISTINCT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature,
                        c.content, c.doc, c.line_start, c.line_end, c.parent_id
                 FROM chunks c
                 JOIN calls ca ON c.id = ca.caller_id
                 WHERE ca.callee_name = ?1
                 ORDER BY c.origin, c.line_start",
            )
            .bind(callee_name)
            .fetch_all(&self.pool)
            .await?;

            let chunks: Vec<ChunkSummary> = rows
                .into_iter()
                .map(|row| {
                    ChunkSummary::from(ChunkRow {
                        id: row.get(0),
                        origin: row.get(1),
                        language: row.get(2),
                        chunk_type: row.get(3),
                        name: row.get(4),
                        signature: row.get(5),
                        content: row.get(6),
                        doc: row.get(7),
                        line_start: row.get::<i64, _>(8) as u32,
                        line_end: row.get::<i64, _>(9) as u32,
                        parent_id: row.get(10),
                    })
                })
                .collect();

            Ok(chunks)
        })
    }

    /// Get all function names called by a given chunk
    pub fn get_callees(&self, chunk_id: &str) -> Result<Vec<String>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT callee_name FROM calls WHERE caller_id = ?1 ORDER BY line_number",
            )
            .bind(chunk_id)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows.into_iter().map(|(s,)| s).collect())
        })
    }

    /// Get call graph statistics
    pub fn call_stats(&self) -> Result<(u64, u64), StoreError> {
        self.rt.block_on(async {
            let (total_calls,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM calls")
                .fetch_one(&self.pool)
                .await?;
            let (unique_callees,): (i64,) =
                sqlx::query_as("SELECT COUNT(DISTINCT callee_name) FROM calls")
                    .fetch_one(&self.pool)
                    .await?;

            Ok((total_calls as u64, unique_callees as u64))
        })
    }

    // ============ Full Call Graph Methods (v5) ============

    /// Insert function calls for a file (full call graph, no size limits)
    pub fn upsert_function_calls(
        &self,
        file: &Path,
        function_calls: &[crate::parser::FunctionCalls],
    ) -> Result<(), StoreError> {
        let file_str = file.to_string_lossy().to_string();

        self.rt.block_on(async {
            sqlx::query("DELETE FROM function_calls WHERE file = ?1")
                .bind(&file_str)
                .execute(&self.pool)
                .await?;

            for fc in function_calls {
                for call in &fc.calls {
                    sqlx::query(
                        "INSERT INTO function_calls (file, caller_name, caller_line, callee_name, call_line)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                    )
                    .bind(&file_str)
                    .bind(&fc.name)
                    .bind(fc.line_start as i64)
                    .bind(&call.callee_name)
                    .bind(call.line_number as i64)
                    .execute(&self.pool)
                    .await?;
                }
            }

            Ok(())
        })
    }

    /// Find all callers of a function (from full call graph)
    pub fn get_callers_full(&self, callee_name: &str) -> Result<Vec<CallerInfo>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<(String, String, i64)> = sqlx::query_as(
                "SELECT DISTINCT file, caller_name, caller_line
                 FROM function_calls
                 WHERE callee_name = ?1
                 ORDER BY file, caller_line",
            )
            .bind(callee_name)
            .fetch_all(&self.pool)
            .await?;

            let callers: Vec<CallerInfo> = rows
                .into_iter()
                .map(|(file, name, line)| CallerInfo {
                    file: PathBuf::from(file),
                    name,
                    line: line as u32,
                })
                .collect();

            Ok(callers)
        })
    }

    /// Get all callees of a function (from full call graph)
    pub fn get_callees_full(&self, caller_name: &str) -> Result<Vec<(String, u32)>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT DISTINCT callee_name, call_line
                 FROM function_calls
                 WHERE caller_name = ?1
                 ORDER BY call_line",
            )
            .bind(caller_name)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(name, line)| (name, line as u32))
                .collect())
        })
    }

    /// Get full call graph statistics
    pub fn function_call_stats(&self) -> Result<(u64, u64, u64), StoreError> {
        self.rt.block_on(async {
            let (total_calls,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM function_calls")
                .fetch_one(&self.pool)
                .await?;
            let (unique_callers,): (i64,) =
                sqlx::query_as("SELECT COUNT(DISTINCT caller_name) FROM function_calls")
                    .fetch_one(&self.pool)
                    .await?;
            let (unique_callees,): (i64,) =
                sqlx::query_as("SELECT COUNT(DISTINCT callee_name) FROM function_calls")
                    .fetch_one(&self.pool)
                    .await?;

            Ok((
                total_calls as u64,
                unique_callers as u64,
                unique_callees as u64,
            ))
        })
    }

    /// Unified search across code chunks and notes
    pub fn search_unified(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<UnifiedResult>, StoreError> {
        let code_results = self.search_filtered(query, filter, limit, threshold)?;
        let note_results = self.search_notes(query, limit, threshold)?;

        let min_code_slots = (limit * 3) / 5;
        let code_count = code_results.len().min(limit);
        let reserved_code = code_count.min(min_code_slots);
        let note_slots = limit.saturating_sub(reserved_code);

        let mut unified: Vec<UnifiedResult> = code_results
            .into_iter()
            .take(limit)
            .map(UnifiedResult::Code)
            .collect();

        let notes_to_add: Vec<UnifiedResult> = note_results
            .into_iter()
            .take(note_slots)
            .map(UnifiedResult::Note)
            .collect();
        unified.extend(notes_to_add);

        unified.sort_by(|a, b| {
            b.score()
                .partial_cmp(&a.score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        unified.truncate(limit);

        Ok(unified)
    }

    /// Unified search with optional vector index
    pub fn search_unified_with_index(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
        index: Option<&dyn VectorIndex>,
    ) -> Result<Vec<UnifiedResult>, StoreError> {
        let code_results =
            self.search_filtered_with_index(query, filter, limit, threshold, index)?;
        let note_results = self.search_notes(query, limit, threshold)?;

        let min_code_slots = (limit * 3) / 5;
        let code_count = code_results.len().min(limit);
        let reserved_code = code_count.min(min_code_slots);
        let note_slots = limit.saturating_sub(reserved_code);

        let mut unified: Vec<UnifiedResult> = code_results
            .into_iter()
            .take(limit)
            .map(UnifiedResult::Code)
            .collect();

        let notes_to_add: Vec<UnifiedResult> = note_results
            .into_iter()
            .take(note_slots)
            .map(UnifiedResult::Note)
            .collect();
        unified.extend(notes_to_add);

        unified.sort_by(|a, b| {
            b.score()
                .partial_cmp(&a.score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        unified.truncate(limit);

        Ok(unified)
    }

    // ============ Note Methods (v8) ============

    /// Insert or update notes in batch
    pub fn upsert_notes_batch(
        &self,
        notes: &[(Note, crate::embedder::Embedding)],
        source_file: &Path,
        file_mtime: i64,
    ) -> Result<usize, StoreError> {
        let source_str = source_file.to_string_lossy().to_string();

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            let now = chrono::Utc::now().to_rfc3339();
            for (note, embedding) in notes {
                let mentions_json = serde_json::to_string(&note.mentions).unwrap_or_default();

                sqlx::query(
                    "INSERT OR REPLACE INTO notes (id, text, sentiment, mentions, embedding, source_file, file_mtime, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .bind(&note.id)
                .bind(&note.text)
                .bind(note.sentiment)
                .bind(&mentions_json)
                .bind(embedding_to_bytes(embedding))
                .bind(&source_str)
                .bind(file_mtime)
                .bind(&now)
                .bind(&now)
                .execute(&mut *tx)
                .await?;

                let _ = sqlx::query("DELETE FROM notes_fts WHERE id = ?1")
                    .bind(&note.id)
                    .execute(&mut *tx)
                    .await;

                sqlx::query("INSERT INTO notes_fts (id, text) VALUES (?1, ?2)")
                    .bind(&note.id)
                    .bind(normalize_for_fts(&note.text))
                    .execute(&mut *tx)
                    .await?;
            }

            tx.commit().await?;
            Ok(notes.len())
        })
    }

    /// Search notes by embedding similarity
    pub fn search_notes(
        &self,
        query: &Embedding,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<NoteSearchResult>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<_> =
                sqlx::query("SELECT id, text, sentiment, mentions, embedding FROM notes")
                    .fetch_all(&self.pool)
                    .await?;

            let mut scored: Vec<(NoteSummary, f32)> = rows
                .into_iter()
                .filter_map(|row| {
                    let id: String = row.get(0);
                    let text: String = row.get(1);
                    let sentiment: f64 = row.get(2);
                    let mentions_json: String = row.get(3);
                    let embedding_bytes: Vec<u8> = row.get(4);

                    let mentions: Vec<String> =
                        serde_json::from_str(&mentions_json).unwrap_or_default();

                    let embedding = embedding_slice(&embedding_bytes)?;
                    let score = cosine_similarity(query.as_slice(), embedding);

                    if score >= threshold {
                        Some((
                            NoteSummary {
                                id,
                                text,
                                sentiment: sentiment as f32,
                                mentions,
                            },
                            score,
                        ))
                    } else {
                        None
                    }
                })
                .collect();

            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(limit);

            Ok(scored
                .into_iter()
                .map(|(note, score)| NoteSearchResult { note, score })
                .collect())
        })
    }

    /// Delete all notes from a source file
    pub fn delete_notes_by_file(&self, source_file: &Path) -> Result<u32, StoreError> {
        let source_str = source_file.to_string_lossy().to_string();

        self.rt.block_on(async {
            sqlx::query(
                "DELETE FROM notes_fts WHERE id IN (SELECT id FROM notes WHERE source_file = ?1)",
            )
            .bind(&source_str)
            .execute(&self.pool)
            .await?;

            let result = sqlx::query("DELETE FROM notes WHERE source_file = ?1")
                .bind(&source_str)
                .execute(&self.pool)
                .await?;

            Ok(result.rows_affected() as u32)
        })
    }

    /// Check if notes file needs reindexing
    pub fn notes_need_reindex(&self, source_file: &Path) -> Result<bool, StoreError> {
        let current_mtime = source_file
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| std::io::Error::other("time error"))?
            .as_secs() as i64;

        self.rt.block_on(async {
            let row: Option<(i64,)> =
                sqlx::query_as("SELECT file_mtime FROM notes WHERE source_file = ?1 LIMIT 1")
                    .bind(source_file.to_string_lossy().to_string())
                    .fetch_optional(&self.pool)
                    .await?;

            match row {
                Some((mtime,)) if mtime >= current_mtime => Ok(false),
                _ => Ok(true),
            }
        })
    }

    /// Get note count
    pub fn note_count(&self) -> Result<u64, StoreError> {
        self.rt.block_on(async {
            let row: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM notes")
                .fetch_optional(&self.pool)
                .await?;
            Ok(row.map(|(c,)| c as u64).unwrap_or(0))
        })
    }

    /// Get note statistics
    pub fn note_stats(&self) -> Result<(u64, u64, u64), StoreError> {
        self.rt.block_on(async {
            let (total,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notes")
                .fetch_one(&self.pool)
                .await
                .unwrap_or((0,));

            let (warnings,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM notes WHERE sentiment < -0.3")
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or((0,));

            let (patterns,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM notes WHERE sentiment > 0.3")
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or((0,));

            Ok((total as u64, warnings as u64, patterns as u64))
        })
    }
}
