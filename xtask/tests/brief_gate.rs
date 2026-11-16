//! `brief-gate` run as a child with hook JSON written by the tests on its
//! standard input. Each test asserts the exit code and the whole standard
//! output, and every run is checked for a `"` or a `\` in its output.

mod common;

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{code_and_stdout, Sandbox};

/// Keep equal to `HOOK_INPUT_MAX_BYTES` in `src/brief/gate.rs`.
const HOOK_INPUT_MAX_BYTES: usize = 8 << 20;

const NO_ROOT: &str = "brief-gate: not checked: no brief root configured\n";
const OUTSIDE: &str = "brief-gate: not checked: task file outside the brief root\n";
const TWO_LINES: &str = "brief-gate: refused: more than one line starts Your task file is\n";
const BAD_PATH: &str = "brief-gate: refused: a line that starts Your task file is does not hold an absolute path ending in .md\n";
const UNREADABLE: &str = "brief-gate: refused: the task file cannot be read\n";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("brief")
        .join(name)
}

/// The output of a refusal for `fail-untagged-premise.md`.
fn refused_untagged() -> String {
    "brief-gate: refused: the task file does not pass brief-lint\n\
     brief-lint: line 9: untagged-premise: premise has no [counted: ], [opened: ] or [unchecked: ] tag\n\
     brief-lint: line 11: unchecked premise\n\
     brief-lint: summary: faults=1 unchecked=1\n"
        .to_owned()
}

/// The hook input of a dispatch whose prompt is `prompt`.
fn hook_json(prompt: &str) -> Vec<u8> {
    serde_json::json!({
        "session_id": "s",
        "hook_event_name": "PreToolUse",
        "tool_name": "Agent",
        "tool_input": { "description": "d", "prompt": prompt }
    })
    .to_string()
    .into_bytes()
}

/// A prompt that names `path` as its task file.
fn naming(path: &Path) -> String {
    format!("Do the work.\nYour task file is {}\n", path.display())
}

/// A sandbox repository with a brief root folder under `outside`.
struct Fixture {
    sb: Sandbox,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let sb = Sandbox::repo();
        let root = sb.outside.join("briefs");
        fs::create_dir_all(&root).expect("create root");
        Fixture { sb, root }
    }

    /// Copies a fixture into the root under `name`.
    fn brief(&self, fixture_name: &str, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::copy(fixture(fixture_name), &path).expect("copy fixture");
        path
    }

    fn pass(&self) -> PathBuf {
        self.brief("pass.md", "pass.md")
    }

    fn fail(&self) -> PathBuf {
        self.brief("fail-untagged-premise.md", "fail.md")
    }
}

/// Runs `cmd` with `input` on standard input; stops it by its handle when it
/// has not ended within `limit`.
fn run_with_input(mut cmd: Command, input: Vec<u8>, limit: Duration) -> (i32, String) {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("start xtask");
    let mut stdin = child.stdin.take().expect("child stdin");
    let writer = std::thread::spawn(move || {
        // The gate may stop reading at its cap; a broken pipe is expected.
        let _ = stdin.write_all(&input);
    });
    let start = Instant::now();
    while child.try_wait().expect("poll xtask").is_none() {
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("brief-gate did not end within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = writer.join();
    let out = child.wait_with_output().expect("collect xtask");
    for stream in [&out.stdout, &out.stderr] {
        let text = String::from_utf8_lossy(stream);
        assert!(
            !text.contains('"') && !text.contains('\\'),
            "output holds a quote or a backslash: {text:?}"
        );
    }
    code_and_stdout(&out)
}

fn gate_cmd(sb: &Sandbox, root: Option<&OsStr>) -> Command {
    let mut cmd = sb.command(env!("CARGO_BIN_EXE_xtask"));
    cmd.arg("brief-gate");
    if let Some(root) = root {
        cmd.env("CQS_BRIEF_ROOT", root);
    }
    cmd
}

/// Runs the gate with `CQS_BRIEF_ROOT` set to `root` (when given).
fn gate(sb: &Sandbox, root: Option<&Path>, input: Vec<u8>) -> (i32, String) {
    run_with_input(
        gate_cmd(sb, root.map(Path::as_os_str)),
        input,
        Duration::from_secs(10),
    )
}

#[test]
fn the_sandbox_removes_the_brief_root_and_the_project_folder() {
    let sb = Sandbox::plain();
    let cmd = sb.command("x");
    let envs: Vec<(&OsStr, Option<&OsStr>)> = cmd.get_envs().collect();
    for name in ["CQS_BRIEF_ROOT", "CLAUDE_PROJECT_DIR"] {
        assert!(
            envs.contains(&(OsStr::new(name), None)),
            "{name} is not removed"
        );
    }
}

// --- step 1 and 2: arguments and input --------------------------------------

#[test]
fn an_argument_is_a_usage_error() {
    let fx = Fixture::new();
    let mut cmd = gate_cmd(&fx.sb, Some(fx.root.as_os_str()));
    cmd.arg("extra");
    let (code, out) = run_with_input(cmd, hook_json("x"), Duration::from_secs(10));
    assert_eq!(code, 2);
    assert_eq!(out, "brief-gate: not checked: usage error\n");
}

#[test]
fn input_one_byte_over_the_cap_is_not_checked() {
    let fx = Fixture::new();
    let (code, out) = gate(&fx.sb, Some(&fx.root), vec![b' '; HOOK_INPUT_MAX_BYTES + 1]);
    assert_eq!(code, 2);
    assert_eq!(out, "brief-gate: not checked: hook input over size limit\n");
    // At the cap the input is read: blanks around a JSON object.
    let mut at_cap = hook_json("Do the work.");
    at_cap.resize(HOOK_INPUT_MAX_BYTES, b' ');
    let (code, out) = gate(&fx.sb, Some(&fx.root), at_cap);
    assert_eq!((code, out.as_str()), (0, ""));
}

#[test]
fn input_that_is_not_json_is_not_checked() {
    let fx = Fixture::new();
    for input in [
        &b"Your task file is /a.md"[..],
        b"{\"tool_input\":",
        b"\xff\xfe",
    ] {
        let (code, out) = gate(&fx.sb, Some(&fx.root), input.to_vec());
        assert_eq!(code, 2);
        assert_eq!(out, "brief-gate: not checked: hook input is not JSON\n");
    }
}

#[test]
fn input_with_no_prompt_string_is_not_checked() {
    let fx = Fixture::new();
    for input in [
        r#"{}"#,
        r#"{"tool_input":{}}"#,
        r#"{"tool_input":{"prompt":7}}"#,
        r#"{"tool_input":"Your task file is /a.md"}"#,
        r#"{"prompt":"Your task file is /a.md"}"#,
    ] {
        let (code, out) = gate(&fx.sb, Some(&fx.root), input.as_bytes().to_vec());
        assert_eq!(code, 2, "{input}");
        assert_eq!(out, "brief-gate: not checked: no prompt in hook input\n");
    }
}

// --- step 3: no task file ---------------------------------------------------

#[test]
fn a_prompt_with_no_task_file_line_passes_silently() {
    let fx = Fixture::new();
    for root in [None, Some(fx.root.as_path())] {
        let prompt = "Do the work.\nThe task file is elsewhere.\n  your task file is /a.md\n";
        let (code, out) = gate(&fx.sb, root, hook_json(prompt));
        assert_eq!((code, out.as_str()), (0, ""), "{root:?}");
    }
}

// --- step 4: the root, looked up before any refusal -------------------------

#[test]
fn no_root_is_not_checked_whatever_the_task_file_lines_hold() {
    let fx = Fixture::new();
    let pass = fx.pass();
    let fail = fx.fail();
    let prompts = [
        naming(&pass),
        naming(&fail),
        format!("{}{}", naming(&pass), naming(&fail)),
        "Your task file is briefs/pass.md\n".to_owned(),
    ];
    for prompt in &prompts {
        let (code, out) = gate(&fx.sb, None, hook_json(prompt));
        assert_eq!(code, 2, "{prompt}");
        assert_eq!(out, NO_ROOT, "{prompt}");
    }
}

#[test]
fn the_root_comes_from_the_git_config_key_alone() {
    let fx = Fixture::new();
    fx.sb
        .git(&["config", "cqs.briefRoot", fx.root.to_str().expect("UTF-8")]);
    let (code, out) = gate(&fx.sb, None, hook_json(&naming(&fx.fail())));
    assert_eq!(code, 1);
    assert_eq!(out, refused_untagged());
    let (code, out) = gate(&fx.sb, None, hook_json(&naming(&fx.pass())));
    assert_eq!((code, out.as_str()), (0, ""));
}

#[test]
fn the_variable_holds_over_the_git_config_key() {
    let fx = Fixture::new();
    let key_root = fx.sb.outside.join("key-briefs");
    fs::create_dir_all(&key_root).expect("create key root");
    fs::copy(
        fixture("fail-untagged-premise.md"),
        key_root.join("fail.md"),
    )
    .expect("copy");
    fx.sb
        .git(&["config", "cqs.briefRoot", key_root.to_str().expect("UTF-8")]);
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&fx.fail())));
    assert_eq!(code, 1);
    assert_eq!(out, refused_untagged());
    let in_key_root = key_root.join("fail.md");
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&in_key_root)));
    assert_eq!(code, 2);
    assert_eq!(out, OUTSIDE);
}

#[test]
fn an_empty_variable_falls_back_to_the_git_config_key() {
    let fx = Fixture::new();
    fx.sb
        .git(&["config", "cqs.briefRoot", fx.root.to_str().expect("UTF-8")]);
    let mut cmd = gate_cmd(&fx.sb, None);
    cmd.env("CQS_BRIEF_ROOT", "");
    let (code, out) = run_with_input(cmd, hook_json(&naming(&fx.fail())), Duration::from_secs(10));
    assert_eq!(code, 1);
    assert_eq!(out, refused_untagged());
}

#[test]
fn a_git_config_that_git_cannot_read_is_git_failed() {
    let fx = Fixture::new();
    fs::write(fx.sb.repo.join(".git").join("config"), "[[[ not a config\n")
        .expect("break the config");
    let (code, out) = gate(&fx.sb, None, hook_json(&naming(&fx.fail())));
    assert_eq!(code, 2);
    assert_eq!(out, "brief-gate: not checked: git failed\n");
}

#[test]
fn a_relative_root_is_not_checked() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.sb.repo.join("briefs")).expect("create relative root");
    let fail = fx.fail();
    for prompt in [naming(&fail), format!("{}{}", naming(&fail), naming(&fail))] {
        let (code, out) = gate(&fx.sb, Some(Path::new("briefs")), hook_json(&prompt));
        assert_eq!(code, 2);
        assert_eq!(
            out,
            "brief-gate: not checked: brief root is not an absolute path\n"
        );
    }
}

#[test]
fn a_root_that_is_not_a_folder_is_not_checked() {
    let fx = Fixture::new();
    let fail = fx.fail();
    for root in [fail.clone(), fx.sb.outside.join("nosuch")] {
        let (code, out) = gate(&fx.sb, Some(&root), hook_json(&naming(&fail)));
        assert_eq!(code, 2);
        assert_eq!(out, "brief-gate: not checked: brief root is not a folder\n");
    }
}

// --- step 5 and 6: the task-file line ---------------------------------------

#[test]
fn two_task_file_lines_are_refused() {
    let fx = Fixture::new();
    let pass = fx.pass();
    let prompt = format!("{}\t{}", naming(&pass), naming(&pass));
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&prompt));
    assert_eq!(code, 1);
    assert_eq!(out, TWO_LINES);
}

#[test]
fn a_path_that_is_relative_or_not_md_is_refused() {
    let fx = Fixture::new();
    let pass = fx.pass();
    let not_md = fx.root.join("pass.txt");
    fs::copy(&pass, &not_md).expect("copy");
    for prompt in [
        "Your task file is briefs/pass.md".to_owned(),
        "Your task file is pass.md".to_owned(),
        naming(&not_md),
        // One trailing dot is removed, so two leave a path ending `.md.`.
        format!("Your task file is {}..", pass.display()),
        "Your task file is ".to_owned(),
    ] {
        let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&prompt));
        assert_eq!(code, 1, "{prompt}");
        assert_eq!(out, BAD_PATH, "{prompt}");
    }
}

#[test]
fn a_path_in_backticks_with_a_trailing_dot_is_read() {
    let fx = Fixture::new();
    let fail = fx.fail();
    let prompt = format!("Your task file is `{}`.  \r\nGo.", fail.display());
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&prompt));
    assert_eq!(code, 1);
    assert_eq!(out, refused_untagged());
}

// --- step 7: inside the root ------------------------------------------------

#[test]
fn a_file_outside_the_root_is_not_checked() {
    let fx = Fixture::new();
    let elsewhere = fx.sb.outside.join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("create folder");
    let outside_file = elsewhere.join("fail.md");
    fs::copy(fixture("fail-untagged-premise.md"), &outside_file).expect("copy");
    let sibling = fx.sb.outside.join("briefs2");
    fs::create_dir_all(&sibling).expect("create sibling");
    let sibling_file = sibling.join("a.md");
    fs::copy(fixture("fail-untagged-premise.md"), &sibling_file).expect("copy");
    let dotdot = fx.root.join("..").join("elsewhere").join("fail.md");
    for path in [
        outside_file,
        sibling_file,
        elsewhere.join("nosuch.md"),
        dotdot,
    ] {
        let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&path)));
        assert_eq!(code, 2, "{}", path.display());
        assert_eq!(out, OUTSIDE, "{}", path.display());
    }
}

#[cfg(unix)]
#[test]
fn a_missing_file_is_placed_by_its_nearest_existing_ancestor() {
    let fx = Fixture::new();
    let elsewhere = fx.sb.outside.join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("create folder");
    fs::create_dir_all(fx.root.join("sub")).expect("create sub");
    std::os::unix::fs::symlink(&elsewhere, fx.root.join("dirlink")).expect("symlink");
    let outside = [
        fx.root.join("..").join("elsewhere").join("nosuch.md"),
        fx.root
            .join("sub")
            .join("..")
            .join("..")
            .join("elsewhere")
            .join("nosuch.md"),
        fx.root.join("dirlink").join("nosuch.md"),
        // The parent is missing too, and the nearest ancestor that exists
        // lies outside the root.
        fx.root.join("..").join("x").join("y").join("a.md"),
        // The nearest ancestor that exists is the root, and the rest of the
        // path holds `..`.
        fx.root.join("nosuch-dir").join("..").join("a.md"),
    ];
    for path in &outside {
        let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(path)));
        assert_eq!(code, 2, "{}", path.display());
        assert_eq!(out, OUTSIDE, "{}", path.display());
    }
    let inside = [
        fx.root.join("nosuch-dir").join("a.md"),
        fx.root.join("sub").join("nosuch.md"),
    ];
    for path in &inside {
        let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(path)));
        assert_eq!(code, 1, "{}", path.display());
        assert_eq!(
            out,
            format!("{UNREADABLE}brief-lint: not checked: file unreadable\n"),
            "{}",
            path.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_inside_the_root_to_a_file_outside_is_outside() {
    let fx = Fixture::new();
    let target = fx.sb.outside.join("fail.md");
    fs::copy(fixture("fail-untagged-premise.md"), &target).expect("copy");
    let link = fx.root.join("link.md");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&link)));
    assert_eq!(code, 2);
    assert_eq!(out, OUTSIDE);
}

#[cfg(unix)]
#[test]
fn a_root_named_through_a_symlink_holds_its_files() {
    let fx = Fixture::new();
    let link_root = fx.sb.outside.join("root-link");
    std::os::unix::fs::symlink(&fx.root, &link_root).expect("symlink");
    let fail = fx.fail();
    for named in [link_root.join("fail.md"), fail] {
        let (code, out) = gate(&fx.sb, Some(&link_root), hook_json(&naming(&named)));
        assert_eq!(code, 1, "{}", named.display());
        assert_eq!(out, refused_untagged());
    }
    for missing in [link_root.join("nosuch.md"), fx.root.join("nosuch.md")] {
        let (code, out) = gate(&fx.sb, Some(&link_root), hook_json(&naming(&missing)));
        assert_eq!(code, 1, "{}", missing.display());
        assert_eq!(
            out,
            format!("{UNREADABLE}brief-lint: not checked: file unreadable\n")
        );
    }
}

// --- step 8: the lint's read ------------------------------------------------

#[test]
fn a_missing_file_inside_the_root_is_refused_as_unreadable() {
    let fx = Fixture::new();
    let missing = fx.root.join("nosuch.md");
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&missing)));
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!("{UNREADABLE}brief-lint: not checked: file unreadable\n")
    );
}

#[cfg(unix)]
#[test]
fn a_named_pipe_inside_the_root_is_refused_without_being_opened() {
    let fx = Fixture::new();
    let fifo = fx.root.join("pipe.md");
    let made = fx
        .sb
        .command("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed");
    let (code, out) = run_with_input(
        gate_cmd(&fx.sb, Some(fx.root.as_os_str())),
        hook_json(&naming(&fifo)),
        Duration::from_secs(5),
    );
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!("{UNREADABLE}brief-lint: not checked: file unreadable\n")
    );
}

#[test]
fn a_file_over_the_cap_is_refused() {
    let fx = Fixture::new();
    let big = fx.root.join("big.md");
    fs::write(&big, vec![b'a'; (1 << 20) + 1]).expect("write big file");
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&big)));
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!("{UNREADABLE}brief-lint: not checked: file over size limit\n")
    );
    let bad = fx.root.join("bad.md");
    fs::write(&bad, b"Branch: `x`\n\xff\n").expect("write bad file");
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&bad)));
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!("{UNREADABLE}brief-lint: not checked: file not UTF-8\n")
    );
}

// --- step 9: the lint's faults ----------------------------------------------

#[test]
fn a_failing_brief_is_refused_with_the_lints_lines() {
    let fx = Fixture::new();
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&fx.fail())));
    assert_eq!(code, 1);
    assert_eq!(out, refused_untagged());
}

#[test]
fn a_root_with_a_trailing_slash_holds_its_files() {
    let fx = Fixture::new();
    let root = PathBuf::from(format!("{}/", fx.root.display()));
    let (code, out) = gate(&fx.sb, Some(&root), hook_json(&naming(&fx.fail())));
    assert_eq!(code, 1);
    assert_eq!(out, refused_untagged());
}

#[test]
fn a_missing_file_in_a_sibling_folder_of_the_root_is_outside() {
    // `<tmp>/briefs2` starts with the text `<tmp>/briefs` but is not under
    // it: the comparison is by path components.
    let fx = Fixture::new();
    let sibling = fx.sb.outside.join("briefs2");
    fs::create_dir_all(&sibling).expect("create sibling");
    for path in [sibling.join("nosuch.md"), sibling.join("x").join("a.md")] {
        let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&path)));
        assert_eq!(code, 2, "{}", path.display());
        assert_eq!(out, OUTSIDE, "{}", path.display());
    }
}

#[test]
fn git_not_on_the_path_is_git_failed() {
    let fx = Fixture::new();
    let empty = fx.sb.outside.join("empty-bin");
    fs::create_dir_all(&empty).expect("create folder");
    let mut cmd = gate_cmd(&fx.sb, None);
    cmd.env("PATH", &empty);
    let (code, out) = run_with_input(cmd, hook_json(&naming(&fx.fail())), Duration::from_secs(10));
    assert_eq!(code, 2);
    assert_eq!(out, "brief-gate: not checked: git failed\n");
}

#[test]
fn a_root_that_is_not_a_folder_comes_before_two_task_file_lines() {
    let fx = Fixture::new();
    let fail = fx.fail();
    let prompt = format!("{}{}", naming(&fail), naming(&fail));
    let (code, out) = gate(&fx.sb, Some(&fail), hook_json(&prompt));
    assert_eq!(code, 2);
    assert_eq!(out, "brief-gate: not checked: brief root is not a folder\n");
}

#[test]
fn a_folder_named_like_a_brief_is_refused_as_unreadable() {
    let fx = Fixture::new();
    let folder = fx.root.join("x.md");
    fs::create_dir_all(&folder).expect("create folder");
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&folder)));
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!("{UNREADABLE}brief-lint: not checked: file unreadable\n")
    );
}

#[test]
fn a_refusal_forwards_fifty_lines_then_a_count_then_the_summary() {
    // Lines 4 to 63 are 60 untagged premises, lines 64 and 65 two premises
    // tagged unchecked: 62 lines before the lint's summary.
    let mut brief = "Branch: `b`\nMODEL: m\n## Premises\n".to_owned();
    for _ in 0..60 {
        brief.push_str("- p\n");
    }
    brief.push_str("- q [unchecked: u]\n- r [unchecked: u]\n");
    brief.push_str("## Task statements\nx\n## Decided judgment calls\nx\n## Stopping\nx\n");
    let fx = Fixture::new();
    let path = fx.root.join("many.md");
    fs::write(&path, brief).expect("write brief");
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&path)));
    let mut want = "brief-gate: refused: the task file does not pass brief-lint\n".to_owned();
    for line in 4..54 {
        want.push_str(&format!(
            "brief-lint: line {line}: untagged-premise: premise has no [counted: ], [opened: ] or [unchecked: ] tag\n"
        ));
    }
    want.push_str("brief-gate: 12 more lines of brief-lint not shown\n");
    want.push_str("brief-lint: summary: faults=60 unchecked=2\n");
    assert_eq!(code, 1);
    assert_eq!(out, want);
}

#[test]
fn a_passing_brief_passes_silently() {
    let fx = Fixture::new();
    let (code, out) = gate(&fx.sb, Some(&fx.root), hook_json(&naming(&fx.pass())));
    assert_eq!((code, out.as_str()), (0, ""));
}

#[test]
fn no_output_line_holds_a_quote_or_a_backslash() {
    // A brief and a path that hold both; `run_with_input` asserts on every
    // run, and this test puts both in front of each step that prints.
    let fx = Fixture::new();
    let quoted = fx.root.join("q\"uo\\te.md");
    fs::write(
        &quoted,
        "Branch: `a\"b`\nMODEL: \"\\\"\n## Premises\n1. \"x\" \\ [opened: \"\\\"]\n",
    )
    .expect("write quoted brief");
    let outside = fx.sb.outside.join("q\"\\.md");
    let cases = [
        (Some(fx.root.as_path()), naming(&quoted), 1),
        (Some(fx.root.as_path()), naming(&outside), 2),
        (
            Some(fx.root.as_path()),
            "Your task file is \"\\\"".to_owned(),
            1,
        ),
        (None, naming(&quoted), 2),
    ];
    for (root, prompt, want) in cases {
        let (code, _) = gate(&fx.sb, root, hook_json(&prompt));
        assert_eq!(code, want, "{prompt}");
    }
}
