# CLAUDE.md

cqs - semantic code search with local embeddings

## Working Style

- Flat, dry, direct. No padding.
- Push back when warranted.
- Ask rather than guess wrong.
- Efficiency over ceremony.

## On Resume

If context just compacted: read tears, then ask "where were we?" rather than guessing.

## Read First

* `PROJECT_CONTINUITY.md` -- what's happening right now
* `docs/HUNCHES.md` -- soft observations, latent risks
* `docs/SCARS.md` -- things we tried that hurt. don't repeat these.
* `ROADMAP.md` -- what's done, what's next

## Code Search

**Use `cqs_search` instead of grep/glob.** It finds code by what it does, not text matching.

Use it for:
- Exploring unfamiliar code
- Finding implementations by behavior
- When you don't know exact names

Fall back to Grep/Glob only for exact string matches or when semantic search returns nothing.

Tools: `cqs_search`, `cqs_stats` (run `cqs watch` to keep index fresh)

## Project Conventions

- Rust edition 2021
- `thiserror` for library errors, `anyhow` in CLI
- No `unwrap()` except in tests
- GPU detection at runtime, graceful CPU fallback

## WSL Workarounds

Git/GitHub operations need PowerShell (Windows has credentials):
```bash
powershell.exe -Command "cd C:\projects\cq; git push"
powershell.exe -Command 'gh pr create --title "..." --body "..."'
powershell.exe -Command 'gh pr merge N --squash --delete-branch'
```

**Use `gh pr checks --watch`** to wait for CI. Don't use `sleep` + poll.

**PowerShell mangles complex strings.** Backticks, quotes, newlines in `gh issue create --body` or `gh pr create --body` will break. Write to a file on `/mnt/c/` and use `--body-file` instead.

**main is protected** - all changes via PR.

## Continuity (Tears)

"Update tears" = capture state before context compacts.

**Keep "Right Now" current** - update it when switching tasks, not just at session end.

* `PROJECT_CONTINUITY.md` -- right now, parked, blockers, open questions, pending
* `docs/HUNCHES.md` -- soft risks, observations
* `docs/SCARS.md` -- failed approaches (add when something hurts)

Don't log activity - git history has that.

*Etymology: PIE \*teks- (weave/construct), collapses with \*der- (rip) and \*dakru- (crying). Portuguese "tear" = loom. Context is woven, then cut—Clotho spins, Lachesis measures, Atropos snips. Construction, destruction, loss.*

---

## Bootstrap (New Project)

Create these files if missing:

**docs/HUNCHES.md:**
```markdown
# Hunches

Soft observations, latent risks. Append as they arise.

---
```

**docs/SCARS.md:**
```markdown
# Scars

Limbic memory. Things that hurt, so we don't touch the stove twice.

---

## <topic>

**Tried:** what we attempted
**Pain:** why it hurt
**Learned:** what to do instead

---
```

**PROJECT_CONTINUITY.md:**
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

**ROADMAP.md:**
```markdown
# Roadmap

## Current Phase

### Done
- [ ] ...

### Next
- [ ] ...
```
