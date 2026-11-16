//! `hooks-check`: reports whether git runs this checkout's hooks from a
//! folder that holds every file of `.githooks/`, byte for byte.
//!
//! Exit 0 when it does, 1 when it does not (naming the folder and the
//! command that points git at `.githooks/`), 2 when git or `.githooks/`
//! cannot be read.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const SET_COMMAND: &str = "git config core.hooksPath .githooks";

fn emit(line: &str) {
    // A failed write to stdout leaves the exit code as the only signal.
    let _ = writeln!(io::stdout().lock(), "hooks-check: {line}");
}

/// Runs git in `dir` (or the current folder) and returns its standard output
/// with the exit code; standard error is dropped.
fn git(dir: Option<&Path>, args: &[&str]) -> Option<(Option<i32>, Vec<u8>)> {
    let mut cmd = Command::new("git");
    cmd.args(args).stdin(Stdio::null()).stderr(Stdio::null());
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let out = cmd.output().ok()?;
    Some((out.status.code(), out.stdout))
}

fn path_from_output(mut bytes: Vec<u8>) -> PathBuf {
    while bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    path_from_bytes(bytes)
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

/// The folder git runs hooks from: `core.hooksPath` (a relative value is
/// resolved against the top of the checkout), else git's own hooks folder.
fn hooks_dir(top: &Path) -> Option<PathBuf> {
    let (code, out) = git(
        Some(top),
        &["config", "--type=path", "--get", "core.hooksPath"],
    )?;
    let dir = match code {
        Some(0) => path_from_output(out),
        Some(1) => {
            let (code, out) = git(Some(top), &["rev-parse", "--git-path", "hooks"])?;
            if code != Some(0) {
                return None;
            }
            path_from_output(out)
        }
        _ => return None,
    };
    Some(if dir.is_absolute() {
        dir
    } else {
        top.join(dir)
    })
}

pub fn run(args: &[OsString]) -> ExitCode {
    if !args.is_empty() {
        emit("usage: hooks-check");
        return ExitCode::from(2);
    }
    let top = match git(None, &["rev-parse", "--show-toplevel"]) {
        Some((Some(0), out)) => path_from_output(out),
        _ => {
            emit("git failed");
            return ExitCode::from(2);
        }
    };
    let Some(dir) = hooks_dir(&top) else {
        emit("git failed");
        return ExitCode::from(2);
    };
    let source = top.join(".githooks");
    let Ok(read) = fs::read_dir(&source) else {
        emit(".githooks/ unreadable");
        return ExitCode::from(2);
    };
    let mut names = Vec::new();
    for entry in read {
        let Ok(entry) = entry else {
            emit(".githooks/ unreadable");
            return ExitCode::from(2);
        };
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            names.push(entry.file_name());
        }
    }
    names.sort();

    let mut faults = 0;
    for name in &names {
        let Ok(want) = fs::read(source.join(name)) else {
            emit(".githooks/ unreadable");
            return ExitCode::from(2);
        };
        let shown = name.to_string_lossy();
        match fs::read(dir.join(name)) {
            Ok(have) if have == want => {}
            Ok(_) => {
                faults += 1;
                emit(&format!("{shown}: differs in {}", dir.display()));
            }
            Err(_) => {
                faults += 1;
                emit(&format!("{shown}: missing from {}", dir.display()));
            }
        }
    }
    if faults == 0 {
        emit(&format!("ok: hooks run from {}", dir.display()));
        return ExitCode::SUCCESS;
    }
    emit(&format!(
        "git runs hooks from {}; to run the hooks of .githooks/: {SET_COMMAND}",
        dir.display()
    ));
    ExitCode::from(1)
}
