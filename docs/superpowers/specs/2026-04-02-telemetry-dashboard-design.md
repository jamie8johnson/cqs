# Telemetry Dashboard Design

## Problem

Agents use a subset of cqs commands. Telemetry data exists (`.cqs/telemetry.jsonl`) but there's no way to view it without manual jq/python. We need a `cqs telemetry` command to show usage patterns at a glance.

## Command: `cqs telemetry [--reset] [--all] [--json]`

### Default (no flags): Dashboard

Reads `.cqs/telemetry.jsonl`, outputs:

1. **Header** — event count, date range
2. **Command frequency** — sorted descending, bar chart, percentage
3. **Categories** — commands grouped into:
   - **Search**: search, gather, scout, onboard, where, related, similar
   - **Structural**: callers, callees, impact, impact-diff, test-map, deps, trace, explain, context, dead
   - **Orchestrator**: task, review, plan, ci
   - **Read/Write**: read, notes, blame, diff, drift, stale, suggest
   - **Infra**: index, init, watch, stats, health, gc, doctor, batch, chat, audit-mode, convert, train-data, completions, project, ref
4. **Sessions** — count (split on reset events or 4h gaps), avg events/session
5. **Top queries** — most frequent query strings, top 10

### `--reset`

Archives current telemetry file to `telemetry_YYYYMMDD_HHMMSS.jsonl`, writes a reset event with reason from positional arg (or default "manual reset").

### `--all`

Reads all `telemetry*.jsonl` files (current + archived), merges for full history view.

### `--json`

Standard TextJsonArgs JSON output.

## Data Format (existing, unchanged)

```jsonl
{"cmd":"search","query":"score_candidate","results":5,"ts":1775156081}
{"event":"reset","ts":1775156303,"reason":"v1.14.0 post-agent-test session"}
```

## Implementation

- New file: `src/cli/commands/telemetry_cmd.rs`
- Reads JSONL, deserializes into `TelemetryEntry` enum (command vs reset event)
- Category assignment is a static map
- Session detection: split on reset events or 4-hour timestamp gaps
- Bar chart: unicode block characters, scaled to terminal width (or 40 cols default)
- Wire into `definitions.rs` (TelemetryArgs with TextJsonArgs) and `dispatch.rs`
