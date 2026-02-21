//! Type edge storage and queries
//!
//! Stores type-level dependency edges extracted by the parser (Phase 2b).
//! Each edge records which chunk references which type, with an edge_kind
//! classification (Param, Return, Field, Impl, Bound, Alias) or empty string
//! for catch-all types found only inside generics.
//!
//! Follows the same patterns as `calls.rs`: sync wrappers over async sqlx,
//! batch-safe SQL (999 bind limit), tracing at appropriate levels.

use std::collections::HashMap;
use std::path::Path;

use super::helpers::{clamp_line_number, ChunkRow, StoreError};
use super::Store;
use crate::store::helpers::ChunkSummary;

/// Statistics about type dependency edges
#[derive(Debug, Clone, Default)]
pub struct TypeEdgeStats {
    /// Total number of type edges
    pub total_edges: u64,
    /// Number of distinct target type names
    pub unique_types: u64,
}

/// In-memory type graph for BFS traversal
///
/// Built from a single scan of the `type_edges` table joined with `chunks`.
/// Forward: chunk_name -> Vec<type_name>, Reverse: type_name -> Vec<chunk_name>.
/// Used by Phase 4 BFS traversal over type edges.
pub struct TypeGraph {
    /// Forward edges: chunk_name -> Vec<type_name>
    pub forward: HashMap<String, Vec<String>>,
    /// Reverse edges: type_name -> Vec<chunk_name>
    pub reverse: HashMap<String, Vec<String>>,
}

/// A type usage relationship from a chunk.
#[derive(Debug, Clone)]
pub struct TypeUsage {
    pub type_name: String,
    pub edge_kind: String,
}

impl Store {
    // ============ Type Edge Upsert Methods ============

    /// Upsert type edges for a single chunk.
    ///
    /// Deletes existing type edges for the chunk, then batch-inserts new ones.
    /// 4 binds per row → 249 rows per batch (996 < 999 SQLite limit).
    pub fn upsert_type_edges(
        &self,
        chunk_id: &str,
        type_refs: &[crate::parser::TypeRef],
    ) -> Result<(), StoreError> {
        if type_refs.is_empty() {
            return Ok(());
        }

        tracing::trace!(chunk_id, count = type_refs.len(), "upserting type edges");

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            sqlx::query("DELETE FROM type_edges WHERE source_chunk_id = ?1")
                .bind(chunk_id)
                .execute(&mut *tx)
                .await?;

            const INSERT_BATCH: usize = 249;
            for batch in type_refs.chunks(INSERT_BATCH) {
                let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                    "INSERT INTO type_edges (source_chunk_id, target_type_name, edge_kind, line_number) ",
                );
                qb.push_values(batch.iter(), |mut b, tr| {
                    let kind_str = tr
                        .kind
                        .as_ref()
                        .map(|k| k.as_str())
                        .unwrap_or("");
                    b.push_bind(chunk_id)
                        .push_bind(&tr.type_name)
                        .push_bind(kind_str)
                        .push_bind(tr.line_number as i64);
                });
                qb.build().execute(&mut *tx).await?;
            }

            tracing::debug!(chunk_id, count = type_refs.len(), "Inserted type edges");
            tx.commit().await?;
            Ok(())
        })
    }

    /// Upsert type edges for all chunks in a file.
    ///
    /// Resolves chunk names to chunk IDs via the chunks table, then deletes old
    /// type edges and batch-inserts new ones. Chunks not found in the database
    /// are warned and skipped (not an error).
    ///
    /// For windowed chunks, associates type edges with the first window
    /// (window_idx IS NULL or window_idx = 0).
    pub fn upsert_type_edges_for_file(
        &self,
        file: &Path,
        chunk_type_refs: &[crate::parser::ChunkTypeRefs],
    ) -> Result<(), StoreError> {
        if chunk_type_refs.is_empty() {
            return Ok(());
        }

        let file_str = file.to_string_lossy().replace('\\', "/");
        let total_refs: usize = chunk_type_refs.iter().map(|c| c.type_refs.len()).sum();
        tracing::trace!(
            file = %file_str,
            chunks = chunk_type_refs.len(),
            total_refs,
            "upserting type edges for file"
        );

        self.rt.block_on(async {
            // DS-14: Begin transaction before reading chunk IDs to prevent TOCTOU
            let mut tx = self.pool.begin().await?;

            // DS-18: ORDER BY window_idx ASC NULLS LAST for deterministic window priority
            let rows: Vec<(String, String, i64, Option<i64>)> = sqlx::query_as(
                "SELECT id, name, line_start, window_idx FROM chunks WHERE origin = ?1 ORDER BY window_idx ASC NULLS LAST",
            )
            .bind(&file_str)
            .fetch_all(&mut *tx)
            .await?;

            // Build lookup: (name, line_start) -> chunk_id
            // For windowed chunks, prefer non-windowed (window_idx IS NULL).
            // Due to NULLS LAST ordering, non-windowed rows arrive last and
            // overwrite any windowed entries, ensuring they always win.
            let mut name_to_id: HashMap<(String, u32), String> = HashMap::new();
            for (id, name, line_start, window_idx) in &rows {
                let key = (name.clone(), clamp_line_number(*line_start));
                let is_primary = window_idx.is_none();
                if is_primary || !name_to_id.contains_key(&key) {
                    name_to_id.insert(key, id.clone());
                }
            }

            // Collect (chunk_id, type_ref) pairs, skipping unresolved chunks
            let mut edges: Vec<(&str, &crate::parser::TypeRef)> = Vec::new();
            for ctr in chunk_type_refs {
                let key = (ctr.name.clone(), ctr.line_start);
                if let Some(chunk_id) = name_to_id.get(&key) {
                    for tr in &ctr.type_refs {
                        edges.push((chunk_id.as_str(), tr));
                    }
                } else {
                    tracing::warn!(
                        name = %ctr.name,
                        line_start = ctr.line_start,
                        file = %file_str,
                        "Chunk not found for type edges, skipping"
                    );
                }
            }

            if edges.is_empty() {
                tx.commit().await?;
                return Ok(());
            }

            // Delete existing type edges for all resolved chunk IDs
            let chunk_ids: Vec<&str> = name_to_id.values().map(|s| s.as_str()).collect();
            for batch in chunk_ids.chunks(500) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "DELETE FROM type_edges WHERE source_chunk_id IN ({})",
                    placeholders
                );
                let mut q = sqlx::query(&sql);
                for id in batch {
                    q = q.bind(id);
                }
                q.execute(&mut *tx).await?;
            }

            // Batch insert new edges
            const INSERT_BATCH: usize = 249;
            for batch in edges.chunks(INSERT_BATCH) {
                let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                    "INSERT INTO type_edges (source_chunk_id, target_type_name, edge_kind, line_number) ",
                );
                qb.push_values(batch.iter(), |mut b, (chunk_id, tr)| {
                    let kind_str = tr
                        .kind
                        .as_ref()
                        .map(|k| k.as_str())
                        .unwrap_or("");
                    b.push_bind(*chunk_id)
                        .push_bind(&tr.type_name)
                        .push_bind(kind_str)
                        .push_bind(tr.line_number as i64);
                });
                qb.build().execute(&mut *tx).await?;
            }

            tracing::info!(
                file = %file_str,
                chunks = chunk_type_refs.len(),
                edges = edges.len(),
                "Indexed type edges"
            );
            tx.commit().await?;
            Ok(())
        })
    }

    // ============ Type Edge Query Methods ============

    /// Get chunks that reference a given type name.
    ///
    /// Forward query: "who uses Config?" Returns chunks that have type edges
    /// pointing to the given type name.
    pub fn get_type_users(&self, type_name: &str) -> Result<Vec<ChunkSummary>, StoreError> {
        tracing::debug!(type_name, "querying type users");

        self.rt.block_on(async {
            let rows: Vec<ChunkRow> = sqlx::query(
                "SELECT DISTINCT c.id, c.origin, c.language, c.chunk_type, c.name,
                        c.signature, c.content, c.doc, c.line_start, c.line_end, c.parent_id
                 FROM type_edges te
                 JOIN chunks c ON te.source_chunk_id = c.id
                 WHERE te.target_type_name = ?1
                 ORDER BY c.origin, c.line_start",
            )
            .bind(type_name)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(ChunkRow::from_row)
            .collect();

            Ok(rows.into_iter().map(ChunkSummary::from).collect())
        })
    }

    /// Get types used by a given chunk (by function name).
    ///
    /// Reverse query: "what types does parse_config use?" Returns [`TypeUsage`] structs
    /// where edge_kind is "" for catch-all types.
    pub fn get_types_used_by(&self, chunk_name: &str) -> Result<Vec<TypeUsage>, StoreError> {
        tracing::debug!(chunk_name, "querying types used by chunk");

        self.rt.block_on(async {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT DISTINCT te.target_type_name, te.edge_kind
                 FROM type_edges te
                 JOIN chunks c ON te.source_chunk_id = c.id
                 WHERE c.name = ?1
                 ORDER BY te.edge_kind, te.target_type_name",
            )
            .bind(chunk_name)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(type_name, edge_kind)| TypeUsage {
                    type_name,
                    edge_kind,
                })
                .collect())
        })
    }

    /// Batch-fetch type users for multiple type names.
    ///
    /// Returns type_name -> Vec<ChunkSummary>. Uses WHERE IN with 200 names per batch.
    pub fn get_type_users_batch(
        &self,
        type_names: &[&str],
    ) -> Result<HashMap<String, Vec<ChunkSummary>>, StoreError> {
        if type_names.is_empty() {
            return Ok(HashMap::new());
        }

        tracing::debug!(count = type_names.len(), "batch querying type users");

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<ChunkSummary>> = HashMap::new();

            const BATCH_SIZE: usize = 200;
            for batch in type_names.chunks(BATCH_SIZE) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT te.target_type_name, c.id, c.origin, c.language, c.chunk_type, c.name,
                            c.signature, c.content, c.doc, c.line_start, c.line_end, c.parent_id
                     FROM type_edges te
                     JOIN chunks c ON te.source_chunk_id = c.id
                     WHERE te.target_type_name IN ({})
                     ORDER BY te.target_type_name, c.origin, c.line_start",
                    placeholders
                );
                let mut q = sqlx::query(&sql);
                for name in batch {
                    q = q.bind(name);
                }
                let rows: Vec<_> = q.fetch_all(&self.pool).await?;
                for row in rows {
                    use sqlx::Row;
                    let type_name: String = row.get(0);
                    // Build ChunkRow from remaining columns (offset by 1)
                    let chunk = ChunkSummary::from(ChunkRow {
                        id: row.get(1),
                        origin: row.get(2),
                        language: row.get(3),
                        chunk_type: row.get(4),
                        name: row.get(5),
                        signature: row.get(6),
                        content: row.get(7),
                        doc: row.get(8),
                        line_start: clamp_line_number(row.get::<i64, _>(9)),
                        line_end: clamp_line_number(row.get::<i64, _>(10)),
                        parent_id: row.get(11),
                    });
                    result.entry(type_name).or_default().push(chunk);
                }
            }

            Ok(result)
        })
    }

    /// Batch-fetch types used by multiple chunk names.
    ///
    /// Returns chunk_name -> Vec<(type_name, edge_kind)>. Uses WHERE IN with 200 names per batch.
    pub fn get_types_used_by_batch(
        &self,
        chunk_names: &[&str],
    ) -> Result<HashMap<String, Vec<(String, String)>>, StoreError> {
        if chunk_names.is_empty() {
            return Ok(HashMap::new());
        }

        tracing::debug!(count = chunk_names.len(), "batch querying types used by");

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<(String, String)>> = HashMap::new();

            const BATCH_SIZE: usize = 200;
            for batch in chunk_names.chunks(BATCH_SIZE) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT c.name, te.target_type_name, te.edge_kind
                     FROM type_edges te
                     JOIN chunks c ON te.source_chunk_id = c.id
                     WHERE c.name IN ({})
                     ORDER BY c.name, te.edge_kind, te.target_type_name",
                    placeholders
                );
                let mut q = sqlx::query(&sql);
                for name in batch {
                    q = q.bind(name);
                }
                let rows: Vec<_> = q.fetch_all(&self.pool).await?;
                for row in rows {
                    use sqlx::Row;
                    let chunk_name: String = row.get(0);
                    let type_name: String = row.get(1);
                    let edge_kind: String = row.get(2);
                    result
                        .entry(chunk_name)
                        .or_default()
                        .push((type_name, edge_kind));
                }
            }

            Ok(result)
        })
    }

    // ============ Type Edge Statistics ============

    /// Get type edge statistics.
    pub fn type_edge_stats(&self) -> Result<TypeEdgeStats, StoreError> {
        tracing::debug!("querying type edge stats");

        self.rt.block_on(async {
            let (total_edges, unique_types): (i64, i64) =
                sqlx::query_as("SELECT COUNT(*), COUNT(DISTINCT target_type_name) FROM type_edges")
                    .fetch_one(&self.pool)
                    .await?;

            Ok(TypeEdgeStats {
                total_edges: total_edges as u64,
                unique_types: unique_types as u64,
            })
        })
    }

    /// Load the type graph as forward + reverse adjacency lists.
    ///
    /// Single SQL scan of `type_edges` joined with `chunks`, capped at 500K edges.
    /// Forward: chunk_name -> Vec<type_name>, Reverse: type_name -> Vec<chunk_name>.
    pub fn get_type_graph(&self) -> Result<TypeGraph, StoreError> {
        let _span = tracing::info_span!("get_type_graph").entered();

        self.rt.block_on(async {
            const MAX_TYPE_GRAPH_EDGES: i64 = 500_000;
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT c.name, te.target_type_name
                 FROM type_edges te
                 JOIN chunks c ON te.source_chunk_id = c.id
                 LIMIT ?1",
            )
            .bind(MAX_TYPE_GRAPH_EDGES)
            .fetch_all(&self.pool)
            .await?;

            if rows.len() as i64 >= MAX_TYPE_GRAPH_EDGES {
                tracing::warn!(
                    cap = MAX_TYPE_GRAPH_EDGES,
                    "Type graph hit edge cap, results may be incomplete"
                );
            }

            let mut forward: HashMap<String, Vec<String>> = HashMap::new();
            let mut reverse: HashMap<String, Vec<String>> = HashMap::new();

            for (chunk_name, type_name) in rows {
                reverse
                    .entry(type_name.clone())
                    .or_default()
                    .push(chunk_name.clone());
                forward.entry(chunk_name).or_default().push(type_name);
            }

            Ok(TypeGraph { forward, reverse })
        })
    }

    /// Find types that share users with target (co-occurrence).
    ///
    /// "Types commonly used alongside Config" → Vec<(type_name, overlap_count)>.
    /// Uses self-join: find other types referenced by the same chunks that reference target.
    pub fn find_shared_type_users(
        &self,
        target_type: &str,
        limit: usize,
    ) -> Result<Vec<(String, u32)>, StoreError> {
        tracing::debug!(target_type, limit, "finding shared type users");

        self.rt.block_on(async {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT te2.target_type_name, COUNT(DISTINCT te2.source_chunk_id) AS overlap
                 FROM type_edges te1
                 JOIN type_edges te2 ON te1.source_chunk_id = te2.source_chunk_id
                 WHERE te1.target_type_name = ?1 AND te2.target_type_name != ?1
                 GROUP BY te2.target_type_name
                 ORDER BY overlap DESC
                 LIMIT ?2",
            )
            .bind(target_type)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(name, count)| (name, count as u32))
                .collect())
        })
    }

    // ============ Type Edge Maintenance ============

    /// Delete type_edges for chunks no longer in the chunks table (GC).
    ///
    /// Returns the number of pruned rows.
    pub fn prune_stale_type_edges(&self) -> Result<u64, StoreError> {
        self.rt.block_on(async {
            let result = sqlx::query(
                "DELETE FROM type_edges WHERE source_chunk_id NOT IN (SELECT id FROM chunks)",
            )
            .execute(&self.pool)
            .await?;
            let count = result.rows_affected();
            if count > 0 {
                tracing::info!(pruned = count, "Pruned stale type edges");
            }
            Ok(count)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{TypeEdgeKind, TypeRef};
    use crate::store::helpers::ModelInfo;
    use std::path::Path;

    fn setup_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();
        (store, dir)
    }

    /// Insert a minimal chunk into the store for testing type edges.
    fn insert_test_chunk(store: &Store, id: &str, name: &str, file: &str) {
        store.rt.block_on(async {
            let embedding = crate::embedder::Embedding::new(vec![0.0f32; crate::EMBEDDING_DIM]);
            let embedding_bytes = crate::store::helpers::embedding_to_bytes(&embedding).unwrap();
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name,
                     signature, content, content_hash, doc, line_start, line_end, embedding,
                     source_mtime, created_at, updated_at)
                     VALUES (?1, ?2, 'file', 'rust', 'function', ?3,
                     '', '', '', NULL, 1, 10, ?4, 0, ?5, ?5)",
            )
            .bind(id)
            .bind(file)
            .bind(name)
            .bind(&embedding_bytes)
            .bind(&now)
            .execute(&store.pool)
            .await
            .unwrap();
        });
    }

    fn make_type_refs() -> Vec<TypeRef> {
        vec![
            TypeRef {
                type_name: "Config".to_string(),
                line_number: 5,
                kind: Some(TypeEdgeKind::Param),
            },
            TypeRef {
                type_name: "Store".to_string(),
                line_number: 5,
                kind: Some(TypeEdgeKind::Return),
            },
            TypeRef {
                type_name: "SqlitePool".to_string(),
                line_number: 8,
                kind: Some(TypeEdgeKind::Field),
            },
        ]
    }

    #[test]
    fn test_upsert_and_query_type_users() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");
        insert_test_chunk(&store, "chunk-b", "func_b", "src/b.rs");

        // func_a uses Config(Param) and Store(Return)
        store
            .upsert_type_edges(
                "chunk-a",
                &[
                    TypeRef {
                        type_name: "Config".to_string(),
                        line_number: 5,
                        kind: Some(TypeEdgeKind::Param),
                    },
                    TypeRef {
                        type_name: "Store".to_string(),
                        line_number: 6,
                        kind: Some(TypeEdgeKind::Return),
                    },
                ],
            )
            .unwrap();

        // func_b uses Config(Param)
        store
            .upsert_type_edges(
                "chunk-b",
                &[TypeRef {
                    type_name: "Config".to_string(),
                    line_number: 10,
                    kind: Some(TypeEdgeKind::Param),
                }],
            )
            .unwrap();

        // Query: who uses Config?
        let users = store.get_type_users("Config").unwrap();
        assert_eq!(users.len(), 2);
        let names: Vec<&str> = users.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"func_a"));
        assert!(names.contains(&"func_b"));

        // Query: who uses Store?
        let users = store.get_type_users("Store").unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, "func_a");
    }

    #[test]
    fn test_upsert_and_query_types_used_by() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");

        store
            .upsert_type_edges("chunk-a", &make_type_refs())
            .unwrap();

        let types = store.get_types_used_by("func_a").unwrap();
        assert_eq!(types.len(), 3);
        let type_names: Vec<&str> = types.iter().map(|t| t.type_name.as_str()).collect();
        assert!(type_names.contains(&"Config"));
        assert!(type_names.contains(&"Store"));
        assert!(type_names.contains(&"SqlitePool"));
    }

    #[test]
    fn test_upsert_replaces_old() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");

        // First upsert: Config + Store
        store
            .upsert_type_edges("chunk-a", &make_type_refs())
            .unwrap();
        let types = store.get_types_used_by("func_a").unwrap();
        assert_eq!(types.len(), 3);

        // Second upsert: only HashMap
        store
            .upsert_type_edges(
                "chunk-a",
                &[TypeRef {
                    type_name: "HashMap".to_string(),
                    line_number: 1,
                    kind: Some(TypeEdgeKind::Return),
                }],
            )
            .unwrap();
        let types = store.get_types_used_by("func_a").unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].type_name, "HashMap");
    }

    #[test]
    fn test_get_type_users_empty() {
        let (store, _dir) = setup_store();
        let users = store.get_type_users("NonexistentType").unwrap();
        assert!(users.is_empty());
    }

    #[test]
    fn test_get_types_used_by_empty() {
        let (store, _dir) = setup_store();
        let types = store.get_types_used_by("nonexistent_func").unwrap();
        assert!(types.is_empty());
    }

    #[test]
    fn test_type_users_batch() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");
        insert_test_chunk(&store, "chunk-b", "func_b", "src/b.rs");

        store
            .upsert_type_edges(
                "chunk-a",
                &[TypeRef {
                    type_name: "Config".to_string(),
                    line_number: 5,
                    kind: Some(TypeEdgeKind::Param),
                }],
            )
            .unwrap();
        store
            .upsert_type_edges(
                "chunk-b",
                &[TypeRef {
                    type_name: "Store".to_string(),
                    line_number: 5,
                    kind: Some(TypeEdgeKind::Param),
                }],
            )
            .unwrap();

        let result = store
            .get_type_users_batch(&["Config", "Store", "Unknown"])
            .unwrap();
        assert_eq!(result.get("Config").map(|v| v.len()).unwrap_or(0), 1);
        assert_eq!(result.get("Store").map(|v| v.len()).unwrap_or(0), 1);
        assert!(result.get("Unknown").is_none());
    }

    #[test]
    fn test_types_used_by_batch() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");
        insert_test_chunk(&store, "chunk-b", "func_b", "src/b.rs");

        store
            .upsert_type_edges("chunk-a", &make_type_refs())
            .unwrap();
        store
            .upsert_type_edges(
                "chunk-b",
                &[TypeRef {
                    type_name: "HashMap".to_string(),
                    line_number: 1,
                    kind: None,
                }],
            )
            .unwrap();

        let result = store
            .get_types_used_by_batch(&["func_a", "func_b"])
            .unwrap();
        assert_eq!(result.get("func_a").map(|v| v.len()).unwrap_or(0), 3);
        assert_eq!(result.get("func_b").map(|v| v.len()).unwrap_or(0), 1);
    }

    #[test]
    fn test_batch_empty_input() {
        let (store, _dir) = setup_store();
        let r1 = store.get_type_users_batch(&[]).unwrap();
        assert!(r1.is_empty());
        let r2 = store.get_types_used_by_batch(&[]).unwrap();
        assert!(r2.is_empty());
    }

    #[test]
    fn test_type_edge_stats() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");

        store
            .upsert_type_edges("chunk-a", &make_type_refs())
            .unwrap();

        let stats = store.type_edge_stats().unwrap();
        assert_eq!(stats.total_edges, 3);
        assert_eq!(stats.unique_types, 3); // Config, Store, SqlitePool
    }

    #[test]
    fn test_get_type_graph() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");
        insert_test_chunk(&store, "chunk-b", "func_b", "src/b.rs");

        store
            .upsert_type_edges(
                "chunk-a",
                &[TypeRef {
                    type_name: "Config".to_string(),
                    line_number: 5,
                    kind: Some(TypeEdgeKind::Param),
                }],
            )
            .unwrap();
        store
            .upsert_type_edges(
                "chunk-b",
                &[TypeRef {
                    type_name: "Config".to_string(),
                    line_number: 10,
                    kind: Some(TypeEdgeKind::Return),
                }],
            )
            .unwrap();

        let graph = store.get_type_graph().unwrap();

        // Forward: func_a -> [Config], func_b -> [Config]
        assert!(graph
            .forward
            .get("func_a")
            .unwrap()
            .contains(&"Config".to_string()));
        assert!(graph
            .forward
            .get("func_b")
            .unwrap()
            .contains(&"Config".to_string()));

        // Reverse: Config -> [func_a, func_b]
        let config_users = graph.reverse.get("Config").unwrap();
        assert_eq!(config_users.len(), 2);
    }

    #[test]
    fn test_find_shared_type_users() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");

        // func_a uses Config and Store
        store
            .upsert_type_edges(
                "chunk-a",
                &[
                    TypeRef {
                        type_name: "Config".to_string(),
                        line_number: 5,
                        kind: Some(TypeEdgeKind::Param),
                    },
                    TypeRef {
                        type_name: "Store".to_string(),
                        line_number: 6,
                        kind: Some(TypeEdgeKind::Return),
                    },
                ],
            )
            .unwrap();

        // Types commonly used with Config = Store (overlap: 1 chunk)
        let shared = store.find_shared_type_users("Config", 10).unwrap();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].0, "Store");
        assert_eq!(shared[0].1, 1);
    }

    #[test]
    fn test_prune_stale_type_edges() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");

        store
            .upsert_type_edges("chunk-a", &make_type_refs())
            .unwrap();

        // Delete the chunk — type edges become orphaned
        store.rt.block_on(async {
            sqlx::query("DELETE FROM chunks WHERE id = 'chunk-a'")
                .execute(&store.pool)
                .await
                .unwrap();
        });

        // CASCADE should have already cleaned them, but prune catches non-FK orphans
        let pruned = store.prune_stale_type_edges().unwrap();
        // CASCADE already deleted them, so prune should find 0
        assert_eq!(pruned, 0);
    }

    #[test]
    fn test_cascade_delete() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");

        store
            .upsert_type_edges("chunk-a", &make_type_refs())
            .unwrap();
        assert_eq!(store.type_edge_stats().unwrap().total_edges, 3);

        // Delete chunk — CASCADE should remove type edges
        store.rt.block_on(async {
            sqlx::query("DELETE FROM chunks WHERE id = 'chunk-a'")
                .execute(&store.pool)
                .await
                .unwrap();
        });

        assert_eq!(store.type_edge_stats().unwrap().total_edges, 0);
    }

    #[test]
    fn test_edge_kind_storage() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");

        store
            .upsert_type_edges(
                "chunk-a",
                &[
                    TypeRef {
                        type_name: "Config".to_string(),
                        line_number: 5,
                        kind: Some(TypeEdgeKind::Param),
                    },
                    TypeRef {
                        type_name: "Generic".to_string(),
                        line_number: 6,
                        kind: None, // catch-all
                    },
                ],
            )
            .unwrap();

        let types = store.get_types_used_by("func_a").unwrap();
        let config = types.iter().find(|t| t.type_name == "Config").unwrap();
        assert_eq!(config.edge_kind, "Param");

        let generic = types.iter().find(|t| t.type_name == "Generic").unwrap();
        assert_eq!(generic.edge_kind, ""); // empty string for catch-all
    }

    #[test]
    fn test_upsert_type_edges_for_file() {
        let (store, _dir) = setup_store();
        // Insert chunks with matching origin
        insert_test_chunk(&store, "chunk-a", "func_a", "src/test.rs");
        insert_test_chunk(&store, "chunk-b", "func_b", "src/test.rs");

        let chunk_type_refs = vec![
            crate::parser::ChunkTypeRefs {
                name: "func_a".to_string(),
                line_start: 1,
                type_refs: vec![TypeRef {
                    type_name: "Config".to_string(),
                    line_number: 5,
                    kind: Some(TypeEdgeKind::Param),
                }],
            },
            crate::parser::ChunkTypeRefs {
                name: "func_b".to_string(),
                line_start: 1,
                type_refs: vec![TypeRef {
                    type_name: "Store".to_string(),
                    line_number: 15,
                    kind: Some(TypeEdgeKind::Return),
                }],
            },
        ];

        store
            .upsert_type_edges_for_file(Path::new("src/test.rs"), &chunk_type_refs)
            .unwrap();

        let users = store.get_type_users("Config").unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, "func_a");

        let users = store.get_type_users("Store").unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, "func_b");
    }

    #[test]
    fn test_upsert_for_file_missing_chunk() {
        let (store, _dir) = setup_store();
        // Only insert one chunk, reference two
        insert_test_chunk(&store, "chunk-a", "func_a", "src/test.rs");

        let chunk_type_refs = vec![
            crate::parser::ChunkTypeRefs {
                name: "func_a".to_string(),
                line_start: 1,
                type_refs: vec![TypeRef {
                    type_name: "Config".to_string(),
                    line_number: 5,
                    kind: Some(TypeEdgeKind::Param),
                }],
            },
            crate::parser::ChunkTypeRefs {
                name: "nonexistent".to_string(), // not in DB
                line_start: 20,
                type_refs: vec![TypeRef {
                    type_name: "Store".to_string(),
                    line_number: 25,
                    kind: Some(TypeEdgeKind::Return),
                }],
            },
        ];

        // Should succeed — skips nonexistent chunk with warning, stores func_a's edges
        store
            .upsert_type_edges_for_file(Path::new("src/test.rs"), &chunk_type_refs)
            .unwrap();

        let users = store.get_type_users("Config").unwrap();
        assert_eq!(users.len(), 1);
        // Store type edge was NOT inserted (chunk not found)
        let users = store.get_type_users("Store").unwrap();
        assert!(users.is_empty());
    }

    #[test]
    fn test_large_batch_crossing_boundary() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk-a", "func_a", "src/a.rs");

        // Create 300 type refs — crosses the 249 batch boundary
        let refs: Vec<TypeRef> = (0..300)
            .map(|i| TypeRef {
                type_name: format!("Type{}", i),
                line_number: i as u32 + 1,
                kind: Some(TypeEdgeKind::Param),
            })
            .collect();

        store.upsert_type_edges("chunk-a", &refs).unwrap();

        let stats = store.type_edge_stats().unwrap();
        assert_eq!(stats.total_edges, 300);
        assert_eq!(stats.unique_types, 300);
    }
}
