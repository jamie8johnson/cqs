//! Batch counts, shared callers/callees, co-occurrence queries.

use sqlx::Row;

use super::FunctionCallStats;
use crate::store::helpers::StoreError;
use crate::store::Store;

impl<Mode> Store<Mode> {
    /// Batch count query for call graph columns.
    /// Shared implementation for caller/callee count queries. Filters by `filter_column`
    /// and groups by `group_column` to count edges.
    async fn batch_count_query(
        &self,
        filter_column: &str,
        group_column: &str,
        count_expr: &str,
        names: &[&str],
    ) -> Result<std::collections::HashMap<String, u64>, StoreError> {
        let mut result = std::collections::HashMap::new();

        // PF-V1.25-8: 500 was sized for the pre-3.32 SQLite 999-var limit.
        // `max_rows_per_statement(1)` returns ~32466 (one bind per row).
        use crate::store::helpers::sql::max_rows_per_statement;
        let batch_size = max_rows_per_statement(1);
        for batch in names.chunks(batch_size) {
            let placeholders = super::super::helpers::make_placeholders(batch.len());
            let sql = format!(
                "SELECT {group_column}, {count_expr} FROM function_calls WHERE {filter_column} IN ({placeholders}) GROUP BY {group_column}",
            );
            let mut q = sqlx::query(&sql);
            for name in batch {
                q = q.bind(name);
            }
            let rows: Vec<_> = q.fetch_all(&self.pool).await?;
            for row in rows {
                let name: String = row.get(0);
                let count: i64 = row.get(1);
                result.insert(name, count as u64);
            }
        }

        Ok(result)
    }

    /// Caller counts for multiple functions in one query.
    /// Returns how many callers each function has. Functions not in the call graph
    /// won't appear in the result map (caller count is implicitly 0).
    pub fn get_caller_counts_batch(
        &self,
        names: &[&str],
    ) -> Result<std::collections::HashMap<String, u64>, StoreError> {
        let _span = tracing::info_span!("get_caller_counts_batch", count = names.len()).entered();
        if names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt
            .block_on(self.batch_count_query("callee_name", "callee_name", "COUNT(*)", names))
    }

    /// Callee counts for multiple functions in one query.
    /// Returns how many callees each function has. Functions not in the call graph
    /// won't appear in the result map (callee count is implicitly 0).
    pub fn get_callee_counts_batch(
        &self,
        names: &[&str],
    ) -> Result<std::collections::HashMap<String, u64>, StoreError> {
        let _span = tracing::info_span!("get_callee_counts_batch", count = names.len()).entered();
        if names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        self.rt.block_on(self.batch_count_query(
            "caller_name",
            "caller_name",
            "COUNT(DISTINCT callee_name)",
            names,
        ))
    }

    /// Functions that share callers with target (called by the same functions).
    /// For target X, finds functions Y where some function A calls both X and Y.
    /// Returns (function_name, overlap_count) sorted by overlap descending.
    pub fn find_shared_callers(
        &self,
        target: &str,
        limit: usize,
    ) -> Result<Vec<(String, u32)>, StoreError> {
        let _span = tracing::debug_span!("find_shared_callers", function = %target).entered();
        self.rt.block_on(async {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT fc2.callee_name, COUNT(DISTINCT fc2.caller_name) AS overlap
                 FROM function_calls fc1
                 JOIN function_calls fc2 ON fc1.caller_name = fc2.caller_name
                 WHERE fc1.callee_name = ?1 AND fc2.callee_name != ?1
                 GROUP BY fc2.callee_name
                 ORDER BY overlap DESC
                 LIMIT ?2",
            )
            .bind(target)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(name, count)| (name, count as u32))
                .collect())
        })
    }

    /// Functions that share callees with target (call the same functions).
    /// For target X, finds functions Y where X and Y both call some function C.
    /// Returns (function_name, overlap_count) sorted by overlap descending.
    pub fn find_shared_callees(
        &self,
        target: &str,
        limit: usize,
    ) -> Result<Vec<(String, u32)>, StoreError> {
        let _span = tracing::debug_span!("find_shared_callees", function = %target).entered();
        self.rt.block_on(async {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT fc2.caller_name, COUNT(DISTINCT fc2.callee_name) AS overlap
                 FROM function_calls fc1
                 JOIN function_calls fc2 ON fc1.callee_name = fc2.callee_name
                 WHERE fc1.caller_name = ?1 AND fc2.caller_name != ?1
                 GROUP BY fc2.caller_name
                 ORDER BY overlap DESC
                 LIMIT ?2",
            )
            .bind(target)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|(name, count)| (name, count as u32))
                .collect())
        })
    }

    /// Get full call graph statistics
    pub fn function_call_stats(&self) -> Result<FunctionCallStats, StoreError> {
        let _span = tracing::debug_span!("function_call_stats").entered();
        self.rt.block_on(async {
            let (total_calls, unique_callers, unique_callees): (i64, i64, i64) = sqlx::query_as(
                "SELECT COUNT(*), COUNT(DISTINCT caller_name), COUNT(DISTINCT callee_name) FROM function_calls",
            )
            .fetch_one(&self.pool)
            .await?;

            Ok(FunctionCallStats {
                total_calls: total_calls as u64,
                unique_callers: unique_callers as u64,
                unique_callees: unique_callees as u64,
            })
        })
    }

    /// Count distinct callers for each callee name.
    /// Returns `(callee_name, distinct_caller_count)` pairs. Used by the
    /// enrichment pass for IDF-style filtering: callees called by many
    /// distinct callers are likely utilities (log, unwrap, etc.).
    pub fn callee_caller_counts(&self) -> Result<Vec<(String, usize)>, StoreError> {
        let _span = tracing::debug_span!("callee_caller_counts").entered();
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT callee_name, COUNT(DISTINCT caller_name) as caller_count \
                 FROM function_calls GROUP BY callee_name",
            )
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .iter()
                .map(|row| {
                    let name: String = row.get("callee_name");
                    let count: i64 = row.get("caller_count");
                    (name, count as usize)
                })
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::test_helpers::setup_store;

    use super::*;

    /// Initializes the store with a predefined call graph for testing purposes.
    /// Creates a test call graph where function A calls B and C, B calls C, and D calls B, then inserts it into the store for the file "src/test.rs".
    /// # Arguments
    /// * `store` - The store instance to populate with the test call graph data.
    /// # Panics
    /// Panics if the `upsert_function_calls` operation fails (via `unwrap()`).
    fn seed_call_graph(store: &Store) {
        // A calls B and C; B calls C; D calls B
        let calls = vec![
            crate::parser::FunctionCalls {
                name: "func_a".to_string(),
                line_start: 1,
                calls: vec![
                    crate::parser::CallSite {
                        callee_name: "func_b".to_string(),
                        line_number: 2,
                    },
                    crate::parser::CallSite {
                        callee_name: "func_c".to_string(),
                        line_number: 3,
                    },
                ],
            },
            crate::parser::FunctionCalls {
                name: "func_b".to_string(),
                line_start: 10,
                calls: vec![crate::parser::CallSite {
                    callee_name: "func_c".to_string(),
                    line_number: 11,
                }],
            },
            crate::parser::FunctionCalls {
                name: "func_d".to_string(),
                line_start: 20,
                calls: vec![crate::parser::CallSite {
                    callee_name: "func_b".to_string(),
                    line_number: 21,
                }],
            },
        ];
        store
            .upsert_function_calls(Path::new("src/test.rs"), &calls)
            .unwrap();
    }

    #[test]
    fn test_get_caller_counts_batch() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let counts = store
            .get_caller_counts_batch(&["func_b", "func_c"])
            .unwrap();
        // func_b is called by func_a and func_d
        assert_eq!(counts.get("func_b").copied().unwrap_or(0), 2);
        // func_c is called by func_a and func_b
        assert_eq!(counts.get("func_c").copied().unwrap_or(0), 2);
    }

    #[test]
    fn test_get_callee_counts_batch() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let counts = store
            .get_callee_counts_batch(&["func_a", "func_b", "func_d"])
            .unwrap();
        // func_a calls func_b and func_c
        assert_eq!(counts.get("func_a").copied().unwrap_or(0), 2);
        // func_b calls func_c
        assert_eq!(counts.get("func_b").copied().unwrap_or(0), 1);
        // func_d calls func_b
        assert_eq!(counts.get("func_d").copied().unwrap_or(0), 1);
    }

    #[test]
    fn test_get_caller_counts_batch_empty() {
        let (store, _dir) = setup_store();
        let counts = store.get_caller_counts_batch(&[]).unwrap();
        assert!(counts.is_empty());
    }

    #[test]
    fn test_get_callee_counts_batch_empty() {
        let (store, _dir) = setup_store();
        let counts = store.get_callee_counts_batch(&[]).unwrap();
        assert!(counts.is_empty());
    }

    #[test]
    fn test_get_caller_counts_batch_unknown_names() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let counts = store
            .get_caller_counts_batch(&["nonexistent_func", "also_missing"])
            .unwrap();
        // Unknown names shouldn't appear in result
        assert!(counts.is_empty());
    }

    #[test]
    fn test_get_callee_counts_batch_unknown_names() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let counts = store
            .get_callee_counts_batch(&["nonexistent_func"])
            .unwrap();
        assert!(counts.is_empty());
    }

    // ===== find_shared_callers / find_shared_callees tests =====

    #[test]
    fn test_find_shared_callers() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        // func_b and func_c are both called by func_a
        // So func_c shares caller func_a with func_b
        let shared = store.find_shared_callers("func_b", 10).unwrap();
        let names: Vec<&str> = shared.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"func_c"),
            "func_c should share caller func_a with func_b"
        );
    }

    #[test]
    fn test_find_shared_callees() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        // func_a and func_b both call func_c
        // So func_b shares callee func_c with func_a
        let shared = store.find_shared_callees("func_a", 10).unwrap();
        let names: Vec<&str> = shared.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"func_b"),
            "func_b should share callee func_c with func_a"
        );
    }

    #[test]
    fn test_find_shared_callers_no_callers() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        // func_a has no callers, so nothing shares callers with it
        let shared = store.find_shared_callers("func_a", 10).unwrap();
        assert!(shared.is_empty());
    }

    #[test]
    fn test_find_shared_callees_no_callees() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        // func_c has no callees, so nothing shares callees with it
        let shared = store.find_shared_callees("func_c", 10).unwrap();
        assert!(shared.is_empty());
    }

    #[test]
    fn test_find_shared_callers_limit() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let shared = store.find_shared_callers("func_b", 1).unwrap();
        assert!(shared.len() <= 1);
    }

    #[test]
    fn test_find_shared_callers_unknown() {
        let (store, _dir) = setup_store();
        seed_call_graph(&store);

        let shared = store.find_shared_callers("nonexistent", 10).unwrap();
        assert!(shared.is_empty());
    }
}
