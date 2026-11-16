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
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use super::lint;

/// Largest hook input read, in bytes.
pub const HOOK_INPUT_MAX_BYTES: u64 = 8 << 20;
/// Most lines of `brief-lint` a refusal forwards before the lint's summary.
pub const REFUSAL_MAX_LINES: usize = 50;

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
/// components against the root's canonical path.
///
/// A file that exists is under the root when its canonical path is. An
/// entry of the path that exists but cannot be canonicalized (a link whose
/// target is missing, a link loop), the file itself or a folder above it,
/// makes the file outside: where that link leads cannot be known. A file
/// that does not exist is placed by its nearest ancestor that does: that
/// ancestor's canonical path must be under the root, and the rest of the
/// path as written, from that ancestor on, must hold no `..` component. So
/// a missing file never counts as under the root through a `..` or a link
/// that leads out of it.
fn inside_root(file: &Path, root: &Path) -> Option<PathBuf> {
    let canon_root = fs::canonicalize(root).ok()?;
    if let Ok(canon) = fs::canonicalize(file) {
        return canon.starts_with(&canon_root).then_some(canon);
    }
    if fs::symlink_metadata(file).is_ok() {
        return None;
    }
    for ancestor in file.ancestors().skip(1) {
        let Ok(canon) = fs::canonicalize(ancestor) else {
            if fs::symlink_metadata(ancestor).is_ok() {
                return None;
            }
            continue;
        };
        if !canon.starts_with(&canon_root) {
            return None;
        }
        let rest = file.strip_prefix(ancestor).ok()?;
        if rest
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return None;
        }
        return Some(canon.join(rest));
    }
    None
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
        Verdict::Refused(Refused::Faults, bounded(report.lines()))
    }
}

/// The lint's lines as a refusal forwards them: at most `REFUSAL_MAX_LINES`
/// of the lines before the summary (faults first, then unchecked premises,
/// as the lint orders them), then one line that counts the rest, then the
/// summary, which carries the full counts.
fn bounded(mut lines: Vec<String>) -> Vec<String> {
    let summary = lines.pop();
    let more = lines.len().saturating_sub(REFUSAL_MAX_LINES);
    lines.truncate(REFUSAL_MAX_LINES);
    if more > 0 {
        let noun = if more == 1 { "line" } else { "lines" };
        lines.push(format!(
            "brief-gate: {more} more {noun} of brief-lint not shown"
        ));
    }
    lines.extend(summary);
    lines
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
    fn a_refusal_forwards_at_most_fifty_lines_before_the_summary() {
        let lines = |n: usize| -> Vec<String> {
            (0..n)
                .map(|i| format!("l{i}"))
                .chain(std::iter::once("s".to_owned()))
                .collect()
        };
        assert_eq!(bounded(lines(REFUSAL_MAX_LINES)), lines(REFUSAL_MAX_LINES));
        let mut want = lines(REFUSAL_MAX_LINES);
        want.pop();
        want.push("brief-gate: 1 more line of brief-lint not shown".to_owned());
        want.push("s".to_owned());
        assert_eq!(bounded(lines(REFUSAL_MAX_LINES + 1)), want);
        assert_eq!(REFUSAL_MAX_LINES, 50);
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
