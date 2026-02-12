---
name: cqs-bootstrap
description: One-command setup for cqs in a new project — skills, tears infrastructure, CLAUDE.md, init, index.
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

<!-- When this grows past ~100 lines, archive completed phases to docs/roadmap-archive.md -->

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
   - `cqs-impact-diff` — diff-aware impact: changed functions, callers, tests to re-run
   - `cqs-test-map` — map functions to their tests
   - `cqs-batch` — execute multiple queries in one call
   - `cqs-context` — module-level file overview
   - `cqs-gather` — smart context assembly (seed search + call graph BFS)
   - `cqs-dead` — find dead code (functions with no callers)
   - `cqs-gc` — report index staleness
   - `cqs-stale` — check index freshness (files changed since last index)
   - `cqs-related` — find functions related by shared callers, callees, or types
   - `cqs-where` — suggest where to add new code based on semantic similarity
   - `cqs-scout` — pre-investigation dashboard (search + callers + tests + staleness + notes)
   - `cqs-plan` — task planning with scout data + task-type templates
   - `troubleshoot` — diagnose common cqs issues
   - `migrate` — handle schema version upgrades

### Phase 3: cqs Init & Index

6. Run `cqs init` (creates `.cqs/` directory with database)
7. Run `cqs index` (indexes all source files + notes)
8. Verify with `cqs stats` — should show chunk count > 0

### Phase 4: .gitignore

9. Add `.cqs/` to `.gitignore` if not already present (the index database is local, not shared)

### Phase 5: CLAUDE.md Integration

10. If CLAUDE.md exists, **append** the cqs sections below. If it doesn't exist, create it with these sections plus a basic header.

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

**Use `cqs search` instead of grep/glob.** It finds code by what it does, not text matching.

```bash
cqs "search query" --json          # semantic search
cqs "function_name" --name-only    # definition lookup (fast, no embedding)
cqs "query" --semantic-only        # pure vector similarity, no RRF
```

Use it for:
- Exploring unfamiliar code
- Finding implementations by behavior
- When you don't know exact names

Fall back to Grep/Glob only for exact string matches or when semantic search returns nothing.

**Key commands** (all support `--json`):
- `cqs read <path>` — file with notes injected. Use instead of raw `Read` for indexed source files.
- `cqs read --focus <function>` — function + type dependencies only. Saves tokens.
- `cqs explain <function>` — function card: signature, callers, callees, similar.
- `cqs similar <function>` — find similar code. Refactoring discovery, duplicates.
- `cqs callers <function>` / `cqs callees <function>` — call graph navigation.
- `cqs impact <function>` — what breaks if you change it. Callers + affected tests.
- `cqs gather "query"` — smart context assembly: seed search + call graph BFS.
- `cqs scout "task"` — pre-investigation dashboard: search + callers/tests + staleness + notes.
- `cqs where "description"` — placement suggestion for new code.
- `cqs related <function>` — co-occurrence: shared callers, callees, types.
- `cqs context <file>` — module-level overview: chunks, callers, callees, notes.
- `cqs trace <source> <target>` — shortest call path between two functions.
- `cqs test-map <function>` — map function to tests that exercise it.
- `cqs dead` — find functions/methods with no callers.
- `cqs stale` — check index freshness.
- `cqs stats` — index statistics.
- `cqs notes add/update/remove` — manage project notes.

Run `cqs watch` in a separate terminal to keep the index fresh, or `cqs index` for one-time refresh.

Use `--no-stale-check` to skip per-file staleness checks (useful on NFS/network mounts).
Set `stale_check = false` in `.cqs.toml` to make it permanent.

## Audit Mode

Before audits or fresh-eyes reviews:
`cqs audit-mode on` to exclude notes and force direct code examination.
After: `cqs audit-mode off` or let it auto-expire (30 min default).

## Continuity (Tears)

"Update tears" = capture state before context compacts.

* `PROJECT_CONTINUITY.md` -- right now, parked, blockers, open questions, pending
* `docs/notes.toml` -- observations with sentiment (indexed by cqs)

**Use `cqs notes add` to add notes** — it indexes immediately. Direct file edits require `cqs index` to become searchable.

**Sentiment is DISCRETE** — only 5 valid values: -1, -0.5, 0, 0.5, 1

## Agent Teams

When spawning agents (via Task tool), always include cqs tool instructions in the agent prompt. Agents start with zero context — they can't use cqs unless told how. Include the key commands block (search, callers, callees, explain, similar, gather, impact, test-map, trace, context, dead, scout) in every agent prompt.
```

### Phase 6: Verify

11. Run `cqs stats` to confirm indexing worked
12. Test a search: `cqs "main entry point" --json` (should return results)
13. Report summary: files created, chunks indexed, skills installed

## Rules

- **Never overwrite** existing files — skip with a message
- **Append, don't replace** CLAUDE.md content
- **Ask before** modifying `.gitignore` if it has complex rules
- If `cqs` binary isn't found, stop and tell the user to install it first
