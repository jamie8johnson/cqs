#!/usr/bin/env python3
"""Reject process-cwd target resolution on the daemon request surface.

Invariant: daemon request paths resolve their filesystem target from the
served-project root carried in BatchContext (`ctx.root` / `BatchView::root`),
never from the running process's current working directory. The daemon is a
long-lived process that may serve a project rooted somewhere other than its
launch cwd (and a future multi-project daemon serves several at once), so a
handler that reads `std::env::current_dir()`, calls `find_project_root()` at
request time, or runs a relative-path / cwd-defaulting filesystem op silently
mis-targets — it operates on the wrong tree. Such a call is correct ONLY by the
accident that `cqs-watch.service` sets `WorkingDirectory` to the served
project; remove that accident and it breaks.

The two historical instances of this class (the kind-fallbacks telemetry
recorder and `run_git_diff`) were fixed by threading the served root from
`ctx`; the cores they call now take an explicit root argument. This guard keeps
the class closed: it FAILS when a new cwd-reading call is added under the daemon
request surface (`src/cli/batch/`, excluding test code), forcing the author to
thread `ctx.root` instead.

Scope: `src/cli/batch/**/*.rs` production code only. The CLI-direct command
entry points (`cmd_*` in `src/cli/commands/`) legitimately use
`find_project_root()` — they run inline on the invoking process, not as a
daemon request — so they are out of scope. The shared `*_core` helpers those
CLI entries share with the daemon take the root as an argument and so carry no
cwd call to flag.

Allowlist: the one-time daemon-startup bind in `session.rs`, where
`find_project_root()` IS the correct call (it establishes the served root once,
which is then threaded to every request via `BatchContext`).

Usage: scripts/check_daemon_cwd.py [SRC_ROOT]
  SRC_ROOT defaults to the repo's `src/cli/batch` relative to this script.
Exit 1 if a banned call is found in production code outside the allowlist.
"""

import os
import re
import sys

# Banned process-state filesystem-target resolutions. Each matches a call that
# resolves a target from process cwd rather than from a threaded served root.
BANNED = [
    re.compile(r"\bstd::env::current_dir\s*\("),
    re.compile(r"\.current_dir\s*\("),  # Command::new(..).current_dir(<cwd>)
    re.compile(r"\bset_current_dir\s*\("),
    re.compile(r"\bfind_project_root\s*\("),
    re.compile(r'\benv::var\s*\(\s*"PWD"'),
]

# Allowlist of (file-suffix, exact-code-line) pairs that are the legitimate
# sites. Keep this list TINY and documented. The match is the WHOLE
# whitespace-normalized code line, not just the token, so adding a SECOND
# cwd-reading call to an allowlisted file still trips — only the precise line
# below is exempt.
#
# - session.rs: the daemon-startup bind. `create_context_with_runtime` resolves
#   the served root ONCE at process start, then threads it to every request via
#   BatchContext. This is the source of the `ctx.root` every handler must use;
#   it is not a per-request cwd read. (The same file's `cmd_batch` stdin loop is
#   a per-request path, so a find_project_root added THERE must NOT be allowed —
#   hence the exact-line match.)
ALLOW = [
    ("cli/batch/session.rs", "let root = crate::cli::config::find_project_root();"),
]


def _norm(s):
    return " ".join(s.split())


def is_allowed(relpath, snippet):
    norm_path = relpath.replace(os.sep, "/")
    norm_snip = _norm(snippet)
    for suffix, exact_line in ALLOW:
        if norm_path.endswith(suffix) and norm_snip == _norm(exact_line):
            return True
    return False


def strip_block_comments(text):
    """Blank out /* ... */ block comments so a banned token inside one is inert.
    Preserves newlines so line numbers stay accurate."""
    out = []
    i = 0
    n = len(text)
    in_block = False
    while i < n:
        if not in_block and text[i] == "/" and i + 1 < n and text[i + 1] == "*":
            in_block = True
            out.append("  ")
            i += 2
        elif in_block and text[i] == "*" and i + 1 < n and text[i + 1] == "/":
            in_block = False
            out.append("  ")
            i += 2
        elif in_block:
            out.append("\n" if text[i] == "\n" else " ")
            i += 1
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def code_part(line):
    """Return the line with its trailing `//` line comment removed, ignoring
    `//` inside a string literal. A banned token mentioned in a comment is not a
    call, so it must not trip the guard (mirrors check_comment_provenance's
    string-aware scan, inverted: we keep CODE, drop the comment)."""
    in_str = False
    escape = False
    i = 0
    while i < len(line) - 1:
        c = line[i]
        if escape:
            escape = False
        elif c == "\\" and in_str:
            escape = True
        elif c == '"':
            in_str = not in_str
        elif not in_str and c == "/" and line[i + 1] == "/":
            return line[:i]
        i += 1
    return line


# A test module/function is gated by an attribute on the line(s) above it. We
# track the brace depth at which a `#[cfg(test)]`-gated item opened, and treat
# everything until its matching close brace as test code (out of scope).
CFG_TEST = re.compile(r"#\[\s*cfg\s*\(\s*test\s*\)\s*\]")
TEST_ATTR = re.compile(r"#\[\s*test\s*\]")


def scan_file(path, relpath):
    raw = open(path, encoding="utf-8").read()
    text = strip_block_comments(raw)
    lines = text.split("\n")

    violations = []
    # Depth tracking: when a #[cfg(test)] (or #[test]) attribute is seen, the
    # NEXT `{` opens a test scope; we record the brace depth just inside it and
    # skip all lines until depth returns below that.
    depth = 0
    test_pending = False  # saw a test attribute, awaiting the opening brace
    test_depth_stack = []  # depths at which an active test scope was entered

    for idx, line in enumerate(lines, start=1):
        code = code_part(line)

        # Attribute lines themselves carry no calls; note a pending test scope.
        if CFG_TEST.search(line) or TEST_ATTR.search(line):
            test_pending = True

        in_test = bool(test_depth_stack)

        # Flag banned calls only in production (non-test) code, off the allowlist.
        if not in_test:
            for pat in BANNED:
                m = pat.search(code)
                if m:
                    snippet = line.strip()[:120]
                    if not is_allowed(relpath, snippet):
                        violations.append((idx, m.group(0), snippet))
                    break

        # Update brace depth using the comment-stripped code, and resolve any
        # pending test scope on the brace that opens it.
        for ch in code:
            if ch == "{":
                if test_pending:
                    test_depth_stack.append(depth)
                    test_pending = False
                depth += 1
            elif ch == "}":
                depth -= 1
                if test_depth_stack and depth == test_depth_stack[-1]:
                    test_depth_stack.pop()

    return violations


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    if len(sys.argv) > 1:
        src_root = sys.argv[1]
    else:
        src_root = os.path.normpath(os.path.join(here, "..", "src", "cli", "batch"))

    if not os.path.isdir(src_root):
        print(f"daemon-cwd guard: scan root not found: {src_root}", file=sys.stderr)
        sys.exit(2)

    all_violations = []
    for dirpath, _dirs, files in os.walk(src_root):
        for fname in files:
            if not fname.endswith(".rs"):
                continue
            path = os.path.join(dirpath, fname)
            relpath = os.path.relpath(path, os.path.join(here, ".."))
            for line_no, tok, snippet in scan_file(path, relpath):
                all_violations.append((relpath, line_no, tok, snippet))

    if all_violations:
        print(
            f"{len(all_violations)} daemon request-path cwd violation(s):",
            file=sys.stderr,
        )
        for relpath, line_no, tok, snippet in all_violations:
            print(f"  {relpath}:{line_no}: [{tok}] {snippet}", file=sys.stderr)
        print(
            "\nDaemon request paths must resolve their filesystem target from the"
            "\nserved-project root (ctx.root / BatchView::root), never from process"
            "\ncwd. Thread the root from BatchContext instead of calling"
            "\ncurrent_dir() / find_project_root() at request time. If this is a"
            "\ngenuinely correct site, add it to ALLOW in scripts/check_daemon_cwd.py"
            "\nwith a justification.",
            file=sys.stderr,
        )
        sys.exit(1)
    print("no daemon request-path cwd resolution found")


if __name__ == "__main__":
    main()
