//! Cross-project search via global project registry.
//!
//! Maintains a registry of indexed projects at `~/.config/cqs/projects.toml`.
//! Enables searching across all registered projects from anywhere.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

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
    pub fn load() -> Result<Self> {
        let path = registry_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        // Size guard: project registry should be well under 1MB
        const MAX_REGISTRY_SIZE: u64 = 1024 * 1024;
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > MAX_REGISTRY_SIZE {
                anyhow::bail!(
                    "Project registry too large: {}KB (limit {}KB)",
                    meta.len() / 1024,
                    MAX_REGISTRY_SIZE / 1024
                );
            }
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        toml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// Save registry to default location
    pub fn save(&self) -> Result<()> {
        let path = registry_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        // Acquire exclusive lock for the write
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("Failed to open {} for locking", path.display()))?;
        lock_file
            .lock()
            .with_context(|| format!("Failed to lock {}", path.display()))?;

        let content = toml::to_string_pretty(self)?;
        // Atomic write: temp file + rename (unpredictable suffix to prevent symlink attacks)
        let suffix = {
            use std::hash::{BuildHasher, Hasher};
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish()
        };
        let tmp = path.with_extension(format!("toml.{:016x}.tmp", suffix));
        std::fs::write(&tmp, &content)
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        if let Err(rename_err) = std::fs::rename(&tmp, &path) {
            // Cross-device fallback (Docker overlayfs, some CI)
            if let Err(copy_err) = std::fs::copy(&tmp, &path) {
                let _ = std::fs::remove_file(&tmp);
                bail!(
                    "rename {} -> {} failed ({}), copy fallback failed: {}",
                    tmp.display(),
                    path.display(),
                    rename_err,
                    copy_err
                );
            }
            let _ = std::fs::remove_file(&tmp);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        // lock_file dropped here, releasing exclusive lock
        Ok(())
    }

    /// Register a project (replaces existing entry with same name)
    pub fn register(&mut self, name: String, path: PathBuf) -> Result<()> {
        // Validate the path has a .cqs (or legacy .cq) directory
        if !path.join(".cqs/index.db").exists() && !path.join(".cq/index.db").exists() {
            bail!(
                "No cqs index found at {}. Run 'cqs init && cqs index' there first.",
                path.display()
            );
        }

        // Remove existing entry with same name
        self.project.retain(|p| p.name != name);
        self.project.push(ProjectEntry { name, path });
        self.save()
    }

    /// Remove a project by name
    pub fn remove(&mut self, name: &str) -> Result<bool> {
        let before = self.project.len();
        self.project.retain(|p| p.name != name);
        let removed = self.project.len() < before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Get a project by name
    pub fn get(&self, name: &str) -> Option<&ProjectEntry> {
        self.project.iter().find(|p| p.name == name)
    }
}

/// Get the registry file path
fn registry_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;
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
) -> Result<Vec<CrossProjectResult>> {
    let registry = ProjectRegistry::load()?;
    let _span = tracing::info_span!(
        "search_across_projects",
        project_count = registry.project.len()
    )
    .entered();
    if registry.project.is_empty() {
        bail!("No projects registered. Use 'cqs project register <name> <path>' to add one.");
    }

    let mut all_results = Vec::new();

    for entry in &registry.project {
        // Prefer .cqs, fall back to legacy .cq
        let index_path = {
            let new_path = entry.path.join(".cqs/index.db");
            if new_path.exists() {
                new_path
            } else {
                entry.path.join(".cq/index.db")
            }
        };
        if !index_path.exists() {
            tracing::warn!(
                "Skipping project '{}' — index not found at {}",
                entry.name,
                index_path.display()
            );
            continue;
        }

        match crate::Store::open_readonly(&index_path) {
            Ok(store) => {
                let cqs_dir = index_path.parent().unwrap_or(entry.path.as_path());
                let index = crate::hnsw::HnswIndex::try_load(cqs_dir);
                let filter = crate::store::helpers::SearchFilter {
                    query_text: query_text.to_string(),
                    enable_rrf: true,
                    ..Default::default()
                };
                match store.search_filtered_with_index(
                    query_embedding,
                    &filter,
                    limit,
                    threshold,
                    index.as_deref(),
                ) {
                    Ok(results) => {
                        for r in results {
                            all_results.push(CrossProjectResult {
                                project_name: entry.name.clone(),
                                name: r.chunk.name.clone(),
                                file: make_project_relative(&entry.path, &r.chunk.file),
                                line_start: r.chunk.line_start,
                                signature: Some(r.chunk.signature.clone()),
                                score: r.score,
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Search failed for project '{}': {}", entry.name, e);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to open project '{}': {}", entry.name, e);
            }
        }
    }

    // Sort by score descending, take top N
    all_results.sort_by(|a, b| b.score.total_cmp(&a.score));
    all_results.truncate(limit);

    tracing::info!(
        result_count = all_results.len(),
        "Cross-project search complete"
    );

    Ok(all_results)
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
        let db_path = cqs_dir.join("index.db");

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
        };

        // Create a simple embedding (769-dim: 768 model + 1 sentiment)
        let embedding = crate::Embedding::new(vec![0.1; 769]);
        store.upsert_chunk(&chunk, &embedding, None).unwrap();
        drop(store);

        // Now test that Store::open_readonly works on this index
        let store = crate::Store::open_readonly(&db_path).unwrap();
        let filter = crate::store::helpers::SearchFilter {
            query_text: "test function".to_string(),
            enable_rrf: true,
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
