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
        user_config.merge(project_config)
    }

    /// Load configuration from a specific file
    fn load_file(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        toml::from_str(&content).ok()
    }

    /// Merge two configs (other overrides self where present)
    fn merge(self, other: Self) -> Self {
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

        std::fs::write(
            &config_path,
            r#"
            limit = 10
            threshold = 0.5
            name_boost = 0.3
            quiet = true
            "#,
        )
        .unwrap();

        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.limit, Some(10));
        assert_eq!(config.threshold, Some(0.5));
        assert_eq!(config.name_boost, Some(0.3));
        assert_eq!(config.quiet, Some(true));
        assert_eq!(config.verbose, None);
    }

    #[test]
    fn test_load_partial_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        std::fs::write(&config_path, "limit = 5\n").unwrap();

        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.limit, Some(5));
        assert_eq!(config.threshold, None);
    }

    #[test]
    fn test_load_empty_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        std::fs::write(&config_path, "").unwrap();

        let config = Config::load_file(&config_path).unwrap();
        assert_eq!(config.limit, None);
    }

    #[test]
    fn test_load_missing_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("nonexistent.toml");

        let config = Config::load_file(&config_path);
        assert!(config.is_none());
    }

    #[test]
    fn test_load_malformed_toml() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        std::fs::write(&config_path, "this is not valid toml [[[").unwrap();

        let config = Config::load_file(&config_path);
        assert!(config.is_none()); // Silently returns None for invalid TOML
    }

    #[test]
    fn test_load_wrong_types() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // limit should be usize, not string
        std::fs::write(&config_path, "limit = \"not a number\"\n").unwrap();

        let config = Config::load_file(&config_path);
        assert!(config.is_none()); // Type mismatch returns None
    }

    #[test]
    fn test_merge_override() {
        let base = Config {
            limit: Some(10),
            threshold: Some(0.5),
            name_boost: None,
            quiet: Some(false),
            verbose: None,
        };

        let override_config = Config {
            limit: Some(20),
            threshold: None,
            name_boost: Some(0.3),
            quiet: None,
            verbose: Some(true),
        };

        let merged = base.merge(override_config);
        assert_eq!(merged.limit, Some(20)); // overridden
        assert_eq!(merged.threshold, Some(0.5)); // kept from base
        assert_eq!(merged.name_boost, Some(0.3)); // new from override
        assert_eq!(merged.quiet, Some(false)); // kept from base
        assert_eq!(merged.verbose, Some(true)); // new from override
    }

    #[test]
    fn test_load_project_overrides_user() {
        let dir = TempDir::new().unwrap();
        let project_config = dir.path().join(".cqs.toml");

        std::fs::write(&project_config, "limit = 15\nquiet = true\n").unwrap();

        // Load from project root (user config may not exist)
        let config = Config::load(dir.path());

        // Project config should be loaded
        assert_eq!(config.limit, Some(15));
        assert_eq!(config.quiet, Some(true));
    }
}
