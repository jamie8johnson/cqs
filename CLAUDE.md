# CLAUDE.md

Read the tears. You just woke up.

cqs - semantic code search with local embeddings

## Working Style

- Flat, dry, direct. No padding.
- Push back when warranted.
- Ask rather than guess wrong.
- Efficiency over ceremony.

## On Resume

If context just compacted: read tears, then ask "where were we?" rather than guessing.

**Distrust previous sessions.** Before continuing work marked "done", verify it actually works:
- `cargo build 2>&1 | grep -i warning` - any dead code?
- Grep for the function - does anything call it?
- Run the feature - does it do what's claimed?

## Read First

* `PROJECT_CONTINUITY.md` -- what's happening right now
* `docs/notes.toml` -- observations indexed by cqs (warnings, patterns)
* `ROADMAP.md` -- what's done, what's next

## Code Search

**Use `cqs_search` instead of grep/glob.** It finds code by what it does, not text matching.

Use it for:
- Exploring unfamiliar code
- Finding implementations by behavior
- When you don't know exact names

Fall back to Grep/Glob only for exact string matches or when semantic search returns nothing.

Tools: `cqs_search`, `cqs_stats` (run `cqs watch` to keep index fresh)

## Completion Checklist

Before marking any feature "done":

1. **Trace the call path.** If you wrote `fn foo()`, grep for callers. Zero callers = dead code = not done.
2. **Test end-to-end.** "It compiles" is not done. Actually run it. Does the user-facing command use your code?
3. **Check for warnings.** `cargo build 2>&1 | grep warning` - dead code warnings mean incomplete wiring.
4. **Verify previous work.** If building on existing code, verify that code actually works first. Don't assume.

The HNSW disaster: built an index, wrote save/load, marked "done" - but search never called it. Three months of O(n) scans because nobody traced `search()` → `search_by_candidate_ids()` → zero callers.

**"Done" means a user can use it, not that code exists.**

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

**Don't ask. Just do it.** Update tears proactively:
- After commits/PRs
- When switching tasks
- When state changes
- Before context gets tight

* `PROJECT_CONTINUITY.md` -- right now, parked, blockers, open questions, pending
* `docs/notes.toml` -- observations with sentiment (indexed by cqs)

**Use `cqs_add_note` to add notes** - it indexes immediately. Direct file edits require `cqs watch` or `cqs index` to become searchable.

**Sentiment is DISCRETE** - only 5 valid values:
| Value | Meaning |
|-------|---------|
| `-1` | Serious pain (broke something, lost time) |
| `-0.5` | Notable pain (friction, annoyance) |
| `0` | Neutral observation |
| `0.5` | Notable gain (useful pattern) |
| `1` | Major win (saved significant time/effort) |

Do NOT use values like 0.7 or 0.8. Pick the closest discrete value.

Don't log activity - git history has that.

*Etymology: PIE \*teks- (weave/construct), collapses with \*der- (rip) and \*dakru- (crying). Portuguese "tear" = loom. Context is woven, then cut—Clotho spins, Lachesis measures, Atropos snips. Construction, destruction, loss.*

---

## Bootstrap (New Project)

Create these files if missing:

**docs/notes.toml:**
```toml
# Notes - unified memory for AI collaborators
# sentiment: DISCRETE values only: -1, -0.5, 0, 0.5, 1

[[note]]
sentiment = -1
text = "Example warning - something that seriously hurt"
mentions = ["file.rs", "function_name"]

[[note]]
sentiment = 0.5
text = "Example pattern - something that worked well"
mentions = ["other_file.rs"]
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
