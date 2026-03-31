#!/bin/bash
# Stop hook: run cqs review on uncommitted changes, surface risk to agent.
# Only produces output when there are changed functions with callers/tests at risk.

cd /mnt/c/Projects/cqs || exit 0

# Skip if no uncommitted changes
if git diff --quiet HEAD 2>/dev/null && git diff --cached --quiet HEAD 2>/dev/null; then
    exit 0
fi

# Run review, extract risk summary
REVIEW=$(cqs review --json 2>/dev/null) || exit 0

# Check if there are any changed functions
CHANGED=$(echo "$REVIEW" | python3 -c "import json,sys; d=json.load(sys.stdin); print(len(d.get('changed_functions',[])))" 2>/dev/null)

if [ "$CHANGED" = "0" ] || [ -z "$CHANGED" ]; then
    exit 0
fi

# Build summary and output valid JSON directly from Python
echo "$REVIEW" | python3 -c "
import json, sys
d = json.load(sys.stdin)
fns = d.get('changed_functions', [])
callers = d.get('affected_callers', [])
tests = d.get('affected_tests', [])
risk = d.get('risk_summary', {})
level = risk.get('level', 'unknown')
parts = []
parts.append(f'{len(fns)} changed functions, {len(callers)} affected callers, {len(tests)} affected tests')
if level in ('high', 'medium'):
    parts.append(f'Risk: {level}')
    for f in fns[:5]:
        name = f.get('name', '?')
        c = f.get('caller_count', 0)
        t = f.get('test_count', 0)
        parts.append(f'  {name}: {c} callers, {t} tests')
summary = '; '.join(parts)
print(json.dumps({'systemMessage': f'Diff review: {summary}'}))
" 2>/dev/null
