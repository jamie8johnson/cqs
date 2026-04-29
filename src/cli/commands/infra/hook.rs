//! `cqs hook` — git-hook integration for watch-mode reconciliation.
//! (#1182 — Layer 1)
//!
//! Closes the **bulk-git missed-event** class: `git checkout`, `merge`,
//! `reset`, `rebase` shift many files at once but inotify (and especially
//! WSL's 9P bridge) routinely drops events under that load. The watch
//! loop never learns the working tree changed — index goes silently
//! stale, eval R@K shifts by 5-25 pp from line-number drift alone, and
//! the user can't tell whether a regression is real or operational.
//!
//! ## Mechanism
//!
//! `cqs hook install` writes three small shell wrappers into
//! `.git/hooks/`:
//!
//! - `post-checkout` — fires after `git checkout` (branch / file)
//! - `post-merge` — fires after `git merge` (or `git pull` fast-forward)
//! - `post-rewrite` — fires after `git rebase` / `git commit --amend`
//!
//! (Git has no `post-reset` hook; that case is left to Layer 2's
//! periodic walk.)
//!
//! Each wrapper runs `cqs hook fire <name> "$@" &` in the background,
//! exits 0 immediately. `cqs hook fire` connects to the running watch
//! daemon's socket and posts a `reconcile` message. If the daemon is
//! down, it touches `.cqs/.dirty` instead — the daemon promotes that
//! marker to a one-shot reconcile on next start.
//!
//! ## Idempotence
//!
//! Hook scripts carry a fixed marker line (`# cqs:hook v1`). The
//! installer skips files that already contain a cqs marker — re-running
//! `cqs hook install` is safe. Files that exist but lack the marker are
//! preserved and the user is warned (don't clobber third-party hooks).

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::cli::find_project_root;

/// Marker line embedded in every cqs-managed hook script. Bump the
/// version suffix when the wrapper template changes — the installer
/// uses the prefix to detect old-version cqs hooks and replace them.
pub(crate) const HOOK_MARKER_PREFIX: &str = "# cqs:hook";
pub(crate) const HOOK_MARKER_CURRENT: &str = "# cqs:hook v1";

/// Hooks the installer manages. Mapped to the `cqs hook fire` first
/// argument when the wrapper runs.
pub(crate) const MANAGED_HOOKS: &[&str] = &["post-checkout", "post-merge", "post-rewrite"];

/// `cqs hook` subcommand surface.
#[derive(clap::Subcommand, Debug)]
pub(crate) enum HookCommand {
    /// Install cqs hooks into `.git/hooks/`. Idempotent — re-running is
    /// safe; already-installed cqs hooks are upgraded in place.
    Install {
        /// Skip writing hooks that don't already exist. Useful in CI
        /// where you want a deterministic set without overwriting any
        /// existing third-party hooks.
        #[arg(long)]
        no_overwrite: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Remove cqs-managed hooks from `.git/hooks/`. Hooks that don't
    /// carry the cqs marker are left alone (they may be third-party).
    Uninstall {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Internal: hook scripts call this. Posts a `reconcile` socket
    /// message; falls back to touching `.cqs/.dirty` if the daemon is
    /// down. Never blocks git — exits 0 within a few hundred ms.
    Fire {
        /// Hook name (`post-checkout`, `post-merge`, `post-rewrite`).
        name: String,
        /// Hook arguments forwarded from git (commit SHAs, etc.).
        /// Captured verbatim for tracing; not used for the reconcile
        /// algorithm itself.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Output as JSON. Default is silent on success, brief stderr on
        /// failure — hooks shouldn't pollute git's output channel.
        #[arg(long)]
        json: bool,
    },
    /// Show installed hooks + daemon connectivity.
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

// PB-V1.30.1-8: `git_dir` and `dirty_marker` carry forward-slash-normalized
// strings, not raw `PathBuf`s, so JSON output matches the rest of the
// surface (per `src/store/types.rs:220`). Backslashes on Windows would
// otherwise leak into reports and break tooling that grep-matches on
// canonical relative paths.
#[derive(Debug, Serialize)]
struct InstallReport {
    git_dir: String,
    installed: Vec<String>,
    upgraded: Vec<String>,
    skipped_existing: Vec<String>,
    skipped_no_overwrite: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UninstallReport {
    git_dir: String,
    removed: Vec<String>,
    skipped_foreign: Vec<String>,
    not_present: Vec<String>,
}

#[derive(Debug, Serialize)]
struct StatusReport {
    git_dir: String,
    installed: Vec<String>,
    foreign: Vec<String>,
    missing: Vec<String>,
    daemon_up: bool,
}

#[derive(Debug, Serialize)]
struct FireReport {
    hook: String,
    args: Vec<String>,
    sent_to_daemon: bool,
    /// Set when the daemon was unreachable. The path of the touched
    /// fallback marker. Normalized to forward-slashes for JSON output.
    dirty_marker: Option<String>,
    /// Daemon error text (if any). Surfaces a connect failure without
    /// failing the hook — the dirty marker is the recovery path.
    daemon_error: Option<String>,
}

/// Top-level dispatch. Each variant handles its own JSON-vs-text output.
pub(crate) fn cmd_hook(subcmd: HookCommand) -> Result<()> {
    match subcmd {
        HookCommand::Install { no_overwrite, json } => cmd_install(no_overwrite, json),
        HookCommand::Uninstall { json } => cmd_uninstall(json),
        HookCommand::Fire { name, args, json } => cmd_fire(name, args, json),
        HookCommand::Status { json } => cmd_status(json),
    }
}

// ─── install ──────────────────────────────────────────────────────────────

fn cmd_install(no_overwrite: bool, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_hook_install", no_overwrite, json).entered();
    let root = find_project_root();
    let git_dir = locate_git_hooks_dir(&root)?;
    std::fs::create_dir_all(&git_dir).with_context(|| format!("create {}", git_dir.display()))?;

    let mut report = InstallReport {
        git_dir: cqs::normalize_path(&git_dir),
        installed: Vec::new(),
        upgraded: Vec::new(),
        skipped_existing: Vec::new(),
        skipped_no_overwrite: Vec::new(),
    };

    for &hook in MANAGED_HOOKS {
        let path = git_dir.join(hook);
        let existing = std::fs::read_to_string(&path).ok();
        match existing {
            None => {
                if no_overwrite {
                    report.skipped_no_overwrite.push(hook.to_string());
                    continue;
                }
                write_hook_script(&path, hook)?;
                report.installed.push(hook.to_string());
            }
            Some(content) => {
                if content.contains(HOOK_MARKER_PREFIX) {
                    // Already a cqs-managed hook (any version) — overwrite
                    // unconditionally so the user gets the current
                    // template on every install.
                    write_hook_script(&path, hook)?;
                    report.upgraded.push(hook.to_string());
                } else {
                    // Foreign hook — never clobber. User's third-party
                    // tooling stays in place; they can chain `cqs hook
                    // fire` from their own script if they want.
                    report.skipped_existing.push(hook.to_string());
                }
            }
        }
    }

    emit(&report, json)?;
    if !report.skipped_existing.is_empty() && !json {
        eprintln!(
            "note: {} pre-existing third-party hook(s) left alone — chain `cqs hook fire {{name}}` from inside if you want both.",
            report.skipped_existing.len()
        );
    }
    Ok(())
}

fn write_hook_script(path: &Path, hook_name: &str) -> Result<()> {
    let body = render_hook_script(hook_name);
    std::fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
    set_executable(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).with_context(|| format!("chmod {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    // Windows git-bash respects the file extension / shebang, no chmod
    // needed.
    Ok(())
}

/// Build the hook wrapper text. Single template — only the hook name
/// embedded in the `cqs hook fire` invocation differs between hooks.
///
/// The wrapper:
///   - Skips silently if `.cqs/` is missing (cqs not used in this
///     working tree).
///   - Skips silently if `cqs` isn't on `$PATH`.
///   - Forks `cqs hook fire <name> "$@" &` so git never waits.
///   - Always exits 0 — a flaky daemon must never break `git checkout`.
fn render_hook_script(hook_name: &str) -> String {
    format!(
        r#"#!/bin/sh
{marker}
# Forwards a {hook_name} event to the running cqs watch daemon. Falls
# back to .cqs/.dirty when the daemon is offline. Never blocks git.

# Bail silently when cqs isn't initialised or installed in this tree.
git_root="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
[ -d "$git_root/.cqs" ] || exit 0
command -v cqs >/dev/null 2>&1 || exit 0

# Background fire-and-forget. `cqs hook fire` is bounded — it has its
# own socket timeout and exits within a few hundred ms even on a wedged
# daemon. The redirect keeps git's output channel clean.
( cqs hook fire {hook_name} "$@" >/dev/null 2>&1 & )

exit 0
"#,
        marker = HOOK_MARKER_CURRENT,
        hook_name = hook_name,
    )
}

// ─── uninstall ────────────────────────────────────────────────────────────

fn cmd_uninstall(json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_hook_uninstall", json).entered();
    let root = find_project_root();
    let git_dir = locate_git_hooks_dir(&root)?;
    let report = do_uninstall(&git_dir)?;
    emit(&report, json)?;
    Ok(())
}

/// TC-HAP-1.30.1-2: path-aware uninstall helper. Body of `cmd_uninstall`
/// extracted so unit tests can drive a tempdir without the global
/// `find_project_root` walk.
fn do_uninstall(git_dir: &Path) -> Result<UninstallReport> {
    let mut report = UninstallReport {
        git_dir: cqs::normalize_path(git_dir),
        removed: Vec::new(),
        skipped_foreign: Vec::new(),
        not_present: Vec::new(),
    };

    for &hook in MANAGED_HOOKS {
        let path = git_dir.join(hook);
        match std::fs::read_to_string(&path) {
            Err(_) => report.not_present.push(hook.to_string()),
            Ok(content) => {
                if content.contains(HOOK_MARKER_PREFIX) {
                    std::fs::remove_file(&path)
                        .with_context(|| format!("remove {}", path.display()))?;
                    report.removed.push(hook.to_string());
                } else {
                    report.skipped_foreign.push(hook.to_string());
                }
            }
        }
    }

    Ok(report)
}

// ─── fire ─────────────────────────────────────────────────────────────────

fn cmd_fire(name: String, args: Vec<String>, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_hook_fire", hook = %name).entered();
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let report = do_fire(&cqs_dir, &name, args, /*try_daemon=*/ true)?;
    emit(&report, json)?;
    Ok(())
}

/// TC-HAP-1.30.1-2: path-aware fire helper. Tries the daemon socket first
/// (when `try_daemon` is true and the platform supports unix sockets),
/// falls back to writing `.cqs/.dirty` atomically (DS-V1.30.1-D5).
fn do_fire(cqs_dir: &Path, name: &str, args: Vec<String>, try_daemon: bool) -> Result<FireReport> {
    let mut report = FireReport {
        hook: name.to_string(),
        args: args.clone(),
        sent_to_daemon: false,
        dirty_marker: None,
        daemon_error: None,
    };

    #[cfg(unix)]
    {
        if try_daemon {
            match cqs::daemon_translate::daemon_reconcile(cqs_dir, Some(name), &args) {
                Ok(_resp) => {
                    report.sent_to_daemon = true;
                    return Ok(report);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "daemon_reconcile failed — touching .cqs/.dirty");
                    report.daemon_error = Some(e.as_message());
                }
            }
        } else {
            report.daemon_error = Some("daemon path skipped (test override)".to_string());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = try_daemon; // unused on non-unix
        report.daemon_error = Some("hook fire requires unix sockets".to_string());
    }

    // Fallback: leave a marker the daemon will pick up on next start.
    let dirty = cqs_dir.join(".dirty");
    std::fs::create_dir_all(cqs_dir).with_context(|| format!("create {}", cqs_dir.display()))?;
    // DS-V1.30.1-D5: stage to .dirty.tmp then atomic_replace so the
    // marker survives a power-cut between write and the next directory
    // sync. atomic_replace fsyncs the tmp before rename and best-effort
    // fsyncs the parent afterwards. The marker is the *only* signal the
    // daemon will see post-reboot, so durability matters more than the
    // empty-file write cost.
    let tmp = cqs_dir.join(".dirty.tmp");
    std::fs::write(&tmp, b"").with_context(|| format!("stage {}", tmp.display()))?;
    cqs::fs::atomic_replace(&tmp, &dirty)
        .with_context(|| format!("promote {} -> {}", tmp.display(), dirty.display()))?;
    report.dirty_marker = Some(cqs::normalize_path(&dirty));
    Ok(report)
}

// ─── status ───────────────────────────────────────────────────────────────

fn cmd_status(json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_hook_status", json).entered();
    let root = find_project_root();
    let git_dir = locate_git_hooks_dir(&root)?;
    let cqs_dir = cqs::resolve_index_dir(&root);

    let mut report = do_hook_status(&git_dir)?;
    #[cfg(unix)]
    {
        report.daemon_up = cqs::daemon_translate::daemon_status(&cqs_dir).is_ok();
    }
    #[cfg(not(unix))]
    {
        let _ = cqs_dir; // unused on non-unix
    }

    emit(&report, json)?;
    Ok(())
}

/// TC-HAP-1.30.1-2: path-aware hook-status helper. Pure file-system
/// classification with no daemon probe — `cmd_status` overlays
/// `daemon_up` separately.
fn do_hook_status(git_dir: &Path) -> Result<StatusReport> {
    let mut report = StatusReport {
        git_dir: cqs::normalize_path(git_dir),
        installed: Vec::new(),
        foreign: Vec::new(),
        missing: Vec::new(),
        daemon_up: false,
    };

    for &hook in MANAGED_HOOKS {
        let path = git_dir.join(hook);
        match std::fs::read_to_string(&path) {
            Err(_) => report.missing.push(hook.to_string()),
            Ok(content) => {
                if content.contains(HOOK_MARKER_PREFIX) {
                    report.installed.push(hook.to_string());
                } else {
                    report.foreign.push(hook.to_string());
                }
            }
        }
    }

    Ok(report)
}

// ─── helpers ──────────────────────────────────────────────────────────────

/// Find `.git/hooks/`. Honors `core.hooksPath` if set (some teams pin a
/// shared hooks dir under their dotfiles repo).
fn locate_git_hooks_dir(root: &Path) -> Result<PathBuf> {
    // Prefer git's own resolution since `core.hooksPath` may rewrite the
    // default. Falls back to `<root>/.git/hooks` if `git` isn't on PATH
    // — the common case in CI containers.
    let cmd = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "hooks"])
        .output();
    if let Ok(out) = cmd {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !raw.is_empty() {
                let p = PathBuf::from(&raw);
                let resolved = if p.is_absolute() { p } else { root.join(p) };
                return Ok(resolved);
            }
        }
    }
    Ok(root.join(".git").join("hooks"))
}

fn emit<T: Serialize>(report: &T, json: bool) -> Result<()> {
    if json {
        crate::cli::json_envelope::emit_json(report)?;
    } else {
        // Text output: dump a compact one-line-per-field view. Hook
        // commands are rare interactive runs; verbosity is fine.
        let pretty = serde_json::to_string_pretty(report)?;
        println!("{pretty}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn render_hook_script_contains_marker_and_name() {
        let body = render_hook_script("post-checkout");
        assert!(body.contains(HOOK_MARKER_CURRENT), "marker missing: {body}");
        assert!(body.contains("post-checkout"), "hook name missing: {body}");
        assert!(body.contains("cqs hook fire post-checkout"));
        assert!(body.starts_with("#!/bin/sh"));
    }

    #[test]
    fn render_hook_script_starts_with_shebang_then_marker() {
        // The shebang has to be *exactly* the first line for the kernel
        // to recognise it. The marker follows on line 2 so detection
        // doesn't depend on script body content.
        let body = render_hook_script("post-merge");
        let first_line = body.lines().next().unwrap_or("");
        let second_line = body.lines().nth(1).unwrap_or("");
        assert_eq!(first_line, "#!/bin/sh");
        assert_eq!(second_line, HOOK_MARKER_CURRENT);
    }

    #[test]
    fn install_writes_three_hooks_into_fresh_repo() {
        let dir = TempDir::new().unwrap();
        // Simulate a real git repo with .git/hooks/.
        let hooks = dir.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();

        // Drive the lower-level write directly so the test doesn't rely
        // on the global `find_project_root` binding to the temp dir.
        for &hook in MANAGED_HOOKS {
            let path = hooks.join(hook);
            write_hook_script(&path, hook).unwrap();
            assert!(path.exists(), "{hook} not written");
            let content = std::fs::read_to_string(&path).unwrap();
            assert!(content.contains(HOOK_MARKER_CURRENT));
            assert!(content.contains(&format!("cqs hook fire {hook}")));
        }
    }

    #[test]
    fn install_skips_foreign_hook_marker_check() {
        // Pre-existing hook without a cqs marker → install_install logic
        // would push to skipped_existing. Reproduce the marker check
        // inline so the assertion is independent of the (file-system-
        // bound) `cmd_install` function.
        let foreign_hook = "#!/bin/sh\necho 'i belong to husky'\n";
        assert!(
            !foreign_hook.contains(HOOK_MARKER_PREFIX),
            "foreign hook must not match cqs marker"
        );

        let cqs_hook = render_hook_script("post-checkout");
        assert!(cqs_hook.contains(HOOK_MARKER_PREFIX));
    }

    #[test]
    fn locate_git_hooks_dir_falls_back_to_dot_git() {
        // No `git` binary in PATH for this test (or the cmd fails for
        // any reason) — the fallback joins `<root>/.git/hooks`. Use a
        // path that doesn't exist so we know we hit the fallback.
        let bogus = PathBuf::from("/nonexistent/cqs-hook-test-tree");
        let resolved = locate_git_hooks_dir(&bogus).unwrap();
        // `git rev-parse` on a non-git tree exits non-zero, so we hit
        // the join branch. Either path shape is acceptable as long as
        // the function doesn't blow up.
        assert!(resolved.to_string_lossy().ends_with("hooks"));
    }

    // ───── TC-HAP-1.30.1-2: cmd_uninstall / cmd_fire / cmd_status ─────────
    //
    // These tests drive the path-aware helpers (`do_uninstall`, `do_fire`,
    // `do_hook_status`) so the body of each cmd_* dispatch is exercised
    // without the global `find_project_root` walk. Marker-classification
    // logic uses `HOOK_MARKER_PREFIX` (per line 45) — `HOOK_MARKER_CURRENT`
    // contains the prefix, so seeding tests with `HOOK_MARKER_CURRENT`
    // exercises the same code path the installer hits.

    #[test]
    fn cmd_uninstall_removes_only_marked_hooks() {
        let tmp = TempDir::new().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();

        // Two cqs-marked hooks + one foreign hook + one missing hook.
        // HOOK_MARKER_CURRENT contains HOOK_MARKER_PREFIX so the
        // matcher at line ~279 fires for both.
        std::fs::write(hooks.join("post-checkout"), HOOK_MARKER_CURRENT).unwrap();
        std::fs::write(hooks.join("post-merge"), HOOK_MARKER_CURRENT).unwrap();
        std::fs::write(hooks.join("post-rewrite"), "#!/bin/sh\necho user").unwrap();

        let report = do_uninstall(&hooks).unwrap();
        assert_eq!(
            report.removed,
            vec!["post-checkout".to_string(), "post-merge".to_string()],
            "only cqs-marked hooks should be removed",
        );
        assert_eq!(
            report.skipped_foreign,
            vec!["post-rewrite".to_string()],
            "foreign hook (no cqs marker) must be left alone",
        );
        assert!(report.not_present.is_empty());

        // Disk reflects the in-memory report.
        assert!(!hooks.join("post-checkout").exists());
        assert!(!hooks.join("post-merge").exists());
        assert!(
            hooks.join("post-rewrite").exists(),
            "foreign hook must survive uninstall",
        );
    }

    #[test]
    fn cmd_uninstall_handles_missing_hooks() {
        // Empty .git/hooks/ — every managed hook should land in not_present
        // with no errors.
        let tmp = TempDir::new().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();

        let report = do_uninstall(&hooks).unwrap();
        assert!(report.removed.is_empty());
        assert!(report.skipped_foreign.is_empty());
        assert_eq!(report.not_present.len(), MANAGED_HOOKS.len());
    }

    #[cfg(unix)]
    #[test]
    fn cmd_fire_writes_dirty_marker_when_daemon_absent() {
        // No socket — daemon unreachable. With try_daemon=false the
        // helper still goes through the fallback path that writes
        // .cqs/.dirty atomically (DS-V1.30.1-D5).
        let tmp = TempDir::new().unwrap();
        let cqs_dir = tmp.path().join(".cqs");

        let report = do_fire(
            &cqs_dir,
            "post-checkout",
            vec![],
            /*try_daemon=*/ false,
        )
        .unwrap();
        assert!(!report.sent_to_daemon);
        // PB-V1.30.1-8: `dirty_marker` is a normalized String now, not PathBuf.
        assert_eq!(
            report.dirty_marker.as_ref().unwrap(),
            &cqs::normalize_path(&cqs_dir.join(".dirty")),
        );
        assert!(cqs_dir.join(".dirty").exists());

        // DS-V1.30.1-D5: the empty-bytes marker must be exactly 0 bytes.
        let meta = std::fs::metadata(cqs_dir.join(".dirty")).unwrap();
        assert_eq!(meta.len(), 0, "marker should be zero-length");

        // atomic_replace consumed the staging tmp file.
        assert!(
            !cqs_dir.join(".dirty.tmp").exists(),
            "staging .dirty.tmp must be cleaned up by the rename",
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn cmd_fire_writes_dirty_marker_on_non_unix() {
        // On non-unix builds the daemon path is compiled out entirely;
        // the dirty marker is the only side effect.
        let tmp = TempDir::new().unwrap();
        let cqs_dir = tmp.path().join(".cqs");

        let report = do_fire(&cqs_dir, "post-checkout", vec![], true).unwrap();
        assert!(!report.sent_to_daemon);
        assert!(cqs_dir.join(".dirty").exists());
        assert_eq!(std::fs::metadata(cqs_dir.join(".dirty")).unwrap().len(), 0);
        assert!(!cqs_dir.join(".dirty.tmp").exists());
    }

    #[test]
    fn cmd_status_classifies_three_hook_states() {
        let tmp = TempDir::new().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        // Installed: marker-bearing (HOOK_MARKER_CURRENT contains PREFIX).
        std::fs::write(hooks.join("post-checkout"), HOOK_MARKER_CURRENT).unwrap();
        // Foreign: file exists but no cqs marker.
        std::fs::write(hooks.join("post-merge"), "#!/bin/sh\nuser stuff").unwrap();
        // Missing: post-rewrite simply isn't on disk.

        let report = do_hook_status(&hooks).unwrap();
        assert_eq!(report.installed, vec!["post-checkout".to_string()]);
        assert_eq!(report.foreign, vec!["post-merge".to_string()]);
        assert_eq!(report.missing, vec!["post-rewrite".to_string()]);
        // do_hook_status doesn't touch daemon_up — that's cmd_status's
        // overlay. Pin the default.
        assert!(!report.daemon_up);
    }
}
