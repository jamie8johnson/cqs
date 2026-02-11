//! Configuration and project root detection
//!
//! Provides project root detection and config file application.

use std::path::PathBuf;

use super::Cli;

// Default values for CLI options — must match the clap `default_value` attributes in `Cli` (cli/mod.rs)
pub(crate) const DEFAULT_LIMIT: usize = 5;
pub(crate) const DEFAULT_THRESHOLD: f32 = 0.3;
pub(crate) const DEFAULT_NAME_BOOST: f32 = 0.2;
pub(crate) const DEFAULT_NOTE_WEIGHT: f32 = 1.0;

/// Find project root by looking for common markers.
///
/// For Cargo projects, detects workspace roots: if a `Cargo.toml` is found,
/// continues walking up to check if it's inside a workspace. A parent directory
/// with `[workspace]` in its `Cargo.toml` takes precedence as the project root.
pub(crate) fn find_project_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = dunce::canonicalize(&cwd).unwrap_or(cwd);
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
                        let ws_root = dunce::canonicalize(&ws_root).unwrap_or(ws_root);
                        return ws_root;
                    }
                }
                let found = current.to_path_buf();
                return dunce::canonicalize(&found).unwrap_or(found);
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
    if cli.limit == DEFAULT_LIMIT {
        if let Some(limit) = config.limit {
            cli.limit = limit;
        }
    }
    if (cli.threshold - DEFAULT_THRESHOLD).abs() < f32::EPSILON {
        if let Some(threshold) = config.threshold {
            cli.threshold = threshold;
        }
    }
    if (cli.name_boost - DEFAULT_NAME_BOOST).abs() < f32::EPSILON {
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
    if (cli.note_weight - DEFAULT_NOTE_WEIGHT).abs() < f32::EPSILON {
        if let Some(note_weight) = config.note_weight {
            cli.note_weight = note_weight;
        }
    }
    if !cli.note_only {
        if let Some(true) = config.note_only {
            cli.note_only = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Mutex to serialize tests that change the process-wide cwd
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    /// Run a closure with cwd temporarily set to `dir`, restoring afterwards.
    fn with_cwd<F: FnOnce()>(dir: &std::path::Path, f: F) {
        let _guard = CWD_LOCK.lock().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        f();
        std::env::set_current_dir(original).unwrap();
    }

    #[test]
    fn test_find_project_root_with_git() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();

        with_cwd(dir.path(), || {
            let root = find_project_root();
            let expected =
                dunce::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
            assert_eq!(root, expected, "Should find .git as project root marker");
        });
    }

    #[test]
    fn test_find_project_root_with_cargo_toml() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();

        with_cwd(dir.path(), || {
            let root = find_project_root();
            let expected =
                dunce::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
            assert_eq!(root, expected, "Should find Cargo.toml as project root");
        });
    }

    #[test]
    fn test_find_project_root_from_subdirectory() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let subdir = dir.path().join("src").join("deep");
        std::fs::create_dir_all(&subdir).unwrap();

        with_cwd(&subdir, || {
            let root = find_project_root();
            let expected =
                dunce::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
            assert_eq!(
                root, expected,
                "Should walk up to find .git from subdirectory"
            );
        });
    }

    #[test]
    fn test_find_project_root_no_markers() {
        let dir = TempDir::new().unwrap();
        let isolated = dir.path().join("isolated");
        std::fs::create_dir(&isolated).unwrap();

        with_cwd(&isolated, || {
            // Should fall back to CWD without panicking
            let root = find_project_root();
            assert!(root.exists(), "Returned root should exist");
        });
    }

    #[test]
    fn test_find_cargo_workspace_root() {
        let dir = TempDir::new().unwrap();

        // Create workspace root
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crate-a\"]\n",
        )
        .unwrap();

        // Create member crate
        let member = dir.path().join("crate-a");
        std::fs::create_dir(&member).unwrap();
        std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"crate-a\"\n").unwrap();

        with_cwd(&member, || {
            let root = find_project_root();
            let expected =
                dunce::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
            assert_eq!(
                root, expected,
                "Should detect workspace root above member crate"
            );
        });
    }
}
