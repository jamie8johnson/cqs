//! `.claude/hooks/brief-gate.sh` and the settings entry that `CONTRIBUTING.md`
//! documents for it, run in a temporary checkout. The checkout holds a copy
//! of the script and, at `xtask/target/debug/xtask`, either a symlink to the
//! binary under test or a stand-in script that exits with a chosen code.

#![cfg(unix)]

mod common;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::Sandbox;

const NOT_RUN_PREFIX: &str = "brief-gate: not checked: ";

/// The gate's refusal of `fail-untagged-premise.md`.
const REFUSED_UNTAGGED: &str = "brief-gate: refused: the task file does not pass brief-lint\n\
     brief-lint: line 9: untagged-premise: premise has no [counted: ], [opened: ] or [unchecked: ] tag\n\
     brief-lint: line 11: unchecked premise\n\
     brief-lint: summary: faults=1 unchecked=1\n";

fn checkout_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent folder")
        .to_path_buf()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("brief")
        .join(name)
}

/// The JSON line the script prints when the gate did not check.
fn context_line(text: &str) -> String {
    format!(
        "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"additionalContext\":\"{text}\"}}}}\n"
    )
}

fn hook_json(prompt: &str) -> Vec<u8> {
    serde_json::json!({ "tool_input": { "prompt": prompt } })
        .to_string()
        .into_bytes()
}

/// A temporary checkout with the hook script and a brief root.
struct Checkout {
    sb: Sandbox,
    root: PathBuf,
    bin: PathBuf,
}

impl Checkout {
    fn new() -> Checkout {
        let sb = Sandbox::repo();
        let hooks = sb.repo.join(".claude").join("hooks");
        fs::create_dir_all(&hooks).expect("create hooks folder");
        fs::copy(
            checkout_root()
                .join(".claude")
                .join("hooks")
                .join("brief-gate.sh"),
            hooks.join("brief-gate.sh"),
        )
        .expect("copy hook script");
        let bin_dir = sb.repo.join("xtask").join("target").join("debug");
        fs::create_dir_all(&bin_dir).expect("create bin folder");
        let root = sb.outside.join("briefs");
        fs::create_dir_all(&root).expect("create root");
        Checkout {
            bin: bin_dir.join("xtask"),
            sb,
            root,
        }
    }

    /// Puts the binary under test where the script looks for it.
    fn with_real_binary(self) -> Checkout {
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_xtask"), &self.bin).expect("symlink");
        self
    }

    /// Puts a stand-in script where the script looks for the binary.
    fn with_stand_in(self, body: &str) -> Checkout {
        fs::write(&self.bin, format!("#!/bin/sh\n{body}\n")).expect("write stand-in");
        fs::set_permissions(&self.bin, fs::Permissions::from_mode(0o755)).expect("chmod");
        self
    }

    fn brief(&self, fixture_name: &str) -> PathBuf {
        let path = self.root.join(fixture_name);
        fs::copy(fixture(fixture_name), &path).expect("copy fixture");
        path
    }

    fn script(&self) -> PathBuf {
        self.sb
            .repo
            .join(".claude")
            .join("hooks")
            .join("brief-gate.sh")
    }
}

/// Runs `cmd` with `input` on standard input; stops it by its handle when it
/// has not ended within `limit`.
fn run(mut cmd: Command, input: Vec<u8>, limit: Duration) -> (Output, Duration) {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd.spawn().expect("start hook");
    let mut stdin = child.stdin.take().expect("child stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    while child.try_wait().expect("poll hook").is_none() {
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("hook did not end within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = writer.join();
    let out = child.wait_with_output().expect("collect hook");
    (out, start.elapsed())
}

/// `(exit code, stdout, stderr)`.
fn streams(out: &Output) -> (i32, String, String) {
    (
        out.status.code().expect("hook exited with a code"),
        String::from_utf8(out.stdout.clone()).expect("stdout is UTF-8"),
        String::from_utf8(out.stderr.clone()).expect("stderr is UTF-8"),
    )
}

/// Runs the script itself under `/bin/sh`, from the checkout's top.
fn run_script(co: &Checkout, input: Vec<u8>) -> ((i32, String, String), Duration) {
    let mut cmd = co.sb.command("/bin/sh");
    cmd.arg(co.script());
    let (out, took) = run(cmd, input, Duration::from_secs(20));
    (streams(&out), took)
}

/// The settings entry documented in `CONTRIBUTING.md`: the first fenced
/// `json` block after the heading of the `brief-gate` subsection.
fn documented_entry() -> serde_json::Value {
    let text = fs::read_to_string(checkout_root().join("CONTRIBUTING.md")).expect("read doc");
    let after = text
        .split_once("\n### `brief-gate`\n")
        .expect("brief-gate heading")
        .1;
    let block = after
        .split_once("\n```json\n")
        .expect("json block")
        .1
        .split_once("\n```")
        .expect("closing fence")
        .0;
    serde_json::from_str(block).expect("entry is JSON")
}

/// Runs the documented command with `sh -c` in `dir`.
fn run_entry(
    co: &Checkout,
    dir: &Path,
    project_dir: Option<&Path>,
    root: Option<&Path>,
    input: Vec<u8>,
) -> (i32, String, String) {
    let entry = documented_entry();
    let command = entry["hooks"][0]["command"]
        .as_str()
        .expect("command string");
    let mut cmd = co.sb.command("sh");
    cmd.arg("-c").arg(command).current_dir(dir);
    if let Some(p) = project_dir {
        cmd.env("CLAUDE_PROJECT_DIR", p);
    }
    if let Some(r) = root {
        cmd.env("CQS_BRIEF_ROOT", r);
    }
    let (out, _) = run(cmd, input, Duration::from_secs(20));
    streams(&out)
}

#[test]
fn the_documented_entry_runs_the_gate() {
    let entry = documented_entry();
    assert_eq!(entry["matcher"], "Agent");
    assert_eq!(entry["hooks"][0]["type"], "command");
    assert_eq!(entry["hooks"][0]["timeout"], 10);
    assert_eq!(entry["hooks"].as_array().map(Vec::len), Some(1));
    assert!(entry.get("env").is_none() && entry["hooks"][0].get("env").is_none());

    let co = Checkout::new().with_real_binary();
    let fail = co.brief("fail-untagged-premise.md");
    let pass = co.brief("pass.md");
    let top = co.sb.repo.clone();
    let names = |p: &Path| hook_json(&format!("Your task file is {}", p.display()));

    let refused = run_entry(&co, &top, Some(&top), Some(&co.root), names(&fail));
    assert_eq!(refused, (2, String::new(), REFUSED_UNTAGGED.to_owned()));

    let passed = run_entry(&co, &top, Some(&top), Some(&co.root), names(&pass));
    assert_eq!(passed, (0, String::new(), String::new()));

    let (code, stdout, stderr) = run_entry(&co, &top, Some(&top), None, names(&pass));
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert_eq!(stdout.lines().count(), 1);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("one JSON line");
    assert_eq!(
        parsed,
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": "brief-gate: not checked: no brief root configured"
            }
        })
    );
}

#[test]
fn the_documented_entry_from_a_subfolder_without_a_project_folder_lets_the_dispatch_go_on() {
    let co = Checkout::new().with_real_binary();
    let sub = co.sb.repo.join("sub");
    fs::create_dir_all(&sub).expect("create subfolder");
    let fail = co.brief("fail-untagged-premise.md");
    let input = hook_json(&format!("Your task file is {}", fail.display()));
    let (code, stdout, stderr) = run_entry(&co, &sub, None, Some(&co.root), input.clone());
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (
            0,
            context_line("brief-gate: not checked: the hook script was not found").as_str(),
            ""
        )
    );
    // The same folder with the project folder set finds the script.
    let top = co.sb.repo.clone();
    let refused = run_entry(&co, &sub, Some(&top), Some(&co.root), input);
    assert_eq!(refused, (2, String::new(), REFUSED_UNTAGGED.to_owned()));
}

#[test]
fn a_missing_binary_lets_the_dispatch_go_on() {
    let co = Checkout::new();
    let ((code, stdout, stderr), _) = run_script(&co, Vec::new());
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (
            0,
            context_line("brief-gate: not checked: the xtask binary is not built").as_str(),
            ""
        )
    );
}

#[test]
fn a_stand_in_pass_is_silent() {
    let co = Checkout::new().with_stand_in("exit 0");
    let ((code, stdout, stderr), _) = run_script(&co, Vec::new());
    assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "", ""));
}

#[test]
fn a_refusal_with_the_prefix_blocks_with_its_lines_on_standard_error() {
    let co = Checkout::new()
        .with_stand_in("echo 'brief-gate: refused: a reason'; echo 'second line'; exit 1");
    let ((code, stdout, stderr), _) = run_script(&co, Vec::new());
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (2, "", "brief-gate: refused: a reason\nsecond line\n")
    );
}

#[test]
fn a_not_checked_line_is_handed_to_the_session() {
    let co = Checkout::new()
        .with_stand_in("echo 'brief-gate: not checked: a reason'; echo 'more'; exit 2");
    let ((code, stdout, stderr), _) = run_script(&co, Vec::new());
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (
            0,
            context_line("brief-gate: not checked: a reason").as_str(),
            ""
        )
    );
}

#[test]
fn an_exit_without_the_gates_prefix_lets_the_dispatch_go_on() {
    let cases = [
        (
            "echo 'xtask: usage: xtask <private-scan|hooks-check> [args]'; exit 2",
            2,
        ),
        ("echo 'refused: no prefix'; exit 1", 1),
        ("echo 'panicked'; exit 101", 101),
        ("exit 3", 3),
    ];
    for (body, exit) in cases {
        let co = Checkout::new().with_stand_in(body);
        let ((code, stdout, stderr), _) = run_script(&co, Vec::new());
        assert_eq!(
            (code, stdout.as_str(), stderr.as_str()),
            (
                0,
                context_line(&format!(
                    "{NOT_RUN_PREFIX}the gate did not run (exit {exit})"
                ))
                .as_str(),
                ""
            ),
            "{body}"
        );
    }
}

#[test]
fn a_gate_that_runs_past_the_time_limit_lets_the_dispatch_go_on() {
    let co = Checkout::new().with_stand_in("sleep 30");
    let ((code, stdout, stderr), took) = run_script(&co, Vec::new());
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (
            0,
            context_line("brief-gate: not checked: the gate did not run (exit 124)").as_str(),
            ""
        )
    );
    assert!(took < Duration::from_secs(12), "took {took:?}");
}

#[test]
fn a_gate_that_ignores_the_first_signal_is_killed_a_second_later() {
    // The ignored TERM is inherited by `sleep`, so only the KILL that
    // `timeout -k 1` sends a second after it ends the stand-in.
    let co = Checkout::new().with_stand_in("trap '' TERM\nsleep 30");
    let ((code, stdout, stderr), took) = run_script(&co, Vec::new());
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (
            0,
            context_line("brief-gate: not checked: the gate did not run (exit 137)").as_str(),
            ""
        )
    );
    assert!(took < Duration::from_secs(12), "took {took:?}");
}

#[test]
fn a_not_checked_line_that_json_cannot_hold_is_not_shown() {
    let hidden = context_line("brief-gate: not checked: the gate's line could not be shown");
    for body in [
        r#"echo 'brief-gate: not checked: a "quoted" reason'; exit 2"#,
        r#"echo 'brief-gate: not checked: a \ reason'; exit 2"#,
        r#"printf 'brief-gate: not checked: a\ttab\n'; exit 2"#,
    ] {
        let co = Checkout::new().with_stand_in(body);
        let ((code, stdout, stderr), _) = run_script(&co, Vec::new());
        assert_eq!(
            (code, stdout.as_str(), stderr.as_str()),
            (0, hidden.as_str(), ""),
            "{body}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
        assert_eq!(
            parsed["hookSpecificOutput"]["additionalContext"],
            "brief-gate: not checked: the gate's line could not be shown"
        );
    }
    // Without the gate's prefix the line is no gate's line at all.
    let co = Checkout::new().with_stand_in(r#"echo 'a "quoted" line'; exit 2"#);
    let ((code, stdout, _), _) = run_script(&co, Vec::new());
    assert_eq!(
        (code, stdout.as_str()),
        (
            0,
            context_line("brief-gate: not checked: the gate did not run (exit 2)").as_str()
        )
    );
}

/// A folder holding links to the two programs the script needs besides
/// the shell's own commands, and nothing else: no `timeout`.
fn path_without_timeout(co: &Checkout) -> PathBuf {
    let dir = co.sb.outside.join("bin-no-timeout");
    fs::create_dir_all(&dir).expect("create bin folder");
    let path = std::env::var_os("PATH").expect("PATH is set");
    for program in ["dirname", "head"] {
        let found = std::env::split_paths(&path)
            .map(|d| d.join(program))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("{program} on PATH"));
        std::os::unix::fs::symlink(found, dir.join(program)).expect("symlink");
    }
    dir
}

#[test]
fn the_script_runs_the_gate_when_timeout_is_not_on_the_path() {
    let co = Checkout::new().with_real_binary();
    let bin_dir = path_without_timeout(&co);
    let fail = co.brief("fail-untagged-premise.md");
    let pass = co.brief("pass.md");
    for (brief, want) in [
        (&fail, (2, String::new(), REFUSED_UNTAGGED.to_owned())),
        (&pass, (0, String::new(), String::new())),
    ] {
        let mut cmd = co.sb.command("/bin/sh");
        cmd.arg(co.script())
            .env("PATH", &bin_dir)
            .env("CQS_BRIEF_ROOT", &co.root);
        let input = hook_json(&format!("Your task file is {}", brief.display()));
        let (out, _) = run(cmd, input, Duration::from_secs(20));
        assert_eq!(streams(&out), want, "{}", brief.display());
    }
}
