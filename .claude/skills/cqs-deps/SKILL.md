---
name: cqs-deps
description: Show type dependencies — who uses a type, or what types a function uses.
disable-model-invocation: false
argument-hint: "<type_or_function_name> [--reverse]"
---

# Type Dependencies

Parse arguments:
- First positional arg = type name (forward) or function name (with --reverse)
- `--reverse` flag = show types used by a function instead of type users

Forward (default): `cqs deps "<name>" --json -q` — who uses this type?
Reverse: `cqs deps --reverse "<name>" --json -q` — what types does this function use?

Present the results to the user. Forward mode shows chunks that reference the type. Reverse mode shows types a function depends on, with edge kinds (Param, Return, Field, Impl, Bound, Alias).
