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

/// Typed error for config file operations (EH-15).
/// Used by `add_reference_to_config` and `remove_reference_from_config`.
/// CLI callers convert to `anyhow::Error` at the boundary via the blanket `From`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("Duplicate reference: {0}")]
    DuplicateReference(String),
    #[error("Invalid config format: {0}")]
    InvalidFormat(String),
}

/// Detect if running under Windows Subsystem for Linux (cached)
#[cfg(unix)]
pub fn is_wsl() -> bool {
    static IS_WSL: OnceLock<bool> = OnceLock::new();
    *IS_WSL.get_or_init(|| {
        // Fast path: WSL sets this env var
        if std::env::var_os("WSL_DISTRO_NAME").is_some() {
            return true;
        }
        // Fallback: check /proc/version
        std::fs::read_to_string("/proc/version")
            .map(|v| {
                let lower = v.to_lowercase();
                lower.contains("microsoft") || lower.contains("wsl")
            })
            .unwrap_or(false)
    })
}

/// Non-Unix platforms are never WSL
#[cfg(not(unix))]
pub fn is_wsl() -> bool {
    false
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

/// Returns the default reference weight used for normalization calculations.
/// # Returns
/// A floating-point value of 0.8 representing the standard reference weight.
fn default_ref_weight() -> f32 {
    0.8
}

/// Optional overrides for search scoring parameters.
/// All fields are optional — unset fields fall through to `ScoringConfig::DEFAULT`.
/// Loaded from the `[scoring]` section of `.cqs.toml` or `~/.config/cqs/config.toml`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ScoringOverrides {
    pub name_exact: Option<f32>,
    pub name_contains: Option<f32>,
    pub name_contained_by: Option<f32>,
    pub name_max_overlap: Option<f32>,
    pub note_boost_factor: Option<f32>,
    pub importance_test: Option<f32>,
    pub importance_private: Option<f32>,
    pub parent_boost_per_child: Option<f32>,
    pub parent_boost_cap: Option<f32>,
    pub splade_alpha: Option<f32>,
    /// RRF fusion constant K (default 60.0). Override via config or `CQS_RRF_K` env var.
    pub rrf_k: Option<f32>,
}

/// Configuration options loaded from config files
/// # Example
/// ```toml
/// # ~/.config/cqs/config.toml or .cqs.toml
/// limit = 10          # Default result limit
/// threshold = 0.3     # Minimum similarity score
/// name_boost = 0.2    # Weight for name matching
/// quiet = false       # Suppress progress output
/// verbose = false     # Enable verbose logging
/// stale_check = false # Disable per-file staleness checks
/// [[reference]]
/// name = "tokio"
/// path = "/home/user/.local/share/cqs/refs/tokio"
/// source = "/home/user/code/tokio"
/// weight = 0.8
/// ```
#[derive(Default, Deserialize)]
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
    /// Disable staleness checks (useful on NFS or slow filesystems)
    pub stale_check: Option<bool>,
    /// HNSW search width (higher = more accurate but slower, default 100)
    pub ef_search: Option<usize>,
    /// LLM model name (overridden by CQS_LLM_MODEL env var)
    pub llm_model: Option<String>,
    /// LLM API base URL (overridden by CQS_API_BASE env var)
    pub llm_api_base: Option<String>,
    /// LLM max tokens for summary generation (overridden by CQS_LLM_MAX_TOKENS env var)
    pub llm_max_tokens: Option<u32>,
    /// LLM max tokens for HyDE query predictions (overridden by CQS_HYDE_MAX_TOKENS env var)
    pub llm_hyde_max_tokens: Option<u32>,
    /// Embedding model configuration
    #[serde(default)]
    pub embedding: Option<crate::embedder::EmbeddingConfig>,
    /// Reranker model repository (overridden by CQS_RERANKER_MODEL env var)
    pub reranker_model: Option<String>,
    /// Reranker max input length in tokens (overridden by CQS_RERANKER_MAX_LENGTH env var)
    pub reranker_max_length: Option<usize>,
    /// Scoring parameter overrides (optional `[scoring]` section)
    #[serde(default)]
    pub scoring: Option<ScoringOverrides>,
    /// Reference indexes for multi-index search
    #[serde(default, rename = "reference")]
    pub references: Vec<ReferenceConfig>,
}

/// SEC-3: Redact a URL for logging — masks credentials (user:pass@host) and
/// returns only the scheme + host. Returns "[redacted]" for unparseable URLs.
fn redact_url(url: &str) -> String {
    // Strip credentials if present (scheme://user:pass@host/path -> scheme://host/path)
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        let host_part = if let Some(at_pos) = after_scheme.find('@') {
            &after_scheme[at_pos + 1..]
        } else {
            after_scheme
        };
        // Keep only scheme + host (strip path)
        let host_only = host_part.split('/').next().unwrap_or(host_part);
        format!("{}://{}/...", &url[..scheme_end], host_only)
    } else {
        "[redacted]".to_string()
    }
}

/// Custom Debug impl for Config that redacts llm_api_base to avoid logging credentials.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("limit", &self.limit)
            .field("threshold", &self.threshold)
            .field("name_boost", &self.name_boost)
            .field("quiet", &self.quiet)
            .field("verbose", &self.verbose)
            .field("stale_check", &self.stale_check)
            .field("ef_search", &self.ef_search)
            .field("llm_model", &self.llm_model)
            .field(
                "llm_api_base",
                &self.llm_api_base.as_deref().map(redact_url),
            )
            .field("llm_max_tokens", &self.llm_max_tokens)
            .field("llm_hyde_max_tokens", &self.llm_hyde_max_tokens)
            .field("embedding", &self.embedding)
            .field("reranker_model", &self.reranker_model)
            .field("reranker_max_length", &self.reranker_max_length)
            .field("scoring", &self.scoring)
            .field("references", &self.references)
            .finish()
    }
}

/// Clamp f32 config value to valid range and warn if out of bounds.
/// TC-48: Also catches NaN (which silently passes all comparisons as false)
/// and clamps it to `min`, preventing silent data loss in downstream filters.
fn clamp_config_f32(value: &mut f32, name: &str, min: f32, max: f32) {
    if value.is_nan() {
        tracing::warn!(field = name, "Config value is NaN, clamping to min");
        *value = min;
        return;
    }
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
    /// Called once from `load()` after merging user + project configs.
    /// Adding a new field? Add its clamping here — this is the single
    /// validation choke point.
    fn validate(&mut self) {
        // SHL-28: Cap reference count. Each reference opens a separate SQLite DB +
        // HNSW index, consuming ~50-100MB RAM. 20 references = ~1-2GB baseline memory.
        // If you need more, consider consolidating related libraries into fewer indexes.
        const MAX_REFERENCES: usize = 20;
        if self.references.len() > MAX_REFERENCES {
            eprintln!(
                "Warning: {} references configured, exceeding limit of {}. \
                 Only the first {} will be loaded. Each reference consumes ~50-100MB RAM.",
                self.references.len(),
                MAX_REFERENCES,
                MAX_REFERENCES
            );
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

        // SEC-4 + SEC-NEW-1: Warn if reference `path` OR `source` is outside
        // project and home directories. SEC-4 covered `r.path` only; SEC-NEW-1
        // (v1.22.0 audit) found that `r.source` was never validated, so a
        // malicious checked-in `.cqs.toml` with `source = "/home/user/.ssh"`
        // would cause `cqs ref update` to index arbitrary files into the
        // reference DB (data exfiltration).
        let home = dirs::home_dir();
        let cwd = std::env::current_dir().ok();
        for r in &self.references {
            // Check both path and source (if present)
            let paths_to_check: Vec<(&str, &std::path::Path)> = {
                let mut v = vec![("path", r.path.as_path())];
                if let Some(ref src) = r.source {
                    v.push(("source", src.as_path()));
                }
                v
            };
            for (field, p) in paths_to_check {
                if let Ok(canonical) = p.canonicalize() {
                    let in_home = home.as_ref().is_some_and(|h| canonical.starts_with(h));
                    let in_project = cwd.as_ref().is_some_and(|p| canonical.starts_with(p));
                    let in_cqs_dir = canonical.components().any(|c| c.as_os_str() == ".cqs");
                    if !in_home && !in_project && !in_cqs_dir {
                        tracing::warn!(
                            name = %r.name,
                            field,
                            path = %canonical.display(),
                            "Reference {field} is outside project and home directories — \
                             a malicious .cqs.toml could use this to index arbitrary files. \
                             Verify the source is intentional."
                        );
                    }
                }
            }
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
        if let Some(ref mut ef) = self.ef_search {
            clamp_config_usize(ef, "ef_search", 10, 1000);
        }
        // SHL-26: Models like Claude support up to 64k output tokens; 4096 was too restrictive.
        if let Some(ref mut mt) = self.llm_max_tokens {
            if *mt == 0 || *mt > 32768 {
                tracing::warn!(
                    field = "llm_max_tokens",
                    value = *mt,
                    "Config value out of bounds, clamping to [1, 32768]"
                );
                *mt = (*mt).clamp(1, 32768);
            }
        }
        if let Some(ref mut s) = self.scoring {
            if let Some(ref mut v) = s.name_exact {
                clamp_config_f32(v, "scoring.name_exact", 0.0, 2.0);
            }
            if let Some(ref mut v) = s.name_contains {
                clamp_config_f32(v, "scoring.name_contains", 0.0, 2.0);
            }
            if let Some(ref mut v) = s.name_contained_by {
                clamp_config_f32(v, "scoring.name_contained_by", 0.0, 2.0);
            }
            if let Some(ref mut v) = s.name_max_overlap {
                clamp_config_f32(v, "scoring.name_max_overlap", 0.0, 2.0);
            }
            if let Some(ref mut v) = s.note_boost_factor {
                clamp_config_f32(v, "scoring.note_boost_factor", 0.0, 1.0);
            }
            if let Some(ref mut v) = s.importance_test {
                clamp_config_f32(v, "scoring.importance_test", 0.0, 1.0);
            }
            if let Some(ref mut v) = s.importance_private {
                clamp_config_f32(v, "scoring.importance_private", 0.0, 1.0);
            }
            if let Some(ref mut v) = s.parent_boost_per_child {
                clamp_config_f32(v, "scoring.parent_boost_per_child", 0.0, 0.5);
            }
            if let Some(ref mut v) = s.parent_boost_cap {
                clamp_config_f32(v, "scoring.parent_boost_cap", 1.0, 2.0);
            }
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
            // Skip permission check on WSL (NTFS always reports 777) or Windows drive mounts.
            // SEC-13: Use `/mnt/[a-z]/` pattern to match WSL drive mounts specifically,
            // not arbitrary /mnt/ subdirectories (e.g., /mnt/data/ on native Linux).
            let is_wsl_mount = is_wsl()
                || path.to_str().is_some_and(|p| {
                    p.len() >= 7
                        && p.starts_with("/mnt/")
                        && p.as_bytes()[5].is_ascii_lowercase()
                        && p.as_bytes()[6] == b'/'
                });
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
            stale_check: other.stale_check.or(self.stale_check),
            ef_search: other.ef_search.or(self.ef_search),
            llm_model: other.llm_model.or(self.llm_model),
            llm_api_base: other.llm_api_base.or(self.llm_api_base),
            llm_max_tokens: other.llm_max_tokens.or(self.llm_max_tokens),
            llm_hyde_max_tokens: other.llm_hyde_max_tokens.or(self.llm_hyde_max_tokens),
            embedding: other.embedding.or(self.embedding),
            reranker_model: other.reranker_model.or(self.reranker_model),
            reranker_max_length: other.reranker_max_length.or(self.reranker_max_length),
            scoring: other.scoring.or(self.scoring),
            references: refs,
        }
    }
}

/// Add a reference to a config file (read-modify-write, preserves unknown fields)
pub fn add_reference_to_config(
    config_path: &Path,
    ref_config: &ReferenceConfig,
) -> Result<(), ConfigError> {
    // Acquire exclusive lock for the entire read-modify-write cycle.
    // Read through the locked fd to avoid TOCTOU between lock and read.
    //
    // NOTE: File locking is advisory only on WSL over 9P (DrvFs/NTFS mounts).
    // This prevents concurrent cqs processes from corrupting the config,
    // but cannot protect against external Windows process modifications.
    let mut lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config_path)?;
    lock_file.lock()?;

    let mut content = String::new();
    use std::io::Read;
    lock_file.read_to_string(&mut content)?;
    let mut table: toml::Table = if content.is_empty() {
        toml::Table::new()
    } else {
        content.parse()?
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
            return Err(ConfigError::DuplicateReference(format!(
                "Reference '{}' already exists in {}",
                ref_config.name,
                config_path.display()
            )));
        }
    }

    let ref_value = toml::Value::try_from(ref_config)?;

    let refs = table
        .entry("reference")
        .or_insert_with(|| toml::Value::Array(vec![]));

    match refs {
        toml::Value::Array(arr) => arr.push(ref_value),
        _ => {
            return Err(ConfigError::InvalidFormat(
                "'reference' in config is not an array".to_string(),
            ))
        }
    }

    // Atomic write: temp file + rename (while holding lock)
    let suffix = crate::temp_suffix();
    let tmp_path = config_path.with_extension(format!("toml.{:016x}.tmp", suffix));
    let serialized = toml::to_string_pretty(&table)?;
    // SEC-1: Write with mode 0o600 from creation so file is never world-readable
    {
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)?;
            f.write_all(serialized.as_bytes())?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&tmp_path, &serialized)?;
        }
    }

    // atomic_replace: fsync tmp, rename with EXDEV fallback, fsync parent dir.
    // SEC-1: the tmp file was opened with mode(0o600), which fs::copy
    // preserves into the xdev fallback destination.
    crate::fs::atomic_replace(&tmp_path, config_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        ConfigError::Io(e)
    })?;

    // lock_file dropped here, releasing exclusive lock
    Ok(())
}

/// Remove a reference from a config file by name (read-modify-write)
pub fn remove_reference_from_config(config_path: &Path, name: &str) -> Result<bool, ConfigError> {
    // Acquire exclusive lock for the entire read-modify-write cycle.
    // Read through the locked fd to avoid TOCTOU between lock and read.
    //
    // NOTE: File locking is advisory only on WSL over 9P (DrvFs/NTFS mounts).
    // This prevents concurrent cqs processes from corrupting the config,
    // but cannot protect against external Windows process modifications.
    let mut lock_file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(config_path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(ConfigError::Io(e)),
    };
    lock_file.lock()?;

    let mut content = String::new();
    use std::io::Read;
    lock_file.read_to_string(&mut content)?;

    let mut table: toml::Table = content.parse()?;

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
        let suffix = crate::temp_suffix();
        let tmp_path = config_path.with_extension(format!("toml.{:016x}.tmp", suffix));
        let serialized = toml::to_string_pretty(&table)?;
        // SEC-1: Write with mode 0o600 from creation so file is never world-readable
        {
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&tmp_path)?;
                f.write_all(serialized.as_bytes())?;
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&tmp_path, &serialized)?;
            }
        }

        // atomic_replace: fsync tmp, rename with EXDEV fallback, fsync parent dir.
        crate::fs::atomic_replace(&tmp_path, config_path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            ConfigError::Io(e)
        })?;
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

    #[test]
    fn test_llm_config_fields() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(
            &config_path,
            r#"
llm_model = "claude-sonnet-4-20250514"
llm_api_base = "https://custom.api/v1"
llm_max_tokens = 200
"#,
        )
        .unwrap();

        let config = Config::load_file(&config_path).unwrap().unwrap();
        assert_eq!(
            config.llm_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
        assert_eq!(
            config.llm_api_base.as_deref(),
            Some("https://custom.api/v1")
        );
        assert_eq!(config.llm_max_tokens, Some(200));
    }

    #[test]
    fn test_llm_max_tokens_clamping() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");

        // Over max (cap is 32768)
        std::fs::write(&config_path, "llm_max_tokens = 99999\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.llm_max_tokens, Some(32768));

        // Zero
        std::fs::write(&config_path, "llm_max_tokens = 0\n").unwrap();
        let config = Config::load(dir.path());
        assert_eq!(config.llm_max_tokens, Some(1));
    }

    #[test]
    fn test_llm_config_merge() {
        let base = Config {
            llm_model: Some("base-model".into()),
            llm_max_tokens: Some(100),
            ..Default::default()
        };
        let override_cfg = Config {
            llm_model: Some("override-model".into()),
            llm_api_base: Some("https://override/v1".into()),
            ..Default::default()
        };

        let merged = base.override_with(override_cfg);
        assert_eq!(merged.llm_model.as_deref(), Some("override-model"));
        assert_eq!(merged.llm_api_base.as_deref(), Some("https://override/v1"));
        assert_eq!(merged.llm_max_tokens, Some(100)); // from base, not overridden
    }

    #[test]
    fn test_embedding_config_preset() {
        let toml = r#"
        [embedding]
        model = "bge-large"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.embedding.as_ref().unwrap().model, "bge-large");
    }

    #[test]
    fn test_embedding_config_custom() {
        let toml = r#"
        [embedding]
        model = "custom"
        repo = "my-org/my-model"
        dim = 384
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let emb = config.embedding.as_ref().unwrap();
        assert_eq!(emb.model, "custom");
        assert_eq!(emb.dim, Some(384));
    }

    #[test]
    fn test_no_embedding_section() {
        let toml = "limit = 10\n";
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.embedding.is_none());
    }

    // ===== TC-36/TC-48: NaN threshold clamped to min =====

    #[test]
    fn tc36_nan_threshold_clamped_to_min() {
        // TC-48: NaN is now caught by clamp_config_f32 and clamped to min (0.0
        // for threshold). Previously NaN silently passed through because all NaN
        // comparisons return false.
        let mut config = Config {
            threshold: Some(f32::NAN),
            ..Default::default()
        };
        config.validate();
        // NaN is now caught and clamped to min (0.0 for threshold)
        assert_eq!(config.threshold, Some(0.0));
    }

    #[test]
    fn tc48_nan_name_boost_clamped_to_min() {
        let mut config = Config {
            name_boost: Some(f32::NAN),
            ..Default::default()
        };
        config.validate();
        assert_eq!(
            config.name_boost,
            Some(0.0),
            "NaN name_boost should be clamped to 0.0"
        );
    }

    // ===== TC-37: Edge case dimension metadata =====

    #[test]
    fn tc37_embedding_config_empty_string_model() {
        // Empty model name should fall back to default via from_preset returning None
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let embedding_cfg = crate::embedder::EmbeddingConfig {
            model: String::new(),
            repo: None,
            onnx_path: None,
            tokenizer_path: None,
            dim: None,
            max_seq_length: None,
            query_prefix: None,
            doc_prefix: None,
        };
        let cfg = crate::embedder::ModelConfig::resolve(None, Some(&embedding_cfg));
        assert_eq!(
            cfg.name, "bge-large",
            "Empty model string should fall back to default"
        );
    }

    // ===== TC-39: embedding section tokenizer_path parsing =====

    #[test]
    fn tc39_embedding_tokenizer_path_parsed() {
        let toml = r#"
        [embedding]
        model = "custom"
        repo = "org/model"
        dim = 384
        tokenizer_path = "custom.json"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let emb = config.embedding.as_ref().unwrap();
        assert_eq!(
            emb.tokenizer_path.as_deref(),
            Some("custom.json"),
            "tokenizer_path should be captured from config"
        );
    }

    #[test]
    fn tc39_embedding_unknown_field_ignored() {
        // Unknown fields like `tokenizer` (without `_path`) should be ignored by serde
        let toml = r#"
        [embedding]
        model = "e5-base"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let emb = config.embedding.as_ref().unwrap();
        assert!(
            emb.tokenizer_path.is_none(),
            "tokenizer_path should be None when not specified"
        );
    }

    // ===== RX-2: ScoringOverrides config parsing =====

    #[test]
    fn test_scoring_overrides_parsed() {
        let toml = r#"
        [scoring]
        name_exact = 0.9
        note_boost_factor = 0.25
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let s = config.scoring.as_ref().unwrap();
        assert!((s.name_exact.unwrap() - 0.9).abs() < f32::EPSILON);
        assert!((s.note_boost_factor.unwrap() - 0.25).abs() < f32::EPSILON);
        assert!(s.name_contains.is_none());
    }

    #[test]
    fn test_scoring_overrides_absent() {
        let toml = "limit = 5\n";
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.scoring.is_none());
    }

    #[test]
    fn test_scoring_overrides_clamped() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join(".cqs.toml");
        std::fs::write(
            &config_path,
            "[scoring]\nname_exact = 5.0\nimportance_test = -1.0\n",
        )
        .unwrap();
        let config = Config::load(dir.path());
        let s = config.scoring.as_ref().unwrap();
        assert!(
            (s.name_exact.unwrap() - 2.0).abs() < f32::EPSILON,
            "name_exact clamped to 2.0"
        );
        assert!(
            (s.importance_test.unwrap() - 0.0).abs() < f32::EPSILON,
            "importance_test clamped to 0.0"
        );
    }

    #[test]
    fn test_scoring_overrides_merge() {
        let base = Config {
            scoring: Some(ScoringOverrides {
                name_exact: Some(0.9),
                ..Default::default()
            }),
            ..Default::default()
        };
        let over = Config {
            scoring: Some(ScoringOverrides {
                note_boost_factor: Some(0.3),
                ..Default::default()
            }),
            ..Default::default()
        };
        // Project overrides user — whole scoring section replaced
        let merged = base.override_with(over);
        let s = merged.scoring.unwrap();
        assert!((s.note_boost_factor.unwrap() - 0.3).abs() < f32::EPSILON);
        // base scoring was replaced, not field-merged
        assert!(s.name_exact.is_none());
    }
}
