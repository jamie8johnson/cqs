//! Cross-project call graph context.
//!
//! Wraps multiple `Store` instances and merges their call graphs
//! for cross-boundary caller/callee/test queries.

use std::collections::HashMap;
use std::sync::Arc;

use crate::store::helpers::{CallGraph, CallerInfo, ChunkSummary, StoreError};
use crate::Store;

/// A named store for cross-project context.
pub struct NamedStore {
    /// Human-readable project name (e.g. "cqs", "openclaw").
    pub name: String,
    /// The open Store handle.
    pub store: Store,
}

impl std::fmt::Debug for NamedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedStore")
            .field("name", &self.name)
            .field("store", &"<Store>")
            .finish()
    }
}

/// Caller info enriched with the originating project name.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrossProjectCaller {
    /// Which project this caller lives in.
    pub project: String,
    #[serde(flatten)]
    pub caller: CallerInfo,
}

/// Callee info enriched with the originating project name.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrossProjectCallee {
    /// Which project this callee lives in.
    pub project: String,
    pub name: String,
    #[serde(rename = "line_start")]
    pub line: u32,
}

/// Test chunk enriched with the originating project name.
#[derive(Debug, Clone)]
pub struct CrossProjectTestChunk {
    /// Which project this test lives in.
    pub project: String,
    pub chunk: ChunkSummary,
}

/// Context holding multiple project stores for cross-project graph queries.
///
/// Lazily loads and caches call graphs per store on first access.
pub struct CrossProjectContext {
    stores: Vec<NamedStore>,
    /// Cached call graphs, keyed by index into `stores`.
    graphs: HashMap<usize, Arc<CallGraph>>,
}

impl CrossProjectContext {
    /// Create a new context from a list of named stores.
    pub fn new(stores: Vec<NamedStore>) -> Self {
        Self {
            stores,
            graphs: HashMap::new(),
        }
    }

    /// Build from the local store and `.cqs.toml` reference config.
    pub fn from_config(root: &std::path::Path) -> Result<Self, crate::store::helpers::StoreError> {
        let _span = tracing::info_span!("cross_project_from_config").entered();
        let config = crate::config::Config::load(root);

        let db_path = root.join(".cqs/index.db");
        let local_store = match Store::open_readonly(&db_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "open_readonly failed, trying writable");
                Store::open(&db_path)?
            }
        };
        let mut stores = vec![NamedStore {
            name: "local".to_string(),
            store: local_store,
        }];

        for ref_cfg in &config.references {
            let db_path = ref_cfg.path.join("index.db");
            match Store::open_readonly(&db_path) {
                Ok(store) => {
                    tracing::debug!(name = %ref_cfg.name, "Reference store opened");
                    stores.push(NamedStore {
                        name: ref_cfg.name.clone(),
                        store,
                    });
                }
                Err(e) => {
                    tracing::warn!(name = %ref_cfg.name, error = %e, "Failed to open reference, skipping");
                }
            }
        }

        tracing::info!(projects = stores.len(), "Cross-project context loaded");
        Ok(Self::new(stores))
    }

    /// Number of projects in this context.
    pub fn project_count(&self) -> usize {
        self.stores.len()
    }

    /// Eagerly load all call graphs that haven't been cached yet.
    /// Called once before iteration loops to avoid borrow conflicts
    /// between `self.stores` (immutable) and `self.graphs` (mutable).
    fn ensure_all_graphs(&mut self) -> Result<(), StoreError> {
        for idx in 0..self.stores.len() {
            if !self.graphs.contains_key(&idx) {
                let graph = self.stores[idx].store.get_call_graph()?;
                self.graphs.insert(idx, graph);
            }
        }
        Ok(())
    }

    /// Get callers of `callee_name` across all projects.
    ///
    /// Returns callers tagged with their project name. When a caller is
    /// found in a different project than the callee was expected in,
    /// a debug trace is emitted for boundary crossing visibility.
    pub fn get_callers_cross(
        &mut self,
        callee_name: &str,
    ) -> Result<Vec<CrossProjectCaller>, StoreError> {
        let _span = tracing::info_span!(
            "get_callers_cross",
            callee = callee_name,
            projects = self.stores.len()
        )
        .entered();

        self.ensure_all_graphs()?;

        let mut all_callers = Vec::new();
        for (idx, ns) in self.stores.iter().enumerate() {
            let graph = &self.graphs[&idx];
            if let Some(callers) = graph.reverse.get(callee_name) {
                for caller_arc in callers {
                    let caller_name = caller_arc.as_ref();
                    tracing::debug!(
                        project = %ns.name,
                        caller = caller_name,
                        callee = callee_name,
                        "Cross-project caller found"
                    );
                    all_callers.push(CrossProjectCaller {
                        project: ns.name.clone(),
                        caller: CallerInfo {
                            name: caller_name.to_string(),
                            file: std::path::PathBuf::new(),
                            line: 0,
                        },
                    });
                }
            }
        }
        Ok(all_callers)
    }

    /// Get callees of `caller_name` across all projects.
    ///
    /// Returns callees tagged with their project name. Emits debug traces
    /// on cross-boundary hops.
    pub fn get_callees_cross(
        &mut self,
        caller_name: &str,
    ) -> Result<Vec<CrossProjectCallee>, StoreError> {
        let _span = tracing::info_span!(
            "get_callees_cross",
            caller = caller_name,
            projects = self.stores.len()
        )
        .entered();

        self.ensure_all_graphs()?;

        let mut all_callees = Vec::new();
        for (idx, ns) in self.stores.iter().enumerate() {
            let graph = &self.graphs[&idx];
            if let Some(callees) = graph.forward.get(caller_name) {
                for callee_arc in callees {
                    let callee_name = callee_arc.as_ref();
                    tracing::debug!(
                        project = %ns.name,
                        caller = caller_name,
                        callee = callee_name,
                        "Cross-project callee found"
                    );
                    all_callees.push(CrossProjectCallee {
                        project: ns.name.clone(),
                        name: callee_name.to_string(),
                        line: 0,
                    });
                }
            }
        }
        Ok(all_callees)
    }

    /// Find test chunks across all projects.
    pub fn find_test_chunks_cross(&mut self) -> Result<Vec<CrossProjectTestChunk>, StoreError> {
        let _span =
            tracing::info_span!("find_test_chunks_cross", projects = self.stores.len()).entered();
        let mut all_tests = Vec::new();
        for ns in &self.stores {
            match ns.store.find_test_chunks() {
                Ok(chunks) => {
                    for chunk in chunks.iter() {
                        all_tests.push(CrossProjectTestChunk {
                            project: ns.name.clone(),
                            chunk: chunk.clone(),
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(project = %ns.name, error = %e, "Failed to load test chunks");
                }
            }
        }
        Ok(all_tests)
    }

    /// Build a merged call graph from all projects.
    ///
    /// Unions the forward and reverse adjacency lists from every project's
    /// call graph into a single `CallGraph`. Used by cross-project test-map
    /// which needs a unified graph for BFS traversal.
    pub fn merged_call_graph(&mut self) -> Result<CallGraph, StoreError> {
        let _span =
            tracing::info_span!("merged_call_graph", projects = self.stores.len()).entered();

        self.ensure_all_graphs()?;

        let mut forward: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::new();
        let mut reverse: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::new();

        for idx in 0..self.stores.len() {
            let graph = &self.graphs[&idx];
            for (caller, callees) in &graph.forward {
                forward
                    .entry(Arc::clone(caller))
                    .or_default()
                    .extend(callees.iter().cloned());
            }
            for (callee, callers) in &graph.reverse {
                reverse
                    .entry(Arc::clone(callee))
                    .or_default()
                    .extend(callers.iter().cloned());
            }
        }

        Ok(CallGraph { forward, reverse })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdMap;

    /// Helper: build a NamedStore backed by a temp Store with a synthetic call graph.
    // NOTE: similar helper exists in impact/cross_project.rs
    fn make_named_store(
        name: &str,
        forward: StdMap<String, Vec<String>>,
        reverse: StdMap<String, Vec<String>>,
    ) -> NamedStore {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        let model_info = crate::store::helpers::ModelInfo::default();
        store.init(&model_info).unwrap();

        // Insert function_calls rows so get_call_graph() builds the graph.
        // We need to insert into function_calls table for each edge.
        for (caller, callees) in &forward {
            for callee in callees {
                store
                    .rt
                    .block_on(async {
                        sqlx::query(
                            "INSERT OR IGNORE INTO function_calls (file, caller_name, callee_name, caller_line, call_line)
                             VALUES ('test.rs', ?1, ?2, 1, 1)",
                        )
                        .bind(caller)
                        .bind(callee)
                        .execute(&store.pool)
                        .await
                    })
                    .unwrap();
            }
        }
        // Also insert reverse edges that aren't covered by forward
        for (callee, callers) in &reverse {
            for caller in callers {
                store
                    .rt
                    .block_on(async {
                        sqlx::query(
                            "INSERT OR IGNORE INTO function_calls (file, caller_name, callee_name, caller_line, call_line)
                             VALUES ('test.rs', ?1, ?2, 1, 1)",
                        )
                        .bind(caller)
                        .bind(callee)
                        .execute(&store.pool)
                        .await
                    })
                    .unwrap();
            }
        }

        // Keep the tempdir alive so the db file survives for the test duration.
        // `into_path` disables automatic cleanup; tests are short-lived so this is fine.
        let _keep = dir.into_path();

        NamedStore {
            name: name.to_string(),
            store,
        }
    }

    #[test]
    fn test_cross_project_callers_single_project() {
        let mut forward = StdMap::new();
        forward.insert("caller_a".to_string(), vec!["target".to_string()]);
        let ctx_store = make_named_store("proj_a", forward, StdMap::new());
        let mut ctx = CrossProjectContext::new(vec![ctx_store]);

        let callers = ctx.get_callers_cross("target").unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].caller.name, "caller_a");
        assert_eq!(callers[0].project, "proj_a");
    }

    #[test]
    fn test_cross_project_callers_multi_project() {
        let mut forward_a = StdMap::new();
        forward_a.insert("caller_a".to_string(), vec!["shared_fn".to_string()]);
        let store_a = make_named_store("proj_a", forward_a, StdMap::new());

        let mut forward_b = StdMap::new();
        forward_b.insert("caller_b".to_string(), vec!["shared_fn".to_string()]);
        let store_b = make_named_store("proj_b", forward_b, StdMap::new());

        let mut ctx = CrossProjectContext::new(vec![store_a, store_b]);
        let callers = ctx.get_callers_cross("shared_fn").unwrap();
        assert_eq!(callers.len(), 2);

        let projects: Vec<&str> = callers.iter().map(|c| c.project.as_str()).collect();
        assert!(projects.contains(&"proj_a"));
        assert!(projects.contains(&"proj_b"));
    }

    #[test]
    fn test_cross_project_callees_multi_project() {
        let mut forward_a = StdMap::new();
        forward_a.insert("shared_fn".to_string(), vec!["callee_a".to_string()]);
        let store_a = make_named_store("proj_a", forward_a, StdMap::new());

        let mut forward_b = StdMap::new();
        forward_b.insert("shared_fn".to_string(), vec!["callee_b".to_string()]);
        let store_b = make_named_store("proj_b", forward_b, StdMap::new());

        let mut ctx = CrossProjectContext::new(vec![store_a, store_b]);
        let callees = ctx.get_callees_cross("shared_fn").unwrap();
        assert_eq!(callees.len(), 2);

        let names: Vec<&str> = callees.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"callee_a"));
        assert!(names.contains(&"callee_b"));
    }

    #[test]
    fn test_cross_project_no_callers() {
        let store_a = make_named_store("proj_a", StdMap::new(), StdMap::new());
        let mut ctx = CrossProjectContext::new(vec![store_a]);
        let callers = ctx.get_callers_cross("nonexistent").unwrap();
        assert!(callers.is_empty());
    }
}
