---
name: cqs-bootstrap
description: One-command setup for cqs in a new project — MCP config, skills, tears infrastructure, CLAUDE.md, init, index.
disable-model-invocation: false
argument-hint: "[project_path]"
---

# Bootstrap Project

Fully self-contained setup for cqs in a new project. After running this, the project has semantic search, notes, continuity tracking, and all skills working.

## Prerequisites

- `cqs` binary installed (check with `which cqs`)
- Project has a git repo initialized
- Running inside Claude Code in the target project directory (or pass project path as argument)

## Process

### Phase 1: Tears Infrastructure

1. Check if files already exist — **don't overwrite**
2. Create `docs/` directory if needed
3. Write each file:

#### docs/notes.toml

```toml
# Notes - unified memory for AI collaborators
# Surprises (prediction errors) worth remembering
# sentiment: DISCRETE values only: -1, -0.5, 0, 0.5, 1
#   -1 = serious pain, -0.5 = notable pain, 0 = neutral, 0.5 = notable gain, 1 = major win

[[note]]
sentiment = 0
text = "Project bootstrapped with cqs tears infrastructure."
mentions = ["docs/notes.toml", "PROJECT_CONTINUITY.md"]
```

#### PROJECT_CONTINUITY.md

```markdown
# Project Continuity

## Right Now

(active task - update when starting something)

## Parked

(threads to revisit later)

## Open Questions

(decisions being weighed, with options)

## Blockers

None.

## Pending Changes

(uncommitted work)
```

#### ROADMAP.md

```markdown
# Roadmap

## Current Phase

### Done
- [ ] ...

### Next
- [ ] ...
```

### Phase 2: Skills

4. Create `.claude/skills/` directory
5. Copy all portable skills from `/mnt/c/Projects/cq/.claude/skills/`:
   - `update-tears` — session state capture
   - `groom-notes` — note cleanup
   - `reindex` — rebuild index with stats
   - `cqs-bootstrap` — this skill (for nested projects)
   - `cqs-search` — semantic code search
   - `cqs-stats` — index statistics
   - `cqs-callers` — find callers of a function
   - `cqs-callees` — find callees of a function
   - `cqs-read` — read file with contextual notes
   - `cqs-explain` — function card (signature, callers, callees, similar)
   - `cqs-similar` — find similar code
   - `cqs-diff` — semantic diff between snapshots
   - `cqs-add-note` — add a note to project memory
   - `cqs-update-note` — update an existing note
   - `cqs-remove-note` — remove a note
   - `cqs-audit-mode` — toggle audit mode for unbiased review
   - `cqs-ref` — manage reference indexes (add/remove/update/list)
   - `cqs-watch` — start file watcher for live index updates
   - `cqs-trace` — follow call chain between two functions
   - `cqs-impact` — what breaks if you change X
   - `cqs-test-map` — map functions to their tests
   - `cqs-batch` — execute multiple queries in one call
   - `cqs-context` — module-level file overview
   - `cqs-gather` — smart context assembly (seed search + call graph BFS)
   - `cqs-dead` — find dead code (functions with no callers)
   - `cqs-gc` — report index staleness
   - `troubleshoot` — diagnose common cqs issues
   - `migrate` — handle schema version upgrades

### Phase 3: MCP Server Config

6. Create `.mcp.json` in project root (if it doesn't exist) or add cqs to existing:

```json
{
  "mcpServers": {
    "cqs": {
      "command": "cqs",
      "args": ["serve", "--project", "."]
    }
  }
}
```

**Notes on the MCP config:**
- Uses bare `cqs` command (assumes it's in PATH via `~/.cargo/bin/`)
- Uses `.` for project path — Claude Code resolves this relative to project root
- If user has a custom install path, ask them and adjust
- If `LD_LIBRARY_PATH` is needed (GPU/CUDA), add an `env` field
- `.mcp.json` is checked into git (shared config for all collaborators)

### Phase 4: cqs Init & Index

7. Run `cqs init` (creates `.cq/` directory with database)
8. Run `cqs index` (indexes all source files + notes)
9. Verify with `cqs stats` — should show chunk count > 0

### Phase 5: .gitignore

10. Add `.cq/` to `.gitignore` if not already present (the index database is local, not shared)

### Phase 6: CLAUDE.md Integration

11. If CLAUDE.md exists, **append** the cqs sections below. If it doesn't exist, create it with these sections plus a basic header.

**Check for existing sections first** — don't duplicate if the user already has cqs config in their CLAUDE.md.

#### Sections to add to CLAUDE.md:

```markdown
## Read First

* `PROJECT_CONTINUITY.md` -- what's happening right now
* `docs/notes.toml` -- observations indexed by cqs (warnings, patterns)
* `ROADMAP.md` -- what's done, what's next

## Skills

Project skills in `.claude/skills/`. Use `/skill-name` to invoke.
Skills are auto-discovered — they appear in `/` autocomplete automatically.

## Code Search

**Use `cqs_search` instead of grep/glob.** It finds code by what it does, not text matching.

Use it for:
- Exploring unfamiliar code
- Finding implementations by behavior
- When you don't know exact names

**Definition search:** Use `name_only=true` for "where is X defined?" queries. Skips embedding, searches function/struct names directly. Faster than glob.

Fall back to Grep/Glob only for exact string matches or when semantic search returns nothing.

**`cqs_read`** — use instead of raw `Read` for indexed source files. Returns file contents with relevant notes injected as comments. Use raw `Read` for non-indexed files (config, markdown, lock files).

Tools: `cqs_search`, `cqs_stats`, `cqs_callers`, `cqs_callees`, `cqs_read`, `cqs_similar`, `cqs_explain`, `cqs_diff`, `cqs_trace`, `cqs_impact`, `cqs_test_map`, `cqs_batch`, `cqs_context`, `cqs_gather`, `cqs_dead`, `cqs_gc`, `cqs_add_note`, `cqs_update_note`, `cqs_remove_note`, `cqs_audit_mode` (20 tools — run `cqs watch` to keep index fresh)

## Audit Mode

Before audits or fresh-eyes reviews:
`cqs_audit_mode(true)` to exclude notes and force direct code examination.
After: `cqs_audit_mode(false)` or let it auto-expire (30 min default).

## Continuity (Tears)

"Update tears" = capture state before context compacts.

* `PROJECT_CONTINUITY.md` -- right now, parked, blockers, open questions, pending
* `docs/notes.toml` -- observations with sentiment (indexed by cqs)

**Use `cqs_add_note` to add notes** - it indexes immediately. Direct file edits require `cqs index` to become searchable.

**Sentiment is DISCRETE** — only 5 valid values: -1, -0.5, 0, 0.5, 1
```

### Phase 7: Verify

12. Run `cqs stats` to confirm indexing worked
13. Test a search: `cqs search "main entry point"` (should return results)
14. Report summary: files created, chunks indexed, skills installed

## Rules

- **Never overwrite** existing files — skip with a message
- **Append, don't replace** CLAUDE.md content
- **Ask before** modifying `.gitignore` if it has complex rules
- If `cqs` binary isn't found, stop and tell the user to install it first
- If `.mcp.json` already has a `cqs` entry, don't duplicate — just verify it's correct
