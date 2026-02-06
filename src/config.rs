//! Configuration file support for cqs
//!
//! Config files are loaded in order (later overrides earlier):
//! 1. `~/.config/cqs/config.toml` (user defaults)
//! 2. `.cqs.toml` in project root (project overrides)
//!
//! CLI flags override all config file values.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Reference index configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceConfig {
    /// Display name (used in results, CLI commands)
    pub name: String,
    /// Directory containing index.db + HNSW files
    pub path: PathBuf,
    /// Original source directory (for `ref update`)
    pub source: Option<PathBuf>,
    /// Score multiplier (0.0-1.0, default 0.8)
    #[serde(default = "default_ref_weight")]
    pub weight: f32,
}

fn default_ref_weight() -> f32 {
    0.8
}

/// Configuration options loaded from config files
///
/// # Example
///
/// ```toml
/// # ~/.config/cqs/config.toml or .cqs.toml
/// limit = 10          # Default result limit
/// threshold = 0.3     # Minimum similarity score
/// name_boost = 0.2    # Weight for name matching
/// quiet = false       # Suppress progress output
/// verbose = false     # Enable verbose logging
///
/// [[reference]]
/// name = "tokio"
/// path = "/home/user/.local/share/cqs/refs/tokio"
/// source = "/home/user/code/tokio"
/// weight = 0.8
/// ```
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default result limit (overridden by -n)
    pub limit: Option<usize>,
    /// Default similarity threshold (overridden by -t)
    pub threshold: Option<f32>,
    /// Default name boost for hybrid search (overridden by --name-boost)
    pub name_boost: Option<f32>,
    /// Enable quiet mode by default
    pub quiet: Option<bool>,
    /// Enable verbose mode by default
    pub verbose: Option<bool>,
    /// Reference indexes for multi-index search
    #[serde(default, rename = "reference")]
    pub references: Vec<ReferenceConfig>,
}

impl Config {
    /// Load configuration from user and project config files
    pub fn load(project_root: &Path) -> Self {
        let user_config = dirs::config_dir()
            .map(|d| d.join("cqs/config.toml"))
            .and_then(|p| Self::load_file(&p))
            .unwrap_or_default();

        let project_config = Self::load_file(&project_root.join(".cqs.toml")).unwrap_or_default();

        // Project overrides user
        let merged = user_config.override_with(project_config);
        tracing::debug!(
            limit = ?merged.limit,
            threshold = ?merged.threshold,
            name_boost = ?merged.name_boost,
            quiet = ?merged.quiet,
            verbose = ?merged.verbose,
            references = merged.references.len(),
            "Effective config after merge"
        );
        merged
    }

    /// Load configuration from a specific file
    fn load_file(path: &Path) -> Option<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!("Failed to read config {}: {}", path.display(), e);
                return None;
            }
        };

        match toml::from_str::<Self>(&content) {
            Ok(config) => {
                tracing::debug!(
                    path = %path.display(),
                    limit = ?config.limit,
                    threshold = ?config.threshold,
                    name_boost = ?config.name_boost,
                    quiet = ?config.quiet,
                    verbose = ?config.verbose,
                    references = config.references.len(),
                    "Loaded config"
                );
                Some(config)
            }
            Err(e) => {
                tracing::warn!("Failed to parse config {}: {}", path.display(), e);
                None
            }
        }
    }

    /// Layer another config on top (other overrides self where present)
    fn override_with(self, other: Self) -> Self {
        // Merge references: project refs replace user refs by name, append new ones
        let mut refs = self.references;
        for proj_ref in other.references {
            if let Some(pos) = refs.iter().position(|r| r.name == proj_ref.name) {
                refs[pos] = proj_ref;
            } else {
                refs.push(proj_ref);
            }
        }

        Config {
            limit: other.limit.or(self.limit),
            threshold: other.threshold.or(self.threshold),
            name_boost: other.name_boost.or(self.name_boost),
            quiet: other.quiet.or(self.quiet),
            verbose: other.verbose.or(self.verbose),
            references: refs,
        }
    }

    // ===== Accessors with defaults =====

    /// Default result limit for search queries
    pub const DEFAULT_LIMIT: usize = 5;
    /// Default similarity threshold (0.0-1.0)
    pub const DEFAULT_THRESHOLD: f32 = 0.3;
    /// Default name boost for hybrid search
    pub const DEFAULT_NAME_BOOST: f32 = 0.2;

    /// Get result limit with default fallback
    pub fn limit_or_default(&self) -> usize {
        self.limit.unwrap_or(Self::DEFAULT_LIMIT)
    }

    /// Get similarity threshold with default fallback
    pub fn threshold_or_default(&self) -> f32 {
        self.threshold.unwrap_or(Self::DEFAULT_THRESHOLD)
    }

    /// Get name boost with default fallback
    pub fn name_boost_or_default(&self) -> f32 {
        self.name_boost.unwrap_or(Self::DEFAULT_NAME_BOOST)
    }

    /// Get quiet mode with default fallback (false)
    pub fn quiet_or_default(&self) -> bool {
        self.quiet.unwrap_or(false)
    }

    /// Get verbose mode with default fallback (false)
    pub fn verbose_or_default(&self) -> bool {
        self.verbose.unwrap_or(false)
    }
}

/// Add a reference to a config file (read-modify-write, preserves unknown fields)
pub fn add_reference_to_config(
    config_path: &Path,
    ref_config: &ReferenceConfig,
) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut table: toml::Table = if content.is_empty() {
        toml::Table::new()
    } else {
        content
            .parse()
            .map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", config_path.display(), e))?
    };

    // Check for duplicate name
    if let Some(toml::Value::Array(arr)) = table.get("reference") {
        let has_duplicate = arr.iter().any(|v| {
            v.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n == ref_config.name)
                .unwrap_or(false)
        });
        if has_duplicate {
            anyhow::bail!(
                "Reference '{}' already exists in {}",
                ref_config.name,
                config_path.display()
            );
        }
    }

    let ref_value = toml::Value::try_from(ref_config)
        .map_err(|e| anyhow::anyhow!("Failed to serialize reference config: {}", e))?;

    let refs = table
        .entry("reference")
        .or_insert_with(|| toml::Value::Array(vec![]));

    match refs {
        toml::Value::Array(arr) => arr.push(ref_value),
        _ => anyhow::bail!("'reference' in config is not an array"),
    }

    std::fs::write(config_path, toml::to_string_pretty(&table)?)?;

    // Restrict permissions — config may contain paths revealing project structure
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// Remove a reference from a config file by name (read-modify-write)
pub fn remove_reference_from_config(config_path: &Path, name: &str) -> anyhow::Result<bool> {
    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };

    let mut table: toml::Table = content
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", config_path.display(), e))?;

    let removed = if let Some(toml::Value::Array(arr)) = table.get_mut("reference") {
        let before = arr.len();
        arr.retain(|v| {
            v.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n != name)
                .unwrap_or(true)
        });
        let removed = arr.len() < before;
        // Clean up empty array
        if arr.is_empty() {
            table.remove("reference");
        }
        removed
    } else {
        false
    };

    if removed {
        std::fs::write(config_path, toml::to_string_pretty(&table)?)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_valid_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "limit = 10\nthreshold = 0.5\n").unwrap();

        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.limit, Some(10));
        assert_eq!(config.threshold, Some(0.5));
    }

    #[test]
    fn test_load_missing_file() {
        let dir = TempDir::new().unwrap();
        let config = Config::load_file(&dir.path().join("nonexistent.toml"));
        assert!(config.is_none());
    }

    #[test]
    fn test_load_malformed_toml() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "not valid [[[").unwrap();

        let config = Config::load_file(&config_path);
        assert!(config.is_none());
    }

    #[test]
    fn test_merge_override() {
        let base = Config {
            limit: Some(10),
            threshold: Some(0.5),
            ..Default::default()
        };
        let override_cfg = Config {
            limit: Some(20),
            name_boost: Some(0.3),
            ..Default::default()
        };

        let merged = base.override_with(override_cfg);
        assert_eq!(merged.limit, Some(20));
        assert_eq!(merged.threshold, Some(0.5));
        assert_eq!(merged.name_boost, Some(0.3));
    }

    #[test]
    fn test_parse_config_with_references() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(
            &config_path,
            r#"
limit = 5

[[reference]]
name = "tokio"
path = "/home/user/.local/share/cqs/refs/tokio"
source = "/home/user/code/tokio"
weight = 0.8

[[reference]]
name = "serde"
path = "/home/user/.local/share/cqs/refs/serde"
"#,
        )
        .unwrap();

        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.limit, Some(5));
        assert_eq!(config.references.len(), 2);
        assert_eq!(config.references[0].name, "tokio");
        assert_eq!(config.references[0].weight, 0.8);
        assert!(config.references[0].source.is_some());
        assert_eq!(config.references[1].name, "serde");
        assert_eq!(config.references[1].weight, 0.8); // default
        assert!(config.references[1].source.is_none());
    }

    #[test]
    fn test_merge_references_replace_by_name() {
        let user = Config {
            references: vec![
                ReferenceConfig {
                    name: "tokio".into(),
                    path: "/old/path".into(),
                    source: None,
                    weight: 0.5,
                },
                ReferenceConfig {
                    name: "serde".into(),
                    path: "/serde/path".into(),
                    source: None,
                    weight: 0.8,
                },
            ],
            ..Default::default()
        };
        let project = Config {
            references: vec![
                ReferenceConfig {
                    name: "tokio".into(),
                    path: "/new/path".into(),
                    source: Some("/src/tokio".into()),
                    weight: 0.9,
                },
                ReferenceConfig {
                    name: "axum".into(),
                    path: "/axum/path".into(),
                    source: None,
                    weight: 0.7,
                },
            ],
            ..Default::default()
        };

        let merged = user.override_with(project);
        assert_eq!(merged.references.len(), 3);
        // tokio replaced
        assert_eq!(merged.references[0].name, "tokio");
        assert_eq!(merged.references[0].path, PathBuf::from("/new/path"));
        assert_eq!(merged.references[0].weight, 0.9);
        // serde kept
        assert_eq!(merged.references[1].name, "serde");
        // axum appended
        assert_eq!(merged.references[2].name, "axum");
    }

    #[test]
    fn test_add_reference_to_config_new_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref_config = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: Some("/src/tokio".into()),
            weight: 0.8,
        };
        add_reference_to_config(&config_path, &ref_config).unwrap();

        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.references.len(), 1);
        assert_eq!(config.references[0].name, "tokio");
    }

    #[test]
    fn test_add_reference_to_config_preserves_fields() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "limit = 10\nthreshold = 0.5\n").unwrap();

        let ref_config = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        add_reference_to_config(&config_path, &ref_config).unwrap();

        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.limit, Some(10));
        assert_eq!(config.threshold, Some(0.5));
        assert_eq!(config.references.len(), 1);
    }

    #[test]
    fn test_add_reference_to_config_appends() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref1 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        let ref2 = ReferenceConfig {
            name: "serde".into(),
            path: "/refs/serde".into(),
            source: None,
            weight: 0.7,
        };
        add_reference_to_config(&config_path, &ref1).unwrap();
        add_reference_to_config(&config_path, &ref2).unwrap();

        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.references.len(), 2);
        assert_eq!(config.references[0].name, "tokio");
        assert_eq!(config.references[1].name, "serde");
    }

    #[test]
    fn test_remove_reference_from_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref1 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        let ref2 = ReferenceConfig {
            name: "serde".into(),
            path: "/refs/serde".into(),
            source: None,
            weight: 0.7,
        };
        add_reference_to_config(&config_path, &ref1).unwrap();
        add_reference_to_config(&config_path, &ref2).unwrap();

        let removed = remove_reference_from_config(&config_path, "tokio").unwrap();
        assert!(removed);

        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.references.len(), 1);
        assert_eq!(config.references[0].name, "serde");
    }

    #[test]
    fn test_remove_reference_not_found() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "limit = 5\n").unwrap();

        let removed = remove_reference_from_config(&config_path, "nonexistent").unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_remove_reference_missing_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("nonexistent.toml");

        let removed = remove_reference_from_config(&config_path, "tokio").unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_remove_last_reference_cleans_array() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref1 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        add_reference_to_config(&config_path, &ref1).unwrap();
        remove_reference_from_config(&config_path, "tokio").unwrap();

        // Should still be valid config, just no references
        let config = Config::load_file(&config_path).unwrap();
        assert!(config.references.is_empty());
    }

    #[test]
    fn test_add_reference_duplicate_name_errors() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        let ref1 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio".into(),
            source: None,
            weight: 0.8,
        };
        add_reference_to_config(&config_path, &ref1).unwrap();

        // Adding same name again should fail
        let ref2 = ReferenceConfig {
            name: "tokio".into(),
            path: "/refs/tokio2".into(),
            source: None,
            weight: 0.5,
        };
        let result = add_reference_to_config(&config_path, &ref2);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));

        // Original should be unchanged
        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.references.len(), 1);
        assert_eq!(config.references[0].weight, 0.8);
    }
}
