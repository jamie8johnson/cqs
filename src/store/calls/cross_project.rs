//! Cross-project call graph context.
//!
//! Wraps multiple `Store` instances and merges their call graphs
//! for cross-boundary caller/callee/test queries.

use std::collections::HashMap;
use std::sync::Arc;

use crate::store::helpers::{CallGraph, CallerInfo, ChunkSummary, StoreError};
use crate::Store;

/// A named store for cross-project context.
///
/// `CrossProjectContext` only issues read queries (callers, callees,
/// test chunks), so the handle is `Store<ReadOnly>` — the typestate makes
/// it a compile-time error to accidentally call a write method on a
/// cross-project reference store.
pub struct NamedStore {
    /// Human-readable project name (e.g. "cqs", "openclaw").
    pub name: String,
    /// The open Store handle.
    pub store: Store<crate::store::ReadOnly>,
    /// The `index.db` path this store was opened from, plus the
    /// `(mtime, size)` identity captured at open time. Used by
    /// [`CrossProjectContext::is_stale`] so a daemon caching the context
    /// across requests self-heals after a `cqs ref update <name>` (which
    /// rewrites a reference's `index.db` without touching the primary
    /// project's). `None` when the file was unstattable at open — the
    /// staleness check then keeps the cached store rather than thrashing on
    /// a transient glitch.
    db_path: std::path::PathBuf,
    loaded_identity: Option<(std::time::SystemTime, u64)>,
}

impl NamedStore {
    /// Construct a `NamedStore`, capturing the `index.db` identity from
    /// `path` for later staleness detection. `path` is the file the store was
    /// opened from; callers that already hold it (e.g.
    /// [`CrossProjectContext::from_config`]) avoid a redundant stat by passing
    /// it through rather than re-deriving it.
    pub fn new(
        name: String,
        store: Store<crate::store::ReadOnly>,
        path: std::path::PathBuf,
    ) -> Self {
        let loaded_identity = stat_identity(&path);
        Self {
            name,
            store,
            db_path: path,
            loaded_identity,
        }
    }
}

impl std::fmt::Debug for NamedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedStore")
            .field("name", &self.name)
            .field("store", &"<Store>")
            .finish()
    }
}

/// Stat `path` and return `(mtime, size)` if readable. Mirrors the helper
/// in `reference.rs`; an unreadable path yields `None` (treated as
/// "unknown", caller keeps the cached value).
fn stat_identity(path: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?;
    Some((mtime, md.len()))
}

/// Caller info enriched with the originating project name.
///
/// The cross cores (`callers_cross_core` / `callees_cross_core`) remap this
/// into the surface-facing `CallerEntry` / `CalleeEntry` JSON shapes field by
/// field, so this struct is never serialized directly — no `Serialize` derive.
#[derive(Debug, Clone)]
pub struct CrossProjectCaller {
    /// Which project this caller lives in.
    pub project: String,
    pub caller: CallerInfo,
}

/// Callee info enriched with the originating project name.
///
/// Like [`CrossProjectCaller`], remapped into the surface JSON shape by the
/// cross cores rather than serialized directly — no `Serialize` derive.
#[derive(Debug, Clone)]
pub struct CrossProjectCallee {
    /// Which project this callee lives in.
    pub project: String,
    pub name: String,
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
    /// Fingerprint of the `references` config this context was built from.
    /// `None` for contexts assembled directly via [`Self::new`] (tests).
    /// The daemon caches a `CrossProjectContext` across requests and compares
    /// this against the current config's fingerprint so a `.cqs.toml` /
    /// `slot.toml` references edit forces a rebuild — see [`Self::config_fingerprint`].
    config_fingerprint: Option<u64>,
}

impl CrossProjectContext {
    /// Create a new context from a list of named stores.
    pub fn new(stores: Vec<NamedStore>) -> Self {
        Self {
            stores,
            graphs: HashMap::new(),
            config_fingerprint: None,
        }
    }

    /// Stable fingerprint of a `references` config slice: each reference's
    /// `(name, path, weight)` folded into a single hash. Two configs with the
    /// same references (order included) produce the same fingerprint; adding,
    /// removing, repointing, or reweighting a reference changes it.
    ///
    /// Used as the daemon cache key so a config edit invalidates a cached
    /// cross-project context even though `index.db` (the primary staleness
    /// discriminator) never moved.
    pub fn config_fingerprint(references: &[crate::config::ReferenceConfig]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        references.len().hash(&mut hasher);
        for r in references {
            r.name.hash(&mut hasher);
            r.path.hash(&mut hasher);
            // f32 isn't Hash; fold the bit pattern so a weight tweak still
            // moves the fingerprint.
            r.weight.to_bits().hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Build from the local store and `.cqs.toml` reference config.
    pub fn from_config(root: &std::path::Path) -> Result<Self, crate::store::helpers::StoreError> {
        let _span = tracing::info_span!("cross_project_from_config").entered();
        let config = crate::config::Config::load(root);
        let fingerprint = Self::config_fingerprint(&config.references);

        // Use the slot-aware resolver so slot layouts
        // (`.cqs/slots/<active>/index.db`) work for `cqs trace
        // --cross-project` and friends. A hardcoded `.cqs/index.db` would only
        // exist in the non-slot layout, making every cross-project command
        // bail with "unable to open database file" on slot-migrated projects.
        let cqs_dir = crate::resolve_index_dir(root);
        let db_path = crate::resolve_index_db(&cqs_dir);
        // Cross-project reads only — open read-only. If the DB doesn't exist
        // yet, propagate the error; creating it here would corrupt the
        // invariant that cross-project queries never mutate.
        //
        // The local project is the throughput-sensitive store on this path
        // (it backs the BFS traversals), so it keeps the 64MB-mmap
        // `open_readonly`. Reference stores below use `open_readonly_small`.
        let local_store = Store::open_readonly(&db_path)?;
        let mut stores = vec![NamedStore::new("local".to_string(), local_store, db_path)];

        for ref_cfg in &config.references {
            let db_path = ref_cfg.path.join(crate::INDEX_DB_FILENAME);
            // `open_readonly_small` (16MB mmap) rather than `open_readonly`
            // (64MB): a cached cross-project session holding N references
            // would otherwise reserve 64MB × N of virtual address space, which
            // has bitten us on WSL via address-space fragmentation. Reference
            // corpora are read for graph edges, not full-scan search, so the
            // smaller mmap costs nothing here.
            match Store::open_readonly_small(&db_path) {
                Ok(store) => {
                    tracing::debug!(name = %ref_cfg.name, "Reference store opened");
                    stores.push(NamedStore::new(ref_cfg.name.clone(), store, db_path));
                }
                Err(e) => {
                    tracing::warn!(name = %ref_cfg.name, error = %e, "Failed to open reference, skipping");
                }
            }
        }

        tracing::info!(projects = stores.len(), "Cross-project context loaded");
        Ok(Self {
            stores,
            graphs: HashMap::new(),
            config_fingerprint: Some(fingerprint),
        })
    }

    /// The config fingerprint this context was built from, if it came from
    /// [`Self::from_config`]. `None` for test-assembled contexts.
    pub fn fingerprint(&self) -> Option<u64> {
        self.config_fingerprint
    }

    /// Has any underlying `index.db` been rewritten since this context was
    /// built? Catches `cqs ref update <name>` (rewrites a reference's
    /// `index.db`) and a local reindex (rewrites the primary `index.db`) —
    /// both leave the cached `graphs` map and the merged graph pointing at a
    /// stale generation.
    ///
    /// Returns `true` on the first store whose current `(mtime, size)`
    /// differs from the value captured at open. Stores with no captured
    /// identity (unstattable at open) or that are now unstattable are
    /// skipped — a transient glitch shouldn't force a rebuild.
    pub fn is_stale(&self) -> bool {
        self.stores.iter().any(|ns| match ns.loaded_identity {
            Some(loaded) => match stat_identity(&ns.db_path) {
                Some(current) => current != loaded,
                None => false,
            },
            None => false,
        })
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
        let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
        let model_info = crate::store::helpers::ModelInfo::default();

        let store = Store::<crate::store::ReadOnly>::open_readonly_after_init(&db_path, |store| {
            store.init(&model_info)?;

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
                        })?;
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
                        })?;
                }
            }
            Ok(())
        })
        .unwrap();

        // Keep the tempdir alive so the db file survives for the test duration.
        // `into_path` disables automatic cleanup; tests are short-lived so this is fine.
        let _keep = dir.keep();

        NamedStore::new(name.to_string(), store, db_path)
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

    #[test]
    fn test_config_fingerprint_changes_on_reference_edit() {
        use crate::config::ReferenceConfig;
        use std::path::PathBuf;

        let base = vec![ReferenceConfig {
            name: "a".to_string(),
            path: PathBuf::from("/refs/a"),
            source: None,
            weight: 0.8,
        }];
        let fp_base = CrossProjectContext::config_fingerprint(&base);

        // Same config → same fingerprint (deterministic).
        assert_eq!(
            fp_base,
            CrossProjectContext::config_fingerprint(&base),
            "identical config must fingerprint identically"
        );

        // Repointing the path moves the fingerprint.
        let repointed = vec![ReferenceConfig {
            path: PathBuf::from("/refs/a2"),
            ..base[0].clone()
        }];
        assert_ne!(
            fp_base,
            CrossProjectContext::config_fingerprint(&repointed),
            "path change must move the fingerprint"
        );

        // Reweighting moves it.
        let reweighted = vec![ReferenceConfig {
            weight: 0.5,
            ..base[0].clone()
        }];
        assert_ne!(
            fp_base,
            CrossProjectContext::config_fingerprint(&reweighted),
            "weight change must move the fingerprint"
        );

        // Adding a reference moves it.
        let mut grown = base.clone();
        grown.push(ReferenceConfig {
            name: "b".to_string(),
            path: PathBuf::from("/refs/b"),
            source: None,
            weight: 0.8,
        });
        assert_ne!(
            fp_base,
            CrossProjectContext::config_fingerprint(&grown),
            "adding a reference must move the fingerprint"
        );

        // Empty config fingerprints stably (and differently from non-empty).
        assert_ne!(
            fp_base,
            CrossProjectContext::config_fingerprint(&[]),
            "empty vs non-empty must differ"
        );
    }

    #[test]
    fn test_is_stale_detects_db_rewrite() {
        // A NamedStore captures its db identity at construction. Rewriting the
        // underlying index.db (the `cqs ref update` / local-reindex shape)
        // must flip is_stale() true.
        let mut forward = StdMap::new();
        forward.insert("caller_a".to_string(), vec!["target".to_string()]);
        let ns = make_named_store("proj_a", forward, StdMap::new());
        let db_path = ns.db_path.clone();
        let ctx = CrossProjectContext::new(vec![ns]);

        assert!(!ctx.is_stale(), "freshly built context is not stale");

        // Append a byte and bump mtime past the 1s FS granularity floor so the
        // (mtime, size) identity changes.
        std::thread::sleep(std::time::Duration::from_secs(2));
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&db_path)
                .unwrap();
            f.write_all(b" ").unwrap();
            f.sync_all().unwrap();
        }

        assert!(
            ctx.is_stale(),
            "rewriting the underlying index.db must mark the context stale"
        );
    }

    #[test]
    fn test_new_context_has_no_fingerprint() {
        // Test-assembled contexts (via `new`) carry no fingerprint — only
        // `from_config` sets one, since only it reads the references config.
        let ctx = CrossProjectContext::new(vec![]);
        assert_eq!(ctx.fingerprint(), None);
    }
}
