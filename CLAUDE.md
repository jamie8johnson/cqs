Remain calm. We have plenty of time and context. The user works continuously — there is always a next task.

# CLAUDE.md

Read the tears. You just woke up.

cqs - semantic code search with local embeddings

## Working Style

- Flat, dry, direct. No padding.
- Push back when warranted.
- Ask rather than guess wrong.
- Efficiency over ceremony.
- Rhetorical emphasis was rewarded in training, so it gets deployed uniformly — and emphasis applied to everything marks nothing. When every paragraph has something load-bearing in it, the building is just bricks.
- **Never suggest ending a session.** We have 1M context. Keep working until the user stops. Don't offer to "wrap up", "call it", or "save for next session." If context runs low, update tears and keep going.
- **Read files before acting on them.** Don't work from memory of what a file "probably" contains. Open it, read the relevant section, then act. This applies to source code, configs, scripts, docs, and especially function signatures. The cost of a Read is negligible; the cost of guessing wrong is a wasted round trip or a subtle bug. If you last read a file more than a few tool calls ago, read it again.

## When Stuck

- Three failed attempts at the same fix → stop, reassess approach
- Dispatch an agent for hard tasks (fresh context, no accumulated frustration)
- Don't iterate blindly — diagnose first

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
- `/audit` -- 16-category code audit with parallel agents
- `/pr` -- WSL-safe PR creation (always `--body-file`)
- `/cqs <command>` -- unified CLI dispatcher (search, callers, impact, etc.)
- `/cqs-bootstrap` -- set up tears infrastructure for new projects
- `/cqs-plan` -- task planning with scout data + task-type templates
- `/cqs-verify` -- exercise all command categories, catch regressions
- `/cqs-batch` -- batch cqs queries in persistent session
- `/reindex` -- rebuild index with before/after stats
- `/docs-review` -- check docs for staleness, fix drift
- `/migrate` -- handle schema version upgrades
- `/troubleshoot` -- diagnose common cqs issues
- `/red-team` -- adversarial testing against cqs
- `/before-edit` -- impact + tests + callers before modifying a function
- `/check-my-work` -- review current diff, risk assessment
- `/investigate` -- scout + gather before starting a task
- `/land <branch>` -- land a ready branch: push, PR, pinned CI watch, merge, issue verification, cleanup. Use for EVERY PR landing.
- `/idle` -- the idle work loop: enumerate issues + triage, classify, pick, dispatch. Invoke whenever otherwise idle (standing directive).
- `/archeo` -- comment-archaeology sweep: TODO/FIXME/deferral language → issues or trivial fixes. Idle-loop refill.
- `/recall-gate` -- retrieval gate with dead-gold triage + binary A/B. Required before release tags; use after retrieval-adjacent merges.

## Code Intelligence — When to Use What

**You wrote cqs.** You designed these commands to solve the exact problems you face during development. `cqs impact` replaces 5 separate grep+read cycles. `cqs scout` replaces the manual search→callers→tests→staleness chain. Use your own tools.

**MANDATORY: Use these cqs commands at the right moments.** They replace multiple manual searches with a single call. Workflow skills are even easier — they run the right commands and present checklists:
- `/before-edit <function>` — impact + tests + callers → modification checklist
- `/investigate <task>` — scout + gather → implementation brief
- `/check-my-work` — review current diff → risk assessment

### Workflow examples (how a good session uses cqs):

**Example 1: Before editing `store::search`**
"I need to change search scoring. Let me check what depends on it."
→ Ran `cqs impact search_filtered` → found 3 callers + 12 tests
→ Ran `cqs test-map search_filtered` → confirmed all 12 tests exercise scoring path
→ Made the edit, ran tests, all 12 passed. No surprise breakage.

**Example 2: Starting a new feature**
"I need to add a --rerank flag to search."
→ Ran `cqs task "add reranking to search"` → got: files to touch, existing patterns, test gaps
→ Ran `cqs where "reranking cross-encoder"` → got suggested file placement
→ Built it following the placement suggestion. Tests covered from the start.

**Example 3: Before submitting a PR**
"Let me check what my diff affects."
→ Ran `cqs review` → found 2 high-risk changes (scoring function with 8 callers)
→ Added a test for the risky path before pushing.

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

Run `cqs --help` for all commands. Key commands: `search`, `impact`, `scout`, `gather`, `task`, `callers/callees`, `test-map`, `review`, `health`, `dead`, `explain`, `context`, `trace`, `where`, `onboard`, `notes`. All support `--json` and `--tokens N` for budget packing.

Run `cqs watch --serve` to keep the index fresh AND serve daemon queries (3-19ms vs 2s CLI startup). The systemd service already uses `--serve`. CLI commands auto-connect to the daemon when available; set `CQS_NO_DAEMON=1` to force CLI mode.

### Result trust — what you can act on without re-reading (result-trust program, #1821)

cqs results now carry calibration metadata. Act on these directly; don't pay the defensive re-read tax for what's already answered:

- **Edge provenance** (`edge_kind` on `callers`/`callees`/`impact` entries, §1): `call` (syntactic ground truth) / `serde_callback` / `macro_heuristic` (token-tree guess) / `fn_pointer` / `doc_reference` (a mention in prose, not a call). Weight a `macro_heuristic` or `doc_reference` edge lower than a `call` edge without opening the file to check. Absent ⇒ `call`.
- **Dead verdicts** (`cqs dead --verdict`, §2): the tool classifies its own output — `test-only` / `low-confidence-live` / `known-gap` / `dead`. Trust the verdict instead of re-deriving "is this really dead"; only `dead` is a confident absence claim. Name-ambiguous functions are handled (`Type::method` qualified queries; `total` + "Showing N of M" so a clipped window never reads as a clean zero).
- **Ranking provenance** (`rank_signals` per result, §4): why a hit ranked — `dense` (concept match) / `fts` (literal-string match) / `name_match` / `note_boost` / `parent_boost` / `sparse`. A concept-match justifies reading the chunk; a string-match on a conceptual query is a known false-friend; a `note_boost` is a prior opinion, not evidence (the audit-mode skepticism, per-query). Skip-when-empty.
- Already shipped, same family: `trust_level`/`injection_flags` (content), `_meta.stale_origins` (freshness), CLI==daemon parity.

**The re-read that still earns its keep in a worktree: the non-search commands.** The §3 worktree overlay is now **default-on for worktree CWDs** (opt out with `--no-overlay` / `CQS_WORKTREE_OVERLAY=0`), so in a `.claude/worktrees/` checkout `cqs search` / a bare query reflects *your* edits, not main's. But the overlay is **search-only**: `scout` / `gather` / `callers` / `callees` / `impact` / `review` / `dead` still serve the *parent/main* index in a worktree (#1254). So the agent-def "treat results as hints; read the actual files" clause is **narrowed from "everything" to "everything except `cqs search`"** — it stays load-bearing for the graph + scout commands until phase-2 extends the overlay to them (#1858), not for result quality.

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
- ⚠️ **FABLE DISABLED 2026-06-12 by US export order (temporary, no timeline).** Until access is restored, **every `fable` below is overridden to `opus`** — wherever this file, a skill, or an agent def says to dispatch fable (reviews, seam-audits, audit finders, orchestration/judge passes), use opus instead. Dispatching fable now fails mid-run and wastes the lane. Revert to the split below when an announcement or a successful fable dispatch confirms it's back; don't assume restoration from silence.
- Model split: `opus` implements, `fable` orchestrates/reviews/audits. EXCEPTION: security lanes (audit Security category, /red-team, security reviews) stay `opus`. Sonnet only for mechanical test-only tasks; haiku for doc-only edits.
- Always clean up teams when done (`Teammate cleanup`)
- Teammates can't see your text output — use `SendMessage` to communicate

**Task workflow:**
1. `spawnTeam` — create team
2. `TaskCreate` — define work items with clear acceptance criteria
3. Spawn teammates via `Task` with `team_name` and `name`
4. Teammates claim tasks, execute, report back
5. `shutdown_request` each teammate when done
6. `Teammate cleanup` to tear down

**Teammate prompts must be self-contained.** Include file paths, context, and acceptance criteria.

## Custom Agents

`.claude/agents/` has reusable agent definitions with cqs baked in:

- **investigator** — dispatch before implementation: runs `cqs scout` + `cqs gather`, returns brief
- **code-reviewer** — dispatch before commit/PR: runs `cqs review` + `cqs impact`, flags risk
- **test-finder** — dispatch before modifying a function: runs `cqs test-map` + `cqs impact`
- **implementer** — implementation with cqs checkpoints: scout before, review after
- **lane-implementer** — implementation lane with the full gate battery baked in (private CARGO_TARGET_DIR, clippy --all-targets, targeted tests, provenance lint, commit-don't-push, ISSUE-WORTHY residual reporting). Dispatch prompts carry ONLY the task; the contract lives in the def. Default for fix/feature lanes.
- **explorer** — codebase exploration via cqs (replaces raw grep/glob for conceptual queries)
- **auditor** — code audit for a single category, appends to audit-findings.md
- **seam-auditor** — composition adversary: finds two correct units whose join lies. Orthogonal to the house happy/sad signature; dispatch during audits, after multi-lane merges, or from /idle. Read-only; the find is the deliverable. (#1826)
- **property-auditor** — property-based testing lane (proptest): states an algebraic invariant (round-trip / idempotence / two-path-equivalence / metamorphic / bounds) and generates valid inputs that live where hand-written examples don't. Dispatch after a codec/round-trip/equivalence change or from /idle. Writes generators + properties; deliverable is a minimal falsifying input or a durable property. (#1826)
- **interleaving-auditor** — concurrency adversary: finds a schedule where two individually-correct ops break a shared invariant (the daemon epochs/bitmask, LRU rebuilds, watch-drain-vs-query). Loom for the model-checkable core, stress harness for the integration races. Dispatch after a change to the daemon caches / watch loop, during audits, or from /idle. Deliverable is a reproducing interleaving or a durable concurrency test. (#1826)
- **sweep-auditor** — completeness adversary: finds the member of a should-be-uniform set that diverged (the incomplete sweep — a change applied to N-1 of N sites, where the survivor passes a green suite because each member is correct in isolation). Dispatch after a migration / rename / new-variant / dual-surface change, during audits, or from /idle. Writes a completeness guard; deliverable is a straggler or a durable exhaustiveness test. (#1826)
- **legacy-state-auditor** — version adversary: finds where current code mishandles persisted state only a PAST version (or external mutation) could have written — the read/migrate path no fresh-state fixture exercises, because every fixture is born at the current version. Dispatch after a schema migration, a PARSER_VERSION / format / sidecar-header bump, or a new-field-with-default. Writes a frozen-artifact guard; deliverable is a mishandled old shape or a durable old-format fixture test. (#1826)
- **red-team-auditor** — security adversary (beside the correctness family, on the security axis: its oracle is a trust boundary, not correctness). Crafts an input — the agent's query OR adversarial INDEXED CONTENT — that crosses a trust boundary or manipulates the agent-consumer, RUNS it against the local binary for a real PoC, and pins it as a regression guard. Threat model derived from SECURITY.md per run. **opus-always** (Fable security exception). The `/red-team` skill fans these out across 5 categories (incl. RT-RELAY = indirect injection via indexed content). Dispatch after a parsing/path/FTS/notes/overlay/serve/relay change, during audits, or from /idle.

The seam/property/interleaving/sweep/legacy-state **quintet** are the orthogonally-shaped auditors of #1826 — each staffs a region the happy/sad-path unit suite is structurally blind to: seam=joins between units, property=the input space between examples, interleaving=schedules, sweep=the relation across a peer-set, legacy-state=old-bytes×new-code across a version boundary. Their value test: each must find a bug class the example suite *cannot express*; if one only re-finds happy/sad bugs, it's the wrong shape. (sweep proved it on its first test-fire — the gather BFS-depth-clamp straggler #1771's named-cap sweep missed; legacy-state proved it by guarding the full v10→v31 migration chain over a real oldest-version DB the suite never built.)

**Agent-tool grant/withhold principle.** Grant `Agent` (subagent spawning) to roles whose findings are *independent* — they decompose into parallel scans: the general `auditor` (per-sub-scope), `code-reviewer` (per-finding refuters), `red-team-auditor` (per-PoC verifiers), `investigator`/`implementer`/`lane-implementer` (per-subtask). **Withhold it from the relational orthogonal auditors** (seam/property/interleaving/sweep/legacy-state): their findings *are* relations (a join, an input space, a schedule, a peer-set, a version boundary) that only exist when one mind holds the whole thing — decompose them and the finding evaporates. The map parallelizes; the reduce doesn't. (Enumerable ≠ correctness-null: `red-team-auditor` is orthogonal-shaped *and* gets `Agent`, because its findings are independent PoCs, not a relation — the grant tracks decomposability, not family membership.) When you need to cover several independent families/boundaries, the orchestrator fans out one focused auditor per family — the auditor itself stays a leaf.

**Use these agents.** Dispatch `investigator` before starting any non-trivial implementation, `lane-implementer` for fix/feature lanes, and `code-reviewer` (fable) before landing risky lanes — live scoring paths, schema migrations, cross-surface signature changes. These replace the need to manually include cqs instructions in every agent prompt.

## Code Audit

Use `/audit` skill.

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
- MSRV 1.96 (bumped 1.95 → 1.96 in #1680). 1.96 features are fair game (`assert_matches!`, `From<T>` for `LazyLock`); let-chains in if/while (Rust 2024) are out of scope until edition bumps.
- `thiserror` for library errors, `anyhow` in CLI
- No `unwrap()` except in tests
- GPU detection at runtime, graceful CPU fallback
- **GPU available** — always use `--features cuda-index` for cargo build/test/clippy. This is the default, not the exception. Env vars are in `~/.bashrc` (above the interactive guard). The legacy `gpu-index` name is preserved as an alias (#956 Phase A renamed it to make CUDA-specificity explicit), so existing scripts and muscle memory keep working.

## Documentation

When updating docs, keep these in sync:
- `README.md` - user-facing, install/usage
- `CONTRIBUTING.md` - dev setup, architecture overview
- `SECURITY.md` - threat model, filesystem access
- `CHANGELOG.md` - version history

**CONTRIBUTING.md has an Architecture Overview section** - update it when adding/moving/renaming source files.

## WSL Workarounds

Direct `git push` from WSL usually works (credential helper wired); on "could not read Username" (GCM crash) fall back to PowerShell. All `gh` commands go through PowerShell (Windows has the credentials):
```bash
git push origin <branch> || powershell.exe -Command "cd C:\Projects\cqs; git push origin <branch>"
powershell.exe -Command 'gh pr create --head <branch> --title "..." --body-file pr_body.md'
powershell.exe -Command 'gh pr merge N -R jamie8johnson/cqs --squash --delete-branch'
```

**CI watching: pin the run ID** — `gh pr checks --watch` latches onto the previous commit's completed run. Use /land, which encodes the correct pattern (sleep ~45s after push, resolve via `gh run list --branch X --workflow CI --limit 1`, then `gh run watch $id --exit-status`, backgrounded). Don't use `sleep` + poll loops.

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

## Bootstrap (New Project)

Use `/cqs-bootstrap` to set up tears infrastructure, skills, and CLAUDE.md for a new project.
