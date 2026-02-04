# Audit Mode Design

## Problem

Notes in `docs/notes.toml` create **false confidence** during code audits. When notes say "X is fine" or "Y works correctly," the AI trusts those observations instead of verifying the code directly. If the notes are stale or wrong, problems get missed.

Audits need fresh eyes - the AI should examine code without prior observations influencing what gets scrutinized.

## Solution

A server-side toggle that excludes notes from search results and file reads during audit sessions.

## Tool: `cqs_audit_mode`

**Parameters:**
- `enabled` (bool, optional) - set mode on/off
- `expires_in` (string, optional) - duration like "30m", "1h". Default: "30m"

**Behavior:**
- No args → returns current state
- `enabled: true` → activates audit mode, starts expiry timer
- `enabled: false` → deactivates immediately
- Re-enabling resets the expiry timer

**Response:**
```json
{
  "audit_mode": true,
  "expires_at": "2026-02-04T14:30:00Z",
  "remaining": "25m"
}
```

## Server State

In-memory only (ephemeral):

```rust
struct AuditMode {
    enabled: bool,
    expires_at: Option<DateTime<Utc>>,
}
```

No persistence. State lost on server restart. Acceptable because:
- Audits are short (~20 minutes)
- Server restarts mid-audit are rare
- Simpler implementation

## Expiry

- Default: 30 minutes
- Checked lazily on each search/read request
- If `now > expires_at`, auto-disables
- No background timer needed

## Integration Points

### `cqs_search`

Before calling `search_notes()`:
1. Check `audit_mode.enabled && !expired`
2. If on: skip `search_notes()`, return code-only results
3. Append status line: `(audit mode: Xm remaining)`

### `cqs_read`

Before injecting note comments:
1. Check `audit_mode.enabled && !expired`
2. If on: skip note injection, return raw file
3. Append status line to response

### Other tools

`cqs_callers`, `cqs_callees`, `cqs_stats` - no changes needed. They don't involve notes.

## Status Indicator

All responses show audit mode status when active:

```
(audit mode: 25m remaining)

[search results or file content]
```

Prevents confusion about why notes aren't appearing.

## CLAUDE.md Addition

```markdown
## Audit Mode

Before audits, fresh-eyes reviews, or unbiased code assessment:
`cqs_audit_mode(true)` to exclude notes and force direct code examination.

After: `cqs_audit_mode(false)` or let it auto-expire (30 min default).

Triggers: audit, fresh eyes, clear eyes, unbiased review, independent review, security audit
```

## Known Limitations

**Not airtight enforcement.** The AI can still:
- Read `docs/notes.toml` directly via Read tool
- See notes from earlier in the conversation
- Have prior context about the codebase

This is friction, not a security boundary. The goal is to prevent automatic inclusion of notes in search/read, not to make notes completely inaccessible.

## Future Considerations

- Additional trigger phrases may be needed as usage patterns emerge
- Could extend to other filtering (e.g., `--code-only` CLI flag for non-MCP usage)
