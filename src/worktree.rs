//! Worktree-to-main-index discovery — see #1254.
//!
//! When `cqs` is invoked from inside a `git worktree`, the worktree
//! has no `.cqs/` of its own (`git worktree add` doesn't copy the
//! directory). Pre-#1254 every cqs command in the worktree errored
//! with "No cqs index found", which led agents to fall back to raw
//! `Read`/`grep` on absolute paths under the parent tree — causing
//! the worktree-leakage class of bugs documented in
//! `feedback_agent_worktrees.md`.
//!
//! This module's [`resolve_main_project_dir`] turns a worktree root
//! into the corresponding main project root by parsing the
//! `gitdir: <path>` line in the worktree's `.git` *file*, then
//! reading the `commondir` file at that gitdir to discover the
//! canonical `.git/` directory. The parent of that canonical
//! `.git/` is the main project root.
//!
//! Callers ([`crate::resolve_index_dir`]) attempt the worktree's own
//! `.cqs/` first; only when it doesn't exist do they fall back to
//! the main project's `.cqs/` returned here. The `worktree_stale`
//! signal in the JSON `_meta` block lets consuming agents know the
//! results came from main's snapshot, not the worktree's branch.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Cap on `.git` worktree file reads. A `.git` link file is normally
/// ~30 bytes (just `gitdir: <path>\n`); 4 KiB rejects pathological
/// content while leaving plenty of headroom for unusual but legitimate
/// layouts. Mirrors the bounded-read pattern used in `slot/mod.rs`.
const MAX_GIT_FILE_BYTES: u64 = 4 * 1024;

/// Detect a git worktree at `dir` and return the main project's root
/// directory if so. Returns `None` for the non-worktree happy path
/// (a regular repo, a non-git directory, or any I/O error).
///
/// **Detection contract:**
///
/// 1. `<dir>/.git` exists and is a *file* (not a directory). A
///    directory means `dir` is a normal repo, not a worktree.
/// 2. The `.git` file's first line is `gitdir: <path>` (per `git`'s
///    on-disk format). The path is typically absolute and points at
///    `<main>/.git/worktrees/<name>/`.
/// 3. `<gitdir>/commondir` exists. Its content is a relative path
///    from the gitdir back to the canonical `.git/` directory
///    (typically `../..` for the standard layout).
/// 4. `<gitdir>/<commondir>` resolves; its parent is the main project
///    root.
///
/// Any deviation (missing files, malformed `gitdir:` line, broken
/// `commondir` link) returns `None`. We never panic on a malformed
/// worktree — caller falls through to its existing "no index"
/// handling.
pub fn resolve_main_project_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = dir.join(".git");
    let metadata = std::fs::metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        // Regular repo — not a worktree. Caller's existing
        // `find_project_root()` already handles this.
        return None;
    }

    // Read `.git` file → "gitdir: <path>" with a bounded cap to defend
    // against a hostile or accidentally-huge file at this path
    // (RB-V1.33-2). 4 KiB is far above realistic content (~30 bytes).
    let mut raw = String::new();
    std::fs::File::open(&dot_git)
        .ok()?
        .take(MAX_GIT_FILE_BYTES)
        .read_to_string(&mut raw)
        .ok()?;
    let gitdir_path_str = raw
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    if gitdir_path_str.is_empty() {
        return None;
    }
    let gitdir = PathBuf::from(gitdir_path_str);
    // The gitdir line is typically absolute on Linux/macOS; on
    // Windows-native it may be a relative POSIX-style path. Resolve
    // relative paths against the worktree dir (parent of `.git`).
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        dir.join(&gitdir)
    };

    // Read `<gitdir>/commondir` → relative path back to canonical `.git/`
    let commondir_file = gitdir.join("commondir");
    let commondir_relative = std::fs::read_to_string(&commondir_file).ok()?;
    let commondir_relative = commondir_relative.trim();
    if commondir_relative.is_empty() {
        return None;
    }

    // Resolve `<gitdir>/<commondir_relative>` → canonical `.git/`.
    // Use `dunce::canonicalize` so Windows returns the non-`\\?\`-prefixed
    // form — downstream `WorktreeUseMain.main_root` is surfaced via JSON
    // envelopes and string-compared by agents (PB-V1.33-3).
    let canonical_git = gitdir.join(commondir_relative);
    let canonical_git = dunce::canonicalize(&canonical_git).ok()?;

    // Canonical `.git/`'s parent = main project root.
    let main_root = canonical_git.parent()?.to_path_buf();
    Some(main_root)
}

/// Convenience wrapper: resolve `dir` to the main project's `.cqs/`
/// directory if `dir` is a worktree without its own `.cqs/`. Returns
/// `None` when `dir` is not a worktree, when `dir` has its own
/// `.cqs/` (caller should use that), or when the main project has
/// no `.cqs/` either.
///
/// Detection result captured in [`MainIndexLookup`] so callers can
/// distinguish "not a worktree" from "worktree but main has no
/// index" — the second case wants a clearer error message naming
/// both paths.
pub fn lookup_main_cqs_dir(dir: &Path) -> MainIndexLookup {
    // PB-V1.36-3 / P2-12: canonicalize `dir` once up-front so the returned
    // `worktree_root` matches `find_project_root()` byte-for-byte on
    // case-insensitive filesystems (Windows NTFS, macOS HFS+/APFS default).
    // resolve_main_project_dir already canonicalizes its result; without
    // this the worktree side stays raw, so downstream string-equality
    // checks against find_project_root output (which IS canonicalized via
    // dunce) report mismatches even when the paths refer to the same dir.
    // That's the #1254-class leakage origin.
    let dir_canonical = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let own_cqs = dir_canonical.join(crate::INDEX_DIR);
    // PB-V1.36-10: is_dir() rather than exists() — a stray `.cqs` *file* (a
    // mistaken `touch .cqs`, or a packaged tarball with the wrong entry)
    // shouldn't be treated as an index dir. Downstream code would otherwise
    // try to open `<file>/index.db` and surface a confusing
    // "is not a directory" instead of "no index here, fall through to main".
    if own_cqs.is_dir() {
        return MainIndexLookup::OwnIndex { path: own_cqs };
    }
    let Some(main_root) = resolve_main_project_dir(&dir_canonical) else {
        return MainIndexLookup::NotWorktree;
    };
    let main_cqs = main_root.join(crate::INDEX_DIR);
    if main_cqs.is_dir() {
        MainIndexLookup::WorktreeUseMain {
            worktree_root: dir_canonical,
            main_root,
            main_cqs,
        }
    } else {
        MainIndexLookup::WorktreeMainEmpty {
            worktree_root: dir_canonical,
            main_root,
        }
    }
}

/// Result of [`lookup_main_cqs_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainIndexLookup {
    /// `dir` is not a worktree (it's a regular project, or `.git`
    /// can't be read). Caller uses its own existing logic — this
    /// helper made no decision.
    NotWorktree,
    /// `dir` has its own `.cqs/` — use it directly. The worktree
    /// fallback path is irrelevant.
    OwnIndex { path: PathBuf },
    /// `dir` is a worktree without its own `.cqs/`, but the main
    /// project has one. Caller should serve queries against
    /// `main_cqs` and tag responses with `worktree_stale: true`.
    WorktreeUseMain {
        worktree_root: PathBuf,
        main_root: PathBuf,
        main_cqs: PathBuf,
    },
    /// `dir` is a worktree but neither it nor the main project has
    /// a `.cqs/`. The caller's "no index" error should name both
    /// paths so the operator knows which one to populate.
    WorktreeMainEmpty {
        worktree_root: PathBuf,
        main_root: PathBuf,
    },
}

/// Read the worktree's name (the directory under
/// `<main>/.git/worktrees/<name>/`) for `_meta.worktree_name` in
/// JSON envelopes. Falls back to the worktree dir's basename when
/// the name can't be derived from `.git`'s `gitdir:` line.
pub fn worktree_name(dir: &Path) -> Option<String> {
    let dot_git = dir.join(".git");
    if std::fs::metadata(&dot_git).ok()?.is_dir() {
        return None;
    }
    // Bounded read — see RB-V1.33-2 / `MAX_GIT_FILE_BYTES`.
    let mut raw = String::new();
    std::fs::File::open(&dot_git)
        .ok()?
        .take(MAX_GIT_FILE_BYTES)
        .read_to_string(&mut raw)
        .ok()?;
    let gitdir_path_str = raw
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    let gitdir = PathBuf::from(gitdir_path_str);
    gitdir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
}

/// Process-wide flag set by [`record_worktree_stale`] when
/// [`crate::resolve_index_dir`] redirects a worktree to the main
/// project's `.cqs/`. JSON envelope readers consult [`is_worktree_stale`]
/// to decide whether to add `worktree_stale: true` (+ `worktree_name`)
/// to the `_meta` block. CLI commands run as one-shot processes so
/// the once-per-process state is fine; the daemon never operates from
/// a worktree (it runs in the main project), so the daemon path
/// always reads `false` and never confuses readers.
static WORKTREE_STALE: std::sync::OnceLock<WorktreeContext> = std::sync::OnceLock::new();

#[derive(Debug, Clone)]
struct WorktreeContext {
    name: Option<String>,
}

/// Mark this process as serving from main's index instead of the
/// worktree's own. Idempotent: subsequent calls are silently
/// ignored (the OnceLock semantics).
pub fn record_worktree_stale(worktree_root: &Path) {
    let _ = WORKTREE_STALE.set(WorktreeContext {
        name: worktree_name(worktree_root),
    });
}

/// True if the current process is reading from main's `.cqs/`
/// because its `find_project_root()` resolved to a worktree without
/// its own index. JSON envelope writers add `worktree_stale: true`
/// + `worktree_name: "<name>"` to `_meta` when this returns true.
pub fn is_worktree_stale() -> bool {
    WORKTREE_STALE.get().is_some()
}

/// Worktree name captured at [`record_worktree_stale`] time, for
/// use in `_meta.worktree_name`. Returns `None` when not stale or
/// when the name couldn't be derived.
pub fn current_worktree_name() -> Option<String> {
    WORKTREE_STALE.get().and_then(|ctx| ctx.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A regular git repo with a `.git/` directory: `resolve_main_project_dir`
    /// must return `None` (it's the caller's job to handle non-worktrees).
    #[test]
    fn resolve_returns_none_for_regular_repo() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(resolve_main_project_dir(dir.path()), None);
    }

    /// A directory with no `.git` at all: `None` (no I/O panic).
    #[test]
    fn resolve_returns_none_for_non_git_dir() {
        let dir = TempDir::new().unwrap();
        assert_eq!(resolve_main_project_dir(dir.path()), None);
    }

    /// A `.git` file with no `gitdir:` line: malformed, `None`.
    #[test]
    fn resolve_returns_none_for_malformed_dot_git_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".git"), "garbage content\n").unwrap();
        assert_eq!(resolve_main_project_dir(dir.path()), None);
    }

    /// Construct a real-shape worktree on the filesystem and verify
    /// `resolve_main_project_dir` walks it back to the main root.
    /// Mirrors what `git worktree add` produces on disk:
    ///
    /// - `<main>/.git/` (canonical git dir)
    /// - `<main>/.git/worktrees/<name>/commondir` containing `../..`
    /// - `<worktree>/.git` containing `gitdir: <main>/.git/worktrees/<name>`
    #[test]
    fn resolve_walks_worktree_back_to_main() {
        let dir = TempDir::new().unwrap();
        let main_root = dir.path().join("main");
        let main_git = main_root.join(".git");
        let worktree_name = "feature-branch";
        let worktree_gitdir = main_git.join("worktrees").join(worktree_name);
        let worktree_root = dir.path().join("wt");
        std::fs::create_dir_all(&main_git).unwrap();
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        std::fs::create_dir_all(&worktree_root).unwrap();

        // commondir back-link: `../..` from `<main>/.git/worktrees/<name>/`
        // resolves to `<main>/.git/`.
        std::fs::write(worktree_gitdir.join("commondir"), "../..\n").unwrap();
        // .git file in the worktree, pointing at the per-worktree gitdir.
        std::fs::write(
            worktree_root.join(".git"),
            format!("gitdir: {}\n", worktree_gitdir.display()),
        )
        .unwrap();

        let canonical_main = std::fs::canonicalize(&main_root).unwrap();
        assert_eq!(
            resolve_main_project_dir(&worktree_root),
            Some(canonical_main),
        );
    }

    /// `lookup_main_cqs_dir` returns `OwnIndex` when the directory
    /// has its own `.cqs/` regardless of worktree status — the
    /// worktree's own index always wins.
    #[test]
    fn lookup_prefers_own_cqs_dir() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(crate::INDEX_DIR)).unwrap();
        match lookup_main_cqs_dir(dir.path()) {
            MainIndexLookup::OwnIndex { path } => {
                assert!(path.ends_with(crate::INDEX_DIR));
            }
            other => panic!("expected OwnIndex, got {other:?}"),
        }
    }

    /// Worktree without its own `.cqs/` but with main's `.cqs/`
    /// populated → `WorktreeUseMain`.
    #[test]
    fn lookup_redirects_worktree_to_populated_main() {
        let dir = TempDir::new().unwrap();
        let main_root = dir.path().join("main");
        let main_git = main_root.join(".git");
        let main_cqs = main_root.join(crate::INDEX_DIR);
        let wt_gitdir = main_git.join("worktrees").join("feature");
        let wt_root = dir.path().join("wt");
        std::fs::create_dir_all(&main_git).unwrap();
        std::fs::create_dir_all(&main_cqs).unwrap();
        std::fs::create_dir_all(&wt_gitdir).unwrap();
        std::fs::create_dir_all(&wt_root).unwrap();
        std::fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            wt_root.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();

        match lookup_main_cqs_dir(&wt_root) {
            MainIndexLookup::WorktreeUseMain {
                worktree_root,
                main_root: detected_main,
                main_cqs: detected_main_cqs,
            } => {
                assert_eq!(worktree_root, wt_root);
                let canon_expected = std::fs::canonicalize(&main_root).unwrap();
                assert_eq!(detected_main, canon_expected);
                assert_eq!(detected_main_cqs, canon_expected.join(crate::INDEX_DIR));
            }
            other => panic!("expected WorktreeUseMain, got {other:?}"),
        }
    }

    /// Worktree without its own `.cqs/` and main also without
    /// `.cqs/` → `WorktreeMainEmpty` so the caller's error message
    /// can name both paths.
    #[test]
    fn lookup_reports_main_empty_when_neither_indexed() {
        let dir = TempDir::new().unwrap();
        let main_root = dir.path().join("main");
        let main_git = main_root.join(".git");
        let wt_gitdir = main_git.join("worktrees").join("orphan");
        let wt_root = dir.path().join("wt");
        std::fs::create_dir_all(&main_git).unwrap();
        std::fs::create_dir_all(&wt_gitdir).unwrap();
        std::fs::create_dir_all(&wt_root).unwrap();
        std::fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            wt_root.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();

        match lookup_main_cqs_dir(&wt_root) {
            MainIndexLookup::WorktreeMainEmpty {
                worktree_root,
                main_root: detected_main,
            } => {
                assert_eq!(worktree_root, wt_root);
                let canon_expected = std::fs::canonicalize(&main_root).unwrap();
                assert_eq!(detected_main, canon_expected);
            }
            other => panic!("expected WorktreeMainEmpty, got {other:?}"),
        }
    }

    /// `worktree_name` derives the per-worktree directory name from
    /// the `.git` file's `gitdir:` line. Falls back to the worktree
    /// dir's basename only when the gitdir path can't be parsed.
    #[test]
    fn worktree_name_reads_from_gitdir_line() {
        let dir = TempDir::new().unwrap();
        let wt_root = dir.path().join("wt-shadow");
        std::fs::create_dir_all(&wt_root).unwrap();
        std::fs::write(
            wt_root.join(".git"),
            "gitdir: /abs/path/main/.git/worktrees/feature-x\n",
        )
        .unwrap();
        assert_eq!(worktree_name(&wt_root).as_deref(), Some("feature-x"));
    }
}
