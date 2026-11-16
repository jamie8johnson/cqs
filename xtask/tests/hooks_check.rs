//! `hooks-check` against temporary repositories that hold a `.githooks/`.

mod common;

use common::{code_and_stdout, Sandbox};

const PRE_COMMIT: &str = "#!/bin/sh\necho pre-commit\n";
const COMMIT_MSG: &str = "#!/bin/sh\necho commit-msg\n";

fn fixture() -> Sandbox {
    let sb = Sandbox::repo();
    sb.write(".githooks/pre-commit", PRE_COMMIT);
    sb.write(".githooks/commit-msg", COMMIT_MSG);
    sb
}

fn hooks_check(sb: &Sandbox) -> (i32, String) {
    let out = sb
        .command(env!("CARGO_BIN_EXE_xtask"))
        .arg("hooks-check")
        .output()
        .expect("run xtask");
    code_and_stdout(&out)
}

fn not_set_up(dir: &str, lines: &[&str]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(&format!("hooks-check: {line} {dir}\n"));
    }
    out.push_str(&format!(
        "hooks-check: git runs hooks from {dir}; to run the hooks of .githooks/: git config core.hooksPath .githooks\n"
    ));
    out
}

#[test]
fn hooks_path_unset_uses_git_s_own_folder() {
    let sb = fixture();
    let dir = format!("{}/.git/hooks", sb.repo.display());
    let (code, out) = hooks_check(&sb);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        not_set_up(
            &dir,
            &["commit-msg: missing from", "pre-commit: missing from"]
        )
    );
}

#[test]
fn hooks_path_set_to_githooks() {
    let sb = fixture();
    sb.git(&["config", "core.hooksPath", ".githooks"]);
    let (code, out) = hooks_check(&sb);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "hooks-check: ok: hooks run from {}/.githooks\n",
            sb.repo.display()
        )
    );
}

#[test]
fn hooks_path_set_to_a_folder_with_another_script() {
    let sb = fixture();
    sb.write("other-hooks/pre-commit", "#!/bin/sh\necho something else\n");
    sb.write("other-hooks/commit-msg", COMMIT_MSG);
    sb.git(&["config", "core.hooksPath", "other-hooks"]);
    let dir = format!("{}/other-hooks", sb.repo.display());
    let (code, out) = hooks_check(&sb);
    assert_eq!(code, 1);
    assert_eq!(out, not_set_up(&dir, &["pre-commit: differs in"]));
}

#[test]
fn hooks_path_set_to_a_folder_with_no_hook() {
    // An absolute path to git's own folder, which holds sample files alone.
    let sb = fixture();
    let dir = format!("{}/.git/hooks", sb.repo.display());
    sb.git(&["config", "core.hooksPath", &dir]);
    let (code, out) = hooks_check(&sb);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        not_set_up(
            &dir,
            &["commit-msg: missing from", "pre-commit: missing from"]
        )
    );
}

#[test]
fn hooks_check_from_a_subfolder_resolves_against_the_top() {
    let sb = fixture();
    sb.write("sub/keep.txt", "x\n");
    sb.git(&["config", "core.hooksPath", ".githooks"]);
    let out = sb
        .command(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(sb.repo.join("sub"))
        .arg("hooks-check")
        .output()
        .expect("run xtask");
    let (code, out) = code_and_stdout(&out);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "hooks-check: ok: hooks run from {}/.githooks\n",
            sb.repo.display()
        )
    );
}
