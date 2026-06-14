//! Cross-project call graph context.
//!
//! Wraps multiple `Store` instances and merges their call graphs
//! for cross-boundary caller/callee/test queries.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::runtime::Runtime;

use crate::store::helpers::{CallGraph, CallerInfo, ChunkSummary, StoreError};
use crate::store::{DataVersionProbe, FileIdentity};
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
    /// The `index.db` path this store was opened from. Kept so staleness
    /// checks can re-stat the file and re-open the data_version probe without
    /// reconstructing the path.
    db_path: std::path::PathBuf,
    /// Freshness key captured at open time, used by
    /// [`CrossProjectContext::is_stale`] so a daemon caching the context
    /// across requests self-heals after a `cqs ref update <name>` (which
    /// rewrites a reference's `index.db` without touching the primary
    /// project's). `None` when the file was unstattable at open — the
    /// staleness check then keeps the cached store rather than thrashing on
    /// a transient glitch.
    loaded_identity: Option<FileIdentity>,
    /// Long-lived `PRAGMA data_version` probe — the second freshness
    /// discriminator. `cqs ref update` reindexes a reference's `index.db`
    /// *in place* (incremental pipeline, not rename-over), so WAL-mode commits
    /// can land before the closing checkpoint moves the file identity; the
    /// probe is the only discriminator that catches that window. `None` when
    /// the probe couldn't be opened (warned, identity-only fallback);
    /// re-opened lazily on the next staleness check.
    data_version_probe: Option<DataVersionProbe>,
    /// Runtime handle (cloned from the store) that drives the probe's async
    /// sqlx queries, so the probe stays on the store's worker pool.
    runtime: Arc<Runtime>,
}

impl NamedStore {
    /// Construct a `NamedStore`, capturing the `index.db` freshness key from
    /// `path` for later staleness detection. `path` is the file the store was
    /// opened from; callers that already hold it (e.g.
    /// [`CrossProjectContext::from_config`]) avoid a redundant stat by passing
    /// it through rather than re-deriving it.
    pub fn new(
        name: String,
        store: Store<crate::store::ReadOnly>,
        path: std::path::PathBuf,
    ) -> Self {
        let loaded_identity = FileIdentity::from_path(&path);
        let runtime = Arc::clone(store.runtime());
        let data_version_probe = DataVersionProbe::open(&runtime, &path);
        Self {
            name,
            store,
            db_path: path,
            loaded_identity,
            data_version_probe,
            runtime,
        }
    }

    /// Has this store's `index.db` been rewritten since it was opened?
    ///
    /// Two discriminators, OR-combined:
    /// 1. [`FileIdentity`] change — catches rename-over and checkpoint (the
    ///    `cqs ref update` close folds the WAL back, moving size/mtime/inode).
    /// 2. `PRAGMA data_version` movement on the long-lived probe — catches the
    ///    in-place WAL-incremental window before that checkpoint.
    ///
    /// On an identity change the probe is re-opened against the (possibly
    /// replaced) file: a rename-over leaves the old fd pointing at the orphaned
    /// inode, whose counter never moves again. An unstattable file or a missing
    /// identity yields `false` (keep the cached store) — a transient glitch
    /// shouldn't thrash the cache.
    fn is_stale(&mut self) -> bool {
        let Some(loaded) = self.loaded_identity else {
            return false;
        };
        let Some(current) = FileIdentity::from_path(&self.db_path) else {
            return false;
        };
        if current != loaded {
            // Identity moved — re-baseline the probe against the new file so a
            // subsequent in-place WAL commit is still caught.
            if let Some(old) = self.data_version_probe.take() {
                old.close(&self.runtime);
            }
            self.data_version_probe = DataVersionProbe::open(&self.runtime, &self.db_path);
            self.loaded_identity = Some(current);
            return true;
        }
        // Identity unchanged — consult the probe for the WAL-incremental case.
        match self.data_version_probe.as_mut() {
            Some(probe) => match probe.changed(&self.runtime) {
                Ok(changed) => changed,
                Err(e) => {
                    tracing::warn!(
                        name = %self.name,
                        error = %e,
                        "data_version probe query failed — dropping probe; will re-open on next staleness check"
                    );
                    self.data_version_probe = None;
                    false
                }
            },
            None => {
                // Earlier open failed (or the probe was dropped after a query
                // error) — retry. Freshly baselined, so nothing to compare.
                self.data_version_probe = DataVersionProbe::open(&self.runtime, &self.db_path);
                false
            }
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
    /// Provenance of the call edge, threaded through the in-memory CallGraph.
    /// Defaults to [`CallEdgeKind::Call`] for an edge with no recorded metadata.
    pub edge_kind: crate::parser::CallEdgeKind,
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
    /// built? Catches `cqs ref update <name>` (reindexes a reference's
    /// `index.db` in place) and a local reindex (rewrites the primary
    /// `index.db`) — both leave the cached `graphs` map and the merged graph
    /// pointing at a stale generation.
    ///
    /// Returns `true` if any store's freshness key moved (file identity or
    /// `PRAGMA data_version`; see [`NamedStore::is_stale`]). Stores with no
    /// captured identity (unstattable at open) or that are now unstattable are
    /// skipped — a transient glitch shouldn't force a rebuild.
    ///
    /// `&mut self` (not `&self`): the data_version probe advances its baseline
    /// on each query, and every store must be polled so no probe falls behind —
    /// hence a full fold rather than a short-circuiting `any`.
    pub fn is_stale(&mut self) -> bool {
        let mut stale = false;
        for ns in self.stores.iter_mut() {
            // Poll every store (no short-circuit) so each probe advances.
            stale |= ns.is_stale();
        }
        stale
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
                    // Look up the edge's provenance (kind + source location)
                    // recorded at graph load — threaded through the in-memory
                    // CallGraph so cross-project callers carry the same
                    // edge_kind/file/line the single-project SQL path does.
                    let meta = graph.edge_meta(caller_name, callee_name);
                    tracing::debug!(
                        project = %ns.name,
                        caller = caller_name,
                        callee = callee_name,
                        edge_kind = %meta.edge_kind,
                        "Cross-project caller found"
                    );
                    all_callers.push(CrossProjectCaller {
                        project: ns.name.clone(),
                        caller: CallerInfo {
                            name: caller_name.to_string(),
                            file: std::path::PathBuf::from(meta.file),
                            line: meta.caller_line,
                            edge_kind: meta.edge_kind,
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
                    // Edge provenance + call-site line from the in-memory graph;
                    // `line` is the call_line (where the call occurs), matching
                    // the single-project `get_callees_full` semantics.
                    let meta = graph.edge_meta(caller_name, callee_name);
                    tracing::debug!(
                        project = %ns.name,
                        caller = caller_name,
                        callee = callee_name,
                        edge_kind = %meta.edge_kind,
                        "Cross-project callee found"
                    );
                    all_callees.push(CrossProjectCallee {
                        project: ns.name.clone(),
                        name: callee_name.to_string(),
                        line: meta.call_line,
                        edge_kind: meta.edge_kind,
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
        let mut edges: HashMap<(Arc<str>, Arc<str>), crate::store::helpers::CallEdgeMeta> =
            HashMap::new();

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
            // Preserve each project's edge provenance. When the same
            // `(caller, callee)` pair appears in more than one project (a shared
            // function name), keep the most-trusted kind — mirroring the
            // within-project MIN-rank collapse so a `call` edge in one project
            // is never demoted to a `doc_reference` from another.
            for (key, meta) in &graph.edges {
                edges
                    .entry((Arc::clone(&key.0), Arc::clone(&key.1)))
                    .and_modify(|existing| {
                        if meta.edge_kind.trust_rank() < existing.edge_kind.trust_rank() {
                            *existing = meta.clone();
                        }
                    })
                    .or_insert_with(|| meta.clone());
            }
        }

        Ok(CallGraph {
            forward,
            reverse,
            edges,
        })
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

    /// Helper: a NamedStore with explicit per-edge provenance. Each tuple is
    /// `(caller, callee, edge_kind, file, caller_line, call_line)`, written
    /// straight into `function_calls` so the in-memory `CallGraph` carries the
    /// kind + source location end-to-end.
    fn make_named_store_with_edges(
        name: &str,
        edges: &[(&str, &str, crate::parser::CallEdgeKind, &str, i64, i64)],
    ) -> NamedStore {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
        let model_info = crate::store::helpers::ModelInfo::default();

        let store = Store::<crate::store::ReadOnly>::open_readonly_after_init(&db_path, |store| {
            store.init(&model_info)?;
            for (caller, callee, kind, file, caller_line, call_line) in edges {
                store.rt.block_on(async {
                    sqlx::query(
                        "INSERT INTO function_calls (file, caller_name, callee_name, caller_line, call_line, edge_kind)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    )
                    .bind(file)
                    .bind(caller)
                    .bind(callee)
                    .bind(caller_line)
                    .bind(call_line)
                    .bind(kind.as_str())
                    .execute(&store.pool)
                    .await
                })?;
            }
            Ok(())
        })
        .unwrap();

        let _keep = dir.keep();
        NamedStore::new(name.to_string(), store, db_path)
    }

    /// Edge provenance (kind + source location) survives the in-memory
    /// `CallGraph` load and surfaces on cross-project callers/callees.
    #[test]
    fn cross_project_caller_carries_edge_kind_and_location() {
        use crate::parser::CallEdgeKind;
        let store = make_named_store_with_edges(
            "proj_a",
            &[(
                "caller_a",
                "target",
                CallEdgeKind::MacroHeuristic,
                "src/a.rs",
                12,
                34,
            )],
        );
        let mut ctx = CrossProjectContext::new(vec![store]);

        let callers = ctx.get_callers_cross("target").unwrap();
        assert_eq!(callers.len(), 1);
        let c = &callers[0];
        assert_eq!(c.caller.name, "caller_a");
        // Provenance threaded through, not defaulted to `call`.
        assert_eq!(c.caller.edge_kind, CallEdgeKind::MacroHeuristic);
        assert_eq!(c.caller.file, std::path::PathBuf::from("src/a.rs"));
        assert_eq!(c.caller.line, 12);

        // Callee side: kind threads through, line is the call_line.
        let callees = ctx.get_callees_cross("caller_a").unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].name, "target");
        assert_eq!(callees[0].edge_kind, CallEdgeKind::MacroHeuristic);
        assert_eq!(callees[0].line, 34);
    }

    /// Multiple raw rows for one `(caller, callee)` pair collapse to the
    /// most-trusted kind at graph load — a co-located `call` and `doc_reference`
    /// surface as `call`, mirroring the single-project MIN-rank collapse.
    #[test]
    fn cross_project_edge_collapse_keeps_most_trusted_kind() {
        use crate::parser::CallEdgeKind;
        let store = make_named_store_with_edges(
            "proj_a",
            &[
                ("c", "t", CallEdgeKind::DocReference, "src/a.rs", 1, 5),
                ("c", "t", CallEdgeKind::Call, "src/a.rs", 1, 9),
            ],
        );
        let mut ctx = CrossProjectContext::new(vec![store]);
        let callers = ctx.get_callers_cross("t").unwrap();
        assert_eq!(callers.len(), 1, "the pair collapses to one edge");
        assert_eq!(
            callers[0].caller.edge_kind,
            CallEdgeKind::Call,
            "call (rank 0) outranks doc_reference (rank 4)"
        );
    }

    /// The cross-project MERGE keeps the most-trusted kind when the same
    /// `(caller, callee)` appears in more than one project — a `call` in one
    /// project is never demoted to a `doc_reference` from another.
    #[test]
    fn merged_call_graph_keeps_most_trusted_across_projects() {
        use crate::parser::CallEdgeKind;
        let store_a = make_named_store_with_edges(
            "proj_a",
            &[("shared", "fn", CallEdgeKind::DocReference, "src/a.rs", 1, 2)],
        );
        let store_b = make_named_store_with_edges(
            "proj_b",
            &[("shared", "fn", CallEdgeKind::Call, "src/b.rs", 3, 4)],
        );
        let mut ctx = CrossProjectContext::new(vec![store_a, store_b]);
        let merged = ctx.merged_call_graph().unwrap();
        let meta = merged.edge_meta("shared", "fn");
        assert_eq!(
            meta.edge_kind,
            CallEdgeKind::Call,
            "merge must keep the most-trusted kind across projects"
        );
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
        let mut ctx = CrossProjectContext::new(vec![ns]);

        assert!(!ctx.is_stale(), "freshly built context is not stale");

        // Append bytes and bump mtime past the 1s FS granularity floor so the
        // file identity (size + mtime) changes.
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

    /// Sub-second, same-size, in-place replacement (rename-over) of a backing
    /// `index.db` must mark the context stale via the file-identity inode bump —
    /// even when mtime and size are unchanged.
    ///
    /// Calibration: RED against the previous `(mtime, size)`-only key (a
    /// same-size rewrite inside one WSL mtime bucket reads as unchanged → stale
    /// serve). GREEN with [`FileIdentity`], which mixes in the inode: the
    /// rename-over gives a new inode immediately. Pins the inode-catchable
    /// replace case.
    #[cfg(unix)]
    #[test]
    fn test_is_stale_detects_same_size_rename_over() {
        use std::os::unix::fs::MetadataExt;

        let mut forward = StdMap::new();
        forward.insert("caller_a".to_string(), vec!["target".to_string()]);
        let ns = make_named_store("proj_a", forward, StdMap::new());
        let db_path = ns.db_path.clone();
        let mut ctx = CrossProjectContext::new(vec![ns]);
        assert!(!ctx.is_stale(), "freshly built context is not stale");

        // Build a byte-identical replacement (copy the file) and rename it over
        // the original. Same size; the copy lands a NEW inode. Pin both
        // preconditions so the test proves it's the inode (not size/mtime)
        // doing the catching.
        let size_before = std::fs::metadata(&db_path).unwrap().len();
        let inode_before = std::fs::metadata(&db_path).unwrap().ino();
        let orig_mtime = std::fs::metadata(&db_path).unwrap().modified().unwrap();
        let replacement = db_path.with_extension("db.replacement");
        std::fs::copy(&db_path, &replacement).unwrap();
        // Force the same mtime as the original so the (mtime, size) key cannot
        // see the change — only the inode moves.
        std::fs::File::open(&replacement)
            .unwrap()
            .set_modified(orig_mtime)
            .unwrap();
        std::fs::rename(&replacement, &db_path).unwrap();

        let md_after = std::fs::metadata(&db_path).unwrap();
        assert_eq!(md_after.len(), size_before, "precondition: size unchanged");
        assert_ne!(
            md_after.ino(),
            inode_before,
            "precondition: rename-over landed a new inode"
        );
        assert_eq!(
            md_after.modified().unwrap(),
            orig_mtime,
            "precondition: mtime forced equal — only the inode discriminator can fire"
        );

        assert!(
            ctx.is_stale(),
            "same-size rename-over (new inode, same mtime/size) must mark the context stale"
        );
    }

    /// A WAL-mode commit with NO checkpoint leaves the backing file's identity
    /// (inode/size/mtime) unchanged, yet the cached context is stale. The
    /// `PRAGMA data_version` probe must catch it.
    ///
    /// Calibration: RED against EITHER `(mtime, size)`-only OR the
    /// `FileIdentity`-only key (neither moves on an uncheckpointed WAL commit).
    /// GREEN only with the long-lived data_version probe. Pins the
    /// WAL-incremental case — the real `cqs ref update` shape (in-place
    /// incremental reindex before the closing checkpoint).
    #[test]
    fn test_is_stale_detects_wal_commit_without_checkpoint() {
        use sqlx::{ConnectOptions, Connection};

        let ns = make_named_store("proj_a", StdMap::new(), StdMap::new());
        let db_path = ns.db_path.clone();
        let mut ctx = CrossProjectContext::new(vec![ns]);
        // Baseline both discriminators.
        assert!(!ctx.is_stale(), "freshly built context is not stale");

        let id_before = FileIdentity::from_path(&db_path).unwrap();

        // Second connection, WAL commit, NO checkpoint, kept open across the
        // assertions so closing the last writer can't auto-checkpoint into the
        // main file and mask the discriminator under test.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut writer = rt
            .block_on(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&db_path)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                    .connect(),
            )
            .unwrap();
        rt.block_on(async {
            sqlx::query("CREATE TABLE IF NOT EXISTS wal_poke (x INTEGER)")
                .execute(&mut writer)
                .await?;
            sqlx::query("INSERT INTO wal_poke (x) VALUES (1)")
                .execute(&mut writer)
                .await?;
            Ok::<_, sqlx::Error>(())
        })
        .unwrap();

        // Precondition: the commit landed in the WAL, not the main file — if
        // identity moved, this would prove nothing about data_version.
        assert_eq!(
            FileIdentity::from_path(&db_path).unwrap(),
            id_before,
            "precondition: WAL commit must leave main-file identity unchanged"
        );

        assert!(
            ctx.is_stale(),
            "WAL commit with no checkpoint must mark the context stale via data_version"
        );

        let _ = rt.block_on(writer.close());
    }

    #[test]
    fn test_new_context_has_no_fingerprint() {
        // Test-assembled contexts (via `new`) carry no fingerprint — only
        // `from_config` sets one, since only it reads the references config.
        let ctx = CrossProjectContext::new(vec![]);
        assert_eq!(ctx.fingerprint(), None);
    }
}
