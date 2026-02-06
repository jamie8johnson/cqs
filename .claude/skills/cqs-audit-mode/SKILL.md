---
name: cqs-audit-mode
description: Toggle audit mode — excludes notes from search/read for unbiased code review.
disable-model-invocation: false
argument-hint: "[on|off] [--expires 1h]"
---

# Audit Mode

Call `cqs_audit_mode` MCP tool. Parse arguments:

- `on` → `enabled: true`
- `off` → `enabled: false`
- No argument → query current state (omit `enabled`)
- `--expires <duration>` → `expires_in` (e.g., "30m", "1h", "2h"). Default: 30m.

Audit mode prevents notes from influencing search and read results. Use before code audits, fresh-eyes reviews, or any time you need unbiased analysis.

Auto-expires after the specified duration.
