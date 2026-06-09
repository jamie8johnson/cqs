#!/usr/bin/env python3
"""Reject newly added comments that carry audit/PR provenance instead of substance.

Scans a unified diff (stdin) for ADDED comment lines containing audit-finding
IDs (SEC-V1.38-2, TC-HAP-V1.40-1, P2 #34, DS-W2, ...) or bare PR/issue
citations ((#1234)). These belong in commit messages and CHANGELOG, not code
comments — comments describe what the code IS, not where it came from.

Allowed: TODO/FIXME lines (tracking IDs for future work are legitimate),
string literals (only the comment portion of a line is scanned).

Usage: git diff <base>...HEAD -- '*.rs' | scripts/check_comment_provenance.py
Exit 1 if violations found.
"""

import re
import sys

AUDIT_CATEGORIES = (
    "AC|AD|API|CQ|DOC|DS|EH|EX|EXT|HP|OB|PB|PERF|PF|PL|RB|RM|RT|SEC|SHL|SQ|TC"
)
PATTERNS = [
    # SEC-V1.38-2, TC-HAP-V1.40-1, RT-RES-9, DS-W2, EX-V1.33-4, PERF-22
    re.compile(rf"\b({AUDIT_CATEGORIES})(-[A-Z]+)*-(V?\d[\d.]*-?\d*|W\d+|NEW-\d+)\b"),
    # P1-P4 audit refs: "P2 #34", "P3.28", "P4-14"
    re.compile(r"\bP[1-4][\s.#-]+\d+\b"),
    # bare PR/issue citation in parens: (#1234)
    re.compile(r"\(#\d{3,5}\)"),
]
ALLOW = re.compile(r"\b(TODO|FIXME)\b")


def comment_part(line):
    """Return the // comment text of a Rust line, ignoring // inside strings."""
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
            return line[i:]
        i += 1
    return None


current_file = None
violations = []
for raw in sys.stdin:
    line = raw.rstrip("\n")
    if line.startswith("+++ "):
        current_file = re.sub(r"^[ab]/", "", line[4:])
        continue
    if not line.startswith("+") or line.startswith("+++"):
        continue
    comment = comment_part(line[1:])
    if comment is None or ALLOW.search(comment):
        continue
    for pat in PATTERNS:
        m = pat.search(comment)
        if m:
            violations.append((current_file, m.group(0), comment.strip()[:100]))
            break

if violations:
    print(f"{len(violations)} added comment(s) carry provenance IDs:", file=sys.stderr)
    for f, tag, text in violations:
        print(f"  {f}: [{tag}] {text}", file=sys.stderr)
    print(
        "\nProvenance belongs in commit messages, not comments. Describe what the"
        "\ncode does now; keep tracking IDs only in TODO/FIXME.",
        file=sys.stderr,
    )
    sys.exit(1)
print("no provenance IDs in added comments")
