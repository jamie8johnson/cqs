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
5. Copy all portable skills from `/mnt/c/Projects/cqs/.claude/skills/`:
   - `cqs` — unified CLI dispatcher (search, graph, quality, notes, infrastructure — all subcommands)
   - `cqs-bootstrap` — this skill (for nested projects)
   - `cqs-batch` — batch mode: persistent Store + Embedder, stdin commands, JSONL output, pipeline syntax
   - `cqs-plan` — task planning with scout data + task-type templates
   - `cqs-verify` — exercise all command categories, catch regressions
   - `before-edit` — run before modifying a function (impact + test-map + explain → checklist)
   - `investigate` — run before starting a task (scout + gather → implementation brief)
   - `check-my-work` — run after changes (review diff → risk checklist)
   - `update-tears` — session state capture
   - `groom-notes` — note cleanup
   - `docs-review` — check docs for staleness
   - `reindex` — rebuild index with stats
   - `troubleshoot` — diagnose common cqs issues
   - `migrate` — handle schema version upgrades
   - `red-team` — adversarial testing against cqs
   - `audit` — 14-category code audit with parallel agents
   - `pr` — WSL-safe PR creation (always --body-file)
   - `release` — version bump, changelog, publish, GitHub release

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

## Code Intelligence — When to Use What

**Use these cqs commands at the right moments.** They replace multiple manual searches with a single call. Workflow skills are even easier:
- `/before-edit <function>` — impact + tests + callers → modification checklist
- `/investigate <task>` — scout + gather → implementation brief
- `/check-my-work` — review current diff → risk assessment

### Before modifying a function:
```bash
cqs impact <function_name> --json
```

### Before writing tests:
```bash
cqs test-map <function_name> --json
```

### Before starting any implementation task:
```bash
cqs scout "task description" --json
```

### Exploring unfamiliar code:
```bash
cqs onboard "concept" --json
cqs gather "query" --json
```

### Searching (use instead of grep/glob):
```bash
cqs "search query" --json
cqs "function_name" --name-only --json
cqs read <path>
cqs read --focus <function>
```

### Full command reference
- `cqs explain <fn>` — function card: signature, callers, callees, similar
- `cqs callers <fn>` / `cqs callees <fn>` — call graph navigation
- `cqs deps <type>` — type dependencies
- `cqs similar <fn>` — find similar code
- `cqs related <fn>` — co-occurrence: shared callers, callees, types
- `cqs where "description"` — placement suggestion
- `cqs trace <source> <target>` — shortest call path
- `cqs context <file>` — module overview
- `cqs impact-diff [--base REF]` — diff-aware impact
- `cqs review` — diff review with risk scoring
- `cqs ci [--base REF] [--gate high|medium|off]` — CI gate
- `cqs blame <fn>` — semantic git blame
- `cqs dead` — find dead code
- `cqs health` — codebase quality snapshot
- `cqs stale` — check index freshness
- `cqs notes add/update/remove` — manage project notes
- `cqs stats` — index statistics
- `cqs batch` — batch mode with pipeline syntax
- `cqs brief <file>` — one-line-per-function summary
- `cqs affected [--base REF]` — diff -> changed functions -> callers -> tests
- `cqs neighbors <fn>` — embedding-space nearest neighbors
- `cqs plan "description"` — task planning with templates
- `cqs test-map <fn>` — find tests that exercise a function
- `cqs doctor [--fix]` — check model, index, hardware
- `cqs diff <ref>` / `cqs drift <ref>` — semantic diff/drift
- `cqs suggest` — auto-suggest notes from patterns
- `cqs chat` — interactive REPL
- `cqs audit-mode on/off` — toggle audit mode
- `cqs gc` — clean stale index entries

Run `cqs watch` in a separate terminal to keep the index fresh, or `cqs index` for one-time refresh.

## Audit Mode

Before audits or fresh-eyes reviews:
`cqs audit-mode on` to exclude notes and force direct code examination.
After: `cqs audit-mode off` or let it auto-expire (30 min default).

## Continuity (Tears)

"Update tears" = capture state before context compacts.

* `PROJECT_CONTINUITY.md` -- right now, parked, blockers, open questions, pending
* `docs/notes.toml` -- observations with sentiment (indexed by cqs)

**Use `cqs notes add` to add notes** — it is available immediately. Direct file edits require `cqs index` to sync to SQLite.

**Sentiment is DISCRETE** — only 5 valid values: -1, -0.5, 0, 0.5, 1

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
