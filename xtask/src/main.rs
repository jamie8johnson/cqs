//! Developer tasks for the cqs repository.
//!
//! A task runs as
//! `cargo run --quiet --manifest-path xtask/Cargo.toml --locked -- <task> [args]`.
//! Each task lives in one source file of its own, or one folder for a family of tasks.

mod brief;
mod hooks_check;
mod private_scan;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

const USAGE: &str = "xtask: usage: xtask <private-scan|hooks-check|brief-lint|brief-gate> [args]";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let task = args.next();
    let rest: Vec<OsString> = args.collect();
    match task.as_deref().and_then(|t| t.to_str()) {
        Some("private-scan") => private_scan::run(&rest),
        Some("hooks-check") => hooks_check::run(&rest),
        Some("brief-lint") => brief::lint::run(&rest),
        Some("brief-gate") => brief::gate::run(&rest),
        // The task name is never echoed: it is an argument's text.
        _ => {
            let _ = writeln!(std::io::stdout().lock(), "{USAGE}");
            ExitCode::from(2)
        }
    }
}
