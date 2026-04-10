#!/usr/bin/env python3
"""PreToolUse hook for Edit: runs `cqs impact` when a specific function is being edited.

Only fires when `old_string` contains a `fn foo` / `pub fn foo` declaration.
Non-fn edits (schema, comments, match arms, string literals) produce no output,
keeping context injection focused on risky function-targeted changes.
"""
import json
import os
import re
import subprocess
import sys

inp = json.load(sys.stdin)
file_path = inp.get("tool_input", {}).get("file_path", "")
old_string = inp.get("tool_input", {}).get("old_string", "")

# Only analyse Rust source files in src/
if not file_path.endswith(".rs") or "/src/" not in file_path:
    sys.exit(0)

# Look for a fn / pub fn / pub async fn / pub(crate) fn declaration in old_string.
# We take the first match — if the edit spans multiple functions the first is a
# reasonable proxy for "what's being changed".
match = re.search(r"(?:pub(?:\s*\([^)]+\))?\s+)?(?:async\s+)?fn\s+(\w+)", old_string)
if not match:
    sys.exit(0)

func_name = match.group(1)

try:
    result = subprocess.run(
        ["cqs", "impact", func_name, "--json"],
        capture_output=True,
        text=True,
        timeout=5,
    )
    if result.returncode != 0 or not result.stdout.strip():
        sys.exit(0)
    data = json.loads(result.stdout)
except Exception:
    sys.exit(0)

callers = int(data.get("caller_count", 0) or 0)
tests = int(data.get("test_count", 0) or 0)
type_impacted = int(data.get("type_impacted_count", 0) or 0)

# Don't spam for leaf functions with no tests.
if callers == 0 and tests == 0:
    sys.exit(0)

try:
    rel = os.path.relpath(file_path)
except ValueError:
    rel = file_path

# Rough risk heuristic: many callers + many tests = high blast radius.
if callers >= 10 or tests >= 20:
    risk = "high"
elif callers >= 3 or tests >= 5:
    risk = "medium"
else:
    risk = "low"

summary = (
    f"Editing {func_name} in {rel} — "
    f"{callers} callers, {tests} tests"
    f"{f', {type_impacted} type deps' if type_impacted else ''}"
    f", risk={risk}"
)

print(
    json.dumps(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": summary,
            }
        }
    )
)
