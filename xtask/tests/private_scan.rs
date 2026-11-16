//! `private-scan` against temporary repositories and lists of invented words.
//! Each test asserts the exit code and the whole standard output, and every
//! run is checked for a term on either stream.

mod common;

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use common::{assert_no_term, code_and_stdout, Sandbox};

/// Keep equal to `FILE_MAX_BYTES` in `src/private_scan.rs`.
const FILE_MAX_BYTES: u64 = 8 << 20;

const LIST: &str = "zorblax\nQuindle\nfernwhistle\n";

fn summary(hits: usize, not_checked: usize) -> String {
    format!("private-scan: summary: hits={hits} not_checked={not_checked}\n")
}

/// Runs `xtask private-scan <args>` in the sandbox with `CQS_PRIVATE_TERMS`
/// set to `terms` (when given) and returns the exit code and stdout.
fn scan_env<S: AsRef<OsStr>>(sb: &Sandbox, terms: Option<&OsStr>, args: &[S]) -> (i32, String) {
    let mut cmd = sb.command(env!("CARGO_BIN_EXE_xtask"));
    cmd.arg("private-scan").args(args);
    if let Some(terms) = terms {
        cmd.env("CQS_PRIVATE_TERMS", terms);
    }
    let out = cmd.output().expect("run xtask");
    assert_no_term(&out);
    code_and_stdout(&out)
}

/// Runs the scan with the standard list written outside the repository.
fn scan<S: AsRef<OsStr>>(sb: &Sandbox, args: &[S]) -> (i32, String) {
    let list = sb.write_outside("terms.txt", LIST);
    scan_env(sb, Some(list.as_os_str()), args)
}

fn path_str(p: &Path) -> &str {
    p.to_str().expect("UTF-8 temp path")
}

// --- --staged ---------------------------------------------------------------

#[test]
fn staged_added_line_with_term_is_a_hit() {
    let sb = Sandbox::repo();
    sb.write("notes.txt", "a\nb\nc\n");
    sb.commit_all("base");
    sb.write("notes.txt", "a\nb\nthe zorblax plan\nc\n");
    sb.git(&["add", "notes.txt"]);
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: staged notes.txt:3 terms=1\n{}",
            summary(1, 0)
        )
    );
}

#[test]
fn staged_clean_change_exits_zero_and_old_lines_are_not_scanned() {
    let sb = Sandbox::repo();
    sb.write("notes.txt", "an old zorblax line\n");
    sb.commit_all("base");
    sb.write("notes.txt", "an old zorblax line\na new plain line\n");
    sb.git(&["add", "notes.txt"]);
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 0);
    assert_eq!(out, summary(0, 0));
}

#[test]
fn staged_first_commit_of_a_repository() {
    let sb = Sandbox::repo();
    sb.write("a.txt", "plain\nquindle here\n");
    sb.git(&["add", "a.txt"]);
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: staged a.txt:2 terms=1\n{}",
            summary(1, 0)
        )
    );
}

#[test]
fn staged_hit_in_a_path_alone_does_not_print_the_path() {
    let sb = Sandbox::repo();
    sb.write("a.txt", "plain\n");
    sb.write("docs/Zorblax-notes.md", "plain words only\n");
    sb.git(&["add", "-A"]);
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: staged path 2 terms=1\n{}",
            summary(1, 0)
        )
    );
}

#[test]
fn staged_content_of_a_hidden_path_is_placed_by_number() {
    let sb = Sandbox::repo();
    sb.write("docs/zorblax.md", "plain\nquindle\n");
    sb.write("caf\u{e9}.txt", "fernwhistle\n");
    sb.git(&["add", "-A"]);
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 1);
    // Git orders `caf\xc3\xa9.txt` before `docs/`; the first path is not
    // printable ASCII, the second holds a term.
    assert_eq!(
        out,
        format!(
            "private-scan: hit: staged path 2 terms=1\n\
             private-scan: hit: staged path 1:1 terms=1\n\
             private-scan: hit: staged path 2:2 terms=1\n{}",
            summary(3, 0)
        )
    );
}

#[test]
fn staged_term_in_other_case() {
    let sb = Sandbox::repo();
    sb.write("a.txt", "QUINDLE\nZorBlax and FERNWHISTLE\n");
    sb.git(&["add", "a.txt"]);
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: staged a.txt:1 terms=1\n\
             private-scan: hit: staged a.txt:2 terms=2\n{}",
            summary(2, 0)
        )
    );
}

#[test]
fn staged_binary_file_is_searched_as_bytes() {
    let sb = Sandbox::repo();
    sb.write("blob.bin", b"\x00\x01\x02 zorblax \x00\xff");
    sb.write("clean.bin", b"\x00\x01\x02 plain \x00\xff");
    sb.git(&["add", "-A"]);
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: staged blob.bin:binary terms=1\n{}",
            summary(1, 0)
        )
    );
}

#[test]
fn staged_deletion_of_a_matching_path_is_no_hit() {
    let sb = Sandbox::repo();
    sb.write("zorblax.txt", "plain\n");
    sb.write("keep.txt", "plain\n");
    sb.commit_all("base");
    sb.git(&["rm", "-q", "zorblax.txt"]);
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 0);
    assert_eq!(out, summary(0, 0));
}

#[test]
fn staged_rename_reports_the_new_path_and_changed_lines_only() {
    let sb = Sandbox::repo();
    let body = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";
    sb.write("old.txt", body);
    sb.commit_all("base");
    sb.git(&["mv", "old.txt", "new name.txt"]);
    sb.write("new name.txt", format!("{body}nine quindle\n"));
    sb.git(&["add", "-A"]);
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: staged new name.txt:9 terms=1\n{}",
            summary(1, 0)
        )
    );
}

#[test]
fn staged_outside_a_repository_is_not_checked() {
    let sb = Sandbox::plain();
    let (code, out) = scan(&sb, &["--staged"]);
    assert_eq!(code, 2);
    assert_eq!(out, "private-scan: not checked: git failed\n");
}

// --- --message ----------------------------------------------------------------

#[test]
fn message_hit_includes_comment_lines_and_never_prints_the_file_path() {
    let sb = Sandbox::repo();
    let msg = sb.write_outside(
        "zorblax-message.txt",
        "Subject line\n\nBody text\n# a comment that names quindle\n",
    );
    let (code, out) = scan(&sb, &[OsStr::new("--message"), msg.as_os_str()]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: message message:4 terms=1\n{}",
            summary(1, 0)
        )
    );
}

#[test]
fn message_clean_and_text_below_scissors_is_skipped() {
    let sb = Sandbox::repo();
    let msg = sb.write_outside(
        "msg.txt",
        "Subject line\n\n# ------------------------ >8 ------------------------\n+a diff line with zorblax\n",
    );
    let (code, out) = scan(&sb, &[OsStr::new("--message"), msg.as_os_str()]);
    assert_eq!(code, 0);
    assert_eq!(out, summary(0, 0));
}

#[test]
fn message_file_that_does_not_exist_is_not_checked() {
    let sb = Sandbox::repo();
    let missing = sb.outside.join("quindle-missing.txt");
    let (code, out) = scan(&sb, &[OsStr::new("--message"), missing.as_os_str()]);
    assert_eq!(code, 2);
    assert_eq!(out, "private-scan: not checked: input file unreadable\n");
}

// --- --range ------------------------------------------------------------------

#[test]
fn range_hit_in_added_line() {
    let sb = Sandbox::repo();
    sb.write("f.txt", "one\n");
    let base = sb.commit_all("base");
    sb.write("g.txt", "plain\n");
    sb.commit_all("clean commit");
    sb.write("f.txt", "one\nzorblax two\n");
    let hit = sb.commit_all("second commit");
    let spec = format!("{base}..HEAD");
    let (code, out) = scan(&sb, &["--range", spec.as_str()]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: range {hit}:f.txt:2 terms=1\n\
             private-scan: summary: hits=1 not_checked=0 commits=2\n"
        )
    );
}

#[test]
fn range_clean_exits_zero() {
    let sb = Sandbox::repo();
    sb.write("f.txt", "one\n");
    let base = sb.commit_all("base");
    sb.write("f.txt", "one\ntwo\n");
    sb.commit_all("plain commit");
    let spec = format!("{base}..main");
    let (code, out) = scan(&sb, &["--range", spec.as_str()]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "private-scan: summary: hits=0 not_checked=0 commits=1\n"
    );
}

#[test]
fn range_hit_in_a_commit_message_alone() {
    let sb = Sandbox::repo();
    sb.write("f.txt", "one\n");
    let base = sb.commit_all("base");
    sb.write("f.txt", "one\ntwo\n");
    let hit = sb.commit_all("Subject\n\nWork for the Quindle effort");
    let spec = format!("{base}..HEAD");
    let (code, out) = scan(&sb, &["--range", spec.as_str()]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: range {hit}:message:3 terms=1\n\
             private-scan: summary: hits=1 not_checked=0 commits=1\n"
        )
    );
}

#[test]
fn range_hit_in_a_path_alone() {
    let sb = Sandbox::repo();
    sb.write("f.txt", "one\n");
    let base = sb.commit_all("base");
    sb.write("a.txt", "plain\n");
    sb.write("fernwhistle/b.txt", "plain\n");
    let hit = sb.commit_all("add files");
    let spec = format!("{base}..HEAD");
    let (code, out) = scan(&sb, &["--range", spec.as_str()]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: range {hit}:path 2 terms=1\n\
             private-scan: summary: hits=1 not_checked=0 commits=1\n"
        )
    );
}

#[test]
fn range_merge_commit_is_compared_with_its_first_parent() {
    let sb = Sandbox::repo();
    sb.write("f.txt", "one\n");
    let base = sb.commit_all("base");
    sb.git(&["checkout", "-q", "-b", "side"]);
    sb.write("s.txt", "side work\n");
    sb.commit_all("side commit");
    sb.git(&["checkout", "-q", "main"]);
    sb.write("m.txt", "main work\n");
    sb.commit_all("main commit");
    sb.git(&["merge", "-q", "--no-ff", "--no-commit", "side"]);
    sb.write("e.txt", "a plain line\nan evil fernwhistle line\n");
    let merge = sb.commit_all("Merge side");
    let spec = format!("{base}..main");
    let (code, out) = scan(&sb, &["--range", spec.as_str()]);
    assert_eq!(code, 1);
    // Against its first parent the merge changes e.txt (1) and s.txt (2).
    assert_eq!(
        out,
        format!(
            "private-scan: hit: range {merge}:e.txt:2 terms=1\n\
             private-scan: summary: hits=1 not_checked=0 commits=3\n"
        )
    );
}

#[test]
fn range_merge_shows_the_side_branch_change_against_its_first_parent() {
    let sb = Sandbox::repo();
    sb.write("f.txt", "one\n");
    let base = sb.commit_all("base");
    sb.git(&["checkout", "-q", "-b", "side"]);
    sb.write("s.txt", "side work for quindle\n");
    let side = sb.commit_all("side commit");
    sb.git(&["checkout", "-q", "main"]);
    sb.write("m.txt", "main work\n");
    sb.commit_all("main commit");
    sb.git(&["merge", "-q", "--no-ff", "-m", "Merge side", "side"]);
    let merge = sb.rev("HEAD");
    let spec = format!("{base}..main");
    let (code, out) = scan(&sb, &["--range", spec.as_str()]);
    assert_eq!(code, 1);
    // Against its second parent the merge would change m.txt alone.
    let side_line = format!("private-scan: hit: range {side}:s.txt:1 terms=1\n");
    let merge_line = format!("private-scan: hit: range {merge}:s.txt:1 terms=1\n");
    assert_eq!(
        out,
        format!("{side_line}{merge_line}private-scan: summary: hits=2 not_checked=0 commits=3\n")
    );
}

#[test]
fn range_first_commit_of_a_history_is_compared_with_the_empty_tree() {
    let sb = Sandbox::repo();
    sb.write("f.txt", "one\n");
    sb.commit_all("base");
    sb.git(&["checkout", "-q", "--orphan", "other"]);
    sb.git(&["rm", "-q", "-r", "--cached", "."]);
    fs::remove_file(sb.repo.join("f.txt")).expect("remove f.txt");
    sb.write("r.txt", "zorblax at the root\n");
    let root = sb.commit_all("root of another history");
    let (code, out) = scan(&sb, &["--range", "main..other"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: range {root}:r.txt:1 terms=1\n\
             private-scan: summary: hits=1 not_checked=0 commits=1\n"
        )
    );
}

#[test]
fn range_side_that_does_not_resolve_is_not_checked() {
    let sb = Sandbox::repo();
    sb.write("f.txt", "one\n");
    sb.commit_all("base");
    let (code, out) = scan(&sb, &["--range", "quindle-branch..HEAD"]);
    assert_eq!(code, 2);
    assert_eq!(out, "private-scan: not checked: range side not resolved\n");
}

#[test]
fn range_spec_without_two_sides_is_a_usage_error() {
    let sb = Sandbox::repo();
    sb.write("f.txt", "one\n");
    sb.commit_all("base");
    for spec in ["HEAD", "HEAD..", "..HEAD", "HEAD...HEAD"] {
        let (code, out) = scan(&sb, &["--range", spec]);
        assert_eq!(code, 2, "{spec}");
        assert_eq!(out, "private-scan: not checked: usage error\n", "{spec}");
    }
}

// --- --files ------------------------------------------------------------------

#[test]
fn files_hit_is_placed_by_argument_number() {
    let sb = Sandbox::repo();
    let clean = sb.write_outside("clean.txt", "plain\n");
    let hit = sb.write_outside("body.md", "plain\nplain\nsee quindle\n");
    let (code, out) = scan(
        &sb,
        &[OsStr::new("--files"), clean.as_os_str(), hit.as_os_str()],
    );
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: files path 2:3 terms=1\n{}",
            summary(1, 0)
        )
    );
}

#[test]
fn files_clean_exits_zero() {
    let sb = Sandbox::repo();
    let clean = sb.write_outside("clean.txt", "plain\n");
    let (code, out) = scan(&sb, &[OsStr::new("--files"), clean.as_os_str()]);
    assert_eq!(code, 0);
    assert_eq!(out, summary(0, 0));
}

#[test]
fn files_hit_in_a_path_alone_and_in_a_binary_file() {
    let sb = Sandbox::repo();
    let named = sb.write_outside("zorblax.md", "plain\n");
    let binary = sb.write_outside("data.bin", b"\x00\x00 fernwhistle \x00");
    let (code, out) = scan(
        &sb,
        &[OsStr::new("--files"), named.as_os_str(), binary.as_os_str()],
    );
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: files path 1 terms=1\n\
             private-scan: hit: files path 2:binary terms=1\n{}",
            summary(2, 0)
        )
    );
}

#[test]
fn files_hit_with_an_input_that_cannot_be_read_exits_one() {
    let sb = Sandbox::repo();
    let hit = sb.write_outside("hit.txt", "zorblax\n");
    let missing = sb.outside.join("missing.txt");
    let (code, out) = scan(
        &sb,
        &[OsStr::new("--files"), hit.as_os_str(), missing.as_os_str()],
    );
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: hit: files path 1:1 terms=1\n\
             private-scan: not checked: input file unreadable\n{}",
            summary(1, 1)
        )
    );
}

#[test]
fn files_over_the_size_limit_is_not_checked() {
    let sb = Sandbox::repo();
    let big = sb.outside.join("big.txt");
    let file = fs::File::create(&big).expect("create big file");
    file.set_len(FILE_MAX_BYTES + 1).expect("size big file");
    let (code, out) = scan(&sb, &[OsStr::new("--files"), big.as_os_str()]);
    assert_eq!(code, 2);
    assert_eq!(out, "private-scan: not checked: input over size limit\n");
}

// --- --text -------------------------------------------------------------------

#[test]
fn text_hit_and_clean() {
    let sb = Sandbox::repo();
    let (code, out) = scan(&sb, &["--text", "feature/QUINDLE-fix"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!("private-scan: hit: text text terms=1\n{}", summary(1, 0))
    );
    let (code, out) = scan(&sb, &["--text", "feature/plain-fix"]);
    assert_eq!(code, 0);
    assert_eq!(out, summary(0, 0));
}

// --- the list -----------------------------------------------------------------

#[test]
fn list_with_carriage_returns_and_a_byte_order_mark() {
    let sb = Sandbox::repo();
    let list = sb.write_outside(
        "crlf.txt",
        "\u{feff}zorblax\r\n\r\n# quindle is a comment here\r\n  fernwhistle  \r\n",
    );
    let (code, out) = scan_env(
        &sb,
        Some(list.as_os_str()),
        &["--text", "zorblax fernwhistle quindle"],
    );
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!("private-scan: hit: text text terms=2\n{}", summary(1, 0))
    );
}

#[test]
fn no_list_configured() {
    let sb = Sandbox::repo();
    let (code, out) = scan_env::<&str>(&sb, None, &["--text", "zorblax"]);
    assert_eq!(code, 2);
    assert_eq!(out, "private-scan: not checked: no list configured\n");
    let (code, out) = scan_env(&sb, Some(OsStr::new("")), &["--text", "zorblax"]);
    assert_eq!(code, 2);
    assert_eq!(out, "private-scan: not checked: no list configured\n");
}

#[test]
fn list_path_that_does_not_exist() {
    let sb = Sandbox::repo();
    let missing = sb.outside.join("quindle-list.txt");
    let (code, out) = scan_env(&sb, Some(missing.as_os_str()), &["--text", "zorblax"]);
    assert_eq!(code, 2);
    assert_eq!(out, "private-scan: not checked: list unreadable\n");
}

#[test]
fn list_of_comments_alone() {
    let sb = Sandbox::repo();
    let list = sb.write_outside("comments.txt", "# zorblax\n#quindle\n\n");
    let (code, out) = scan_env(&sb, Some(list.as_os_str()), &["--text", "zorblax"]);
    assert_eq!(code, 2);
    assert_eq!(out, "private-scan: not checked: list has no term\n");
}

#[test]
fn list_from_git_config_when_the_variable_is_unset_or_empty() {
    let sb = Sandbox::repo();
    let one = sb.write_outside("one.txt", "zorblax\n");
    let two = sb.write_outside("two.txt", "quindle\n");
    sb.git(&["config", "--add", "cqs.privateTerms", path_str(&one)]);
    sb.git(&["config", "--add", "cqs.privateTerms", path_str(&two)]);
    for terms in [None, Some(OsStr::new(""))] {
        let (code, out) = scan_env(&sb, terms, &["--text", "zorblax and quindle"]);
        assert_eq!(code, 1);
        assert_eq!(
            out,
            format!("private-scan: hit: text text terms=2\n{}", summary(1, 0))
        );
    }
}

#[test]
fn several_lists_in_the_variable_are_a_union() {
    let sb = Sandbox::repo();
    let one = sb.write_outside("one.txt", "zorblax\nquindle\n");
    let two = sb.write_outside("two.txt", "QUINDLE\nfernwhistle\n");
    let joined = format!("{}:{}", path_str(&one), path_str(&two));
    let (code, out) = scan_env(
        &sb,
        Some(OsStr::new(&joined)),
        &["--text", "zorblax quindle fernwhistle"],
    );
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!("private-scan: hit: text text terms=3\n{}", summary(1, 0))
    );
}

#[test]
fn hit_with_one_list_unreadable_exits_one() {
    let sb = Sandbox::repo();
    let one = sb.write_outside("one.txt", "zorblax\n");
    let missing = sb.outside.join("missing.txt");
    let joined = format!("{}:{}", path_str(&one), path_str(&missing));
    let (code, out) = scan_env(&sb, Some(OsStr::new(&joined)), &["--text", "zorblax"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "private-scan: not checked: list unreadable\n\
             private-scan: hit: text text terms=1\n{}",
            summary(1, 1)
        )
    );
}

// --- usage --------------------------------------------------------------------

#[test]
fn usage_errors_exit_two_without_echoing_the_argument() {
    let sb = Sandbox::repo();
    let usage = "private-scan: not checked: usage error\n\
                 private-scan: usage: private-scan --staged | --message <file> | --range <a>..<b> | --files <path>... | --text <literal>\n";
    let cases: &[&[&str]] = &[
        &[],
        &["--text"],
        &["--files"],
        &["--staged", "--text", "zorblax"],
        &["--zorblax"],
        &["--text", "a", "quindle"],
    ];
    for &args in cases {
        let (code, out) = scan(&sb, args);
        assert_eq!(code, 2, "{args:?}");
        assert_eq!(out, usage, "{args:?}");
    }
}
