---
name: cqs-audit-mode
description: Toggle audit mode — excludes notes from search/read for unbiased code review.
disable-model-invocation: false
argument-hint: "[on|off] [--expires 1h]"
---

# Audit Mode

Parse arguments:

- `on` → enable audit mode
- `off` → disable audit mode
- No argument → query current state
- `--expires <duration>` → expiry duration (e.g., "30m", "1h", "2h"). Default: 30m.

Run via Bash: `cqs audit-mode [on|off] [--expires 30m] --json -q`

Audit mode prevents notes from influencing search and read results. Use before code audits, fresh-eyes reviews, or any time you need unbiased analysis.

Auto-expires after the specified duration.
