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

## Skills

Project skills in `.claude/skills/`. Use `/skill-name` to invoke:

- `/update-tears` -- capture state before compaction or task switch
- `/groom-notes` -- review and clean up stale notes
- `/release` -- version bump, changelog, publish, GitHub release
- `/audit` -- 20-category code audit with parallel agents
- `/pr` -- WSL-safe PR creation (always `--body-file`)
- `/bootstrap` -- set up tears infrastructure for new projects
- `/reindex` -- rebuild index with before/after stats

## Code Search

**Use `cqs_search` instead of grep/glob.** It finds code by what it does, not text matching.

Use it for:
- Exploring unfamiliar code
- Finding implementations by behavior
- When you don't know exact names

**Definition search:** Use `name_only=true` for "where is X defined?" queries. Skips embedding, searches function/struct names directly. Faster than glob.

Fall back to Grep/Glob only for exact string matches or when semantic search returns nothing.

Tools: `cqs_search`, `cqs_stats` (run `cqs watch` to keep index fresh)

## Audit Mode

Before audits, fresh-eyes reviews, clear-eyes reviews, or unbiased code assessment:
`cqs_audit_mode(true)` to exclude notes and force direct code examination.

After: `cqs_audit_mode(false)` or let it auto-expire (30 min default).

**Triggers:** audit, fresh eyes, clear eyes, unbiased review, independent review, security audit

Audit mode prevents false confidence from stale notes - forces you to examine code directly instead of trusting prior observations.

## Agent Teams

Use teams when dispatching 2+ agents that need coordination. Teams provide task lists, message passing, and structured shutdown.

**When to use:**
- Audit batches (5 parallel category reviewers)
- Multi-file implementation with independent units
- Research + implementation in parallel
- Any work that benefits from task tracking across agents

**Conventions:**
- Name teams by purpose: `audit-batch-1`, `feat-streaming`, `refactor-errors`
- Use `haiku` for simple/mechanical tasks, `sonnet` for judgment-heavy work, `opus` for complex reasoning
- Always clean up teams when done (`Teammate cleanup`)
- Teammates can't see your text output — use `SendMessage` to communicate

**Task workflow:**
1. `spawnTeam` — create team
2. `TaskCreate` — define work items with clear acceptance criteria
3. Spawn teammates via `Task` with `team_name` and `name`
4. Teammates claim tasks, execute, report back
5. `shutdown_request` each teammate when done
6. `Teammate cleanup` to tear down

**Teammate prompts must be self-contained.** Include file paths, context, and acceptance criteria. Teammates start with zero context — they can't see your conversation.

## Code Audit

Full design: `docs/plans/2026-02-04-20-category-audit-design.md`

**Quick reference:**
- 14 categories in 3 batches (5, 5, 4) — consolidated from 20/4 after v0.5.3 audit found 38% duplication
- Collect all findings first, then fix by impact × effort
- Stop at diminishing returns during discovery
- Once triaged, complete the tier. Don't suggest stopping mid-priority.

**Batches:**
1. Code Quality: Code Quality, Documentation, API Design, Error Handling, Observability
2. Behavior: Test Coverage, Robustness, Algorithm Correctness, Extensibility, Platform Behavior
3. Infrastructure: Security, Data Safety, Performance, Resource Management

**Execution:**
1. Enable audit mode before each batch (`cqs_audit_mode(true, expires_in="2h")`)
2. `TeamCreate` per batch, agents per category (sonnet for judgment, haiku for mechanical)
3. Each agent writes findings to `docs/audit-findings.md` (append, don't overwrite)
4. Shutdown team, cleanup before next batch
5. After all batches: triage into `docs/audit-triage.md` (append version section with P1-P4 tables), then fix

**Why:** Findings get lost when context compacts. Issues make work visible to future sessions.

## Completion Checklist

Before marking any feature "done":

1. **Trace the call path.** If you wrote `fn foo()`, grep for callers. Zero callers = dead code = not done.
2. **Test end-to-end.** "It compiles" is not done. Actually run it. Does the user-facing command use your code?
3. **Check for warnings.** `cargo build 2>&1 | grep warning` - dead code warnings mean incomplete wiring.
4. **Verify previous work.** If building on existing code, verify that code actually works first. Don't assume.

The HNSW disaster: built an index, wrote save/load, marked "done" - but search never called it. Three months of O(n) scans because nobody traced `search()` → `search_by_candidate_ids()` → zero callers.

**"Done" means a user can use it, not that code exists.**

5. **Update the roadmap.** Check off completed items in `ROADMAP.md`. Stale roadmaps cause duplicate work.

## Project Conventions

- Rust edition 2021
- `thiserror` for library errors, `anyhow` in CLI
- No `unwrap()` except in tests
- GPU detection at runtime, graceful CPU fallback

## Documentation

When updating docs, keep these in sync:
- `README.md` - user-facing, install/usage
- `CONTRIBUTING.md` - dev setup, architecture overview
- `SECURITY.md` - threat model, filesystem access
- `CHANGELOG.md` - version history

**CONTRIBUTING.md has an Architecture Overview section** - update it when adding/moving/renaming source files.

## WSL Workarounds

Git/GitHub operations need PowerShell (Windows has credentials):
```bash
powershell.exe -Command "cd C:\projects\cq; git push"
powershell.exe -Command 'gh pr create --title "..." --body "..."'
powershell.exe -Command 'gh pr merge N --squash --delete-branch'
```

**Use `gh pr checks --watch`** to wait for CI. Don't use `sleep` + poll.

**ALWAYS use `--body-file` for PR/issue bodies.** Never inline heredocs or multiline strings in `gh pr create --body` or `gh issue create --body`. Two reasons: (1) PowerShell mangles complex strings, (2) Claude Code captures the entire multiline command as a permission entry in `settings.local.json`, corrupting the file and breaking startup. Write body to `/mnt/c/Projects/cq/pr_body.md`, use `--body-file`, delete after.

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

Also set up `.claude/skills/` with portable skills. Use `/bootstrap` if available, or copy from an existing cqs project. Skills are auto-discovered from `.claude/skills/*/SKILL.md`.
