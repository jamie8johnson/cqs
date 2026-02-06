---
name: bootstrap
description: Bootstrap a new project with cqs tears infrastructure (notes.toml, PROJECT_CONTINUITY.md, ROADMAP.md).
disable-model-invocation: true
---

# Bootstrap Project

Set up tears infrastructure for a new project.

## Files to create

### docs/notes.toml

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

### PROJECT_CONTINUITY.md

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

### ROADMAP.md

```markdown
# Roadmap

## Current Phase

### Done
- [ ] ...

### Next
- [ ] ...
```

## Process

1. Check if files already exist — don't overwrite
2. Create `docs/` directory if needed
3. Write each file
4. Create `.claude/skills/` directory
5. Copy portable skills from `/mnt/c/Projects/cq/.claude/skills/` if available:
   - `update-tears` — session state capture
   - `groom-notes` — note cleanup
   - `reindex` — rebuild index with stats
   - `bootstrap` — this skill (for nested projects)
6. Run `cqs init` if not already initialized
7. Run `cqs index` to index the new notes
8. Suggest adding to `.gitignore`: `.cq/` (index database)
9. Add skills section to CLAUDE.md (see cqs project for reference format)

## Notes

- These files are checked into git (they're shared team context)
- `.cq/` directory is NOT checked in (local index)
- `.claude/skills/` is checked into git (shared skill definitions)
- CLAUDE.md should reference these files in its "Read First" section
- Skills are auto-discovered by Claude Code from `.claude/skills/*/SKILL.md`
- No registration needed — they appear in `/` autocomplete automatically
