//! The hooks of `.githooks/`, run as `/bin/sh <hook> [args]` in a temporary
//! repository with a stand-in `cargo` first on `PATH`. The stand-in logs its
//! arguments, prints one line, and exits with the code the test asks for, so
//! these tests show what each hook does with each exit code of the scan
//! without running the scan.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{code_and_stdout, Sandbox};

const STAND_IN: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$STANDIN_LOG"
case "$1" in
    run) echo "private-scan: stand-in output"; exit "${STANDIN_RUN_EXIT:-0}" ;;
    test) echo "stand-in test output"; exit "${STANDIN_TEST_EXIT:-0}" ;;
    *) exit 0 ;;
esac
"#;

const FMT_PASSED: &str = "Running cargo fmt --check...\nFormat check passed.\n";
const COULD_NOT_RUN: &str = "private-scan: not checked: could not run\n";
const STAND_IN_LINE: &str = "private-scan: stand-in output\n";

fn checkout_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent folder")
        .to_path_buf()
}

fn hook(name: &str) -> PathBuf {
    checkout_root().join(".githooks").join(name)
}

struct HookRun {
    code: i32,
    stdout: String,
    log: Vec<String>,
}

/// A temporary repository with a stand-in `cargo`.
struct Fixture {
    sb: Sandbox,
    bin: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let sb = Sandbox::repo();
        let bin = sb.outside.join("bin");
        fs::create_dir_all(&bin).expect("create bin dir");
        let cargo = bin.join("cargo");
        fs::write(&cargo, STAND_IN).expect("write stand-in cargo");
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).expect("chmod stand-in");
        let log = sb.outside.join("cargo.log");
        Fixture { sb, bin, log }
    }

    fn stage(&self, rel: &str) {
        self.sb.write(rel, "content\n");
        self.sb.git(&["add", rel]);
    }

    fn run(&self, hook_name: &str, args: &[&str], run_exit: i32, test_exit: i32) -> HookRun {
        let _ = fs::remove_file(&self.log);
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd: Command = self.sb.command("/bin/sh");
        cmd.arg(hook(hook_name))
            .args(args)
            .env("PATH", path)
            .env("STANDIN_LOG", &self.log)
            .env("STANDIN_RUN_EXIT", run_exit.to_string())
            .env("STANDIN_TEST_EXIT", test_exit.to_string());
        let out = cmd.output().expect("run hook");
        let (code, stdout) = code_and_stdout(&out);
        let log = fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        HookRun { code, stdout, log }
    }

    fn scan_call(&self, scan_args: &str) -> String {
        let top = self.sb.repo.display();
        format!(
            "run --quiet --manifest-path {top}/xtask/Cargo.toml --target-dir {top}/xtask/target --locked -- private-scan {scan_args}"
        )
    }
}

#[test]
fn pre_commit_acts_on_each_exit_code_of_the_scan() {
    for code in [0, 1, 2, 101] {
        let fx = Fixture::new();
        fx.stage("a.txt");
        let run = fx.run("pre-commit", &[], code, 0);
        let scan_call = fx.scan_call("--staged");
        match code {
            0 | 2 => {
                assert_eq!(run.code, 0, "scan exit {code}");
                assert_eq!(
                    run.stdout,
                    format!("{STAND_IN_LINE}{FMT_PASSED}"),
                    "scan exit {code}"
                );
                assert_eq!(run.log, vec![scan_call, "fmt --check".to_owned()]);
            }
            1 => {
                assert_eq!(run.code, 1, "scan exit {code}");
                assert_eq!(
                    run.stdout,
                    format!(
                        "{STAND_IN_LINE}Commit stopped: private-scan found a private term in the staged change.\n"
                    )
                );
                assert_eq!(run.log, vec![scan_call]);
            }
            _ => {
                assert_eq!(run.code, 0, "scan exit {code}");
                assert_eq!(run.stdout, format!("{COULD_NOT_RUN}{FMT_PASSED}"));
                assert_eq!(run.log, vec![scan_call, "fmt --check".to_owned()]);
            }
        }
    }
}

#[test]
fn commit_msg_acts_on_each_exit_code_of_the_scan() {
    for code in [0, 1, 2, 101] {
        let fx = Fixture::new();
        let msg = fx.sb.write(".git/COMMIT_EDITMSG", "Subject\n");
        let msg_arg = msg.to_str().expect("UTF-8 temp path");
        let run = fx.run("commit-msg", &[msg_arg], code, 0);
        assert_eq!(run.log, vec![fx.scan_call(&format!("--message {msg_arg}"))]);
        match code {
            0 | 2 => {
                assert_eq!(run.code, 0, "scan exit {code}");
                assert_eq!(run.stdout, STAND_IN_LINE, "scan exit {code}");
            }
            1 => {
                assert_eq!(run.code, 1, "scan exit {code}");
                assert_eq!(
                    run.stdout,
                    format!(
                        "{STAND_IN_LINE}Commit stopped: private-scan found a private term in the commit message.\n"
                    )
                );
            }
            _ => {
                assert_eq!(run.code, 0, "scan exit {code}");
                assert_eq!(run.stdout, COULD_NOT_RUN);
            }
        }
    }
}

#[test]
fn pre_commit_stops_when_the_env_var_docs_test_fails() {
    let fx = Fixture::new();
    fx.stage("README.md");
    let run = fx.run("pre-commit", &[], 0, 1);
    assert_eq!(run.code, 1);
    assert_eq!(
        run.stdout,
        format!(
            "{STAND_IN_LINE}{FMT_PASSED}Running cargo test --test env_var_docs...\n\
             stand-in test output\n\
             \n\
             Error: env_var_docs test failed. Either:\n  \
             - add the missing CQS_* var to the env-var table in README.md, or\n  \
             - if it's a test-only fixture, name it CQS_TEST_* (allowlisted).\n"
        )
    );
}

#[test]
fn pre_commit_passes_when_the_env_var_docs_test_passes() {
    let fx = Fixture::new();
    fx.stage("README.md");
    let run = fx.run("pre-commit", &[], 0, 0);
    assert_eq!(run.code, 0);
    assert_eq!(
        run.stdout,
        format!(
            "{STAND_IN_LINE}{FMT_PASSED}Running cargo test --test env_var_docs...\n\
             env_var_docs check passed.\n"
        )
    );
}

#[test]
fn pre_commit_lock_check_runs_for_the_root_manifests_only() {
    for (staged, checks) in [
        ("xtask/Cargo.toml", false),
        ("docs/Cargo.toml.md", false),
        ("Cargo.toml", true),
        ("cqs-macros/Cargo.toml", true),
    ] {
        let fx = Fixture::new();
        fx.stage(staged);
        let run = fx.run("pre-commit", &[], 0, 0);
        assert_eq!(run.code, 0, "{staged}");
        let mut want_log = vec![fx.scan_call("--staged"), "fmt --check".to_owned()];
        let mut want_out = format!("{STAND_IN_LINE}{FMT_PASSED}");
        if checks {
            want_log.push("check --quiet".to_owned());
            want_out.push_str("Checking Cargo.lock sync...\n");
        }
        assert_eq!(run.log, want_log, "{staged}");
        assert_eq!(run.stdout, want_out, "{staged}");
    }
}

#[test]
fn both_hooks_are_executable_in_the_index() {
    let sb = Sandbox::plain();
    let mut cmd = sb.command("git");
    cmd.current_dir(checkout_root()).args([
        "ls-files",
        "-s",
        "--",
        ".githooks/commit-msg",
        ".githooks/pre-commit",
    ]);
    let out = cmd.output().expect("run git ls-files");
    let (code, stdout) = code_and_stdout(&out);
    assert_eq!(code, 0);
    let modes: Vec<(&str, &str)> = stdout
        .lines()
        .map(|l| {
            let mode = l.split(' ').next().unwrap_or("");
            let path = l.split('\t').nth(1).unwrap_or("");
            (path, mode)
        })
        .collect();
    assert_eq!(
        modes,
        vec![
            (".githooks/commit-msg", "100755"),
            (".githooks/pre-commit", "100755")
        ]
    );
}
