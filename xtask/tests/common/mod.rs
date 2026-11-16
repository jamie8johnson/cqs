//! Shared fixtures: a temporary git repository whose child processes run
//! with a scrubbed environment. Every child gets `CQS_PRIVATE_TERMS`,
//! `CQS_BRIEF_ROOT`, `CLAUDE_PROJECT_DIR`, every `GIT_*` variable and
//! `XDG_CONFIG_HOME` removed, `HOME` pointed at a temporary folder, and
//! `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` pointed at an empty file. The
//! environment is set on each child, never on the test process; a test that
//! needs one of the removed variables sets it on its own child.

#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// Every invented word the tests use as a term. No output line of any test
/// may hold one, in any case.
pub const ALL_WORDS: &[&str] = &["zorblax", "quindle", "fernwhistle", "plimzot"];

pub struct Sandbox {
    _root: TempDir,
    /// The repository (or plain folder) the children run in.
    pub repo: PathBuf,
    /// A folder outside the repository for lists and other inputs.
    pub outside: PathBuf,
    home: PathBuf,
    empty_config: PathBuf,
}

impl Sandbox {
    /// A sandbox whose `repo` is a fresh git repository on branch `main`.
    pub fn repo() -> Sandbox {
        let sb = Sandbox::plain();
        sb.git(&["init", "-q", "-b", "main"]);
        sb.git(&["config", "user.name", "Test Author"]);
        sb.git(&["config", "user.email", "author@example.invalid"]);
        sb.git(&["config", "commit.gpgsign", "false"]);
        sb
    }

    /// A sandbox whose `repo` is an empty folder that is not a repository.
    pub fn plain() -> Sandbox {
        let root = tempfile::Builder::new()
            .prefix("xtask-test-")
            .tempdir()
            .expect("create temp dir");
        let base = fs::canonicalize(root.path()).expect("canonicalize temp dir");
        let repo = base.join("repo");
        let outside = base.join("outside");
        let home = base.join("home");
        for dir in [&repo, &outside, &home] {
            fs::create_dir_all(dir).expect("create sandbox dir");
        }
        let empty_config = base.join("empty-gitconfig");
        fs::write(&empty_config, b"").expect("write empty config");
        Sandbox {
            _root: root,
            repo,
            outside,
            home,
            empty_config,
        }
    }

    /// A command for `program` with the scrubbed environment, run in `repo`.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.repo);
        cmd.env_remove("CQS_PRIVATE_TERMS");
        cmd.env_remove("CQS_BRIEF_ROOT");
        cmd.env_remove("CLAUDE_PROJECT_DIR");
        cmd.env_remove("XDG_CONFIG_HOME");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&key);
            }
        }
        cmd.env("HOME", &self.home);
        cmd.env("GIT_CONFIG_GLOBAL", &self.empty_config);
        cmd.env("GIT_CONFIG_SYSTEM", &self.empty_config);
        cmd
    }

    /// Runs git in `repo`, asserts success, and returns standard output.
    pub fn git(&self, args: &[&str]) -> String {
        let out = self.command("git").args(args).output().expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("git output is UTF-8")
    }

    /// The full id of a revision.
    pub fn rev(&self, rev: &str) -> String {
        self.git(&["rev-parse", rev]).trim().to_owned()
    }

    /// Writes a file under `repo`, creating its folders.
    pub fn write(&self, rel: &str, data: impl AsRef<[u8]>) -> PathBuf {
        write_file(&self.repo.join(rel), data)
    }

    /// Writes a file under `outside`.
    pub fn write_outside(&self, rel: &str, data: impl AsRef<[u8]>) -> PathBuf {
        write_file(&self.outside.join(rel), data)
    }

    /// Stages everything and commits it with `message`.
    pub fn commit_all(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.rev("HEAD")
    }
}

fn write_file(path: &Path, data: impl AsRef<[u8]>) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(path, data).expect("write file");
    path.to_path_buf()
}

/// Asserts that no line of either stream holds one of `ALL_WORDS`.
pub fn assert_no_term(out: &Output) {
    for (name, stream) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        let text = String::from_utf8_lossy(stream).to_ascii_lowercase();
        for line in text.lines() {
            for word in ALL_WORDS {
                assert!(!line.contains(word), "{name} line holds a term: {line:?}");
            }
        }
    }
}

/// The exit code and standard output of a finished child.
pub fn code_and_stdout(out: &Output) -> (i32, String) {
    (
        out.status.code().expect("child exited with a code"),
        String::from_utf8(out.stdout.clone()).expect("stdout is UTF-8"),
    )
}
