//! Configuration and project root detection
//!
//! Provides project root detection and config file application.

use std::path::PathBuf;

use clap::parser::ValueSource;
use clap::CommandFactory;

use super::Cli;

// Default values for CLI options.
//
// EX-V1.29-8: These constants are retained for tests that assert against the
// default (see `cli::definitions::test_default_constants`), but the runtime
// SYNC REQUIREMENT between them and clap's `default_value` attributes is
// gone — `apply_config_defaults` now uses `ArgMatches::value_source()` to
// detect "user didn't set this on the CLI". Changing a clap default no
// longer silently breaks config-defaulting; you only need to update the
// test expectation if you care about the compile-time assertion.
pub(crate) const DEFAULT_LIMIT: usize = 5;
/// Minimum cosine similarity threshold for search results.
/// Tuned for BGE-large and E5-base with enrichment. Different embedding models
/// produce different score distributions (BGE-large scores higher than E5-base
/// for the same query-document pair). If using a custom model, you may need to
/// adjust this via the config file `threshold` field or `--threshold` CLI flag.
///
/// EX-V1.29-8: only consumed by tests (`test_default_constants`) after the
/// runtime sync with clap defaults moved to `ValueSource`. Gated on
/// `#[cfg(test)]` so release builds don't warn on dead code.
#[cfg(test)]
pub(crate) const DEFAULT_THRESHOLD: f32 = 0.3;
// DEFAULT_NAME_BOOST lives in cqs::store (single source of truth).
// EX-V1.29-8: only tests reference this re-export after the ValueSource
// refactor; gate on `#[cfg(test)]` to keep release builds warning-free.
#[cfg(test)]
pub(crate) use cqs::store::DEFAULT_NAME_BOOST;

/// EX-V1.29-8: represents a single optional section of the loaded
/// `cqs::config::Config` that can project its fields onto the parsed CLI.
///
/// Adding a new top-level config section becomes a single impl block: a
/// newtype wrapper around the section and a single call to
/// `<Wrapper as ConfigSection>::apply_to_cli(&section, cli, matches)`.
/// Each impl is responsible for checking `ValueSource::DefaultValue` on
/// its own fields so CLI flags always win over config file values.
trait ConfigSection {
    /// Apply this section's values to the parsed `Cli` when the
    /// corresponding CLI argument was not explicitly set by the user.
    /// `matches` carries the clap `ValueSource` for every argument, so
    /// the impl can distinguish "clap default" from "user passed the
    /// default value on the command line".
    fn apply_to_cli(&self, cli: &mut Cli, matches: &clap::ArgMatches);
}

/// Returns `true` if the CLI argument with `id` was left at its clap
/// default (not explicitly set by the user or the environment). Missing
/// args (e.g., optional flags not present at all) also count as "default"
/// so the config file is free to populate them.
fn is_cli_default(matches: &clap::ArgMatches, id: &str) -> bool {
    matches!(
        matches.value_source(id),
        Some(ValueSource::DefaultValue) | None
    )
}

/// Top-level scalar fields on `cqs::config::Config`. Grouping them behind
/// one `ConfigSection` keeps `apply_config_defaults` a single `for_each`
/// at the call site even for the historical flat shape. Future section
/// types (e.g. `[scoring]`, `[embedding]`) each become their own impl.
struct TopLevelScalars<'a>(&'a cqs::config::Config);

impl ConfigSection for TopLevelScalars<'_> {
    fn apply_to_cli(&self, cli: &mut Cli, matches: &clap::ArgMatches) {
        let cfg = self.0;
        if is_cli_default(matches, "limit") {
            if let Some(limit) = cfg.limit {
                cli.limit = limit;
            }
        }
        if is_cli_default(matches, "threshold") {
            if let Some(threshold) = cfg.threshold {
                cli.threshold = threshold;
            }
        }
        if is_cli_default(matches, "name_boost") {
            if let Some(name_boost) = cfg.name_boost {
                cli.name_boost = name_boost;
            }
        }
        // Boolean flags: `value_source == DefaultValue` when the flag is
        // absent (clap stores `false` as its default). We only apply the
        // config value when the user hasn't already flipped the flag.
        if is_cli_default(matches, "quiet") && !cli.quiet {
            if let Some(true) = cfg.quiet {
                cli.quiet = true;
            }
        }
        if is_cli_default(matches, "verbose") && !cli.verbose {
            if let Some(true) = cfg.verbose {
                cli.verbose = true;
            }
        }
        // `stale_check = false` in config file → set `--no-stale-check` on CLI.
        // (Semantics invert because the CLI flag is worded negatively.)
        if is_cli_default(matches, "no_stale_check") && !cli.no_stale_check {
            if let Some(false) = cfg.stale_check {
                cli.no_stale_check = true;
            }
        }
    }
}

/// P3.29: Project-root marker filenames in priority order — first match wins.
///
/// `(filename, label)` — `label` is informational only (used in tracing /
/// future diagnostics), not in the lookup. Adding Maven / Gradle / .NET /
/// Bazel becomes a one-row change here instead of editing the loop body.
///
/// EX-5: Intentionally NOT derived from `LanguageDef` — see comment in
/// `find_project_root` for the orthogonality argument.
static PROJECT_ROOT_MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "rust"),       // Rust (with workspace-root detection)
    ("package.json", "node"),     // Node.js / JavaScript / TypeScript
    ("pyproject.toml", "python"), // Python (modern)
    ("setup.py", "python"),       // Python (legacy)
    ("go.mod", "go"),             // Go
    (".git", "fallback"),         // Universal VCS fallback
];

/// Find project root by looking for common markers.
/// For Cargo projects, detects workspace roots: if a `Cargo.toml` is found,
/// continues walking up to check if it's inside a workspace. A parent directory
/// with `[workspace]` in its `Cargo.toml` takes precedence as the project root.
pub(crate) fn find_project_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = dunce::canonicalize(&cwd).unwrap_or(cwd);
    let mut current = cwd.as_path();
    let mut depth = 0;
    const MAX_DEPTH: usize = 20;

    loop {
        if depth >= MAX_DEPTH {
            tracing::warn!(
                max_depth = MAX_DEPTH,
                "Exceeded max directory walk depth, using CWD"
            );
            break;
        }
        // Check for project markers (build files and VCS root).
        // Marker priority and labels live in `PROJECT_ROOT_MARKERS`.
        for (marker, _label) in PROJECT_ROOT_MARKERS {
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
        depth += 1;
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

/// Apply config file defaults to CLI options. CLI flags always override
/// config values.
///
/// EX-V1.29-8: dispatches through [`ConfigSection`] impls; adding a new
/// top-level section is a single new `impl ConfigSection` + one line in
/// the `sections` array below. The "did user set this explicitly" check
/// uses clap's `ArgMatches::value_source()` rather than
/// `cli.limit == DEFAULT_LIMIT`-style comparisons — the old pattern would
/// treat `cqs -n 5 …` (user explicitly passed the default) as unset and
/// silently override it with the config file, and it forced every
/// `DEFAULT_*` constant to track the clap `default_value` attribute.
pub(super) fn apply_config_defaults(cli: &mut Cli, config: &cqs::config::Config) {
    // Rebuild the `ArgMatches` from the process argv so we can ask clap
    // "was this field user-supplied?" without threading matches through
    // `main.rs -> run_with()` and every test helper. The extra parse is
    // pure clap-side work (a few microseconds on our arg shape) and runs
    // once per process.
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    apply_config_defaults_with_argv(cli, config, &argv);
}

/// Test-friendly variant of `apply_config_defaults` that accepts the argv
/// explicitly. Production callers use `apply_config_defaults`, which reads
/// `std::env::args_os()`; tests inject the same argv they passed to
/// `Cli::try_parse_from` so `ValueSource::DefaultValue` resolves against
/// the test's fake CLI, not the `cargo test ...` invocation.
pub(super) fn apply_config_defaults_with_argv<I, T>(
    cli: &mut Cli,
    config: &cqs::config::Config,
    argv: I,
) where
    I: IntoIterator<Item = T> + Clone,
    T: Into<std::ffi::OsString> + Clone,
{
    let cmd = Cli::command();
    let matches = match cmd.clone().try_get_matches_from(argv) {
        Ok(m) => m,
        Err(e) => {
            // Should never fail — argv was already parsed successfully by
            // `Cli::parse()` upstream. If it somehow does (e.g. racing
            // env changes between the two parses), log and skip so the
            // user sees clap defaults + CLI overrides, never a panic.
            tracing::warn!(
                error = %e,
                "EX-V1.29-8: clap re-parse failed; skipping config defaults"
            );
            return;
        }
    };

    // Each section registers itself by being constructed and appearing in
    // this slice. Order is irrelevant — sections touch disjoint CLI
    // fields.
    let sections: &[&dyn ConfigSection] = &[&TopLevelScalars(config)];
    for section in sections {
        section.apply_to_cli(cli, &matches);
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
