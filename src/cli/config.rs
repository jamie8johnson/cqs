//! Configuration and project root detection
//!
//! Provides project root detection and config file application.

use std::path::PathBuf;

use super::Cli;

/// Find project root by looking for common markers.
///
/// For Cargo projects, detects workspace roots: if a `Cargo.toml` is found,
/// continues walking up to check if it's inside a workspace. A parent directory
/// with `[workspace]` in its `Cargo.toml` takes precedence as the project root.
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
                // For Cargo projects, check if we're inside a workspace
                if *marker == "Cargo.toml" {
                    if let Some(ws_root) = find_cargo_workspace_root(current) {
                        return ws_root;
                    }
                }
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

/// Walk up from a directory containing Cargo.toml to find a workspace root.
///
/// Returns `Some(path)` if a parent directory has a `Cargo.toml` with `[workspace]`,
/// `None` if no workspace root found (the original dir is the root).
fn find_cargo_workspace_root(from: &std::path::Path) -> Option<PathBuf> {
    let mut candidate = from.parent()?;

    loop {
        let cargo_toml = candidate.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
                if content.contains("[workspace]") {
                    tracing::info!(
                        workspace_root = %candidate.display(),
                        member = %from.display(),
                        "Detected Cargo workspace root"
                    );
                    return Some(candidate.to_path_buf());
                }
            }
        }

        candidate = candidate.parent()?;
    }
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
