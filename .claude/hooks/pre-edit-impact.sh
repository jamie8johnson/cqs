#!/bin/bash
# Pre-edit hook: run cqs impact on the function being edited.
# Injects caller count, test coverage, and risk into conversation context.
# Fires on PreToolUse:Edit for Rust source files.

set -euo pipefail

# Read hook input from stdin
INPUT=$(cat)

# Extract file path from tool input
FILE_PATH=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty' 2>/dev/null)

# Only analyze Rust source files in src/
if [[ -z "$FILE_PATH" ]] || [[ ! "$FILE_PATH" =~ ^.*/src/.*\.rs$ ]]; then
    exit 0
fi

# Extract the old_string to guess what function is being edited
OLD_STRING=$(echo "$INPUT" | jq -r '.tool_input.old_string // empty' 2>/dev/null)

# Try to find a function name in the old_string (first fn/pub fn match)
FUNC_NAME=$(echo "$OLD_STRING" | grep -oP '(?:pub\s+)?fn\s+(\w+)' | head -1 | grep -oP '\w+$')

if [[ -z "$FUNC_NAME" ]]; then
    # No function found in the edit — skip analysis
    exit 0
fi

# Run cqs impact (with timeout to avoid blocking)
IMPACT=$(timeout 5 cqs impact "$FUNC_NAME" --json 2>/dev/null) || exit 0

# Extract key metrics
CALLERS=$(echo "$IMPACT" | jq -r '.direct_callers | length' 2>/dev/null || echo "?")
TRANSITIVE=$(echo "$IMPACT" | jq -r '.transitive_callers | length' 2>/dev/null || echo "?")
TESTS=$(echo "$IMPACT" | jq -r '.test_coverage.tested_by | length' 2>/dev/null || echo "?")
RISK=$(echo "$IMPACT" | jq -r '.risk_score // "unknown"' 2>/dev/null || echo "?")

# Only inject context if there are callers (skip leaf functions)
if [[ "$CALLERS" == "0" ]] || [[ "$CALLERS" == "?" ]]; then
    exit 0
fi

# Output additional context as JSON
cat <<EOF
{
  "additionalContext": "Editing ${FUNC_NAME} — ${CALLERS} direct callers, ${TRANSITIVE} transitive, ${TESTS} tests, risk=${RISK}"
}
EOF
