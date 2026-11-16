//! `private-scan`: searches what is about to leave this machine (a staged
//! change, a commit message, a range of commits, named files, one literal)
//! for the terms of a private list kept outside the repository.
//!
//! Output invariant: no line printed, on any exit, holds a term, the text
//! that matched, a list's path or an argument's text. Every line is built
//! from fixed strings, counts, commit ids, and paths that passed the path
//! rule (printable ASCII and no term); a place that would still hold a term
//! is printed as `withheld`. Git's standard error is captured and dropped.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::thread;

/// Largest term list file read, in bytes.
pub const LIST_MAX_BYTES: u64 = 1 << 20;
/// Largest single input read whole, in bytes: a `--files` file, a message
/// file, one commit's message, one binary blob.
pub const FILE_MAX_BYTES: u64 = 8 << 20;
/// Largest output read from one git command, in bytes (one diff of the
/// staged change or of one commit, or the commit list of a range).
pub const DIFF_MAX_BYTES: u64 = 64 << 20;

/// Bytes git inspects for a NUL to call a file binary; `--files` uses the same rule.
const BINARY_SNIFF_BYTES: usize = 8000;

const ENV_VAR: &str = "CQS_PRIVATE_TERMS";
const GIT_KEY: &str = "cqs.privateTerms";

/// Flags of every diff the scan reads. Explicit prefixes, renames and
/// submodule format pin the output shape against user configuration.
const DIFF_FLAGS: &[&str] = &[
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "-z",
    "--no-relative",
    "--find-renames",
    "--submodule=short",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

const USAGE: &str = "private-scan: usage: private-scan --staged | --message <file> | --range <a>..<b> | --files <path>... | --text <literal>";

/// Why an input was not checked. The strings are the whole fixed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    NoList,
    ListUnreadable,
    ListTooLarge,
    ListEmpty,
    InputUnreadable,
    InputTooLarge,
    GitFailed,
    DiffTooLarge,
    GitOutput,
    RangeUnresolved,
    Usage,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::NoList => "no list configured",
            Reason::ListUnreadable => "list unreadable",
            Reason::ListTooLarge => "list over size limit",
            Reason::ListEmpty => "list has no term",
            Reason::InputUnreadable => "input file unreadable",
            Reason::InputTooLarge => "input over size limit",
            Reason::GitFailed => "git failed",
            Reason::DiffTooLarge => "diff over size limit",
            Reason::GitOutput => "git output not understood",
            Reason::RangeUnresolved => "range side not resolved",
            Reason::Usage => "usage error",
        }
    }
}

fn emit(line: &str) {
    // A failed write to stdout leaves the exit code as the only signal; there
    // is nowhere else to report it.
    let _ = writeln!(io::stdout().lock(), "{line}");
}

fn emit_not_checked(reason: Reason) {
    emit(&format!("private-scan: not checked: {}", reason.as_str()));
}

enum Mode {
    Staged,
    Message(OsString),
    Range(String),
    Files(Vec<OsString>),
    Text(OsString),
}

impl Mode {
    fn name(&self) -> &'static str {
        match self {
            Mode::Staged => "staged",
            Mode::Message(_) => "message",
            Mode::Range(_) => "range",
            Mode::Files(_) => "files",
            Mode::Text(_) => "text",
        }
    }
}

fn parse_args(args: &[OsString]) -> Option<Mode> {
    let (flag, rest) = args.split_first()?;
    match (flag.to_str()?, rest) {
        ("--staged", []) => Some(Mode::Staged),
        ("--message", [file]) => Some(Mode::Message(file.clone())),
        ("--range", [spec]) => Some(Mode::Range(spec.to_str()?.to_owned())),
        ("--files", paths) if !paths.is_empty() => Some(Mode::Files(paths.to_vec())),
        ("--text", [literal]) => Some(Mode::Text(literal.clone())),
        _ => None,
    }
}

/// The union of the configured lists: lowercased, deduplicated, non-empty.
struct Terms(Vec<Vec<u8>>);

impl Terms {
    /// Number of distinct terms found in `hay`, without regard to ASCII case.
    fn count(&self, hay: &[u8]) -> usize {
        if hay.is_empty() {
            return 0;
        }
        let lower = hay.to_ascii_lowercase();
        self.0.iter().filter(|t| contains(&lower, t)).count()
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, _)) = needle.split_first() else {
        return false;
    };
    if needle.len() > hay.len() {
        return false;
    }
    let last_start = hay.len() - needle.len();
    let mut i = 0;
    while i <= last_start {
        match hay[i..=last_start].iter().position(|&b| b == first) {
            None => return false,
            Some(off) => {
                let start = i + off;
                if &hay[start..start + needle.len()] == needle {
                    return true;
                }
                i = start + 1;
            }
        }
    }
    false
}

fn parse_list(data: &[u8], into: &mut BTreeSet<Vec<u8>>) {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    for line in data.split(|&b| b == b'\n') {
        let term = line.trim_ascii();
        if term.is_empty() || term.starts_with(b"#") {
            continue;
        }
        into.insert(term.to_ascii_lowercase());
    }
}

#[cfg(unix)]
fn os_from_bytes(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
fn os_from_bytes(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// The list paths: from the environment variable when it is set and not
/// empty, else from git config. `Ok(empty)` means no list is configured.
fn list_paths() -> Result<Vec<OsString>, Reason> {
    if let Some(value) = std::env::var_os(ENV_VAR) {
        if !value.is_empty() {
            return Ok(value
                .as_encoded_bytes()
                .split(|&b| b == b':')
                .filter(|p| !p.is_empty())
                .map(|p| os_from_bytes(p.to_vec()))
                .collect());
        }
    }
    let out = run_git(
        &osv(&["config", "--type=path", "-z", "--get-all", GIT_KEY]),
        LIST_MAX_BYTES,
        Reason::GitFailed,
    )?;
    if out.code == Some(1) && out.stdout.is_empty() {
        // `git config --get-all` exits 1 for a key that is not set.
        return Ok(Vec::new());
    }
    if !out.success {
        return Err(Reason::GitFailed);
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| os_from_bytes(p.to_vec()))
        .collect())
}

/// Loads the terms. `failures` receives a reason per list that could not be
/// read; `Err` carries the reasons to print when there is nothing to scan with.
fn load_terms(failures: &mut Vec<Reason>) -> Result<Terms, Vec<Reason>> {
    let paths = list_paths().map_err(|r| vec![r])?;
    if paths.is_empty() {
        return Err(vec![Reason::NoList]);
    }
    let mut set = BTreeSet::new();
    for path in &paths {
        match read_capped(path, LIST_MAX_BYTES) {
            Ok(data) => parse_list(&data, &mut set),
            Err(ReadError::TooLarge) => failures.push(Reason::ListTooLarge),
            Err(ReadError::Unreadable) => failures.push(Reason::ListUnreadable),
        }
    }
    if set.is_empty() {
        if failures.is_empty() {
            return Err(vec![Reason::ListEmpty]);
        }
        return Err(std::mem::take(failures));
    }
    Ok(Terms(set.into_iter().collect()))
}

enum ReadError {
    Unreadable,
    TooLarge,
}

fn read_capped(path: &OsStr, limit: u64) -> Result<Vec<u8>, ReadError> {
    let file = fs::File::open(path).map_err(|_| ReadError::Unreadable)?;
    let meta = file.metadata().map_err(|_| ReadError::Unreadable)?;
    if !meta.is_file() {
        return Err(ReadError::Unreadable);
    }
    if meta.len() > limit {
        return Err(ReadError::TooLarge);
    }
    let mut buf = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|_| ReadError::Unreadable)?;
    if buf.len() as u64 > limit {
        return Err(ReadError::TooLarge);
    }
    Ok(buf)
}

fn osv(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

struct GitOut {
    success: bool,
    code: Option<i32>,
    stdout: Vec<u8>,
}

/// Runs git with the inherited environment (so a hook's `GIT_INDEX_FILE`
/// holds), reads at most `limit` bytes of its standard output, and drops its
/// standard error. Output over the limit ends the child and yields `too_large`.
fn run_git(args: &[OsString], limit: u64, too_large: Reason) -> Result<GitOut, Reason> {
    let mut child = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| Reason::GitFailed)?;
    let stderr = child.stderr.take();
    let drain = thread::spawn(move || {
        if let Some(mut err) = stderr {
            let _ = io::copy(&mut err, &mut io::sink());
        }
    });
    let mut buf = Vec::new();
    let read_ok = match child.stdout.take() {
        Some(out) => out.take(limit + 1).read_to_end(&mut buf).is_ok(),
        None => false,
    };
    if !read_ok || buf.len() as u64 > limit {
        // The child may still be writing; ending it is the intent, and a
        // failure to end one that already exited changes nothing.
        let _ = child.kill();
        let _ = child.wait();
        let _ = drain.join();
        return Err(if read_ok {
            too_large
        } else {
            Reason::GitFailed
        });
    }
    let status = child.wait().map_err(|_| Reason::GitFailed)?;
    let _ = drain.join();
    Ok(GitOut {
        success: status.success(),
        code: status.code(),
        stdout: buf,
    })
}

/// Runs git and requires success.
fn git_ok(args: &[OsString], limit: u64, too_large: Reason) -> Result<Vec<u8>, Reason> {
    let out = run_git(args, limit, too_large)?;
    if out.success {
        Ok(out.stdout)
    } else {
        Err(Reason::GitFailed)
    }
}

/// One changed file of a diff, by its new side.
struct Entry {
    deleted: bool,
    path: Vec<u8>,
    sha: String,
    binary: bool,
}

/// Parses `git diff --raw -z --no-abbrev`.
fn parse_raw(out: &[u8]) -> Option<Vec<Entry>> {
    let mut toks = out.split(|&b| b == 0);
    let mut entries = Vec::new();
    while let Some(meta) = toks.next() {
        if meta.is_empty() {
            // The trailing NUL leaves one empty token at the end.
            if toks.next().is_some() {
                return None;
            }
            break;
        }
        let fields: Vec<&[u8]> = meta.strip_prefix(b":")?.split(|&b| b == b' ').collect();
        let [_, _, _, dst_sha, status] = fields.as_slice() else {
            return None;
        };
        let letter = *status.first()?;
        let first_path = toks.next()?;
        let path = if letter == b'R' || letter == b'C' {
            toks.next()?
        } else {
            first_path
        };
        entries.push(Entry {
            deleted: letter == b'D',
            path: path.to_vec(),
            sha: String::from_utf8(dst_sha.to_vec()).ok()?,
            binary: false,
        });
    }
    Some(entries)
}

/// Marks the binary entries from `git diff --numstat -z`, whose records come
/// in the same order as the raw ones; each record's path is checked.
fn apply_numstat(out: &[u8], entries: &mut [Entry]) -> Option<()> {
    let mut toks = out.split(|&b| b == 0);
    for entry in entries.iter_mut() {
        let rec = toks.next()?;
        let mut parts = rec.splitn(3, |&b| b == b'\t');
        let added = parts.next()?;
        let removed = parts.next()?;
        let mut path = parts.next()?;
        if path.is_empty() {
            // A rename: the old and new paths follow as their own tokens.
            toks.next()?;
            path = toks.next()?;
        }
        if path != entry.path.as_slice() {
            return None;
        }
        entry.binary = added == b"-" && removed == b"-";
    }
    match toks.next() {
        None => Some(()),
        Some(rest) if rest.is_empty() && toks.next().is_none() => Some(()),
        Some(_) => None,
    }
}

/// The new-side path of a `+++ ` header line; `None` inside for `/dev/null`.
fn header_path(s: &[u8]) -> Option<Option<Vec<u8>>> {
    if s == b"/dev/null" {
        return Some(None);
    }
    let full = if s.first() == Some(&b'"') {
        unquote(&s[1..])?
    } else {
        // Git appends a tab to an unquoted name that holds a space.
        s.strip_suffix(b"\t").unwrap_or(s).to_vec()
    };
    full.strip_prefix(b"b/").map(|p| Some(p.to_vec()))
}

/// Undoes git's C-style path quoting; `s` starts after the opening quote.
fn unquote(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        i += 1;
        match b {
            b'"' => return Some(out),
            b'\\' => {
                let e = *s.get(i)?;
                i += 1;
                let v = match e {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => 9,
                    b'n' => 10,
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => 13,
                    b'"' => b'"',
                    b'\\' => b'\\',
                    b'0'..=b'3' => {
                        let d1 = *s.get(i)?;
                        let d2 = *s.get(i + 1)?;
                        if !(b'0'..=b'7').contains(&d1) || !(b'0'..=b'7').contains(&d2) {
                            return None;
                        }
                        i += 2;
                        (e - b'0') * 64 + (d1 - b'0') * 8 + (d2 - b'0')
                    }
                    _ => return None,
                };
                out.push(v);
            }
            _ => out.push(b),
        }
    }
    None
}

/// `(old_count, new_start, new_count)` of a `@@ -a[,b] +c[,d] @@` line.
fn hunk_header(line: &[u8]) -> Option<(u64, u64, u64)> {
    let rest = line.strip_prefix(b"@@ -")?;
    let mut parts = rest.split(|&b| b == b' ');
    let old = parts.next()?;
    let new = parts.next()?.strip_prefix(b"+")?;
    let (_, old_count) = range_pair(old)?;
    let (new_start, new_count) = range_pair(new)?;
    Some((old_count, new_start, new_count))
}

fn range_pair(s: &[u8]) -> Option<(u64, u64)> {
    let text = std::str::from_utf8(s).ok()?;
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

fn is_printable_ascii(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| (0x20..=0x7e).contains(b))
}

/// A run of the scan: the terms, the counts, and the output as it goes.
struct Scan {
    terms: Terms,
    mode: &'static str,
    hits: usize,
    not_checked: usize,
}

impl Scan {
    /// Prints a hit line for `place` when `hay` holds a term.
    fn check(&mut self, hay: &[u8], place: impl FnOnce() -> String) {
        let n = self.terms.count(hay);
        if n == 0 {
            return;
        }
        self.hits += 1;
        let mut place = place();
        if self.terms.count(place.as_bytes()) > 0 {
            place = "withheld".to_owned();
        }
        emit(&format!(
            "private-scan: hit: {} {place} terms={n}",
            self.mode
        ));
    }

    fn not_checked(&mut self, reason: Reason) {
        self.not_checked += 1;
        emit_not_checked(reason);
    }

    /// Checks a path itself (`<prefix>path <n>`) and returns the label its
    /// content places use: the path when it may be printed, else `path <n>`.
    fn path(&mut self, path: &[u8], n: usize, prefix: &str) -> String {
        self.check(path, || format!("{prefix}path {n}"));
        if is_printable_ascii(path) && self.terms.count(path) == 0 {
            String::from_utf8_lossy(path).into_owned()
        } else {
            format!("path {n}")
        }
    }

    /// Checks each line of `data`; places are `<prefix><line>`.
    fn lines(&mut self, data: &[u8], prefix: &str) {
        for (i, line) in data.split(|&b| b == b'\n').enumerate() {
            self.check(line, || format!("{prefix}{}", i + 1));
        }
    }

    /// Checks a staged change or one commit's change. `diff` is the git
    /// command up to its options (`diff --cached`, or `diff <parent> <commit>`);
    /// `prefix` comes before every place (empty, or `<commit>:`).
    fn diff(&mut self, diff: &[OsString], prefix: &str) {
        let with = |extra: &[&str]| -> Vec<OsString> {
            let mut v = diff.to_vec();
            v.extend(DIFF_FLAGS.iter().map(OsString::from));
            v.extend(extra.iter().map(OsString::from));
            v.push(OsString::from("--"));
            v
        };
        let raw = match git_ok(
            &with(&["--raw", "--no-abbrev"]),
            DIFF_MAX_BYTES,
            Reason::DiffTooLarge,
        ) {
            Ok(out) => out,
            Err(r) => return self.not_checked(r),
        };
        let Some(mut entries) = parse_raw(&raw) else {
            return self.not_checked(Reason::GitOutput);
        };
        let numstat = match git_ok(&with(&["--numstat"]), DIFF_MAX_BYTES, Reason::DiffTooLarge) {
            Ok(out) => out,
            Err(r) => return self.not_checked(r),
        };
        if apply_numstat(&numstat, &mut entries).is_none() {
            return self.not_checked(Reason::GitOutput);
        }

        // New-side paths, numbered from 1 in git's order, deletions left out.
        let mut labels: HashMap<Vec<u8>, String> = HashMap::new();
        let mut binaries: Vec<(String, String)> = Vec::new();
        for (i, entry) in entries.iter().filter(|e| !e.deleted).enumerate() {
            let label = self.path(&entry.path, i + 1, prefix);
            if entry.binary {
                binaries.push((entry.sha.clone(), label.clone()));
            }
            labels.insert(entry.path.clone(), label);
        }

        for (sha, label) in binaries {
            match git_ok(
                &osv(&["cat-file", "blob", &sha]),
                FILE_MAX_BYTES,
                Reason::InputTooLarge,
            ) {
                Ok(blob) => self.check(&blob, || format!("{prefix}{label}:binary")),
                Err(r) => self.not_checked(r),
            }
        }

        let patch = match git_ok(&with(&["-p", "-U0"]), DIFF_MAX_BYTES, Reason::DiffTooLarge) {
            Ok(out) => out,
            Err(r) => return self.not_checked(r),
        };
        if self.patch(&patch, &labels, prefix).is_none() {
            self.not_checked(Reason::GitOutput);
        }
    }

    /// Checks the added lines of a `-U0` patch. `None` when the patch has a
    /// shape this parser does not know; lines before that point are checked.
    fn patch(
        &mut self,
        patch: &[u8],
        labels: &HashMap<Vec<u8>, String>,
        prefix: &str,
    ) -> Option<()> {
        let lines: Vec<&[u8]> = patch.split(|&b| b == b'\n').collect();
        let mut label: Option<&str> = None;
        let mut in_header = false;
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            i += 1;
            if line.starts_with(b"diff --git ") {
                label = None;
                in_header = true;
            } else if in_header && line.starts_with(b"+++ ") {
                label = match header_path(&line[4..])? {
                    None => None,
                    Some(path) => Some(labels.get(&path)?.as_str()),
                };
            } else if line.starts_with(b"@@ -") {
                in_header = false;
                let (mut removed, mut line_no, mut added) = hunk_header(line)?;
                while removed > 0 || added > 0 {
                    let body = *lines.get(i)?;
                    i += 1;
                    match body.first() {
                        Some(b'+') if added > 0 => {
                            let at = label?;
                            self.check(&body[1..], || format!("{prefix}{at}:{line_no}"));
                            added -= 1;
                            line_no += 1;
                        }
                        Some(b'-') if removed > 0 => removed -= 1,
                        Some(b'\\') => {}
                        _ => return None,
                    }
                }
            }
        }
        Some(())
    }
}

/// True for git's scissors line: a comment string, a space, then the cut mark.
fn is_scissors(line: &[u8]) -> bool {
    const CUT: &[u8] = b" ------------------------ >8 ------------------------";
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    match line.strip_suffix(CUT) {
        Some(lead) => !lead.is_empty() && !lead.iter().any(u8::is_ascii_whitespace),
        None => false,
    }
}

fn scan_message(scan: &mut Scan, file: &OsStr) {
    let data = match read_capped(file, FILE_MAX_BYTES) {
        Ok(d) => d,
        Err(ReadError::Unreadable) => return scan.not_checked(Reason::InputUnreadable),
        Err(ReadError::TooLarge) => return scan.not_checked(Reason::InputTooLarge),
    };
    let mut end = data.len();
    let mut start = 0;
    for line in data.split(|&b| b == b'\n') {
        if is_scissors(line) {
            end = start;
            break;
        }
        start += line.len() + 1;
    }
    scan.lines(&data[..end.min(data.len())], "message:");
}

fn rev_parse(side: &str) -> Result<String, Reason> {
    let spec = format!("{side}^{{commit}}");
    let out = run_git(
        &osv(&[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ]),
        4096,
        Reason::GitOutput,
    )?;
    if !out.success {
        return Err(Reason::RangeUnresolved);
    }
    let sha = String::from_utf8(out.stdout).map_err(|_| Reason::GitOutput)?;
    let sha = sha.trim();
    if sha.is_empty() || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Reason::GitOutput);
    }
    Ok(sha.to_owned())
}

/// Scans a range; returns the count of commits scanned.
fn scan_range(scan: &mut Scan, spec: &str) -> usize {
    let (from, to) = match spec.split_once("..") {
        Some((a, b)) if !a.is_empty() && !b.is_empty() && !b.starts_with('.') => (a, b),
        _ => {
            scan.not_checked(Reason::Usage);
            return 0;
        }
    };
    let (from, to) = match (rev_parse(from), rev_parse(to)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(r), _) | (_, Err(r)) => {
            scan.not_checked(r);
            return 0;
        }
    };
    let list = match git_ok(
        &osv(&[
            "rev-list",
            "--reverse",
            "--parents",
            &to,
            &format!("^{from}"),
        ]),
        DIFF_MAX_BYTES,
        Reason::DiffTooLarge,
    ) {
        Ok(out) => out,
        Err(r) => {
            scan.not_checked(r);
            return 0;
        }
    };
    let Ok(list) = String::from_utf8(list) else {
        scan.not_checked(Reason::GitOutput);
        return 0;
    };
    let mut empty_tree: Option<String> = None;
    let mut scanned = 0;
    for (k, line) in list.lines().enumerate() {
        let mut ids = line.split(' ');
        let Some(commit) = ids.next().filter(|c| !c.is_empty()) else {
            scan.not_checked(Reason::GitOutput);
            continue;
        };
        let parent = match ids.next() {
            Some(p) => p.to_owned(),
            None => match &empty_tree {
                Some(t) => t.clone(),
                None => match empty_tree_id() {
                    Ok(t) => {
                        empty_tree = Some(t.clone());
                        t
                    }
                    Err(r) => {
                        scan.not_checked(r);
                        continue;
                    }
                },
            },
        };
        let label = if scan.terms.count(commit.as_bytes()) > 0 {
            format!("commit {}", k + 1)
        } else {
            commit.to_owned()
        };
        let prefix = format!("{label}:");
        scan.diff(&osv(&["diff", &parent, commit]), &prefix);
        match git_ok(
            &osv(&["cat-file", "commit", commit]),
            FILE_MAX_BYTES,
            Reason::InputTooLarge,
        ) {
            Ok(raw) => {
                let message = match raw.windows(2).position(|w| w == b"\n\n") {
                    Some(at) => &raw[at + 2..],
                    None => &[][..],
                };
                scan.lines(message, &format!("{prefix}message:"));
            }
            Err(r) => scan.not_checked(r),
        }
        scanned += 1;
    }
    scanned
}

/// The id of the empty tree in this repository's hash.
fn empty_tree_id() -> Result<String, Reason> {
    let out = git_ok(
        &osv(&["hash-object", "-t", "tree", "--stdin"]),
        4096,
        Reason::GitOutput,
    )?;
    let id = String::from_utf8(out).map_err(|_| Reason::GitOutput)?;
    Ok(id.trim().to_owned())
}

fn scan_files(scan: &mut Scan, paths: &[OsString]) {
    for (i, path) in paths.iter().enumerate() {
        let n = i + 1;
        // A `--files` path is an argument, so its places are numbered and
        // the path itself is never printed.
        scan.check(path.as_encoded_bytes(), || format!("path {n}"));
        match read_capped(path, FILE_MAX_BYTES) {
            Ok(data) => {
                let sniff = &data[..data.len().min(BINARY_SNIFF_BYTES)];
                if sniff.contains(&0) {
                    scan.check(&data, || format!("path {n}:binary"));
                } else {
                    scan.lines(&data, &format!("path {n}:"));
                }
            }
            Err(ReadError::Unreadable) => scan.not_checked(Reason::InputUnreadable),
            Err(ReadError::TooLarge) => scan.not_checked(Reason::InputTooLarge),
        }
    }
}

pub fn run(args: &[OsString]) -> ExitCode {
    let Some(mode) = parse_args(args) else {
        emit_not_checked(Reason::Usage);
        emit(USAGE);
        return ExitCode::from(2);
    };
    let mut list_failures = Vec::new();
    let terms = match load_terms(&mut list_failures) {
        Ok(t) => t,
        Err(reasons) => {
            for r in reasons {
                emit_not_checked(r);
            }
            return ExitCode::from(2);
        }
    };
    let mut scan = Scan {
        terms,
        mode: mode.name(),
        hits: 0,
        not_checked: 0,
    };
    for r in list_failures {
        scan.not_checked(r);
    }
    let mut commits = None;
    match &mode {
        Mode::Staged => scan.diff(&osv(&["diff", "--cached"]), ""),
        Mode::Message(file) => scan_message(&mut scan, file),
        Mode::Range(spec) => commits = Some(scan_range(&mut scan, spec)),
        Mode::Files(paths) => scan_files(&mut scan, paths),
        Mode::Text(literal) => scan.check(literal.as_encoded_bytes(), || "text".to_owned()),
    }
    if scan.hits == 0 && scan.not_checked > 0 {
        return ExitCode::from(2);
    }
    let mut summary = format!(
        "private-scan: summary: hits={} not_checked={}",
        scan.hits, scan.not_checked
    );
    if let Some(k) = commits {
        summary.push_str(&format!(" commits={k}"));
    }
    emit(&summary);
    ExitCode::from(if scan.hits > 0 { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_finds_needle_anywhere() {
        assert!(contains(b"abczorblaxdef", b"zorblax"));
        assert!(contains(b"zorblax", b"zorblax"));
        assert!(contains(b"zzorblax", b"zorblax"));
        assert!(!contains(b"zorbla", b"zorblax"));
        assert!(!contains(b"", b"z"));
    }

    #[test]
    fn list_drops_bom_crlf_comments_and_blanks() {
        let mut set = BTreeSet::new();
        parse_list(b"\xEF\xBB\xBFOne\r\n\r\n# two\r\n  three \r\n", &mut set);
        let got: Vec<&[u8]> = set.iter().map(Vec::as_slice).collect();
        assert_eq!(got, vec![b"one".as_slice(), b"three".as_slice()]);
    }

    #[test]
    fn unquote_handles_escapes_and_octal() {
        assert_eq!(
            unquote(br#"b/a\tb\"c\\d\303\251""#),
            Some(b"b/a\tb\"c\\d\xc3\xa9".to_vec())
        );
        assert_eq!(unquote(b"b/unterminated"), None);
    }

    #[test]
    fn header_path_strips_prefix_and_tab() {
        assert_eq!(header_path(b"b/a b.txt\t"), Some(Some(b"a b.txt".to_vec())));
        assert_eq!(header_path(b"/dev/null"), Some(None));
        assert_eq!(header_path(b"\"b/x\\ty\""), Some(Some(b"x\ty".to_vec())));
    }

    #[test]
    fn hunk_header_defaults_count_to_one() {
        assert_eq!(hunk_header(b"@@ -3 +4,2 @@ fn x"), Some((1, 4, 2)));
        assert_eq!(hunk_header(b"@@ -0,0 +1 @@"), Some((0, 1, 1)));
    }

    #[test]
    fn scissors_needs_a_comment_lead() {
        assert!(is_scissors(
            b"# ------------------------ >8 ------------------------"
        ));
        assert!(is_scissors(
            b"; ------------------------ >8 ------------------------\r"
        ));
        assert!(!is_scissors(
            b" ------------------------ >8 ------------------------"
        ));
        assert!(!is_scissors(
            b"text ------------------------ >8 ------------------------ more"
        ));
    }

    #[test]
    fn raw_parses_renames_and_deletions() {
        let raw = b":100644 100644 aaa bbb R090\0old\0new\0:100644 000000 ccc 000 D\0gone\0";
        let entries = parse_raw(raw).unwrap_or_default();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, b"new");
        assert!(!entries[0].deleted);
        assert_eq!(entries[1].path, b"gone");
        assert!(entries[1].deleted);
    }
}
