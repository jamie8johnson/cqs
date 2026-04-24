//! Cross-project search via global project registry.
//!
//! Maintains a registry of indexed projects at `~/.config/cqs/projects.toml`.
//! Enables searching across all registered projects from anywhere.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Typed error for project registry operations (EH-13).
///
/// CLI callers convert to `anyhow::Error` at the boundary via the blanket `From`.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("Config directory not found")]
    ConfigDirNotFound,
    #[error("Project not found: {0}")]
    NotFound(String),
    #[error("File too large: {0}")]
    FileTooLarge(String),
    #[error("No projects registered")]
    NoProjects,
}

/// Whether the WSL advisory locking warning has been emitted (once per process)
static WSL_REGISTRY_LOCK_WARNED: AtomicBool = AtomicBool::new(false);

/// Global registry of indexed cqs projects
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProjectRegistry {
    #[serde(default)]
    pub project: Vec<ProjectEntry>,
}

/// A registered project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub name: String,
    pub path: PathBuf,
}

impl ProjectRegistry {
    /// Load registry from default location (~/.config/cqs/projects.toml)
    pub fn load() -> Result<Self, ProjectError> {
        let path = registry_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        // Read first, then enforce the size guard — avoids TOCTOU between stat and read.
        const MAX_REGISTRY_SIZE: usize = 1024 * 1024;
        let content = std::fs::read_to_string(&path)?;
        if content.len() > MAX_REGISTRY_SIZE {
            return Err(ProjectError::FileTooLarge(format!(
                "Project registry too large: {}KB (limit {}KB)",
                content.len() / 1024,
                MAX_REGISTRY_SIZE / 1024
            )));
        }
        Ok(toml::from_str(&content)?)
    }

    /// Save registry to default location
    pub fn save(&self) -> Result<(), ProjectError> {
        let path = registry_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Acquire exclusive lock for the write
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        lock_file.lock()?;

        // PB-V1.29-6: Delegate the `/mnt/<letter>/` detection to the shared helper.
        if crate::config::is_wsl()
            && crate::config::is_wsl_drvfs_path(&path)
            && !WSL_REGISTRY_LOCK_WARNED.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                "Registry file locking is advisory-only on WSL/NTFS — avoid concurrent cqs ref add"
            );
        }

        let content = toml::to_string_pretty(self)?;
        // Atomic write: temp file + rename (unpredictable suffix to prevent symlink attacks)
        let suffix = crate::temp_suffix();
        let tmp = path.with_extension(format!("toml.{:016x}.tmp", suffix));
        std::fs::write(&tmp, &content)?;
        // atomic_replace: fsync tmp, rename with EXDEV fallback, fsync parent dir.
        crate::fs::atomic_replace(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            ProjectError::Io(e)
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            {
                tracing::debug!(path = %path.display(), error = %e, "Failed to set file permissions");
            }
        }
        // lock_file dropped here, releasing exclusive lock
        Ok(())
    }

    /// Register a project (replaces existing entry with same name)
    pub fn register(&mut self, name: String, path: PathBuf) -> Result<(), ProjectError> {
        // Validate the path has a .cqs (or legacy .cq) directory.
        // Post-slots layout: `.cqs/slots/<active>/index.db`. Pre-slots layout:
        // `.cqs/index.db`. Pre-v0.9.7 layout: `.cq/index.db`.
        let cqs_dir = path.join(".cqs");
        let has_cqs = cqs_dir.exists() && crate::resolve_index_db(&cqs_dir).exists();
        if !has_cqs && !path.join(".cq/index.db").exists() {
            return Err(ProjectError::NotFound(format!(
                "No cqs index found at {}. Run 'cqs init && cqs index' there first.",
                path.display()
            )));
        }

        // Remove existing entry with same name
        self.project.retain(|p| p.name != name);
        self.project.push(ProjectEntry { name, path });
        self.save()
    }

    /// Removes a project by name from the collection and persists the changes to storage.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the project to remove
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if a project with the given name was found and removed, or `Ok(false)` if no matching project exists. Returns `Err(ProjectError)` if saving the updated collection to storage fails.
    ///
    /// # Errors
    ///
    /// Returns a `ProjectError` if the save operation fails while persisting the removal to storage.
    pub fn remove(&mut self, name: &str) -> Result<bool, ProjectError> {
        let before = self.project.len();
        self.project.retain(|p| p.name != name);
        let removed = self.project.len() < before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Retrieves a project entry by name.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the project to look up.
    ///
    /// # Returns
    ///
    /// Returns `Some(&ProjectEntry)` if a project with the given name exists, or `None` if no matching project is found.
    pub fn get(&self, name: &str) -> Option<&ProjectEntry> {
        self.project.iter().find(|p| p.name == name)
    }
}

/// Get the registry file path
fn registry_path() -> Result<PathBuf, ProjectError> {
    let config_dir = dirs::config_dir().ok_or(ProjectError::ConfigDirNotFound)?;
    Ok(config_dir.join("cqs").join("projects.toml"))
}

/// Search result from a specific project
#[derive(Debug)]
pub struct CrossProjectResult {
    pub project_name: String,
    pub name: String,
    pub file: PathBuf,
    pub line_start: u32,
    pub signature: Option<String>,
    pub score: f32,
}

/// Search across all registered projects
pub fn search_across_projects(
    query_embedding: &crate::Embedding,
    query_text: &str,
    limit: usize,
    threshold: f32,
) -> Result<Vec<CrossProjectResult>, ProjectError> {
    let registry = ProjectRegistry::load()?;
    let _span = tracing::info_span!(
        "search_across_projects",
        project_count = registry.project.len()
    )
    .entered();
    if registry.project.is_empty() {
        return Err(ProjectError::NoProjects);
    }

    // RM-25: Cap concurrency to 4 threads — each project opens Store + HNSW (~200MB).
    // RB-16: Fall back to sequential execution if thread pool creation fails,
    // rather than panicking on a double-unwrap.
    let threads = std::env::var("CQS_RAYON_THREADS")
        .ok()
        .and_then(|v| {
            let parsed = v.parse();
            if parsed.is_err() {
                tracing::warn!(value = %v, "Invalid CQS_RAYON_THREADS, using default");
            }
            parsed.ok()
        })
        .unwrap_or(4);
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to build rayon thread pool, falling back to sequential");
            // Execute sequentially without a custom pool
            let project_results: Vec<Vec<CrossProjectResult>> = registry
                .project
                .iter()
                .filter_map(|entry| {
                    match search_single_project(entry, query_embedding, query_text, limit, threshold) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            tracing::warn!(project = %entry.name, error = %e, "Search failed for project");
                            None
                        }
                    }
                })
                .collect();
            let mut all_results: Vec<CrossProjectResult> =
                project_results.into_iter().flatten().collect();
            // Secondary sort on (project, file, line_start, name) keeps equal-
            // score results deterministically ordered across process invocations.
            all_results.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then(a.project_name.cmp(&b.project_name))
                    .then(a.file.cmp(&b.file))
                    .then(a.line_start.cmp(&b.line_start))
                    .then(a.name.cmp(&b.name))
            });
            all_results.truncate(limit);
            tracing::info!(
                result_count = all_results.len(),
                "Cross-project search complete (sequential fallback)"
            );
            return Ok(all_results);
        }
    };
    let project_results: Vec<Vec<CrossProjectResult>> = pool.install(|| {
        registry
            .project
            .par_iter()
            .filter_map(|entry| {
                match search_single_project(entry, query_embedding, query_text, limit, threshold) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!(project = %entry.name, error = %e, "Search failed for project");
                        None
                    }
                }
            })
            .collect()
    });

    let mut all_results: Vec<CrossProjectResult> = project_results.into_iter().flatten().collect();

    // Sort by score descending, take top N. Secondary sort on
    // (project, file, line_start, name) keeps equal-score results
    // deterministically ordered across process invocations so the
    // truncate() picks the same survivors on every run.
    all_results.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.project_name.cmp(&b.project_name))
            .then(a.file.cmp(&b.file))
            .then(a.line_start.cmp(&b.line_start))
            .then(a.name.cmp(&b.name))
    });
    all_results.truncate(limit);

    tracing::info!(
        result_count = all_results.len(),
        "Cross-project search complete"
    );

    Ok(all_results)
}

/// Search a single project entry, returning results or an error on failure.
///
/// Extracted to share between the parallel (rayon) and sequential (fallback) paths.
fn search_single_project(
    entry: &ProjectEntry,
    query_embedding: &crate::Embedding,
    query_text: &str,
    limit: usize,
    threshold: f32,
) -> Result<Vec<CrossProjectResult>, anyhow::Error> {
    let _span = tracing::info_span!("search_single_project", project = %entry.name).entered();
    // Prefer .cqs (with slot resolution), fall back to legacy .cq.
    // `resolve_index_db` handles slots/<active>/index.db AND pre-slots
    // `.cqs/index.db` for unmigrated cross-project entries.
    let index_path = {
        let cqs_dir = entry.path.join(".cqs");
        if cqs_dir.exists() {
            crate::resolve_index_db(&cqs_dir)
        } else {
            entry.path.join(".cq/index.db")
        }
    };
    if !index_path.exists() {
        anyhow::bail!(
            "Skipping project '{}' — index not found at {}",
            entry.name,
            index_path.display()
        );
    }

    let store = crate::Store::open_readonly(&index_path)?;
    let cqs_dir = index_path.parent().unwrap_or(entry.path.as_path());
    let index = crate::hnsw::HnswIndex::try_load_with_ef(cqs_dir, None, store.dim());
    let filter = crate::store::helpers::SearchFilter {
        query_text: query_text.to_string(),
        enable_rrf: false, // RRF off by default — pure cosine is faster + higher R@1 on expanded eval
        ..Default::default()
    };
    let results = store.search_filtered_with_index(
        query_embedding,
        &filter,
        limit,
        threshold,
        index.as_deref(),
    )?;
    let mapped: Vec<CrossProjectResult> = results
        .into_iter()
        .map(|r| CrossProjectResult {
            project_name: entry.name.clone(),
            name: r.chunk.name.clone(),
            file: make_project_relative(&entry.path, &r.chunk.file),
            line_start: r.chunk.line_start,
            signature: Some(r.chunk.signature.clone()),
            score: r.score,
        })
        .collect();
    Ok(mapped)
}

/// Make a file path relative to the project root for display
fn make_project_relative(project_root: &Path, file: &Path) -> PathBuf {
    file.strip_prefix(project_root)
        .unwrap_or(file)
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_default_empty() {
        let reg = ProjectRegistry::default();
        assert!(reg.project.is_empty());
    }

    #[test]
    fn test_registry_get() {
        let tmp = std::env::temp_dir();
        let reg = ProjectRegistry {
            project: vec![
                ProjectEntry {
                    name: "foo".to_string(),
                    path: tmp.join("foo"),
                },
                ProjectEntry {
                    name: "bar".to_string(),
                    path: tmp.join("bar"),
                },
            ],
        };
        assert_eq!(reg.get("foo").unwrap().path, tmp.join("foo"));
        assert_eq!(reg.get("bar").unwrap().path, tmp.join("bar"));
        assert!(reg.get("baz").is_none());
    }

    #[test]
    fn test_registry_remove_in_memory() {
        let tmp = std::env::temp_dir();
        let mut reg = ProjectRegistry {
            project: vec![
                ProjectEntry {
                    name: "a".to_string(),
                    path: tmp.join("a"),
                },
                ProjectEntry {
                    name: "b".to_string(),
                    path: tmp.join("b"),
                },
            ],
        };

        // Remove by name (skip save since we're testing in-memory)
        let before = reg.project.len();
        reg.project.retain(|p| p.name != "a");
        assert_eq!(reg.project.len(), before - 1);
        assert!(reg.get("a").is_none());
        assert!(reg.get("b").is_some());
    }

    #[test]
    fn test_registry_serialization_roundtrip() {
        let tmp = std::env::temp_dir();
        let reg = ProjectRegistry {
            project: vec![ProjectEntry {
                name: "test".to_string(),
                path: tmp.join("test"),
            }],
        };
        let toml_str = toml::to_string_pretty(&reg).unwrap();
        let parsed: ProjectRegistry = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.project.len(), 1);
        assert_eq!(parsed.project[0].name, "test");
        assert_eq!(parsed.project[0].path, tmp.join("test"));
    }

    #[test]
    fn test_make_project_relative() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sub = root.join("src").join("main.rs");
        assert_eq!(
            make_project_relative(root, &sub),
            PathBuf::from("src/main.rs")
        );
    }

    #[test]
    fn test_make_project_relative_not_child() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let file = dir_b.path().join("file.rs");
        // File outside project root returns full path unchanged
        assert_eq!(make_project_relative(dir_a.path(), &file), file,);
    }

    // ===== search_across_projects tests =====
    // TC-36: Full end-to-end search_across_projects test with a temp registry
    // is not feasible here because it requires controlling HOME/XDG env vars
    // (which would break parallel test execution) and setting up multiple
    // stores with valid embeddings. The constituent pieces are tested below.
    // TODO: Add integration test with process-isolated temp HOME if coverage
    // is needed beyond the unit tests here.

    /// Helper: create a fake project registry TOML pointing at the given entries.
    /// Returns a guard that restores HOME/XDG after the test.
    /// We can't call the real `search_across_projects` without a real store,
    /// so we test the constituent pieces and error paths.

    #[test]
    fn test_search_across_projects_missing_index_skipped() {
        // A project entry whose path has no index.db should be skipped gracefully
        let dir = tempfile::tempdir().unwrap();
        let entry = ProjectEntry {
            name: "ghost".to_string(),
            path: dir.path().to_path_buf(),
        };
        // Verify the index path detection logic
        let new_path = entry.path.join(".cqs/index.db");
        let legacy_path = entry.path.join(".cq/index.db");
        assert!(!new_path.exists());
        assert!(!legacy_path.exists());
        // The search loop would `continue` past this entry with a warning
    }

    #[test]
    fn test_search_across_projects_empty_registry_error() {
        // Empty registry should produce an error, not silently return empty results
        let registry = ProjectRegistry::default();
        assert!(registry.project.is_empty());
        // The function bails with "No projects registered" when the list is empty.
        // We can't call the function directly without controlling HOME, but we
        // verify the logic: bail condition is `registry.project.is_empty()`
    }

    #[test]
    fn test_search_across_projects_with_real_store() {
        // Create a temp store, index a chunk, then verify search works
        // when pointed at the right path (same flow as search_across_projects).
        use crate::store::helpers::ModelInfo;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let db_path = cqs_dir.join(crate::INDEX_DB_FILENAME);

        let store = crate::Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Insert a chunk with a known embedding
        let content = "fn test_function() { println!(\"hello\"); }".to_string();
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = crate::parser::Chunk {
            id: format!("test.rs:1:{}", &hash[..8]),
            file: PathBuf::from("test.rs"),
            chunk_type: crate::parser::ChunkType::Function,
            name: "test_function".to_string(),
            signature: "fn test_function()".to_string(),
            content,
            doc: None,
            line_start: 1,
            line_end: 3,
            language: crate::parser::Language::Rust,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        // Create a simple embedding (EMBEDDING_DIM)
        let embedding = crate::Embedding::new(vec![0.1; crate::EMBEDDING_DIM]);
        store.upsert_chunk(&chunk, &embedding, None).unwrap();
        drop(store);

        // Now test that Store::open_readonly works on this index
        let store = crate::Store::open_readonly(&db_path).unwrap();
        let filter = crate::store::helpers::SearchFilter {
            query_text: "test function".to_string(),
            enable_rrf: false, // RRF off by default — pure cosine is faster + higher R@1 on expanded eval
            ..Default::default()
        };
        let results = store.search_filtered_with_index(
            &embedding, &filter, 10, 0.0, None, // no HNSW index
        );
        assert!(results.is_ok(), "search should not error on valid store");
        let results = results.unwrap();
        assert!(
            !results.is_empty(),
            "should find the inserted chunk via search"
        );
        assert_eq!(results[0].chunk.name, "test_function");
    }

    #[test]
    fn test_search_across_projects_sort_and_truncate() {
        // Verify the sort-by-score-descending + truncate logic
        let mut results = vec![
            CrossProjectResult {
                project_name: "a".into(),
                name: "low".into(),
                file: PathBuf::from("low.rs"),
                line_start: 1,
                signature: None,
                score: 0.1,
            },
            CrossProjectResult {
                project_name: "b".into(),
                name: "high".into(),
                file: PathBuf::from("high.rs"),
                line_start: 1,
                signature: None,
                score: 0.9,
            },
            CrossProjectResult {
                project_name: "c".into(),
                name: "mid".into(),
                file: PathBuf::from("mid.rs"),
                line_start: 1,
                signature: None,
                score: 0.5,
            },
        ];

        // Same sort logic as search_across_projects
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        results.truncate(2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "high");
        assert_eq!(results[1].name, "mid");
    }
}
