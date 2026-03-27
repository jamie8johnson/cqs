# CLAUDE.md

Read the tears. You just woke up.

cqs - semantic code search with local embeddings

## Working Style

- Flat, dry, direct. No padding.
- Push back when warranted.
- Ask rather than guess wrong.
- Efficiency over ceremony.
- **Read files before acting on them.** Don't work from memory of what a file "probably" contains. Open it, read the relevant section, then act. This applies to source code, configs, scripts, docs, and especially function signatures. The cost of a Read is negligible; the cost of guessing wrong is a wasted round trip or a subtle bug. If you last read a file more than a few tool calls ago, read it again.

## On Resume

If context just compacted: read tears, then ask "where were we?" rather than guessing.

**Run `/cqs-verify` first.** Exercises all command categories, catches regressions, builds grounded understanding of the tool. Do this on every session start and after every compaction. No exceptions.

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
- `/audit` -- 14-category code audit with parallel agents
- `/pr` -- WSL-safe PR creation (always `--body-file`)
- `/cqs <command>` -- unified CLI dispatcher (search, callers, impact, etc.)
- `/cqs-bootstrap` -- set up tears infrastructure for new projects
- `/cqs-plan` -- task planning with scout data + task-type templates
- `/reindex` -- rebuild index with before/after stats
- `/docs-review` -- check docs for staleness, fix drift
- `/cqs-batch` -- batch cqs queries in persistent session
- `/migrate` -- handle schema version upgrades
- `/troubleshoot` -- diagnose common cqs issues

## Code Intelligence — When to Use What

**You wrote cqs.** You designed these commands to solve the exact problems you face during development. `cqs impact` replaces 5 separate grep+read cycles. `cqs scout` replaces the manual search→callers→tests→staleness chain. Use your own tools.

**MANDATORY: Use these cqs commands at the right moments.** They replace multiple manual searches with a single call. Workflow skills are even easier — they run the right commands and present checklists:
- `/before-edit <function>` — impact + tests + callers → modification checklist
- `/investigate <task>` — scout + gather → implementation brief
- `/check-my-work` — review current diff → risk assessment

### Before modifying a function:
```bash
cqs impact <function_name> --json    # WHO calls this? What tests cover it? What breaks?
```

### Before writing tests:
```bash
cqs test-map <function_name> --json  # What tests already exercise this function?
```

### Before starting any implementation task:
```bash
cqs scout "task description" --json  # Search + callers + tests + staleness + notes in one call
```

### Exploring unfamiliar code:
```bash
cqs onboard "concept" --json         # Guided tour: entry point → call chain → types → tests
cqs gather "query" --json            # Smart context: seed search + BFS call graph expansion
```

### Planning where to add new code:
```bash
cqs task "description" --json        # Full implementation brief: scout + gather + impact + placement
```

### Checking code health:
```bash
cqs health --json                    # Dead code, staleness, hotspots, untested functions
cqs dead --json                      # Find dead code (zero callers)
```

### Searching (use instead of grep/glob):
```bash
cqs "search query" --json            # Semantic search (hybrid RRF)
cqs "function_name" --name-only --json  # Definition lookup (fast)
cqs read <path>                      # File with notes injected
cqs read --focus <function>          # Function + type dependencies only
```

### Full command reference

`--json` works on all commands. `--format mermaid` on impact/trace. `--ref <name>` for cross-index search.

- `cqs explain <fn>` — function card: signature, callers, callees, similar
- `cqs callers <fn>` / `cqs callees <fn>` — call graph navigation
- `cqs deps <type>` — type dependencies
- `cqs similar <fn>` — find duplicate/similar code
- `cqs related <fn>` — co-occurrence: shared callers, callees, types
- `cqs where "description"` — placement suggestion with local patterns
- `cqs trace <source> <target>` — shortest call path
- `cqs context <file>` — module overview
- `cqs impact-diff [--base REF]` — diff-aware impact analysis
- `cqs review` — diff review with risk scoring
- `cqs ci [--base REF] [--gate high|medium|off]` — CI gate
- `cqs blame <fn>` — semantic git blame
- `cqs stale` — check index freshness
- `cqs diff <ref>` / `cqs drift <ref>` — semantic diff/drift between snapshots
- `cqs suggest` — auto-suggest notes from patterns
- `cqs notes add/update/remove/list` — manage project notes
- `cqs gc` — clean stale index entries
- `cqs batch` — batch mode with pipeline syntax
- `cqs chat` — interactive REPL
- `cqs audit-mode on/off` — toggle audit mode
- `cqs convert <path>` — convert PDF/HTML/CHM/MD to Markdown
- `cqs train-data` — generate training data from git history
- `cqs stats` — index statistics

**Token budgeting** — `--tokens N` on `query`, `gather`, `context`, `explain`, `scout`, `onboard`, and `task` packs results into a token budget (greedy knapsack by score). Commands that don't normally output content (`context`, `explain`, `scout`) include source code within the budget. `task` uses waterfall budgeting across sections (scout 15%, code 50%, impact 15%, placement 10%, notes 10%). JSON output adds `token_count` and `token_budget` fields.

Run `cqs watch` in a separate terminal to keep the index fresh, or `cqs index` for one-time refresh.

## Audit Mode

Before audits, fresh-eyes reviews, clear-eyes reviews, or unbiased code assessment:
`cqs audit-mode on` to exclude notes and force direct code examination.

After: `cqs audit-mode off` or let it auto-expire (30 min default).

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
- Use `opus` for all agent dispatches
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

**Every agent prompt MUST include cqs tool instructions.** Agents can't use cqs unless told how. Include the key commands: `search, read, read --focus, callers, callees, explain, similar, gather, impact, impact-diff, test-map, trace, context, dead, scout, task, plan, onboard, where, deps, related, diff, drift, batch, review, ci, health, suggest, stale, gc, convert, ref, notes, blame, doctor, index, stats`.

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
1. Enable audit mode before each batch (`cqs audit-mode on --expires 2h`)
2. `TeamCreate` per batch, agents per category (sonnet for judgment, haiku for mechanical)
3. Each agent writes findings to `docs/audit-findings.md` (append, don't overwrite)
4. Shutdown team, cleanup before next batch
5. After all batches: triage into `docs/audit-triage.md` (fresh file with P1-P4 tables), then fix

**Archive workflow:** Before each audit, rename existing `audit-findings.md` and `audit-triage.md` with version suffix (e.g., `audit-findings-v0.9.1.md`). Each audit starts fresh.

**Why:** Findings get lost when context compacts. Issues make work visible to future sessions.

## Completion Checklist

Before marking any feature "done":

1. **Trace the call path.** If you wrote `fn foo()`, grep for callers. Zero callers = dead code = not done.
2. **Test end-to-end.** "It compiles" is not done. Actually run it. Does the user-facing command use your code?
3. **Check for warnings.** `cargo build 2>&1 | grep warning` - dead code warnings mean incomplete wiring.
4. **Verify previous work.** If building on existing code, verify that code actually works first. Don't assume.

The HNSW disaster: built an index, wrote save/load, marked "done" - but search never called it. Three months of O(n) scans because nobody traced `search()` → `search_by_candidate_ids()` → zero callers.

**"Done" means a user can use it, not that code exists.**

5. **Verify wiring after parallel execution.** When agents build APIs in parallel, the *glue* between them is where bugs hide. After all agents finish: grep for the old pattern (e.g., `resolve(None, None)`) — if it still exists at call sites that should use the new API, the wiring is incomplete. Run `cqs impact <new_function>` to verify it has production callers.

The configurable models disaster: `build_batched_with_dim()` existed and worked, but all 20 production callers still used `build_batched()` which hardcoded 768. The convenience wrapper masked the problem — no compiler warning, all tests passed, feature was completely non-functional.

6. **Update the roadmap.** Check off completed items in `ROADMAP.md`. Stale roadmaps cause duplicate work.

## Project Conventions

- Rust edition 2021
- `thiserror` for library errors, `anyhow` in CLI
- No `unwrap()` except in tests
- GPU detection at runtime, graceful CPU fallback
- **GPU available** — always use `--features gpu-index` for cargo build/test/clippy. This is the default, not the exception. Env vars are in `~/.bashrc` (above the interactive guard).

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
powershell.exe -Command "cd C:\Projects\cqs; git push"
powershell.exe -Command 'gh pr create --title "..." --body "..."'
powershell.exe -Command 'gh pr merge N --squash --delete-branch'
```

**Use `gh pr checks --watch`** to wait for CI. Don't use `sleep` + poll.

**ALWAYS use `--body-file` for PR/issue bodies.** Never inline heredocs or multiline strings in `gh pr create --body` or `gh issue create --body`. Two reasons: (1) PowerShell mangles complex strings, (2) Claude Code captures the entire multiline command as a permission entry in `settings.local.json`, corrupting the file and breaking startup. Write body to `/mnt/c/Projects/cqs/pr_body.md`, use `--body-file`, delete after.

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

**Use `cqs notes add` to add notes** — it is available immediately. Direct file edits require `cqs index` to sync to SQLite. Sentiment affects code search rankings: positive boosts mentioned code, negative demotes it.

```bash
cqs notes add "note text" --sentiment -0.5 --mentions file.rs,concept
cqs notes update "exact text" --new-text "updated" --new-sentiment 0.5
cqs notes remove "exact text"
cqs notes list --json
```

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

Also set up `.claude/skills/` with portable skills. Use `/cqs-bootstrap` if available, or copy from an existing cqs project. Skills are auto-discovered from `.claude/skills/*/SKILL.md`.
