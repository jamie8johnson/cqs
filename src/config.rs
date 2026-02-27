//! Configuration file support for cqs
//!
//! Config files are loaded in order (later overrides earlier):
//! 1. `~/.config/cqs/config.toml` (user defaults)
//! 2. `.cqs.toml` in project root (project overrides)
//!
//! CLI flags override all config file values.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Detect if running under Windows Subsystem for Linux (cached)
fn is_wsl() -> bool {
    static IS_WSL: OnceLock<bool> = OnceLock::new();
    *IS_WSL.get_or_init(|| {
        std::fs::read_to_string("/proc/version")
            .map(|v| {
                let lower = v.to_lowercase();
                lower.contains("microsoft") || lower.contains("wsl")
            })
            .unwrap_or(false)
    })
}

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
/// stale_check = false # Disable per-file staleness checks
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
    /// Default note weight for unified search (overridden by --note-weight)
    pub note_weight: Option<f32>,
    /// Default note-only mode (overridden by --note-only)
    pub note_only: Option<bool>,
    /// Disable staleness checks (useful on NFS or slow filesystems)
    pub stale_check: Option<bool>,
    /// Reference indexes for multi-index search
    #[serde(default, rename = "reference")]
    pub references: Vec<ReferenceConfig>,
}

/// Clamp f32 config value to valid range and warn if out of bounds
fn clamp_config_f32(value: &mut f32, name: &str, min: f32, max: f32) {
    if *value < min || *value > max {
        tracing::warn!(
            field = name,
            value = *value,
            min,
            max,
            "Config value out of bounds, clamping"
        );
        *value = value.clamp(min, max);
    }
}

/// Clamp usize config value to valid range and warn if out of bounds
fn clamp_config_usize(value: &mut usize, name: &str, min: usize, max: usize) {
    if *value < min || *value > max {
        tracing::warn!(
            field = name,
            value = *value,
            min,
            max,
            "Config value out of bounds, clamping"
        );
        *value = (*value).clamp(min, max);
    }
}

impl Config {
    /// Load configuration from user and project config files
    pub fn load(project_root: &Path) -> Self {
        let user_config = dirs::config_dir()
            .map(|d| d.join("cqs/config.toml"))
            .and_then(|p| match Self::load_file(&p) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load config file");
                    None
                }
            })
            .unwrap_or_default();

        let project_config = match Self::load_file(&project_root.join(".cqs.toml")) {
            Ok(c) => c.unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load config file");
                Config::default()
            }
        };

        // Project overrides user
        let mut merged = user_config.override_with(project_config);
        merged.validate();

        tracing::debug!(?merged, "Effective config");
        merged
    }

    /// Clamp all fields to valid ranges and enforce invariants.
    ///
    /// Called once from `load()` after merging user + project configs.
    /// Adding a new field? Add its clamping here — this is the single
    /// validation choke point.
    fn validate(&mut self) {
        // Limit reference count
        const MAX_REFERENCES: usize = 20;
        if self.references.len() > MAX_REFERENCES {
            tracing::warn!(
                count = self.references.len(),
                max = MAX_REFERENCES,
                "Too many references configured, truncating"
            );
            self.references.truncate(MAX_REFERENCES);
        }

        // Clamp reference weights to [0.0, 1.0]
        for r in &mut self.references {
            clamp_config_f32(&mut r.weight, "reference.weight", 0.0, 1.0);
        }
        if let Some(ref mut limit) = self.limit {
            clamp_config_usize(limit, "limit", 1, 100);
        }
        if let Some(ref mut t) = self.threshold {
            clamp_config_f32(t, "threshold", 0.0, 1.0);
        }
        if let Some(ref mut nb) = self.name_boost {
            clamp_config_f32(nb, "name_boost", 0.0, 1.0);
        }
        if let Some(ref mut nw) = self.note_weight {
            clamp_config_f32(nw, "note_weight", 0.0, 1.0);
        }
    }

    /// Load configuration from a specific file
    fn load_file(path: &Path) -> Result<Option<Self>, String> {
        // Size guard: config files should be well under 1MB
        const MAX_CONFIG_SIZE: u64 = 1024 * 1024;
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > MAX_CONFIG_SIZE {
                return Err(format!(
                    "Config file too large: {}KB (limit {}KB)",
                    meta.len() / 1024,
                    MAX_CONFIG_SIZE / 1024
                ));
            }
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!("Failed to read config {}: {}", path.display(), e));
            }
        };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Skip permission check on WSL (NTFS always reports 777) or /mnt/ paths
            let is_wsl_mount = is_wsl() || path.to_str().is_some_and(|p| p.starts_with("/mnt/"));
            if !is_wsl_mount {
                if let Ok(meta) = std::fs::metadata(path) {
                    let mode = meta.permissions().mode();
                    if mode & 0o077 != 0 {
                        tracing::warn!(
                            path = %path.display(),
                            mode = format!("{:o}", mode & 0o777),
                            "Config file is accessible by other users. Consider: chmod 600 {}",
                            path.display()
                        );
                    }
                }
            }
        }

        match toml::from_str::<Self>(&content) {
            Ok(config) => {
                tracing::debug!(path = %path.display(), ?config, "Loaded config");
                Ok(Some(config))
            }
            Err(e) => Err(format!("Failed to parse config {}: {}", path.display(), e)),
        }
    }

    /// Layer another config on top (other overrides self where present)
    fn override_with(self, other: Self) -> Self {
        // Merge references: project refs replace user refs by name, append new ones
        let mut refs = self.references;
        for proj_ref in other.references {
            if let Some(pos) = refs.iter().position(|r| r.name == proj_ref.name) {
                tracing::warn!(
                    name = proj_ref.name,
                    "Project config overrides user reference '{}'",
                    proj_ref.name
                );
                refs[pos] = proj_ref;
            } else {
                refs.push(proj_ref);
            }
        }

        // MERGE: add new Option<T> fields here (other.field.or(self.field))
        Config {
            limit: other.limit.or(self.limit),
            threshold: other.threshold.or(self.threshold),
            name_boost: other.name_boost.or(self.name_boost),
            quiet: other.quiet.or(self.quiet),
            verbose: other.verbose.or(self.verbose),
            note_weight: other.note_weight.or(self.note_weight),
            note_only: other.note_only.or(self.note_only),
            stale_check: other.stale_check.or(self.stale_check),
            references: refs,
        }
    }
}

/// Add a reference to a config file (read-modify-write, preserves unknown fields)
pub fn add_reference_to_config(
    config_path: &Path,
    ref_config: &ReferenceConfig,
) -> anyhow::Result<()> {
    // Acquire exclusive lock for the entire read-modify-write cycle
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config_path)?;
    lock_file.lock()?;

    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
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

    // Atomic write: temp file + rename (while holding lock)
    let tmp_path = config_path.with_extension(format!(
        "toml.tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let serialized = toml::to_string_pretty(&table)?;
    std::fs::write(&tmp_path, &serialized)?;
    if let Err(rename_err) = std::fs::rename(&tmp_path, config_path) {
        if let Err(copy_err) = std::fs::copy(&tmp_path, config_path) {
            let _ = std::fs::remove_file(&tmp_path);
            anyhow::bail!(
                "rename failed ({}), copy fallback failed: {}",
                rename_err,
                copy_err
            );
        }
        let _ = std::fs::remove_file(&tmp_path);
    }

    // Restrict permissions — config may contain paths revealing project structure
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600));
    }

    // lock_file dropped here, releasing exclusive lock
    Ok(())
}

/// Remove a reference from a config file by name (read-modify-write)
pub fn remove_reference_from_config(config_path: &Path, name: &str) -> anyhow::Result<bool> {
    // Acquire exclusive lock for the entire read-modify-write cycle
    let lock_file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(config_path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    lock_file.lock()?;

    let content = std::fs::read_to_string(config_path)?;

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
        // Atomic write: temp file + rename (while holding lock)
        let tmp_path = config_path.with_extension(format!(
            "toml.tmp.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let serialized = toml::to_string_pretty(&table)?;
        std::fs::write(&tmp_path, &serialized)?;
        if let Err(rename_err) = std::fs::rename(&tmp_path, config_path) {
            if let Err(copy_err) = std::fs::copy(&tmp_path, config_path) {
                let _ = std::fs::remove_file(&tmp_path);
                anyhow::bail!(
                    "rename failed ({}), copy fallback failed: {}",
                    rename_err,
                    copy_err
                );
            }
            let _ = std::fs::remove_file(&tmp_path);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600));
        }
    }
    // lock_file dropped here, releasing exclusive lock
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

        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(config.limit, Some(10));
        assert_eq!(config.threshold, Some(0.5));
    }

    #[test]
    fn test_load_missing_file() {
        let dir = TempDir::new().unwrap();
        let config = Config::load_file(&dir.path().join("nonexistent.toml"));
        assert!(config.unwrap().is_none());
    }

    #[test]
    fn test_load_malformed_toml() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(&config_path, "not valid [[[").unwrap();

        let config = Config::load_file(&config_path);
        assert!(config.is_err());
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

        let config = Config::load_file(&config_path).unwrap().unwrap();
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

        let config = Config::load_file(&config_path).unwrap().unwrap();
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

        let config = Config::load_file(&config_path).unwrap().unwrap();
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

        let config = Config::load_file(&config_path).unwrap().unwrap();
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

        let config = Config::load_file(&config_path).unwrap().unwrap();
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
        let config = Config::load_file(&config_path).unwrap().unwrap();
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
        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(config.references.len(), 1);
        assert_eq!(config.references[0].weight, 0.8);
    }

    #[test]
    fn test_weight_clamping() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with out-of-bounds weights
        std::fs::write(
            &config_path,
            r#"
[[reference]]
name = "over"
path = "/refs/over"
weight = 1.5

[[reference]]
name = "under"
path = "/refs/under"
weight = -0.5

[[reference]]
name = "valid"
path = "/refs/valid"
weight = 0.7
"#,
        )
        .unwrap();

        // Load config (should clamp weights)
        let config = Config::load(dir.path());

        // Find the references
        let over_ref = config.references.iter().find(|r| r.name == "over").unwrap();
        let under_ref = config
            .references
            .iter()
            .find(|r| r.name == "under")
            .unwrap();
        let valid_ref = config
            .references
            .iter()
            .find(|r| r.name == "valid")
            .unwrap();

        assert_eq!(
            over_ref.weight, 1.0,
            "Weight > 1.0 should be clamped to 1.0"
        );
        assert_eq!(
            under_ref.weight, 0.0,
            "Weight < 0.0 should be clamped to 0.0"
        );
        assert_eq!(
            valid_ref.weight, 0.7,
            "Valid weight should remain unchanged"
        );
    }

    #[test]
    fn test_threshold_clamping() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with out-of-bounds threshold
        std::fs::write(&config_path, "threshold = 1.5\n").unwrap();

        let config = Config::load(dir.path());
        assert_eq!(config.threshold, Some(1.0));
    }

    #[test]
    fn test_name_boost_clamping() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with out-of-bounds name_boost
        std::fs::write(&config_path, "name_boost = -0.1\n").unwrap();

        let config = Config::load(dir.path());
        assert_eq!(config.name_boost, Some(0.0));
    }

    #[test]
    fn test_limit_clamping_zero() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with limit=0
        std::fs::write(&config_path, "limit = 0\n").unwrap();

        let config = Config::load(dir.path());
        assert_eq!(config.limit, Some(1));
    }

    #[test]
    fn test_limit_clamping_large() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Write config with limit=200
        std::fs::write(&config_path, "limit = 200\n").unwrap();

        let config = Config::load(dir.path());
        assert_eq!(config.limit, Some(100));
    }

    #[test]
    fn test_stale_check_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // stale_check = false disables staleness warnings
        std::fs::write(&config_path, "stale_check = false\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.stale_check, Some(false));

        // stale_check = true (explicit enable, default behavior)
        std::fs::write(&config_path, "stale_check = true\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.stale_check, Some(true));

        // Not set: defaults to None
        std::fs::write(&config_path, "limit = 5\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.stale_check, None);
    }
}
