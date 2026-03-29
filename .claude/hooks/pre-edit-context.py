#!/usr/bin/env python3
"""PreToolUse hook for Edit: injects cqs module context for .rs files."""
import json, sys, subprocess, os

inp = json.load(sys.stdin)
f = inp.get('tool_input', {}).get('file_path', '')
if not f.endswith('.rs'):
    sys.exit(0)

try:
    f = os.path.relpath(f)
except ValueError:
    pass

try:
    result = subprocess.run(
        ['cqs', 'context', f, '--json'],
        capture_output=True, text=True, timeout=10
    )
    if result.returncode != 0 or not result.stdout.strip():
        sys.exit(0)
    data = json.loads(result.stdout)
    chunks = data.get('chunks', [])
    ext_callers = data.get('external_callers', [])
    if not chunks:
        sys.exit(0)
    summary = f'Editing {f} — {len(chunks)} functions, {len(ext_callers)} external callers'
    for c in chunks[:10]:
        summary += f"\n  {c.get('chunk_type','?')} {c.get('name','?')}"
    if len(chunks) > 10:
        summary += f"\n  ... and {len(chunks)-10} more"
    out = {
        'hookSpecificOutput': {
            'hookEventName': 'PreToolUse',
            'additionalContext': summary
        }
    }
    print(json.dumps(out))
except Exception:
    pass  # Never block edits on hook failure
