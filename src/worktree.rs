//! Worktree-to-main-index discovery.
//!
//! When `cqs` is invoked from inside a `git worktree`, the worktree
//! has no `.cqs/` of its own (`git worktree add` doesn't copy the
//! directory). Without this discovery a cqs command in the worktree
//! errors with "No cqs index found", which leads agents to fall back
//! to raw `Read`/`grep` on absolute paths under the parent tree —
//! causing the worktree-leakage class of bugs documented in
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
    // against a hostile or accidentally-huge file at this path. 4 KiB is
    // far above realistic content (~30 bytes).
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

    // Read `<gitdir>/commondir` → relative path back to canonical `.git/`.
    //
    // Cap the read at MAX_GIT_FILE_BYTES (4 KiB), matching the .git-file read
    // above. A hostile or corrupt `<gitdir>/commondir` (the path ultimately
    // derives from the worktree's untrusted `.git` link) could otherwise OOM
    // the CLI on every invocation from inside a worktree.
    use std::io::Read;
    let commondir_file = gitdir.join("commondir");
    let mut commondir_relative = String::with_capacity(64);
    std::fs::File::open(&commondir_file)
        .ok()?
        .take(MAX_GIT_FILE_BYTES)
        .read_to_string(&mut commondir_relative)
        .ok()?;
    let commondir_relative = commondir_relative.trim();
    if commondir_relative.is_empty() {
        return None;
    }

    // Resolve `<gitdir>/<commondir_relative>` → canonical `.git/`.
    // Use `dunce::canonicalize` so Windows returns the non-`\\?\`-prefixed
    // form — downstream `WorktreeUseMain.main_root` is surfaced via JSON
    // envelopes and string-compared by agents.
    let canonical_git = gitdir.join(commondir_relative);
    let canonical_git = dunce::canonicalize(&canonical_git).ok()?;

    // Canonical `.git/`'s parent = main project root.
    let main_root = canonical_git.parent()?.to_path_buf();
    Some(main_root)
}

/// Resolve a worktree's per-worktree gitdir to its canonical filesystem
/// path, reading `<dir>/.git`'s `gitdir: <path>` line.
///
/// **Security primitive.** [`resolve_main_project_dir`] trusts the worktree's
/// own `.git` → `commondir` chain to discover the main root, which an attacker
/// who controls `<dir>/.git` can forge to point at *any* project's `.git/`
/// (the unregistered-worktree bypass). A registration check needs the
/// *gitdir* itself — for a legitimate linked worktree the gitdir lives at
/// `<main>/.git/worktrees/<name>/`, a directory *inside* the served project
/// that the attacker does not control. Comparing the canonical gitdir against
/// the served project's real `.git/worktrees/` therefore cannot be forged
/// from the worktree side.
///
/// Returns the canonicalized gitdir (so the caller can do an ancestor check
/// against a likewise-canonicalized `.git/worktrees/`). Returns `None` for a
/// non-worktree (regular repo, no `.git`, malformed link, or a gitdir that
/// does not exist on disk). Bounded read mirrors [`resolve_main_project_dir`].
pub fn worktree_gitdir(dir: &Path) -> Option<PathBuf> {
    let dot_git = dir.join(".git");
    let metadata = std::fs::metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        // Regular repo — not a linked worktree, so there is no per-worktree
        // gitdir under any project's `.git/worktrees/`.
        return None;
    }

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
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        dir.join(&gitdir)
    };
    // Canonicalize so the ancestor check below compares real paths (symlinks
    // and `..` resolved). A gitdir that does not exist canonicalizes to an
    // error → `None`, which the caller treats as "not registered".
    dunce::canonicalize(&gitdir).ok()
}

/// Read git's BACK-POINTER from a per-worktree gitdir: `<gitdir>/gitdir`
/// holds the absolute path of the worktree's real `.git` *file*. Returns
/// it canonicalized.
///
/// **Security primitive (the second half of the registration check).**
/// [`worktree_gitdir`] proves the worktree's *forward* link (its `.git` →
/// gitdir) lands UNDER the served project's `.git/worktrees/`, but git's
/// worktree link is *bidirectional* and an attacker controls only the
/// forward half. A forged `<evil>/.git` can name *any* real registered
/// gitdir of the served project (e.g. `gitdir: <served>/.git/worktrees/wt`),
/// passing the forward check — but the back-pointer at
/// `<served>/.git/worktrees/wt/gitdir` was written by git for the *real*
/// `wt` and points at `wt`'s `.git`, not `<evil>/.git`. The attacker cannot
/// rewrite it (it lives inside the served project's `.git/`). Requiring the
/// back-pointer to equal `<overlay_root>/.git` therefore binds the registry
/// entry to THIS specific overlay root, defeating the registered-gitdir
/// hijack.
///
/// Returns `None` when the back-pointer file is absent, unreadable, empty,
/// or does not canonicalize (a registry entry with no live `gitdir` back-link
/// is not a binding the caller can trust). Bounded read mirrors
/// [`resolve_main_project_dir`].
pub fn read_gitdir_backpointer(gitdir: &Path) -> Option<PathBuf> {
    let backpointer_file = gitdir.join("gitdir");
    let mut raw = String::with_capacity(128);
    std::fs::File::open(&backpointer_file)
        .ok()?
        .take(MAX_GIT_FILE_BYTES)
        .read_to_string(&mut raw)
        .ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // The back-pointer is the absolute path of the worktree's `.git` file.
    // Canonicalize so the caller compares real paths; a stale pointer to a
    // moved/deleted worktree canonicalizes to an error → `None`.
    dunce::canonicalize(raw).ok()
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
    // Canonicalize `dir` once up-front so the returned `worktree_root`
    // matches `find_project_root()` byte-for-byte on case-insensitive
    // filesystems (Windows NTFS, macOS HFS+/APFS default).
    // resolve_main_project_dir already canonicalizes its result; without
    // this the worktree side stays raw, so downstream string-equality checks
    // against find_project_root output (which IS canonicalized via dunce)
    // report mismatches even when the paths refer to the same dir — the
    // origin of worktree-leakage.
    let dir_canonical = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let own_cqs = dir_canonical.join(crate::INDEX_DIR);
    // is_dir() rather than exists() — a stray `.cqs` *file* (a mistaken
    // `touch .cqs`, or a packaged tarball with the wrong entry) shouldn't be
    // treated as an index dir. Downstream code would otherwise
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

/// Walk up from `start` to the nearest enclosing git repository or
/// worktree root — the directory containing a `.git` entry, whether
/// that entry is a *directory* (a regular checkout) or a *file* (a
/// linked worktree, per `git worktree add`). This mirrors
/// `git rev-parse --show-toplevel` semantics without shelling out.
///
/// Returns `None` when no `.git` is found within the depth cap (a tree
/// with no VCS root, or a deeper layout than we walk). The returned
/// path is canonicalized so callers can byte-compare it against
/// [`crate::cli::config::find_project_root`] output (which is
/// canonicalized via `dunce`).
pub fn enclosing_git_root(start: &Path) -> Option<PathBuf> {
    // Match `find_project_root`'s depth cap so the two walks agree on
    // how far up they look — a guard that fires for a root the resolver
    // would never have reached is a false positive.
    const MAX_WALK_DEPTH: usize = 20;
    let start = dunce::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    let mut current: &Path = &start;
    for _ in 0..MAX_WALK_DEPTH {
        // `.git` may be a directory (regular repo) OR a file (linked
        // worktree). `exists()` covers both; we don't care which here —
        // either marks a VCS toplevel for boundary-crossing detection.
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
    None
}

/// Decide whether a WRITE command's resolved project root crossed a
/// git-worktree (or Cargo-workspace) boundary *upward* relative to the
/// invocation's own enclosing git root.
///
/// The hazard: from a worktree under a parent workspace,
/// `find_project_root()` walks up past the worktree's own `.git` to the
/// parent's `Cargo.toml [workspace]` (or, for an out-of-tree worktree,
/// `resolve_index_dir` redirects to main's `.cqs/`). Reads are meant to
/// see main's snapshot; writes silently mutating an index outside the
/// current worktree defeat isolation.
///
/// Returns `Some(worktree_root)` when:
///   1. CWD has an enclosing git root (a checkout or worktree), AND
///   2. the resolved `project_root` differs from that enclosing root, AND
///   3. the resolved root is an *ancestor* of the enclosing root
///      (the walk crossed the boundary upward — the parent index case).
///
/// Returns `None` for the safe cases: a regular repo where CWD's git
/// root equals the resolved root, or any layout where the resolved root
/// is not an ancestor of CWD's git root (so no upward boundary crossing
/// happened). On filesystem-canonicalization failure it returns `None`
/// (fail-open: never block a write on a path-resolution quirk).
pub fn parent_index_boundary_crossed(cwd: &Path, project_root: &Path) -> Option<PathBuf> {
    let worktree_root = enclosing_git_root(cwd)?;
    let project_root =
        dunce::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    if worktree_root == project_root {
        // Regular repo / non-worktree: resolver landed on the same root
        // the invocation lives in. No boundary crossed.
        return None;
    }
    // Upward crossing only: the resolved root must be a strict ancestor
    // of the enclosing git root. A sibling or descendant resolution is
    // not the parent-index hazard and stays silent.
    if worktree_root.starts_with(&project_root) {
        Some(worktree_root)
    } else {
        None
    }
}

/// Resolve the worktree root to build a search overlay for, or `None`
/// when no overlay applies.
///
/// The worktree search overlay (`src/worktree_overlay.rs`) serves a
/// worktree's *dirty delta* on top of the parent index. It applies only
/// when the invocation lives in a worktree whose reads were redirected to
/// the parent project's index — exactly the two layouts the codebase
/// already detects:
///
///   1. **Nested worktrees** (`.claude/worktrees/<agent>/` under the
///      parent): caught by [`parent_index_boundary_crossed`], where the
///      resolved root is a strict ancestor of the enclosing git root.
///   2. **Out-of-tree worktrees** (`git worktree add ../wt`): caught by
///      [`lookup_main_cqs_dir`] returning
///      [`MainIndexLookup::WorktreeUseMain`] — the worktree has no own
///      `.cqs/` and resolution redirected to main's.
///
/// Returns `Some(worktree_root)` (canonicalized) when either half fires
/// AND the enclosing git root differs from `resolved_root` (a regular
/// repo where they coincide has no parent index to overlay onto).
/// Returns `None` otherwise — the safe non-overlay path. Wraps the two
/// existing predicates so the disjunction lives in one place; the flag
/// layer in PR-2 calls this to decide whether an overlay is even
/// eligible.
///
/// `resolved_root` is the project root the invocation resolved to (the
/// CLI's `find_project_root` output / the daemon's served root).
pub fn overlay_root(cwd: &Path, resolved_root: &Path) -> Option<PathBuf> {
    // Nested case: the boundary-crossing predicate already returns the
    // worktree root when the resolver walked up past the worktree's own
    // `.git` to an ancestor (the parent-index hazard #1814 guards).
    if let Some(root) = parent_index_boundary_crossed(cwd, resolved_root) {
        return Some(root);
    }

    // Out-of-tree case: the worktree is NOT under the parent, so the
    // boundary predicate's ancestor check never fires. Detect it via the
    // same `.git`-link resolution `resolve_index_dir` uses. `WorktreeUseMain`
    // means "this is a worktree with no own index, redirected to main".
    //
    // Resolve the enclosing git root FIRST and feed THAT to the lookup:
    // `lookup_main_cqs_dir` reads `<dir>/.git`, which only exists at the
    // worktree root. From a subdirectory (`wt/src/`) it would return
    // `NotWorktree`. Walking up to the `.git`-bearing root makes the
    // predicate fire from anywhere inside the worktree.
    let probe = enclosing_git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    if let MainIndexLookup::WorktreeUseMain {
        worktree_root,
        main_root,
        ..
    } = lookup_main_cqs_dir(&probe)
    {
        // Guard: the redirect target must actually differ from the worktree
        // itself. Compare against the lookup's own `main_root` (the redirect
        // target it resolved), NOT the passed `resolved_root`: the CLI's
        // `find_project_root()` returns the WORKTREE root for a redirected
        // worktree (the index dir is redirected, the project root is not), so
        // `resolved_root == worktree_root` here and comparing against it would
        // wrongly reject every out-of-tree worktree. A regular repo never
        // reaches this arm (it returns `OwnIndex` above), so `worktree_root !=
        // main_root` holds exactly when there is a genuine parent index to
        // overlay onto.
        if worktree_root != main_root {
            return Some(worktree_root);
        }
    }

    None
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
    // Bounded read — see `MAX_GIT_FILE_BYTES`.
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
    // The `.git` link file can carry a Windows verbatim (`\\?\C:\...`)
    // prefix and backslash separators. Strip the prefix and normalize to
    // forward slashes before `file_name()`, otherwise on a non-Windows host
    // `PathBuf::from` treats the whole backslash string as one component and
    // returns the prefixed mess instead of the worktree basename.
    let gitdir_normalized =
        crate::strip_windows_verbatim_prefix(gitdir_path_str).replace('\\', "/");
    let gitdir = PathBuf::from(&gitdir_normalized);
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
    // Log on producer side. The cross-worktree stale flag is a
    // cross-process signal that's near-impossible to diagnose without a
    // journal trail.
    let name = worktree_name(worktree_root);
    if WORKTREE_STALE
        .set(WorktreeContext { name: name.clone() })
        .is_ok()
    {
        tracing::info!(
            worktree_root = %worktree_root.display(),
            worktree_name = name.as_deref().unwrap_or(""),
            "worktree marked stale (reading from main's .cqs/)"
        );
    }
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

    /// `worktree_gitdir` returns the canonical per-worktree gitdir for a
    /// real-shape linked worktree (the directory under
    /// `<main>/.git/worktrees/<name>/`). This is the path the overlay
    /// registration check compares against the served project's
    /// `.git/worktrees/`.
    #[test]
    fn worktree_gitdir_resolves_canonical_gitdir() {
        let dir = TempDir::new().unwrap();
        let main_git = dir.path().join("main").join(".git");
        let gitdir = main_git.join("worktrees").join("feature");
        let wt_root = dir.path().join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::create_dir_all(&wt_root).unwrap();
        std::fs::write(
            wt_root.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        let expected = dunce::canonicalize(&gitdir).unwrap();
        assert_eq!(worktree_gitdir(&wt_root), Some(expected));
    }

    /// `worktree_gitdir` returns `None` for a regular repo (`.git` is a dir,
    /// not a link) and for a worktree whose gitdir does not exist on disk
    /// (canonicalize fails) — the "not a registered worktree" cases.
    #[test]
    fn worktree_gitdir_none_for_non_worktree_and_missing_gitdir() {
        let dir = TempDir::new().unwrap();
        // Regular repo.
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(worktree_gitdir(&repo), None);

        // Worktree link pointing at a gitdir that does not exist.
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /no/such/path/.git/worktrees/x\n").unwrap();
        assert_eq!(worktree_gitdir(&wt), None);
    }

    /// `read_gitdir_backpointer` reads git's `<gitdir>/gitdir` back-link and
    /// canonicalizes it to the worktree's real `.git` file path.
    #[test]
    fn read_gitdir_backpointer_returns_canonical_dot_git() {
        let dir = TempDir::new().unwrap();
        let gitdir = dir
            .path()
            .join("main")
            .join(".git")
            .join("worktrees")
            .join("f");
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        // The worktree's real `.git` file (target of the back-pointer).
        let dot_git = wt.join(".git");
        std::fs::write(&dot_git, "gitdir: whatever\n").unwrap();
        // git's back-link: `<gitdir>/gitdir` holds the abs path of `<wt>/.git`.
        std::fs::write(gitdir.join("gitdir"), format!("{}\n", dot_git.display())).unwrap();

        let expected = dunce::canonicalize(&dot_git).unwrap();
        assert_eq!(read_gitdir_backpointer(&gitdir), Some(expected));
    }

    /// `read_gitdir_backpointer` returns `None` when the back-link file is
    /// absent, empty, or points at a path that does not exist — the cases the
    /// registration check treats as an unbound (untrusted) registry entry.
    #[test]
    fn read_gitdir_backpointer_none_when_absent_empty_or_dangling() {
        let dir = TempDir::new().unwrap();
        let gitdir = dir.path().join("g");
        std::fs::create_dir_all(&gitdir).unwrap();
        // Absent back-link file.
        assert_eq!(read_gitdir_backpointer(&gitdir), None);
        // Empty back-link file.
        std::fs::write(gitdir.join("gitdir"), "\n").unwrap();
        assert_eq!(read_gitdir_backpointer(&gitdir), None);
        // Dangling back-link (points at a non-existent path).
        std::fs::write(gitdir.join("gitdir"), "/no/such/wt/.git\n").unwrap();
        assert_eq!(read_gitdir_backpointer(&gitdir), None);
    }

    /// A stray `.cqs` *file* (mistaken `touch .cqs`, packaged tarball with
    /// the wrong entry) must NOT be treated as an index dir. Pins the
    /// `is_dir()` contract so a regression to `.exists()` is caught at test
    /// time.
    #[test]
    fn lookup_ignores_stray_cqs_file_falls_through() {
        let dir = TempDir::new().unwrap();
        // Create a stray FILE named `.cqs` instead of a directory.
        std::fs::write(dir.path().join(crate::INDEX_DIR), b"oops").unwrap();
        match lookup_main_cqs_dir(dir.path()) {
            MainIndexLookup::NotWorktree => { /* expected */ }
            other => panic!("stray .cqs file must fall through to NotWorktree, got {other:?}"),
        }
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

    #[test]
    fn worktree_name_strips_windows_verbatim_prefix() {
        let dir = TempDir::new().unwrap();
        let wt_root = dir.path().join("wt-verbatim");
        std::fs::create_dir_all(&wt_root).unwrap();
        // Windows-style `.git` link with a `\\?\` verbatim prefix and
        // backslash separators, as `git worktree add` writes under WSL/Windows.
        std::fs::write(
            wt_root.join(".git"),
            "gitdir: \\\\?\\C:\\Projects\\cqs\\.git\\worktrees\\feature-x\n",
        )
        .unwrap();
        assert_eq!(worktree_name(&wt_root).as_deref(), Some("feature-x"));
    }

    #[test]
    fn worktree_name_strips_verbatim_unc_prefix() {
        let dir = TempDir::new().unwrap();
        let wt_root = dir.path().join("wt-unc");
        std::fs::create_dir_all(&wt_root).unwrap();
        // `\\?\UNC\server\share\...` verbatim UNC form.
        std::fs::write(
            wt_root.join(".git"),
            "gitdir: \\\\?\\UNC\\server\\share\\repo\\.git\\worktrees\\hotfix-9\n",
        )
        .unwrap();
        assert_eq!(worktree_name(&wt_root).as_deref(), Some("hotfix-9"));
    }

    /// `enclosing_git_root` finds the nearest dir with a `.git` *directory*
    /// (regular repo) walking up from a nested subdir.
    #[test]
    fn enclosing_git_root_finds_dir_marker_from_subdir() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let nested = repo.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        let canon_repo = dunce::canonicalize(&repo).unwrap();
        assert_eq!(enclosing_git_root(&nested), Some(canon_repo));
    }

    /// `enclosing_git_root` also accepts a `.git` *file* (a linked
    /// worktree), returning the worktree root rather than walking past it.
    #[test]
    fn enclosing_git_root_accepts_git_file_worktree() {
        let dir = TempDir::new().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /somewhere/.git/worktrees/x\n").unwrap();

        let canon_wt = dunce::canonicalize(&wt).unwrap();
        assert_eq!(enclosing_git_root(&wt), Some(canon_wt));
    }

    /// No `.git` anywhere in the walk → `None` (no panic).
    #[test]
    fn enclosing_git_root_none_without_git() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(enclosing_git_root(&sub), None);
    }

    /// The worktree-under-workspace fixture: a worktree (`.git` *file*)
    /// nested UNDER a parent workspace (`.git` *directory*). A write
    /// resolving to the parent root crossed the boundary upward →
    /// `Some(worktree_root)`.
    #[test]
    fn boundary_crossed_when_write_resolves_to_parent_of_worktree() {
        let dir = TempDir::new().unwrap();
        let parent = dir.path().join("workspace");
        // Parent is a real repo with a `.git/` dir AND a `.cqs/` index.
        std::fs::create_dir_all(parent.join(".git")).unwrap();
        std::fs::create_dir_all(parent.join(crate::INDEX_DIR)).unwrap();
        // Worktree nested under the parent, with a `.git` *file* and no
        // `.cqs/` of its own — exactly the `.claude/worktrees/<agent>/` shape.
        let wt = parent.join(".claude").join("worktrees").join("agent-x");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /abs/.git/worktrees/agent-x\n").unwrap();

        let canon_parent = dunce::canonicalize(&parent).unwrap();
        let canon_wt = dunce::canonicalize(&wt).unwrap();

        // A write command resolved its project root to the PARENT (what
        // `find_cargo_workspace_root` does). The guard must flag it.
        let crossed = parent_index_boundary_crossed(&wt, &canon_parent);
        assert_eq!(
            crossed,
            Some(canon_wt),
            "writing to the parent workspace from a nested worktree must be flagged"
        );
    }

    /// A regular repo where the write resolves to the SAME root the
    /// invocation lives in → no boundary crossing, guard stays silent.
    #[test]
    fn boundary_not_crossed_for_regular_repo() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(crate::INDEX_DIR)).unwrap();
        let canon_repo = dunce::canonicalize(&repo).unwrap();

        // cwd inside the repo, resolved root = the repo itself.
        assert_eq!(parent_index_boundary_crossed(&repo, &canon_repo), None);
        // From a subdir of the repo: still the same root, still no crossing.
        let sub = repo.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(parent_index_boundary_crossed(&sub, &canon_repo), None);
    }

    /// A sibling/unrelated resolved root (not an ancestor of CWD's git
    /// root) is not the parent-index hazard → `None`. Guards against a
    /// guard that fires on any path mismatch.
    #[test]
    fn boundary_not_crossed_for_non_ancestor_root() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let other = dir.path().join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        let canon_other = dunce::canonicalize(&other).unwrap();

        // CWD's git root is `repo`, but the resolved root is a sibling
        // dir — not an ancestor, so no upward crossing.
        assert_eq!(parent_index_boundary_crossed(&repo, &canon_other), None);
    }

    /// CWD with no enclosing git root at all → `None` (the guard can't
    /// detect a worktree boundary, so it stays out of the way).
    #[test]
    fn boundary_not_crossed_without_enclosing_git() {
        let dir = TempDir::new().unwrap();
        let loose = dir.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(parent_index_boundary_crossed(&loose, dir.path()), None);
    }

    /// `overlay_root` fires for the nested worktree shape — the same layout
    /// `parent_index_boundary_crossed` flags. Returns the worktree root so
    /// the overlay builder knows which checkout's delta to compute.
    #[test]
    fn overlay_root_some_for_nested_worktree() {
        let dir = TempDir::new().unwrap();
        let parent = dir.path().join("workspace");
        std::fs::create_dir_all(parent.join(".git")).unwrap();
        std::fs::create_dir_all(parent.join(crate::INDEX_DIR)).unwrap();
        let wt = parent.join(".claude").join("worktrees").join("agent-x");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /abs/.git/worktrees/agent-x\n").unwrap();

        let canon_parent = dunce::canonicalize(&parent).unwrap();
        let canon_wt = dunce::canonicalize(&wt).unwrap();

        assert_eq!(
            overlay_root(&wt, &canon_parent),
            Some(canon_wt),
            "nested worktree resolving to its parent index is overlay-eligible"
        );
    }

    /// `overlay_root` returns `None` for a regular repo — CWD's git root
    /// equals the resolved root, so there is no parent index to overlay onto.
    #[test]
    fn overlay_root_none_for_regular_repo() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(crate::INDEX_DIR)).unwrap();
        let canon_repo = dunce::canonicalize(&repo).unwrap();
        assert_eq!(overlay_root(&repo, &canon_repo), None);
        let sub = repo.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(overlay_root(&sub, &canon_repo), None);
    }

    /// `overlay_root` returns `None` when CWD has no enclosing git root —
    /// nothing to overlay, and no worktree to compute a delta from.
    #[test]
    fn overlay_root_none_without_enclosing_git() {
        let dir = TempDir::new().unwrap();
        let loose = dir.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(overlay_root(&loose, dir.path()), None);
    }

    /// REGRESSION: the production shape `overlay_root(&wt, &wt)` — an
    /// out-of-tree worktree whose resolved project root IS the worktree
    /// itself (the CLI's `find_project_root()` returns the worktree root; only
    /// the *index dir* redirects to main). The earlier guard compared
    /// `worktree_root` against the passed `resolved_root`, which equals the
    /// worktree here, so it wrongly returned `None` for every out-of-tree
    /// worktree. The fix compares against the lookup's own `main_root`. The
    /// existing nested test passes `parent` as `resolved_root` and would pass
    /// under BOTH the old and the new comparison — only this shape pins the fix.
    #[test]
    fn overlay_root_some_for_out_of_tree_worktree_resolved_to_itself() {
        let dir = TempDir::new().unwrap();
        // Main project: a real `.git/` dir + a `.cqs/` index. Sibling of wt.
        let main = dir.path().join("main");
        std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt")).unwrap();
        std::fs::create_dir_all(main.join(crate::INDEX_DIR)).unwrap();

        // Out-of-tree worktree: a sibling dir (NOT nested under main), whose
        // `.git` is a FILE pointing at main's per-worktree gitdir, with a
        // `commondir` resolving back to main's `.git/` — exactly the layout
        // `git worktree add ../wt` produces.
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let canon_main = dunce::canonicalize(&main).unwrap();
        let gitdir = canon_main.join(".git").join("worktrees").join("wt");
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        // commondir: relative path from <gitdir> back to canonical `.git/`.
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();

        let canon_wt = dunce::canonicalize(&wt).unwrap();

        // The production call shape: resolved_root == the worktree itself.
        assert_eq!(
            overlay_root(&canon_wt, &canon_wt),
            Some(canon_wt.clone()),
            "an out-of-tree worktree resolved to itself is overlay-eligible \
             (the index redirects to main even though the project root is the wt)"
        );
    }
}
