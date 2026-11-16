//! `brief-lint` run as a child on the fixtures of `tests/fixtures/brief/` and
//! on briefs written by the tests. Each test asserts the exit code and the
//! whole standard output.

mod common;

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{assert_no_term, code_and_stdout, Sandbox};

/// Keep equal to `BRIEF_MAX_BYTES` in `src/brief/lint.rs`.
const BRIEF_MAX_BYTES: usize = 1 << 20;

const USAGE: &str = "xtask: usage: xtask <private-scan|hooks-check|brief-lint|brief-gate> [args]\n";

const UNCHECKED_11: &str = "brief-lint: line 11: unchecked premise\n";
const OPENED_WHOLE_SET: &str = "opened-whole-set: premise says none, all, every or only and its tags are all opened: count it, mark it unchecked, or put a word quoted from a file in inline code";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("brief")
        .join(name)
}

fn summary(faults: usize, unchecked: usize) -> String {
    format!("brief-lint: summary: faults={faults} unchecked={unchecked}\n")
}

/// Runs `xtask brief-lint <args>` in the sandbox.
fn lint<S: AsRef<OsStr>>(sb: &Sandbox, args: &[S]) -> (i32, String) {
    let out = sb
        .command(env!("CARGO_BIN_EXE_xtask"))
        .arg("brief-lint")
        .args(args)
        .output()
        .expect("run xtask");
    assert_no_term(&out);
    code_and_stdout(&out)
}

#[test]
fn pass_fixture_has_no_fault() {
    let sb = Sandbox::plain();
    let (code, out) = lint(&sb, &[fixture("pass.md")]);
    assert_eq!(code, 0);
    assert_eq!(out, format!("{UNCHECKED_11}{}", summary(0, 1)));
}

#[test]
fn byte_order_mark_and_crlf_line_ends_read_the_same() {
    let sb = Sandbox::plain();
    let text = fs::read_to_string(fixture("pass.md")).expect("read pass.md");
    let crlf = format!("\u{feff}{}", text.replace('\n', "\r\n"));
    assert!(crlf.contains("\r\n") && crlf.starts_with('\u{feff}'));
    let path = sb.write_outside("crlf.md", crlf);
    let (code, out) = lint(&sb, &[path]);
    assert_eq!(code, 0);
    assert_eq!(out, format!("{UNCHECKED_11}{}", summary(0, 1)));
}

#[test]
fn each_fail_fixture_has_its_one_fault() {
    let cases: &[(&str, &str, usize)] = &[
        (
            "missing-branch",
            "brief-lint: missing-branch: no line that starts with Branch: and a branch name in backticks\n",
            1,
        ),
        (
            "missing-model",
            "brief-lint: missing-model: no line that starts with MODEL:\n",
            1,
        ),
        (
            "missing-section",
            "brief-lint: missing-section: no section ## Stopping\n",
            1,
        ),
        (
            "duplicate-section",
            "brief-lint: line 33: duplicate-section: a second section ## Stopping\n",
            1,
        ),
        (
            "empty-section",
            "brief-lint: line 25: empty-section: section ## Decided judgment calls has no text outside fenced blocks\n",
            1,
        ),
        (
            "no-premise",
            "brief-lint: line 7: no-premise: the Premises section has no numbered or bulleted item\n",
            0,
        ),
        (
            "untagged-premise",
            "brief-lint: line 9: untagged-premise: premise has no [counted: ], [opened: ] or [unchecked: ] tag\n",
            1,
        ),
        (
            "opened-whole-set",
            "brief-lint: line 10: opened-whole-set: premise says none, all, every or only and its tags are all opened: count it, mark it unchecked, or put a word quoted from a file in inline code\n",
            1,
        ),
        (
            "empty-tag",
            "brief-lint: line 9: empty-tag: tag with no text after its colon\n",
            1,
        ),
    ];
    let sb = Sandbox::plain();
    for (rule, fault, unchecked) in cases {
        let (code, out) = lint(&sb, &[fixture(&format!("fail-{rule}.md"))]);
        let unchecked_line = if *unchecked == 1 { UNCHECKED_11 } else { "" };
        assert_eq!(code, 1, "{rule}");
        assert_eq!(
            out,
            format!("{fault}{unchecked_line}{}", summary(1, *unchecked)),
            "{rule}"
        );
    }
    let ids: Vec<&str> = cases.iter().map(|c| c.0).collect();
    assert_eq!(
        ids,
        [
            "missing-branch",
            "missing-model",
            "missing-section",
            "duplicate-section",
            "empty-section",
            "no-premise",
            "untagged-premise",
            "opened-whole-set",
            "empty-tag",
        ]
    );
}

#[test]
fn faults_print_lineless_first_then_by_line_then_by_rule() {
    // Lines 5 and 6 hold premise faults; line 12 a second Premises heading,
    // found before the items are read. Line 5 holds two faults.
    let brief = "# A brief with faults\n\
                 \n\
                 ## Premises\n\
                 \n\
                 1. The first. [opened: ] and all of it.\n\
                 2. The second has no tag.\n\
                 \n\
                 ## Task statements\n\
                 \n\
                 Do it.\n\
                 \n\
                 ## Premises\n\
                 \n\
                 3. Again. [counted: ok]\n";
    let sb = Sandbox::plain();
    let path = sb.write_outside("faults.md", brief);
    let (code, out) = lint(&sb, &[path]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "brief-lint: missing-branch: no line that starts with Branch: and a branch name in backticks\n\
             brief-lint: missing-model: no line that starts with MODEL:\n\
             brief-lint: missing-section: no section ## Decided judgment calls\n\
             brief-lint: missing-section: no section ## Stopping\n\
             brief-lint: line 5: {OPENED_WHOLE_SET}\n\
             brief-lint: line 5: empty-tag: tag with no text after its colon\n\
             brief-lint: line 6: untagged-premise: premise has no [counted: ], [opened: ] or [unchecked: ] tag\n\
             brief-lint: line 12: duplicate-section: a second section ## Premises\n\
             {}",
            summary(8, 0)
        )
    );
}

#[test]
fn usage_errors_are_not_checked() {
    let sb = Sandbox::plain();
    let pass = fixture("pass.md");
    let no_args: [&OsStr; 0] = [];
    for args in [no_args.to_vec(), vec![pass.as_os_str(), pass.as_os_str()]] {
        let (code, out) = lint(&sb, &args);
        assert_eq!(code, 2, "{args:?}");
        assert_eq!(out, "brief-lint: not checked: usage error\n", "{args:?}");
    }
}

#[test]
fn a_missing_file_or_a_folder_is_unreadable() {
    let sb = Sandbox::plain();
    for path in [sb.outside.join("nosuch.md"), sb.outside.clone()] {
        let (code, out) = lint(&sb, &[&path]);
        assert_eq!(code, 2, "{}", path.display());
        assert_eq!(out, "brief-lint: not checked: file unreadable\n");
    }
}

#[cfg(unix)]
#[test]
fn a_named_pipe_is_unreadable_and_never_opened() {
    let sb = Sandbox::plain();
    let fifo = sb.outside.join("pipe.md");
    let made = sb
        .command("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed");
    let mut child = sb
        .command(env!("CARGO_BIN_EXE_xtask"))
        .arg("brief-lint")
        .arg(&fifo)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start xtask");
    let start = Instant::now();
    loop {
        if child.try_wait().expect("poll xtask").is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("brief-lint on a named pipe did not end within 5 seconds");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("collect xtask");
    let (code, stdout) = code_and_stdout(&out);
    assert_eq!(code, 2);
    assert_eq!(stdout, "brief-lint: not checked: file unreadable\n");
}

#[test]
fn a_file_one_byte_over_the_cap_is_not_checked() {
    let sb = Sandbox::plain();
    let at_cap = sb.write_outside("at-cap.md", vec![b'a'; BRIEF_MAX_BYTES]);
    let (code, out) = lint(&sb, &[at_cap]);
    assert_eq!(code, 1, "a brief at the cap is read");
    assert!(out.ends_with(&summary(6, 0)), "{out}");
    let over = sb.write_outside("over.md", vec![b'a'; BRIEF_MAX_BYTES + 1]);
    let (code, out) = lint(&sb, &[over]);
    assert_eq!(code, 2);
    assert_eq!(out, "brief-lint: not checked: file over size limit\n");
}

#[test]
fn a_file_with_an_invalid_utf8_byte_is_not_checked() {
    let sb = Sandbox::plain();
    let mut bytes = fs::read(fixture("pass.md")).expect("read pass.md");
    bytes.insert(40, 0xff);
    let path = sb.write_outside("bad.md", bytes);
    let (code, out) = lint(&sb, &[path]);
    assert_eq!(code, 2);
    assert_eq!(out, "brief-lint: not checked: file not UTF-8\n");
}

#[test]
fn no_line_holds_the_briefs_text_or_its_path() {
    // Every line of the brief, and the file's folder and name, hold an
    // invented word; `assert_no_term` checks both streams for it.
    let brief = "Branch: zorblax\n\
                 MODEL:zorblax\n\
                 ## Premises zorblax\n\
                 1. zorblax all [opened: zorblax]\n\
                 2. zorblax [unchecked: zorblax]\n\
                 - zorblax untagged\n\
                 * zorblax [counted:   ]\n\
                 ```zorblax\n\
                 zorblax\n\
                 ```\n\
                 ## Premises zorblax\n\
                 zorblax\n\
                 ## Task statements zorblax\n\
                 ## Stoppingzorblax\n\
                 # zorblax\n";
    let sb = Sandbox::plain();
    let path = sb.write_outside("zorblax/zorblax.md", brief);
    let (code, out) = lint(&sb, &[path]);
    assert_eq!(code, 1);
    assert!(
        out.contains("brief-lint: line 4: opened-whole-set:"),
        "{out}"
    );
    assert!(out.ends_with(&summary(9, 1)), "{out}");
}

#[test]
fn an_item_of_backtick_runs_that_never_close_is_read_in_linear_time() {
    // Runs of 1, 2, 3, ... backticks: no run has a later run of its own
    // length, so none opens a code span. A reader that searches forward
    // from each run does work quadratic in the item's length.
    let head = "Branch: `x`\nMODEL: m\n## Premises\n1. a ";
    let tail =
        "[counted: c]\n## Task statements\nx\n## Decided judgment calls\nx\n## Stopping\nx\n";
    let mut brief = head.to_owned();
    let mut n = 1;
    while brief.len() + n + 1 + tail.len() <= BRIEF_MAX_BYTES {
        brief.push_str(&"`".repeat(n));
        brief.push(' ');
        n += 1;
    }
    brief.push_str(tail);
    assert!(n > 1000, "{n} runs");
    let sb = Sandbox::plain();
    let path = sb.write_outside("runs.md", brief);
    let mut child = sb
        .command(env!("CARGO_BIN_EXE_xtask"))
        .arg("brief-lint")
        .arg(&path)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start xtask");
    let start = Instant::now();
    while child.try_wait().expect("poll xtask").is_none() {
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("brief-lint did not end within 5 seconds");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("collect xtask");
    let (code, stdout) = code_and_stdout(&out);
    assert_eq!(code, 0);
    assert_eq!(stdout, summary(0, 0));
}

#[test]
fn an_unknown_task_prints_the_usage_line() {
    let sb = Sandbox::plain();
    let out = sb
        .command(env!("CARGO_BIN_EXE_xtask"))
        .arg("nosuch")
        .output()
        .expect("run xtask");
    let (code, stdout) = code_and_stdout(&out);
    assert_eq!(code, 2);
    assert_eq!(stdout, USAGE);
}
