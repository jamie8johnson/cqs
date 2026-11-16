//! `brief-gate`: the `Agent` hook. Reads the hook's JSON from standard
//! input, finds the task file the dispatch's prompt names on a line that
//! starts `Your task file is `, and runs `brief-lint` on it when it lies
//! under the brief root.
//!
//! Exit 0 lets the dispatch go on (nothing printed), 1 refuses it (first line
//! `brief-gate: refused: <reason>`), 2 means it was not checked (one line,
//! `brief-gate: not checked: <reason>`). Output invariant: every line is a
//! fixed string or a line of `brief-lint`, so no line holds the prompt's
//! text, a path, `"` or `\`.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use super::lint;

/// Largest hook input read, in bytes.
pub const HOOK_INPUT_MAX_BYTES: u64 = 8 << 20;

const TASK_FILE_LINE: &str = "Your task file is ";
const ROOT_VAR: &str = "CQS_BRIEF_ROOT";
const ROOT_KEY: &str = "cqs.briefRoot";

/// Why the gate did not check. The strings are the whole fixed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotChecked {
    Usage,
    InputTooLarge,
    NotJson,
    NoPrompt,
    NoRoot,
    GitFailed,
    RootNotAbsolute,
    RootNotFolder,
    OutsideRoot,
}

impl NotChecked {
    fn as_str(self) -> &'static str {
        match self {
            NotChecked::Usage => "usage error",
            NotChecked::InputTooLarge => "hook input over size limit",
            NotChecked::NotJson => "hook input is not JSON",
            NotChecked::NoPrompt => "no prompt in hook input",
            NotChecked::NoRoot => "no brief root configured",
            NotChecked::GitFailed => "git failed",
            NotChecked::RootNotAbsolute => "brief root is not an absolute path",
            NotChecked::RootNotFolder => "brief root is not a folder",
            NotChecked::OutsideRoot => "task file outside the brief root",
        }
    }
}

/// Why the gate refused. The strings are the whole fixed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refused {
    TwoTaskFiles,
    BadPath,
    Unreadable,
    Faults,
}

impl Refused {
    fn as_str(self) -> &'static str {
        match self {
            Refused::TwoTaskFiles => "more than one line starts Your task file is",
            Refused::BadPath => {
                "a line that starts Your task file is does not hold an absolute path ending in .md"
            }
            Refused::Unreadable => "the task file cannot be read",
            Refused::Faults => "the task file does not pass brief-lint",
        }
    }
}

/// The gate's verdict and the lines it prints.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Pass,
    Refused(Refused, Vec<String>),
    NotChecked(NotChecked),
}

fn emit(line: &str) {
    // A failed write to stdout leaves the exit code as the only signal.
    let _ = writeln!(io::stdout().lock(), "{line}");
}

/// Reads at most `HOOK_INPUT_MAX_BYTES` from `input`. An input that cannot
/// be read whole is not JSON the gate can parse.
fn read_input(mut input: impl Read) -> Result<Vec<u8>, NotChecked> {
    let mut buf = Vec::new();
    (&mut input)
        .take(HOOK_INPUT_MAX_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|_| NotChecked::NotJson)?;
    if buf.len() as u64 > HOOK_INPUT_MAX_BYTES {
        return Err(NotChecked::InputTooLarge);
    }
    Ok(buf)
}

/// The prompt at `tool_input.prompt`; the gate reads no other field.
fn prompt(input: &[u8]) -> Result<String, NotChecked> {
    let value: serde_json::Value =
        serde_json::from_slice(input).map_err(|_| NotChecked::NotJson)?;
    value
        .get("tool_input")
        .and_then(|t| t.get("prompt"))
        .and_then(|p| p.as_str())
        .map(str::to_owned)
        .ok_or(NotChecked::NoPrompt)
}

/// The rest of each prompt line that starts, after spaces and tabs, with
/// `Your task file is `.
fn task_file_lines(prompt: &str) -> Vec<&str> {
    prompt
        .lines()
        .filter_map(|line| {
            line.trim_start_matches([' ', '\t'])
                .strip_prefix(TASK_FILE_LINE)
        })
        .collect()
}

/// The path a task-file line names: trailing spaces removed, then one
/// trailing `.`, then one pair of enclosing backticks.
fn task_path(rest: &str) -> &str {
    let p = rest.trim_end_matches(' ');
    let p = p.strip_suffix('.').unwrap_or(p);
    p.strip_prefix('`')
        .and_then(|q| q.strip_suffix('`'))
        .unwrap_or(p)
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// The brief root: `CQS_BRIEF_ROOT` when set and not empty, else the git
/// config key, read in the current folder. Git's standard error is dropped.
fn brief_root() -> Result<PathBuf, NotChecked> {
    if let Some(v) = std::env::var_os(ROOT_VAR).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(v));
    }
    let out = Command::new("git")
        .args(["config", "--type=path", "--get", ROOT_KEY])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| NotChecked::GitFailed)?;
    match out.status.code() {
        Some(0) => {
            let mut bytes = out.stdout;
            while bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            Ok(path_from_bytes(bytes))
        }
        Some(1) => Err(NotChecked::NoRoot),
        _ => Err(NotChecked::GitFailed),
    }
}

/// The path to read when `file` lies under `root`, compared by path
/// components: the file's canonical path under the root's canonical path;
/// for a file that cannot be canonicalized, the path as written under the
/// root as configured or under its canonical path.
fn inside_root(file: &Path, root: &Path) -> Option<PathBuf> {
    let canon_root = fs::canonicalize(root).ok();
    match fs::canonicalize(file) {
        Ok(canon) => canon_root
            .is_some_and(|r| canon.starts_with(r))
            .then_some(canon),
        Err(_) => {
            let under = file.starts_with(root) || canon_root.is_some_and(|r| file.starts_with(r));
            under.then(|| file.to_path_buf())
        }
    }
}

/// The steps of the gate after its input is read, in order.
fn verdict(prompt: &str, root: impl FnOnce() -> Result<PathBuf, NotChecked>) -> Verdict {
    let lines = task_file_lines(prompt);
    if lines.is_empty() {
        return Verdict::Pass;
    }
    let root = match root() {
        Ok(r) => r,
        Err(r) => return Verdict::NotChecked(r),
    };
    if !root.is_absolute() {
        return Verdict::NotChecked(NotChecked::RootNotAbsolute);
    }
    if !root.is_dir() {
        return Verdict::NotChecked(NotChecked::RootNotFolder);
    }
    if lines.len() > 1 {
        return Verdict::Refused(Refused::TwoTaskFiles, Vec::new());
    }
    let path = task_path(lines[0]);
    let file = Path::new(path);
    if !file.is_absolute() || !path.ends_with(".md") {
        return Verdict::Refused(Refused::BadPath, Vec::new());
    }
    let Some(read_path) = inside_root(file, &root) else {
        return Verdict::NotChecked(NotChecked::OutsideRoot);
    };
    let text = match lint::read_brief(read_path.as_os_str()) {
        Ok(t) => t,
        Err(r) => return Verdict::Refused(Refused::Unreadable, vec![r.line()]),
    };
    let report = lint::lint(&text);
    if report.faults.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Refused(Refused::Faults, report.lines())
    }
}

pub fn run(args: &[OsString]) -> ExitCode {
    let v = if !args.is_empty() {
        Verdict::NotChecked(NotChecked::Usage)
    } else {
        match read_input(io::stdin().lock()).and_then(|input| prompt(&input)) {
            Ok(p) => verdict(&p, brief_root),
            Err(r) => Verdict::NotChecked(r),
        }
    };
    match v {
        Verdict::Pass => ExitCode::SUCCESS,
        Verdict::Refused(reason, lines) => {
            emit(&format!("brief-gate: refused: {}", reason.as_str()));
            for line in &lines {
                emit(line);
            }
            ExitCode::from(1)
        }
        Verdict::NotChecked(reason) => {
            emit(&format!("brief-gate: not checked: {}", reason.as_str()));
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_path_strips_spaces_then_a_dot_then_backticks() {
        assert_eq!(task_path("/a/b.md"), "/a/b.md");
        assert_eq!(task_path("/a/b.md.  "), "/a/b.md");
        assert_eq!(task_path("`/a/b.md`."), "/a/b.md");
        assert_eq!(task_path("`/a/b.md`"), "/a/b.md");
        assert_eq!(task_path("/a/b.md.."), "/a/b.md.");
        assert_eq!(task_path("`/a/b.md"), "`/a/b.md");
    }

    #[test]
    fn task_file_lines_start_after_spaces_and_tabs() {
        let prompt =
            "Do this.\n  \tYour task file is /a.md\nyour task file is /b.md\n Your task file is";
        assert_eq!(task_file_lines(prompt), ["/a.md"]);
    }

    #[test]
    fn no_task_file_line_passes_without_a_root() {
        assert_eq!(
            verdict("Do this.", || panic!("root looked up")),
            Verdict::Pass
        );
    }

    #[test]
    fn every_reason_is_free_of_quotes_and_backslashes() {
        let not_checked = [
            NotChecked::Usage,
            NotChecked::InputTooLarge,
            NotChecked::NotJson,
            NotChecked::NoPrompt,
            NotChecked::NoRoot,
            NotChecked::GitFailed,
            NotChecked::RootNotAbsolute,
            NotChecked::RootNotFolder,
            NotChecked::OutsideRoot,
        ];
        let refused = [
            Refused::TwoTaskFiles,
            Refused::BadPath,
            Refused::Unreadable,
            Refused::Faults,
        ];
        let all = not_checked
            .iter()
            .map(|r| r.as_str())
            .chain(refused.iter().map(|r| r.as_str()));
        for s in all {
            assert!(!s.contains('"') && !s.contains('\\'), "{s}");
        }
    }
}
