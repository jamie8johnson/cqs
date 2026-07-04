#!/usr/bin/env python3
r"""Ledgered guard for dead-code-on-a-sibling-target regressions (Windows).

## The gap this closes

`.github/workflows/ci.yml` builds and lints only on `ubuntu-latest`. A `#[cfg]`
divergence that compiles clean on Linux but rots on a sibling target in the
release matrix is invisible to PR CI. The `windows-check` job caught the
HARD-ERROR members of that family (E0432 / E0277) via a native `cargo check`,
but it runs WITHOUT `-D warnings`, so the WARNING-only member — a `cfg(unix)`-
gated helper whose Windows twin becomes `dead_code` because nothing on the
Windows target references it — was unguarded. `platform_cfg_sweep_test.rs`
pins only the E0277 unsized-binding shape; a fresh straggler in this class ships
green on both Linux CI (cfg'd out) and windows-check (warnings not denied).

## Why a ledger, not `-D warnings`

Windows carries a large, INTENTIONAL `dead_code` backlog: daemon-client and
MCP-bridge code that is live only via the (unix-only) socket path and becomes
live on Windows when the named-pipe transport lands. Flipping the job to
`-D warnings` would force annotating ~100 items that then invert. So instead we
freeze today's backlog into a checked-in ledger and fail only on:

  * a dead_code item NOT in the ledger      -> a NEW straggler (the regression), and
  * a ledger entry with NO matching item    -> STALE (the backlog shrank; the
                                               item was made live or deleted).

The STALE half is the anti-stale floor (mirrors the JSON-noise guard's exact-
match ledger): a cleared backlog item is celebrated and removed, never silently
kept. When the backlog reaches zero the ledger is empty and the job stays green
until a new straggler appears — at which point flipping to `-D warnings` outright
becomes the simpler guard and this script retires.

## Acquisition

The dead_code SET is cfg-driven and only materializes when the `cqs` crate is
actually compiled for a Windows target, which needs the MSVC toolchain — a
cross `cargo check` from Linux dies in the C-dep build scripts (ring / onig_sys /
esaxx-rs: no `lib.exe`) long before `Compiling cqs`. So this runs on the
`windows-latest` runner, consuming its `cargo check --message-format=json`.

## Ledger key

`<relative src path>\t<full rustc message>` (e.g.
`src/cli/watch/runtime.rs\tfunction \`format_uptime\` is never used`). No line
number: the key survives line drift, so an unrelated edit above the item does
not churn the ledger. A rename/move changes the message or path, so the old
entry goes STALE and the new one is NEW — both loud. rustc phrasing is stable
within a toolchain; a compiler upgrade that rewords "is never used" invalidates
the ledger loudly and en masse (re-bless with `--update`), never silently.

## Scope

Only `dead_code` diagnostics whose primary span is a relative `src/…` path (the
`cqs` crate's own source). Dependency warnings carry absolute registry paths and
are excluded. `unused_imports` and the other unused_* lints are out of scope: a
cfg-gated `use` is normally gated by the same cfg as its consumer, so it does not
exhibit this sibling-rot shape.

## Usage

    check_windows_deadcode.py [--ledger PATH] [--cargo-rc N] [--update] CARGO_JSON

  CARGO_JSON   file of `cargo check --message-format=json` output (`-` = stdin).
  --ledger     ledger path (default: tests/windows_deadcode_ledger.txt).
  --cargo-rc   exit code of the `cargo check` that produced CARGO_JSON. Non-zero
               means a hard error (the windows-check gate proper); the script
               renders the errors and fails without touching the ledger.
  --update     rewrite the ledger from CARGO_JSON's dead_code items and exit 0.
               Use to seed the ledger from a windows-check run, or to bless an
               intentional backlog change.

Exit 0 clean; 1 on a ledger mismatch or a hard error; 2 on bad invocation.
"""

import argparse
import json
import os
import sys

DEFAULT_LEDGER = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "tests",
    "windows_deadcode_ledger.txt",
)

# Copy-paste markers: on a mismatch the script prints the corrected ledger body
# between these lines so seeding / re-blessing is a mechanical replace.
BEGIN = "===WINDOWS-DEADCODE-LEDGER-BEGIN==="
END = "===WINDOWS-DEADCODE-LEDGER-END==="


def parse_diagnostics(raw):
    """Return (deadcode_items, rendered_errors, rendered_all).

    deadcode_items: sorted list of "<relpath>\t<message>" keys for dead_code
    warnings in the cqs crate's own `src/`. rendered_*: the human `rendered`
    strings for CI-log readability.
    """
    items = set()
    errors = []
    rendered = []
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if obj.get("reason") != "compiler-message":
            continue
        msg = obj.get("message") or {}
        level = msg.get("level")
        if level not in ("error", "warning"):
            continue
        text = msg.get("rendered")
        if text:
            rendered.append(text)
        if level == "error":
            errors.append(msg.get("message", ""))
            continue
        code = (msg.get("code") or {}).get("code")
        if code != "dead_code":
            continue
        primary = next(
            (s for s in msg.get("spans", []) if s.get("is_primary")),
            None,
        )
        if primary is None:
            continue
        fname = (primary.get("file_name") or "").replace("\\", "/")
        # Only the cqs crate's own source: relative `src/…`. Deps and generated
        # files carry absolute / registry paths.
        if not fname.startswith("src/"):
            continue
        message = msg.get("message", "")
        items.add(f"{fname}\t{message}")
    return sorted(items), errors, rendered


def load_ledger(path):
    """Return the set of real ledger entries (comments / blanks ignored).

    Malformed entries (no tab) are a hard error — the ledger is machine-managed
    via --update, so a hand-corruption should fail loudly.
    """
    if not os.path.exists(path):
        return set()
    entries = set()
    with open(path, encoding="utf-8") as fh:
        for lineno, line in enumerate(fh, 1):
            stripped = line.rstrip("\n")
            if not stripped.strip() or stripped.lstrip().startswith("#"):
                continue
            if "\t" not in stripped:
                print(
                    f"windows-deadcode guard: malformed ledger entry at "
                    f"{path}:{lineno} (no TAB separator): {stripped!r}",
                    file=sys.stderr,
                )
                sys.exit(2)
            entries.add(stripped)
    return entries


def ledger_body(items):
    """The checked-in ledger body for `items` (header + sorted entries)."""
    header = [
        "# Windows-only dead_code backlog ledger. Machine-managed; regenerate",
        "# with: python scripts/check_windows_deadcode.py --update cargo-check.json",
        "# (from a windows-latest `cargo check --message-format=json` capture).",
        "# One entry per line: <relative src path>\\t<full rustc message>.",
        "# See scripts/check_windows_deadcode.py for the rationale.",
    ]
    return "\n".join(header + list(items)) + "\n"


def main():
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("cargo_json")
    ap.add_argument("--ledger", default=DEFAULT_LEDGER)
    ap.add_argument("--cargo-rc", type=int, default=0)
    ap.add_argument("--update", action="store_true")
    args = ap.parse_args()

    if args.cargo_json == "-":
        raw = sys.stdin.read()
    else:
        if not os.path.exists(args.cargo_json):
            print(
                f"windows-deadcode guard: cargo JSON not found: {args.cargo_json}",
                file=sys.stderr,
            )
            sys.exit(2)
        with open(args.cargo_json, encoding="utf-8") as fh:
            raw = fh.read()

    items, errors, rendered = parse_diagnostics(raw)

    # Surface every diagnostic in the CI log (JSON format suppresses the human
    # render otherwise).
    for text in rendered:
        sys.stderr.write(text)
    sys.stderr.flush()

    if args.update:
        with open(args.ledger, "w", encoding="utf-8") as fh:
            fh.write(ledger_body(items))
        print(
            f"windows-deadcode guard: wrote {len(items)} item(s) to {args.ledger}"
        )
        return

    # Hard-error gate: a non-zero cargo exit (or any error diagnostic) is the
    # E0432/E0277 class the windows-check job exists to catch. Fail without
    # consulting the ledger — the backlog comparison is meaningless on a build
    # that did not type-check.
    if args.cargo_rc != 0 or errors:
        print(
            f"\nwindows-deadcode guard: `cargo check` reported {len(errors)} "
            f"error(s) (exit {args.cargo_rc}) on the Windows target. A sibling-"
            f"target HARD error (an ungated unix-only API, or an unsized cfg-arm "
            f"binding) would skip the Windows release binary. Fix the error(s) "
            f"above.",
            file=sys.stderr,
        )
        sys.exit(1)

    ledger = load_ledger(args.ledger)
    current = set(items)
    new = sorted(current - ledger)
    stale = sorted(ledger - current)

    if not new and not stale:
        print(
            f"windows-deadcode guard: OK — {len(current)} dead_code item(s), "
            f"all in the ledger."
        )
        return

    if new:
        print(
            f"\nwindows-deadcode guard: {len(new)} NEW dead_code item(s) on the "
            f"Windows target, absent from the ledger — a cfg(unix)-gated helper's "
            f"Windows twin has rotted (or a genuinely-Windows-dead item was "
            f"added):",
            file=sys.stderr,
        )
        for e in new:
            path, _, message = e.partition("\t")
            print(f"  {path}: {message}", file=sys.stderr)

    if stale:
        print(
            f"\nwindows-deadcode guard: {len(stale)} STALE ledger entry(ies) with "
            f"no matching dead_code item — the backlog shrank (item made live or "
            f"deleted). Remove them so the ledger stays exact:",
            file=sys.stderr,
        )
        for e in stale:
            path, _, message = e.partition("\t")
            print(f"  {path}: {message}", file=sys.stderr)

    print(
        f"\nEach NEW item is either a real straggler to fix (gate it / make it "
        f"live) OR an intentional Windows-dead addition to accept. To re-bless "
        f"(seed or accept the backlog change), replace the ledger body with the "
        f"block below (or run `--update`):\n\n{BEGIN}\n"
        f"{ledger_body(sorted(current))}{END}",
        file=sys.stderr,
    )
    sys.exit(1)


if __name__ == "__main__":
    main()
