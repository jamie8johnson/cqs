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
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write {}", path.display()))
    }

    /// Register a project (replaces existing entry with same name)
    pub fn register(&mut self, name: String, path: PathBuf) -> Result<()> {
        // Validate the path has a .cq directory
        if !path.join(".cq/index.db").exists() {
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
    limit: usize,
    threshold: f32,
) -> Result<Vec<CrossProjectResult>> {
    let registry = ProjectRegistry::load()?;
    if registry.project.is_empty() {
        bail!("No projects registered. Use 'cqs project register <name> <path>' to add one.");
    }

    let mut all_results = Vec::new();

    for entry in &registry.project {
        let index_path = entry.path.join(".cq/index.db");
        if !index_path.exists() {
            tracing::warn!(
                "Skipping project '{}' — index not found at {}",
                entry.name,
                index_path.display()
            );
            continue;
        }

        match crate::Store::open(&index_path) {
            Ok(store) => match store.search(query_embedding, limit, threshold) {
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
            },
            Err(e) => {
                tracing::warn!("Failed to open project '{}': {}", entry.name, e);
            }
        }
    }

    // Sort by score descending, take top N
    all_results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(limit);

    Ok(all_results)
}

/// Make a file path relative to the project root for display
fn make_project_relative(project_root: &Path, file: &Path) -> PathBuf {
    file.strip_prefix(project_root)
        .unwrap_or(file)
        .to_path_buf()
}
