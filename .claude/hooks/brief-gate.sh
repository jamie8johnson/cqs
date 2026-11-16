#!/bin/sh
# PreToolUse hook for the Agent tool: runs `xtask brief-gate` on the hook's
# input. The gate's refusal (its exit 1) blocks the dispatch: exit 2, with the
# gate's lines on standard error. Anything else lets the dispatch through: a
# pass silently; a gate that did not check, or did not run, with one line of
# context that says so. The gate's own not-checked line goes into the JSON only
# when it holds no `"`, no `\` and no control character; otherwise a fixed
# line says it could not be shown. The time limit ends the gate with TERM
# after 8 s and with KILL one second later.

top="$(cd "$(dirname "$0")/../.." && pwd)"
bin="$top/xtask/target/debug/xtask"

not_checked() {
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}\n' "$1"
    exit 0
}

if [ ! -x "$bin" ]; then
    not_checked "brief-gate: not checked: the xtask binary is not built"
fi
if command -v timeout >/dev/null 2>&1; then
    out="$({ cd "$top" && timeout -k 1 8 "$bin" brief-gate; } 2>/dev/null)"
else
    out="$({ cd "$top" && "$bin" brief-gate; } 2>/dev/null)"
fi
code=$?
first="$(printf '%s\n' "$out" | head -n 1)"
case "$code" in
    0)
        exit 0
        ;;
    1)
        case "$first" in
            "brief-gate: refused: "*)
                printf '%s\n' "$out" >&2
                exit 2
                ;;
        esac
        ;;
    2)
        case "$first" in
            *[\"\\[:cntrl:]]*)
                case "$first" in
                    "brief-gate: not checked: "*)
                        not_checked "brief-gate: not checked: the gate's line could not be shown"
                        ;;
                esac
                ;;
            "brief-gate: not checked: "*)
                not_checked "$first"
                ;;
        esac
        ;;
esac
not_checked "brief-gate: not checked: the gate did not run (exit $code)"
