//! `brief-lint`: checks the form of a lane's brief. It finds the parts by
//! their marks (a `Branch: ` line, a `MODEL: ` line, the sections of
//! `SECTIONS`) and checks that each premise carries a tag that fits it.
//!
//! Output invariant: every line is built from fixed strings, the names of
//! `SECTIONS` and line numbers. No line holds the brief's text or the file's
//! path.
//!
//! The reading of Markdown is deliberately narrow: backtick fences, `# ` and
//! `## ` headings, items that start in the first column, backtick code spans
//! and bracketed tags. Nothing else of Markdown changes what a rule sees.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::process::ExitCode;

/// Largest brief read, in bytes.
pub const BRIEF_MAX_BYTES: u64 = 1 << 20;
/// The sections a brief holds, in the order their faults print. The first is
/// the one whose items are premises.
pub const SECTIONS: &[&str] = &[
    "Premises",
    "Task statements",
    "Decided judgment calls",
    "Stopping",
];
/// The kinds of tag a premise can carry: `[<kind>: <text>]`.
pub const TAG_KINDS: &[&str] = &["counted", "opened", "unchecked"];
/// Words that claim a whole set; a premise that holds one needs a tag other
/// than `opened`.
pub const WHOLE_SET_WORDS: &[&str] = &["none", "all", "every", "only"];

const OPENED: &str = "opened";
const UNCHECKED: &str = "unchecked";

/// The rules, in the order their faults print on one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    MissingBranch,
    MissingModel,
    MissingSection,
    DuplicateSection,
    EmptySection,
    NoPremise,
    UntaggedPremise,
    OpenedWholeSet,
    EmptyTag,
}

impl Rule {
    /// Every rule, in table order.
    #[cfg(test)]
    pub const ALL: [Rule; 9] = [
        Rule::MissingBranch,
        Rule::MissingModel,
        Rule::MissingSection,
        Rule::DuplicateSection,
        Rule::EmptySection,
        Rule::NoPremise,
        Rule::UntaggedPremise,
        Rule::OpenedWholeSet,
        Rule::EmptyTag,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Rule::MissingBranch => "missing-branch",
            Rule::MissingModel => "missing-model",
            Rule::MissingSection => "missing-section",
            Rule::DuplicateSection => "duplicate-section",
            Rule::EmptySection => "empty-section",
            Rule::NoPremise => "no-premise",
            Rule::UntaggedPremise => "untagged-premise",
            Rule::OpenedWholeSet => "opened-whole-set",
            Rule::EmptyTag => "empty-tag",
        }
    }

    /// The fixed message; `section` is a name of `SECTIONS` for the three
    /// section rules and is not read by the others.
    pub fn message(self, section: &str) -> String {
        match self {
            Rule::MissingBranch => {
                "no line that starts with Branch: and a branch name in backticks".to_owned()
            }
            Rule::MissingModel => "no line that starts with MODEL:".to_owned(),
            Rule::MissingSection => format!("no section ## {section}"),
            Rule::DuplicateSection => format!("a second section ## {section}"),
            Rule::EmptySection => format!("section ## {section} has no text outside fenced blocks"),
            Rule::NoPremise => {
                "the Premises section has no numbered or bulleted item".to_owned()
            }
            Rule::UntaggedPremise => {
                "premise has no [counted: ], [opened: ] or [unchecked: ] tag".to_owned()
            }
            Rule::OpenedWholeSet => "premise says none, all, every or only and its tags are all opened: count it, mark it unchecked, or put a word quoted from a file in inline code".to_owned(),
            Rule::EmptyTag => "tag with no text after its colon".to_owned(),
        }
    }
}

/// One fault: a rule, the 1-based line it points at (none for a missing
/// part), and the section name for the section rules (a `SECTIONS` entry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub line: Option<usize>,
    pub rule: Rule,
    pub section: &'static str,
}

/// What the lint found: the faults in print order, and the 1-based first
/// line of each premise tagged `unchecked`, in line order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub faults: Vec<Fault>,
    pub unchecked: Vec<usize>,
}

impl Report {
    /// The output lines of exit 0 and exit 1, in print order.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for f in &self.faults {
            let msg = f.rule.message(f.section);
            match f.line {
                Some(n) => out.push(format!("brief-lint: line {n}: {}: {msg}", f.rule.id())),
                None => out.push(format!("brief-lint: {}: {msg}", f.rule.id())),
            }
        }
        for n in &self.unchecked {
            out.push(format!("brief-lint: line {n}: unchecked premise"));
        }
        out.push(format!(
            "brief-lint: summary: faults={} unchecked={}",
            self.faults.len(),
            self.unchecked.len()
        ));
        out
    }
}

/// Why a brief was not checked. The strings are the whole fixed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Usage,
    Unreadable,
    TooLarge,
    NotUtf8,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Usage => "usage error",
            Reason::Unreadable => "file unreadable",
            Reason::TooLarge => "file over size limit",
            Reason::NotUtf8 => "file not UTF-8",
        }
    }

    /// The one line printed when a brief is not checked.
    pub fn line(self) -> String {
        format!("brief-lint: not checked: {}", self.as_str())
    }
}

fn emit(line: &str) {
    // A failed write to stdout leaves the exit code as the only signal.
    let _ = writeln!(io::stdout().lock(), "{line}");
}

/// Reads a brief: a regular file of at most `BRIEF_MAX_BYTES`, in UTF-8. The
/// path is checked to name a regular file before it is opened, so a named
/// pipe or a device is reported as unreadable and never opened.
pub fn read_brief(path: &OsStr) -> Result<String, Reason> {
    let meta = fs::metadata(path).map_err(|_| Reason::Unreadable)?;
    if !meta.is_file() {
        return Err(Reason::Unreadable);
    }
    if meta.len() > BRIEF_MAX_BYTES {
        return Err(Reason::TooLarge);
    }
    let file = fs::File::open(path).map_err(|_| Reason::Unreadable)?;
    let opened = file.metadata().map_err(|_| Reason::Unreadable)?;
    if !opened.is_file() {
        return Err(Reason::Unreadable);
    }
    let mut buf = Vec::new();
    file.take(BRIEF_MAX_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|_| Reason::Unreadable)?;
    if buf.len() as u64 > BRIEF_MAX_BYTES {
        return Err(Reason::TooLarge);
    }
    String::from_utf8(buf).map_err(|_| Reason::NotUtf8)
}

/// Whether `s` holds nothing but white space (ASCII: space, tab, line ends).
fn is_blank(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_whitespace())
}

/// The number of leading spaces, when it is at most three.
fn indent_upto3(line: &str) -> Option<usize> {
    let n = line.bytes().take_while(|&b| b == b' ').count();
    (n <= 3).then_some(n)
}

fn backtick_run(b: &[u8], from: usize) -> usize {
    b[from..].iter().take_while(|&&c| c == b'`').count()
}

/// The backtick count of a line that opens a fence: three or more backticks
/// after at most three spaces, and no backtick in the rest of the line (the
/// info word), so a line that starts with an inline span opens no fence.
fn fence_open(line: &str) -> Option<usize> {
    let lead = indent_upto3(line)?;
    let b = line.as_bytes();
    let n = backtick_run(b, lead);
    (n >= 3 && !b[lead + n..].contains(&b'`')).then_some(n)
}

/// Whether `line` closes a fence opened with `n` backticks.
fn fence_closes(line: &str, n: usize) -> bool {
    let Some(lead) = indent_upto3(line) else {
        return false;
    };
    let b = line.as_bytes();
    let m = backtick_run(b, lead);
    m >= n && b[lead + m..].iter().all(|&c| c == b' ')
}

/// For each line, whether a rule may read it: false for a fence line and a
/// line inside a fenced block. An unclosed fence runs to the end.
fn readable_lines(lines: &[&str]) -> Vec<bool> {
    let mut out = Vec::with_capacity(lines.len());
    let mut open: Option<usize> = None;
    for line in lines {
        match open {
            Some(n) => {
                if fence_closes(line, n) {
                    open = None;
                }
                out.push(false);
            }
            None => {
                if let Some(n) = fence_open(line) {
                    open = Some(n);
                    out.push(false);
                } else {
                    out.push(true);
                }
            }
        }
    }
    out
}

/// The text of a `# ` or `## ` heading; `### ` and deeper are not headings.
fn heading(line: &str) -> Option<(u8, &str)> {
    if let Some(rest) = line.strip_prefix("# ") {
        Some((1, rest))
    } else {
        line.strip_prefix("## ").map(|rest| (2, rest))
    }
}

/// The index in `SECTIONS` that a `## ` heading's text names.
fn section_index(text: &str) -> Option<usize> {
    SECTIONS.iter().position(|name| {
        text.strip_prefix(name)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

fn has_branch_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("Branch: `") else {
        return false;
    };
    let name_len = rest.bytes().take_while(|&b| b != b'`' && b != b' ').count();
    name_len > 0 && rest.as_bytes().get(name_len) == Some(&b'`')
}

fn has_model_line(line: &str) -> bool {
    line.strip_prefix("MODEL: ")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c != ' ')
}

fn is_item_start(line: &str) -> bool {
    if line.starts_with("- ") || line.starts_with("* ") {
        return true;
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with(". ")
}

/// For each byte of an item, whether it lies in a code span (delimiters
/// included). A run of N backticks closes at the next run of exactly N; a
/// run with no close is plain text.
///
/// Linear in the item's length: each run length keeps a cursor into the
/// list of runs of that length, and the cursor only moves forward, so no
/// run is passed over twice in the search for a close.
fn code_mask(b: &[u8]) -> Vec<bool> {
    let mut mask = vec![false; b.len()];
    // Every maximal run of backticks, as (start, length), in order.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let n = backtick_run(b, i);
            runs.push((i, n));
            i += n;
        } else {
            i += 1;
        }
    }
    let mut by_len: HashMap<usize, Vec<usize>> = HashMap::new();
    for (r, &(_, n)) in runs.iter().enumerate() {
        by_len.entry(n).or_default().push(r);
    }
    let mut cursor: HashMap<usize, usize> = HashMap::new();
    let mut r = 0;
    while r < runs.len() {
        let (start, n) = runs[r];
        let same = by_len.get(&n).map(Vec::as_slice).unwrap_or(&[]);
        let c = cursor.entry(n).or_insert(0);
        while *c < same.len() && same[*c] <= r {
            *c += 1;
        }
        match same.get(*c) {
            Some(&close) => {
                let end = runs[close].0 + n;
                mask[start..end].iter_mut().for_each(|m| *m = true);
                r = close + 1;
            }
            None => r += 1,
        }
    }
    mask
}

/// What a premise holds, as the rules read it.
#[derive(Debug, Default, PartialEq, Eq)]
struct ItemFacts {
    /// The kind of each tag, as an index in `TAG_KINDS`.
    tags: Vec<usize>,
    /// Some tag's text holds nothing but white space.
    empty_tag: bool,
    /// Some word outside code spans and tags is in `WHOLE_SET_WORDS`.
    whole_set_word: bool,
}

fn item_facts(item: &str) -> ItemFacts {
    let b = item.as_bytes();
    let code = code_mask(b);
    let mut in_tag = vec![false; b.len()];
    let mut facts = ItemFacts::default();
    let mut i = 0;
    while i < b.len() {
        if code[i] || b[i] != b'[' {
            i += 1;
            continue;
        }
        let kind = TAG_KINDS.iter().position(|k| {
            b[i + 1..].starts_with(k.as_bytes()) && b.get(i + 1 + k.len()) == Some(&b':')
        });
        let Some(kind) = kind else {
            i += 1;
            continue;
        };
        let text_start = i + 1 + TAG_KINDS[kind].len() + 1;
        let mut depth = 1usize;
        let mut end = b.len();
        for (j, &c) in b.iter().enumerate().skip(text_start) {
            if code[j] {
                continue;
            }
            if c == b'[' {
                depth += 1;
            } else if c == b']' {
                depth -= 1;
                if depth == 0 {
                    end = j;
                    break;
                }
            }
        }
        facts.tags.push(kind);
        if is_blank(&item[text_start..end]) {
            facts.empty_tag = true;
        }
        let stop = (end + 1).min(b.len());
        in_tag[i..stop].iter_mut().for_each(|t| *t = true);
        i = stop;
    }
    let mut word = String::new();
    for (j, &c) in b.iter().enumerate() {
        if !code[j] && !in_tag[j] && c.is_ascii_alphanumeric() {
            word.push(c.to_ascii_lowercase() as char);
            continue;
        }
        if WHOLE_SET_WORDS.contains(&word.as_str()) {
            facts.whole_set_word = true;
        }
        word.clear();
    }
    if WHOLE_SET_WORDS.contains(&word.as_str()) {
        facts.whole_set_word = true;
    }
    facts
}

/// A section found in the brief: the 0-based index of its heading and of the
/// line after its last line.
struct Section {
    heading: usize,
    end: usize,
}

/// Lints a brief's text. A leading byte-order mark is dropped, and `\r\n`
/// ends a line as `\n` does.
pub fn lint(text: &str) -> Report {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let readable = readable_lines(&lines);

    let mut branch = false;
    let mut model = false;
    let mut found: Vec<Vec<Section>> = SECTIONS.iter().map(|_| Vec::new()).collect();
    let mut open: Option<(usize, usize)> = None;
    for (idx, line) in lines.iter().enumerate() {
        if !readable[idx] {
            continue;
        }
        branch |= has_branch_line(line);
        model |= has_model_line(line);
        if let Some((level, text)) = heading(line) {
            if let Some((s, h)) = open.take() {
                found[s].push(Section {
                    heading: h,
                    end: idx,
                });
            }
            if level == 2 {
                open = section_index(text).map(|s| (s, idx));
            }
        }
    }
    if let Some((s, h)) = open {
        found[s].push(Section {
            heading: h,
            end: lines.len(),
        });
    }

    let mut lineless = Vec::new();
    let mut lined = Vec::new();
    let mut unchecked = Vec::new();
    if !branch {
        lineless.push(Fault {
            line: None,
            rule: Rule::MissingBranch,
            section: "",
        });
    }
    if !model {
        lineless.push(Fault {
            line: None,
            rule: Rule::MissingModel,
            section: "",
        });
    }
    for (s, name) in SECTIONS.iter().enumerate() {
        let Some(first) = found[s].first() else {
            lineless.push(Fault {
                line: None,
                rule: Rule::MissingSection,
                section: name,
            });
            continue;
        };
        for dup in &found[s][1..] {
            lined.push(Fault {
                line: Some(dup.heading + 1),
                rule: Rule::DuplicateSection,
                section: name,
            });
        }
        let body = first.heading + 1..first.end;
        let has_text = body.clone().any(|i| readable[i] && !is_blank(lines[i]));
        if !has_text {
            lined.push(Fault {
                line: Some(first.heading + 1),
                rule: Rule::EmptySection,
                section: name,
            });
            continue;
        }
        if s != 0 {
            continue;
        }
        let items = premise_items(&lines, &readable, body);
        if items.is_empty() {
            lined.push(Fault {
                line: Some(first.heading + 1),
                rule: Rule::NoPremise,
                section: name,
            });
        }
        for (start, item) in items {
            let line = Some(start + 1);
            let facts = item_facts(&item);
            if facts.tags.is_empty() {
                lined.push(Fault {
                    line,
                    rule: Rule::UntaggedPremise,
                    section: "",
                });
            }
            let all_opened =
                !facts.tags.is_empty() && facts.tags.iter().all(|&k| TAG_KINDS[k] == OPENED);
            if facts.whole_set_word && all_opened {
                lined.push(Fault {
                    line,
                    rule: Rule::OpenedWholeSet,
                    section: "",
                });
            }
            if facts.empty_tag {
                lined.push(Fault {
                    line,
                    rule: Rule::EmptyTag,
                    section: "",
                });
            }
            if facts.tags.iter().any(|&k| TAG_KINDS[k] == UNCHECKED) {
                unchecked.push(start + 1);
            }
        }
    }
    lined.sort_by_key(|f| (f.line, f.rule));
    lineless.extend(lined);
    Report {
        faults: lineless,
        unchecked,
    }
}

/// The items of the Premises section: the 0-based index of each item's first
/// line, and its lines joined by `\n`. An item runs to the line before the
/// next item start, blank line, fence line or heading.
fn premise_items(
    lines: &[&str],
    readable: &[bool],
    body: std::ops::Range<usize>,
) -> Vec<(usize, String)> {
    let mut items: Vec<(usize, String)> = Vec::new();
    let mut in_item = false;
    for i in body {
        let line = lines[i];
        if !readable[i] || is_blank(line) || heading(line).is_some() {
            in_item = false;
            continue;
        }
        if is_item_start(line) {
            items.push((i, line.to_owned()));
            in_item = true;
        } else if in_item {
            if let Some((_, text)) = items.last_mut() {
                text.push('\n');
                text.push_str(line);
            }
        }
    }
    items
}

pub fn run(args: &[OsString]) -> ExitCode {
    if args.len() != 1 {
        emit(&Reason::Usage.line());
        return ExitCode::from(2);
    }
    let text = match read_brief(&args[0]) {
        Ok(t) => t,
        Err(r) => {
            emit(&r.line());
            return ExitCode::from(2);
        }
    };
    let report = lint(&text);
    for line in report.lines() {
        emit(&line);
    }
    ExitCode::from(if report.faults.is_empty() { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A brief with every part and no fault; its premises are lines 6 to 8.
    const PASS: &str = "Branch: `feat/knob`\n\
                        MODEL: any\n\
                        \n\
                        ## Premises\n\
                        \n\
                        1. The knob is round. [counted: `ls knobs`]\n\
                        2. The lever is green. [opened: `lever.txt:3`]\n\
                        3. The paint is dry. [unchecked: not touched]\n\
                        \n\
                        ## Task statements\n\
                        \n\
                        Paint it.\n\
                        \n\
                        ## Decided judgment calls\n\
                        \n\
                        - Blue.\n\
                        \n\
                        ## Stopping\n\
                        \n\
                        Stop when it is blue.\n";

    /// `PASS` with its second premise replaced by `item` (one or more lines).
    fn with_premise(item: &str) -> String {
        let old = "2. The lever is green. [opened: `lever.txt:3`]";
        assert!(PASS.contains(old));
        PASS.replace(old, item)
    }

    /// Each fault as `(line, rule id, section)`.
    fn faults(text: &str) -> Vec<(Option<usize>, &'static str, &'static str)> {
        lint(text)
            .faults
            .iter()
            .map(|f| (f.line, f.rule.id(), f.section))
            .collect()
    }

    fn one(line: usize, rule: Rule) -> Vec<(Option<usize>, &'static str, &'static str)> {
        vec![(Some(line), rule.id(), "")]
    }

    #[test]
    fn pass_has_no_fault_and_one_unchecked_premise() {
        assert_eq!(
            lint(PASS),
            Report {
                faults: vec![],
                unchecked: vec![8]
            }
        );
        assert_eq!(
            lint(PASS).lines(),
            [
                "brief-lint: line 8: unchecked premise",
                "brief-lint: summary: faults=0 unchecked=1"
            ]
        );
    }

    #[test]
    fn a_whole_set_word_needs_a_tag_other_than_opened() {
        for word in WHOLE_SET_WORDS {
            let opened = format!("2. The lever is {word} green. [opened: `lever.txt:3`]");
            assert_eq!(
                faults(&with_premise(&opened)),
                one(7, Rule::OpenedWholeSet),
                "{word}"
            );
            for kind in ["counted", "unchecked"] {
                let two = format!("{opened} [{kind}: by hand]");
                assert_eq!(faults(&with_premise(&two)), vec![], "{word} with {kind}");
            }
            let upper = format!(
                "2. The lever is {} green. [opened: `lever.txt:3`]",
                word.to_ascii_uppercase()
            );
            assert_eq!(
                faults(&with_premise(&upper)),
                one(7, Rule::OpenedWholeSet),
                "{word} in capitals"
            );
            let quoted = format!("2. The file says `{word} green`. [opened: `lever.txt:3`]");
            assert_eq!(faults(&with_premise(&quoted)), vec![], "{word} in code");
            let in_tag = format!("2. The lever is green. [opened: `lever.txt:3` {word}]");
            assert_eq!(faults(&with_premise(&in_tag)), vec![], "{word} in a tag");
        }
    }

    #[test]
    fn a_whole_set_word_inside_a_longer_word_is_no_word() {
        for longer in ["everyone", "allowed", "nonetheless", "onlyx"] {
            let item = format!("2. The lever is {longer} green. [opened: `lever.txt:3`]");
            assert_eq!(faults(&with_premise(&item)), vec![], "{longer}");
        }
    }

    #[test]
    fn an_untagged_premise_with_a_whole_set_word_is_only_untagged() {
        let item = "2. The lever is all green.";
        assert_eq!(faults(&with_premise(item)), one(7, Rule::UntaggedPremise));
    }

    #[test]
    fn each_missing_section_is_its_one_fault() {
        for (s, name) in SECTIONS.iter().enumerate() {
            let heading = format!("## {name}\n");
            let start = PASS.find(&heading).expect("section in PASS");
            let end = SECTIONS
                .get(s + 1)
                .map(|next| PASS.find(&format!("## {next}\n")).expect("next section"))
                .unwrap_or(PASS.len());
            let text = format!("{}{}", &PASS[..start], &PASS[end..]);
            assert_eq!(
                faults(&text),
                vec![(None, Rule::MissingSection.id(), *name)],
                "{name}"
            );
        }
    }

    #[test]
    fn a_heading_that_only_starts_with_a_name_or_is_level_three_is_no_section() {
        for heading in ["## Stoppingx", "### Stopping"] {
            let text = PASS.replace("## Stopping", heading);
            assert_eq!(
                faults(&text),
                vec![(None, Rule::MissingSection.id(), "Stopping")],
                "{heading}"
            );
        }
        let longer = PASS.replace("## Stopping", "## Stopping (when to)");
        assert_eq!(faults(&longer), vec![]);
    }

    #[test]
    fn each_tag_kind_is_a_tag() {
        for kind in TAG_KINDS {
            let item = format!("2. The lever is green. [{kind}: `lever.txt:3`]");
            assert_eq!(faults(&with_premise(&item)), vec![], "{kind}");
        }
        let item = "2. The lever is green. [Opened: `lever.txt:3`]";
        assert_eq!(faults(&with_premise(item)), one(7, Rule::UntaggedPremise));
    }

    #[test]
    fn a_tag_closes_at_the_bracket_that_balances_it() {
        let item = "2. The lever is green. [opened: a [b] all c]";
        assert_eq!(faults(&with_premise(item)), vec![]);
        let item = "2. The lever is green. [opened: a [b] c] all";
        assert_eq!(faults(&with_premise(item)), one(7, Rule::OpenedWholeSet));
        let item = "2. The lever is green. [opened: a `]` all c]";
        assert_eq!(faults(&with_premise(item)), vec![]);
    }

    #[test]
    fn an_item_runs_over_an_indented_bullet_to_its_tag() {
        let item = "2. The lever is green:\n  - on the left side,\n  as the file says. [opened: `lever.txt:3`]";
        assert_eq!(faults(&with_premise(item)), vec![]);
        let item = "2. The lever is green,\nas a second line says,\nand a third. [counted: `wc -l lever.txt`]";
        assert_eq!(faults(&with_premise(item)), vec![]);
        let item =
            "2. The lever is green,\nas a second line says,\nall of it. [opened: `lever.txt:3`]";
        assert_eq!(faults(&with_premise(item)), one(7, Rule::OpenedWholeSet));
    }

    #[test]
    fn an_item_ends_at_a_blank_line_a_fence_or_a_heading() {
        let blank = "2. The lever is green.\n\n[opened: `lever.txt:3`]";
        assert_eq!(faults(&with_premise(blank)), one(7, Rule::UntaggedPremise));
        let fence = "2. The lever is green.\n```\n[opened: `lever.txt:3`]\n```";
        assert_eq!(faults(&with_premise(fence)), one(7, Rule::UntaggedPremise));
    }

    #[test]
    fn two_unchecked_tags_and_one_empty_tag() {
        let item = "2. The lever is green. [unchecked: a] [unchecked: b] [opened:  ]";
        let report = lint(&with_premise(item));
        assert_eq!(
            report
                .faults
                .iter()
                .map(|f| (f.line, f.rule))
                .collect::<Vec<_>>(),
            vec![(Some(7), Rule::EmptyTag)]
        );
        assert_eq!(report.unchecked, vec![7, 8]);
    }

    #[test]
    fn a_tag_of_one_code_span_is_not_empty() {
        let item = "2. The lever is green. [counted: `ls`]";
        assert_eq!(faults(&with_premise(item)), vec![]);
        let item = "2. The lever is green. [counted:]";
        assert_eq!(faults(&with_premise(item)), one(7, Rule::EmptyTag));
    }

    #[test]
    fn a_branch_name_needs_backticks() {
        let text = PASS.replace("Branch: `feat/knob`", "Branch: main");
        assert_eq!(faults(&text), vec![(None, Rule::MissingBranch.id(), "")]);
        let text = PASS.replace("Branch: `feat/knob`", "Branch: `feat knob`");
        assert_eq!(faults(&text), vec![(None, Rule::MissingBranch.id(), "")]);
        let text = PASS.replace("Branch: `feat/knob`", "Branch: `none` ");
        assert_eq!(faults(&text), vec![]);
    }

    #[test]
    fn a_model_line_needs_text() {
        let text = PASS.replace("MODEL: any", "MODEL:  any");
        assert_eq!(faults(&text), vec![(None, Rule::MissingModel.id(), "")]);
    }

    #[test]
    fn a_heading_inside_a_fence_is_not_read() {
        let text = PASS.replace("## Premises\n", "```\n## Premises\n```\n");
        assert_eq!(
            faults(&text),
            vec![(None, Rule::MissingSection.id(), "Premises")]
        );
    }

    #[test]
    fn a_four_backtick_fence_holds_a_three_backtick_line() {
        // Lines 13 to 17 are the fence; the real heading moves to line 20.
        let text = PASS.replace(
            "Paint it.\n",
            "Paint it.\n````md\n```\n## Decided judgment calls\n```\n````\nText.\n",
        );
        assert_eq!(faults(&text), vec![]);
        // With three backticks the fence closes at line 14, so line 15 is
        // a heading and the real one is a second section.
        let text = PASS.replace(
            "Paint it.\n",
            "Paint it.\n```md\n```\n## Decided judgment calls\n```\n```\nText.\n",
        );
        assert_eq!(
            faults(&text),
            vec![(
                Some(20),
                Rule::DuplicateSection.id(),
                "Decided judgment calls"
            )]
        );
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let text = PASS.replace(
            "## Decided judgment calls\n",
            "```\n## Decided judgment calls\n",
        );
        assert_eq!(
            faults(&text),
            vec![
                (None, Rule::MissingSection.id(), "Decided judgment calls"),
                (None, Rule::MissingSection.id(), "Stopping"),
            ]
        );
    }

    #[test]
    fn a_fence_after_more_than_three_spaces_is_no_fence() {
        let text = PASS.replace("Paint it.\n", "Paint it.\n    ```\n");
        assert_eq!(faults(&text), vec![]);
        let text = PASS.replace("Paint it.\n", "Paint it.\n   ```\n");
        assert_eq!(
            faults(&text),
            vec![
                (None, Rule::MissingSection.id(), "Decided judgment calls"),
                (None, Rule::MissingSection.id(), "Stopping"),
            ]
        );
    }

    #[test]
    fn a_two_backtick_code_span_holds_one_backtick() {
        let item = "2. The file says `` all ` of it ``. [opened: `lever.txt:3`]";
        assert_eq!(faults(&with_premise(item)), vec![]);
        // A run of one backtick with no run of exactly one after it is
        // plain text, so the word is read.
        let item = "2. The file says ` all `` of it. [opened: lever.txt line 3]";
        assert_eq!(faults(&with_premise(item)), one(7, Rule::OpenedWholeSet));
    }

    /// The rule of `code_mask` read directly: from each run, search forward
    /// for the next run of the same length.
    fn code_mask_by_search(b: &[u8]) -> Vec<bool> {
        let mut mask = vec![false; b.len()];
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'`' {
                i += 1;
                continue;
            }
            let n = backtick_run(b, i);
            let mut j = i + n;
            let mut close = None;
            while j < b.len() {
                if b[j] == b'`' {
                    let m = backtick_run(b, j);
                    if m == n {
                        close = Some(j);
                        break;
                    }
                    j += m;
                } else {
                    j += 1;
                }
            }
            match close {
                Some(j) => {
                    mask[i..j + n].iter_mut().for_each(|m| *m = true);
                    i = j + n;
                }
                None => i += n,
            }
        }
        mask
    }

    #[test]
    fn code_mask_matches_the_forward_search_on_every_short_item() {
        for len in 0..=12u32 {
            for bits in 0..(1u32 << len) {
                let item: Vec<u8> = (0..len)
                    .map(|k| if bits >> k & 1 == 1 { b'`' } else { b'a' })
                    .collect();
                assert_eq!(
                    code_mask(&item),
                    code_mask_by_search(&item),
                    "{}",
                    String::from_utf8_lossy(&item)
                );
            }
        }
    }

    #[test]
    fn a_tag_inside_a_code_span_is_no_tag() {
        let item = "2. The lever is green. `[opened: lever.txt:3]`";
        assert_eq!(faults(&with_premise(item)), one(7, Rule::UntaggedPremise));
    }

    #[test]
    fn duplicate_empty_and_premise_free_sections() {
        let text = format!("{PASS}\n## Task statements\n\nAgain.\n");
        assert_eq!(
            faults(&text),
            vec![(Some(22), Rule::DuplicateSection.id(), "Task statements")]
        );
        let text = PASS.replace("Paint it.\n", "");
        assert_eq!(
            faults(&text),
            vec![(Some(10), Rule::EmptySection.id(), "Task statements")]
        );
        let text = PASS.replace("Paint it.\n", "```\nPaint it.\n```\n");
        assert_eq!(
            faults(&text),
            vec![(Some(10), Rule::EmptySection.id(), "Task statements")]
        );
        let premises = "1. The knob is round. [counted: `ls knobs`]\n\
                        2. The lever is green. [opened: `lever.txt:3`]\n\
                        3. The paint is dry. [unchecked: not touched]\n";
        let text = PASS.replace(premises, "The premises are in the plan.\n");
        assert_eq!(
            lint(&text),
            Report {
                faults: vec![Fault {
                    line: Some(4),
                    rule: Rule::NoPremise,
                    section: "Premises"
                }],
                unchecked: vec![]
            }
        );
        let text = PASS.replace(premises, "");
        assert_eq!(
            faults(&text),
            vec![(Some(4), Rule::EmptySection.id(), "Premises")]
        );
    }

    #[test]
    fn a_line_whose_rest_holds_a_backtick_opens_no_fence() {
        // Three backticks, a word and three backticks start an inline span,
        // not a fence: the item goes on to its tag.
        let item = "2. The file says\n```all``` in line 3 [opened: f:3]";
        assert_eq!(faults(&with_premise(item)), vec![]);
        // Inside an open fence such a line does not close it: the close
        // keeps its own rule (nothing but spaces after the run), so the
        // heading on the next line stays inside the fence.
        let text = PASS.replace(
            "Paint it.\n",
            "Paint it.\n```\n```all``` x\n## Decided judgment calls\n```\n",
        );
        assert_eq!(faults(&text), vec![]);
        // An info word alone after the run still opens a fence.
        let text = PASS.replace(
            "Paint it.\n",
            "Paint it.\n```text\n## Decided judgment calls\n```\n",
        );
        assert_eq!(faults(&text), vec![]);
    }

    #[test]
    fn a_fence_closes_only_on_a_run_as_long_and_nothing_after_it() {
        // Text after the run: no close, so the heading stays fenced.
        let text = PASS.replace(
            "Paint it.\n",
            "Paint it.\n```\n``` x\n## Decided judgment calls\n```\n",
        );
        assert_eq!(faults(&text), vec![]);
        // A longer run closes, so the brief after it is read.
        let text = PASS.replace("Paint it.\n", "Paint it.\n```\nx\n````\nText.\n");
        assert_eq!(faults(&text), vec![]);
        // Four spaces before the run: no close.
        let text = PASS.replace(
            "Paint it.\n",
            "Paint it.\n```\nx\n    ```\n## Decided judgment calls\n```\n",
        );
        assert_eq!(faults(&text), vec![]);
    }

    #[test]
    fn a_level_one_heading_ends_a_section() {
        let text = PASS.replace(
            "3. The paint is dry. [unchecked: not touched]\n",
            "3. The paint is dry. [unchecked: not touched]\n# Notes\n- not a premise\n",
        );
        assert_eq!(faults(&text), vec![]);
        let text = PASS.replace(
            "## Stopping\n\nStop when it is blue.\n",
            "## Stopping\n# Appendix\nStop when it is blue.\n",
        );
        assert_eq!(
            faults(&text),
            vec![(Some(18), Rule::EmptySection.id(), "Stopping")]
        );
    }

    #[test]
    fn branch_and_model_lines_inside_a_fence_are_not_read() {
        let text = PASS.replace(
            "Branch: `feat/knob`\nMODEL: any\n",
            "```\nBranch: `feat/knob`\nMODEL: any\n```\n",
        );
        assert_eq!(
            faults(&text),
            vec![
                (None, Rule::MissingBranch.id(), ""),
                (None, Rule::MissingModel.id(), ""),
            ]
        );
    }

    #[test]
    fn a_tag_with_no_closing_bracket_runs_to_the_end_of_the_item() {
        let item = "2. The lever is green. [opened: f.txt all of it";
        assert_eq!(faults(&with_premise(item)), vec![]);
        let item = "2. The lever is green. [unchecked: a guess,\nall of it";
        assert_eq!(faults(&with_premise(item)), vec![]);
        assert_eq!(lint(&with_premise(item)).unchecked, vec![7, 9]);
        let item = "2. The lever is green. [counted:";
        assert_eq!(faults(&with_premise(item)), one(7, Rule::EmptyTag));
    }

    #[test]
    fn tags_do_not_nest() {
        // A bracket inside a tag is text of that tag: the inner opener is
        // no tag, so it neither saves the outer one, nor lists the item as
        // unchecked, nor counts as an empty tag.
        let item = "2. All knobs are round. [opened: f.txt:3 [counted: `ls`]]";
        assert_eq!(faults(&with_premise(item)), one(7, Rule::OpenedWholeSet));
        let item = "2. The lever is green. [opened: f.txt:3 [unchecked: guess]]";
        assert_eq!(lint(&with_premise(item)).unchecked, vec![8]);
        assert_eq!(faults(&with_premise(item)), vec![]);
        let item = "2. The lever is green. [opened: [counted:]]";
        assert_eq!(faults(&with_premise(item)), vec![]);
    }

    #[test]
    fn a_section_of_white_space_or_a_fence_alone_has_no_text() {
        for body in ["\t\n", "  \t \n", "```\nPaint it.\n```\n"] {
            let text = PASS.replace("Paint it.\n", body);
            assert_eq!(
                faults(&text),
                vec![(Some(10), Rule::EmptySection.id(), "Task statements")],
                "{body:?}"
            );
        }
        assert_eq!(
            Rule::EmptySection.message("Task statements"),
            "section ## Task statements has no text outside fenced blocks"
        );
    }

    #[test]
    fn a_byte_order_mark_and_crlf_read_as_plain_text() {
        let crlf = format!("\u{feff}{}", PASS.replace('\n', "\r\n"));
        assert_eq!(lint(&crlf), lint(PASS));
    }

    #[test]
    fn the_opened_whole_set_message_names_each_word() {
        let msg = Rule::OpenedWholeSet.message("");
        for word in WHOLE_SET_WORDS {
            assert!(
                msg.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|w| w == *word),
                "{word}"
            );
        }
    }

    #[test]
    fn rule_ids_are_the_fixed_set_in_table_order() {
        let ids: Vec<&str> = Rule::ALL.iter().map(|r| r.id()).collect();
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
        let mut sorted = Rule::ALL;
        sorted.sort();
        assert_eq!(sorted, Rule::ALL);
        assert_eq!(SECTIONS[0], "Premises");
    }

    #[test]
    fn no_rule_message_holds_a_quote_or_a_backslash() {
        for rule in Rule::ALL {
            for name in SECTIONS {
                let msg = rule.message(name);
                assert!(!msg.contains('"') && !msg.contains('\\'), "{}", rule.id());
            }
        }
    }
}
