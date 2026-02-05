//! Configuration file support for cqs
//!
//! Config files are loaded in order (later overrides earlier):
//! 1. `~/.config/cqs/config.toml` (user defaults)
//! 2. `.cqs.toml` in project root (project overrides)
//!
//! CLI flags override all config file values.

use serde::Deserialize;
use std::path::Path;

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
        Config {
            limit: other.limit.or(self.limit),
            threshold: other.threshold.or(self.threshold),
            name_boost: other.name_boost.or(self.name_boost),
            quiet: other.quiet.or(self.quiet),
            verbose: other.verbose.or(self.verbose),
        }
    }
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
}
