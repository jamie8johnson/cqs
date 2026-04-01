//! Git operations for training data extraction.
//!
//! Thin wrappers around `git log`, `git diff-tree`, `git show`, and
//! `git rev-parse` that return parsed Rust types. All functions accept
//! a repo path and use `git -C <repo>` to avoid changing directories.

use std::path::Path;
use std::process::Command;

use super::TrainDataError;

// ─── Types ───────────────────────────────────────────────────────────────────

/// A parsed git commit from `git log`.
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub sha: String,
    pub message: String,
    pub date: String,
}

// ─── Git wrapper functions ───────────────────────────────────────────────────

/// List non-merge commits in reverse chronological order.
/// Uses `--format="%H%x00%s%x00%aI"` with NUL separators for reliable parsing
/// (commit messages can contain any printable character). `--no-merges` excludes
/// merge commits which typically have no meaningful diff.
/// `max_commits == 0` means no limit.
pub fn git_log(repo: &Path, max_commits: usize) -> Result<Vec<CommitInfo>, TrainDataError> {
    let _span = tracing::info_span!("git_log", repo = %repo.display(), max_commits).entered();

    let mut cmd = Command::new("git");
    cmd.args(["-C"])
        .arg(repo)
        .args(["log", "--format=%H%x00%s%x00%aI", "--no-merges"]);

    if max_commits > 0 {
        cmd.args(["-n", &max_commits.to_string()]);
    }

    let output = cmd.output().map_err(|e| {
        tracing::warn!(error = %e, "Failed to spawn git log");
        e
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(TrainDataError::Git(format!(
            "git log failed: {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(3, '\0').collect();
        if parts.len() != 3 {
            tracing::warn!(
                line,
                "Skipping malformed git log line (expected 3 NUL-separated fields)"
            );
            continue;
        }

        commits.push(CommitInfo {
            sha: parts[0].to_string(),
            message: parts[1].to_string(),
            date: parts[2].to_string(),
        });
    }

    tracing::debug!(count = commits.len(), "Parsed git log commits");
    if commits.len() > 100_000 {
        tracing::warn!(
            count = commits.len(),
            "git_log returned >100K commits — consider setting max_commits to limit memory usage"
        );
    }
    Ok(commits)
}

/// Get the unified diff for a single commit.
/// Uses `--root` so the initial commit (no parent) produces a diff against
/// the empty tree. `--no-commit-id -r -p` gives raw recursive patch output.
pub fn git_diff_tree(repo: &Path, sha: &str) -> Result<String, TrainDataError> {
    let _span = tracing::info_span!("git_diff_tree", repo = %repo.display(), sha).entered();

    if sha.starts_with('-') || sha.contains('\0') {
        return Err(TrainDataError::Git(format!(
            "Invalid SHA '{}': must not start with '-' or contain null bytes",
            sha
        )));
    }

    let output = Command::new("git")
        .args(["-C"])
        .arg(repo)
        .args(["diff-tree", "--root", "--no-commit-id", "-r", "-p", sha])
        .output()
        .map_err(|e| {
            tracing::warn!(error = %e, "Failed to spawn git diff-tree");
            e
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(TrainDataError::Git(format!(
            "git diff-tree failed for {}: {}",
            sha,
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Maximum file size to retrieve via `git show` (50 MB).
const MAX_SHOW_SIZE: usize = 50 * 1024 * 1024;

/// Retrieve file content at a specific commit.
/// Returns `Ok(None)` if the content exceeds 50 MB or is not valid UTF-8
/// (binary files). Returns `Err` if git itself fails (e.g., path doesn't
/// exist at that commit).
pub fn git_show(repo: &Path, sha: &str, path: &str) -> Result<Option<String>, TrainDataError> {
    let _span = tracing::info_span!("git_show", repo = %repo.display(), sha, path).entered();

    if sha.starts_with('-') || sha.contains('\0') {
        return Err(TrainDataError::Git(format!(
            "Invalid SHA '{}': must not start with '-' or contain null bytes",
            sha
        )));
    }
    if path.starts_with('-') || path.contains('\0') {
        return Err(TrainDataError::Git(format!(
            "Invalid path '{}': must not start with '-' or contain null bytes",
            path
        )));
    }
    if path.contains(':') {
        return Err(TrainDataError::Git(format!(
            "Invalid path '{}': must not contain ':' (reserved for git rev:path syntax)",
            path
        )));
    }

    let spec = format!("{}:{}", sha, path);
    let output = Command::new("git")
        .args(["-C"])
        .arg(repo)
        .args(["show", &spec])
        .output()
        .map_err(|e| {
            tracing::warn!(error = %e, "Failed to spawn git show");
            e
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(TrainDataError::Git(format!(
            "git show failed for {}: {}",
            spec,
            stderr.trim()
        )));
    }

    // Size guard
    if output.stdout.len() > MAX_SHOW_SIZE {
        tracing::debug!(
            size = output.stdout.len(),
            max = MAX_SHOW_SIZE,
            "Skipping oversized file"
        );
        return Ok(None);
    }

    // UTF-8 guard — binary files are not useful for training
    match String::from_utf8(output.stdout) {
        Ok(content) => Ok(Some(content)),
        Err(_) => {
            tracing::debug!(path, "Skipping non-UTF-8 file");
            Ok(None)
        }
    }
}

/// Check whether the repository is a shallow clone.
/// Returns `true` if `git rev-parse --is-shallow-repository` says "true".
/// Returns `false` on any error (conservative: assume full history).
pub fn is_shallow(repo: &Path) -> bool {
    let _span = tracing::info_span!("is_shallow", repo = %repo.display()).entered();

    let output = match Command::new("git")
        .args(["-C"])
        .arg(repo)
        .args(["rev-parse", "--is-shallow-repository"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check shallow status");
            return false;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim() == "true"
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a minimal git repo with one commit containing `test.rs`.
    fn create_test_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let repo = dir.path();

        // git init
        let status = Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["init"])
            .output()
            .unwrap();
        assert!(status.status.success(), "git init failed");

        // Configure user for commits
        Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["config", "user.name", "Test"])
            .output()
            .unwrap();

        // Write test.rs
        std::fs::write(repo.join("test.rs"), "fn hello() { println!(\"hi\"); }\n").unwrap();

        // git add + commit
        Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["commit", "-m", "initial commit"])
            .output()
            .unwrap();

        dir
    }

    /// Create a repo with two commits: initial + a modification.
    fn create_test_repo_with_change() -> TempDir {
        let dir = create_test_repo();
        let repo = dir.path();

        // Modify test.rs
        std::fs::write(
            repo.join("test.rs"),
            "fn hello() { println!(\"hello world\"); }\nfn goodbye() { }\n",
        )
        .unwrap();

        Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["commit", "-m", "update hello and add goodbye"])
            .output()
            .unwrap();

        dir
    }
    /// Verifies that `git_log` correctly retrieves commit information from a test repository.
    /// Creates a temporary test repository, calls `git_log` with offset 0, and asserts that the returned commits list is non-empty and contains valid commit data (non-empty SHA, message, and date fields).
    /// # Panics
    /// Panics if any of the assertions fail, indicating that `git_log` did not return expected commit data or the test repository could not be created.

    #[test]
    fn git_log_on_test_repo() {
        let dir = create_test_repo();
        let commits = git_log(dir.path(), 0).unwrap();
        assert!(!commits.is_empty());
        assert!(!commits[0].sha.is_empty());
        assert!(!commits[0].message.is_empty());
        assert!(!commits[0].date.is_empty());
    }
    /// Tests that the git_log function correctly respects the max_commits parameter to limit the number of returned commits.
    /// # Arguments
    /// This is a test function with no parameters.
    /// # Behavior
    /// Creates a test repository with multiple commits, then verifies that:
    /// - git_log with max_commits=0 returns all commits (2 in this case)
    /// - git_log with max_commits=1 returns only 1 commit
    /// - The returned commit matches the most recent commit from the full log

    #[test]
    fn git_log_respects_max_commits() {
        let dir = create_test_repo_with_change();
        let all = git_log(dir.path(), 0).unwrap();
        assert_eq!(all.len(), 2);

        let limited = git_log(dir.path(), 1).unwrap();
        assert_eq!(limited.len(), 1);
        // Most recent commit first
        assert_eq!(limited[0].sha, all[0].sha);
    }
    /// Tests that `git_log` returns commit dates in ISO 8601 format.
    /// # Arguments
    /// This is a test function with no parameters.
    /// # Returns
    /// Returns nothing. This is a test function that asserts the date format of git commits contains either 'T' or '-' characters, which are present in ISO 8601 formatted dates (e.g., 2026-03-19T14:30:00+00:00).
    /// # Panics
    /// Panics if the assertion fails, indicating that `git_log` did not return dates in the expected ISO 8601 format.

    #[test]
    fn git_log_returns_iso_date() {
        let dir = create_test_repo();
        let commits = git_log(dir.path(), 0).unwrap();
        // ISO 8601 format from %aI: e.g. 2026-03-19T14:30:00+00:00
        assert!(
            commits[0].date.contains('T') || commits[0].date.contains('-'),
            "Expected ISO date, got: {}",
            commits[0].date
        );
    }
    /// Tests the git_diff_tree function with a test repository containing a committed change.
    /// Creates a test repository with a change, retrieves the most recent commit, and generates a diff tree for that commit. Asserts that the diff output contains references to the modified file (test.rs) and includes standard unified diff hunk headers (@@).
    /// # Panics
    /// Panics if the test repository creation fails, git_log returns an error, git_diff_tree returns an error, or if the generated diff does not contain the expected file reference or hunk headers.

    #[test]
    fn git_diff_tree_on_test_repo() {
        let dir = create_test_repo_with_change();
        let commits = git_log(dir.path(), 0).unwrap();
        let diff = git_diff_tree(dir.path(), &commits[0].sha).unwrap();
        assert!(diff.contains("test.rs"), "diff should reference test.rs");
        assert!(diff.contains("@@"), "diff should contain hunk headers");
    }
    /// Verifies that `git_diff_tree` correctly generates a diff for the initial commit in a repository.
    /// # Arguments
    /// This function takes no parameters. It creates a test repository internally.
    /// # Returns
    /// Returns nothing. This is a test function that asserts expected behavior.
    /// # Panics
    /// Panics if the initial commit diff does not contain a reference to "test.rs".

    #[test]
    fn git_diff_tree_initial_commit() {
        let dir = create_test_repo();
        let commits = git_log(dir.path(), 0).unwrap();
        // --root makes the initial commit produce a diff
        let diff = git_diff_tree(dir.path(), &commits[0].sha).unwrap();
        assert!(
            diff.contains("test.rs"),
            "initial commit diff should reference test.rs"
        );
    }
    /// Test function that verifies `git_show` correctly retrieves file content from a git repository.
    /// # Arguments
    /// None. This function creates its own test repository and uses hardcoded test data.
    /// # Returns
    /// None. This function is a test assertion function that panics if assertions fail.
    /// # Panics
    /// Panics if:
    /// - The test repository creation fails
    /// - `git_log` fails to retrieve commits
    /// - `git_show` fails to retrieve file content
    /// - The returned content is `None`
    /// - The file content does not contain the expected string "fn hello"

    #[test]
    fn git_show_returns_content() {
        let dir = create_test_repo();
        let commits = git_log(dir.path(), 0).unwrap();
        let content = git_show(dir.path(), &commits[0].sha, "test.rs").unwrap();
        assert!(content.is_some());
        assert!(content.unwrap().contains("fn hello"));
    }
    /// Tests that `git_show` returns an error when attempting to retrieve a nonexistent file from a git commit.
    /// # Arguments
    /// This function takes no parameters. It creates its own temporary test repository internally.
    /// # Panics
    /// Panics if the test repository cannot be created, if `git_log` fails unexpectedly, or if the commits list is empty.

    #[test]
    fn git_show_nonexistent_file_errors() {
        let dir = create_test_repo();
        let commits = git_log(dir.path(), 0).unwrap();
        let result = git_show(dir.path(), &commits[0].sha, "nonexistent.rs");
        assert!(result.is_err(), "Should error for nonexistent file");
    }
    /// Tests that a normally cloned repository is not detected as shallow.
    /// # Arguments
    /// None
    /// # Returns
    /// None (unit test)
    /// # Panics
    /// Panics if the assertion fails, indicating the repository was incorrectly identified as shallow.

    #[test]
    fn is_shallow_on_normal_repo() {
        let dir = create_test_repo();
        assert!(!is_shallow(dir.path()));
    }
    /// Tests that `is_shallow` returns false for a nonexistent repository path instead of panicking.
    /// # Arguments
    /// None. This is a test function that uses hardcoded paths.
    /// # Returns
    /// Nothing. This is a test that asserts `is_shallow` returns `false` when given a nonexistent path.

    #[test]
    fn is_shallow_on_nonexistent_path() {
        // Should return false (conservative default), not panic
        assert!(!is_shallow(Path::new("/nonexistent/repo/path")));
    }
}
