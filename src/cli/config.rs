//! Configuration and project root detection
//!
//! Provides project root detection and config file application.

use std::path::PathBuf;

use super::Cli;

/// Find project root by looking for common markers
pub(crate) fn find_project_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut current = cwd.as_path();

    loop {
        // Check for project markers (build files and VCS root)
        // Listed in priority order: if multiple exist, first match wins
        let markers = [
            "Cargo.toml",     // Rust
            "package.json",   // Node.js
            "pyproject.toml", // Python (modern)
            "setup.py",       // Python (legacy)
            "go.mod",         // Go
            ".git",           // Git repository root (fallback)
        ];

        for marker in &markers {
            if current.join(marker).exists() {
                return current.to_path_buf();
            }
        }

        // Move up
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    // Fall back to CWD with warning
    tracing::warn!("No project root found, using current directory");
    cwd
}

/// Apply config file defaults to CLI options
/// CLI flags always override config values
pub(super) fn apply_config_defaults(cli: &mut Cli, config: &cqs::config::Config) {
    // Only apply config if CLI has default values
    // (we can't detect if user explicitly passed the default, so this is imperfect)
    if cli.limit == 5 {
        if let Some(limit) = config.limit {
            cli.limit = limit;
        }
    }
    if (cli.threshold - 0.3).abs() < f32::EPSILON {
        if let Some(threshold) = config.threshold {
            cli.threshold = threshold;
        }
    }
    if (cli.name_boost - 0.2).abs() < f32::EPSILON {
        if let Some(name_boost) = config.name_boost {
            cli.name_boost = name_boost;
        }
    }
    if !cli.quiet {
        if let Some(true) = config.quiet {
            cli.quiet = true;
        }
    }
    if !cli.verbose {
        if let Some(true) = config.verbose {
            cli.verbose = true;
        }
    }
}
