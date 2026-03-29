# OpenClaw Contributions

Tracking file for contributions to [openclaw/openclaw](https://github.com/openclaw/openclaw).

## Strategy

High-confidence findings that are likely to merge. Issues include corrective prompts for agent-fixable patterns. Quality over quantity.

**Anti-patterns to avoid:**
- Design-level changes (they'll reject or endlessly discuss)
- "Features not bugs" (compaction preserving keys, permissive file access — both intentional)
- False positive dead code (framework lifecycle callbacks)
- Anything requiring deep context on their architecture

## Submitted

| PR/Issue | Title | Status | Filed |
|----------|-------|--------|-------|
| [PR #56278](https://github.com/openclaw/openclaw/pull/56278) | fix: strip credentials from diagnostic payload logs | Open, CI green, Greptile 5/5 | 2026-03-28 |
| [PR #56972](https://github.com/openclaw/openclaw/pull/56972) | fix: replace duplicate clamp and isRecord with shared utils imports | Open, Greptile 5/5 | 2026-03-29 |
| [#11202 comment](https://github.com/openclaw/openclaw/issues/11202#issuecomment-4149969291) | Document that apiKey leak is fixed in current codebase | Comment posted | 2026-03-29 |
| [Issue #56973](https://github.com/openclaw/openclaw/issues/56973) | refactor: remove normalizeChannelId wrapper collision | Filed with corrective prompt | 2026-03-29 |
| [Issue #56974](https://github.com/openclaw/openclaw/issues/56974) | refactor: consolidate duplicated fileExists implementations | Filed with corrective prompt | 2026-03-29 |
| [Issue #56994](https://github.com/openclaw/openclaw/issues/56994) | fix: use atomic writes for crash-safety | Filed with corrective prompt | 2026-03-29 |
| [PR #56999](https://github.com/openclaw/openclaw/pull/56999) | perf: hoist regex patterns in detectImageReferences | Open | 2026-03-29 |
| [PR #57000](https://github.com/openclaw/openclaw/pull/57000) | fix: command poll backoff skips first tier after output | Open | 2026-03-29 |
| [PR #57001](https://github.com/openclaw/openclaw/pull/57001) | perf: hoist billing error normalization to module level | Open | 2026-03-29 |
| [PR #57002](https://github.com/openclaw/openclaw/pull/57002) | perf: reuse char estimate cache in context guard | Open | 2026-03-29 |

## Candidates

### Issue-with-prompt (agent-fixable)

| # | Title | Scope | Verified | Verdict |
|---|-------|-------|----------|---------|
| C1 | Consolidate duplicated `fileExists` | 2 clean drop-ins, 4 probable (access→stat). NOT: state-migrations (sync) | Yes | File as issue — moderate scope |
| C4 | Remove `normalizeChannelId` wrapper collision | Both wrappers delegate to canonical names (`normalizeChatChannelId`, `normalizeAnyChannelId`) already used by 45+ call sites. The wrappers (5 + 24 callers) force aliasing. | Yes | **Best issue-with-prompt** — mechanical rename, eliminates real collision |
| C5 | `DEFAULT_PING_PONG_TURNS == MAX_PING_PONG_TURNS` | `sessions-send-helpers.ts:10-11`, config override silently capped at default | No | Needs verification |

**Rejected after verification:**
- ~~C2 ensureDir~~: All 4 locals have different contracts (sync, file-path-not-dir, DI, result objects)
- ~~C3 pathExists~~: Only `marketplace.ts` is clean (1 file, too small for an issue). `boundary-path.ts` intentionally stricter.

### PR candidates (simple code changes)

| # | Title | File | Verified | Verdict |
|---|-------|------|----------|---------|
| C6 | Replace local `clamp` with `clamp` from utils | `pi-tools.read.ts:64` — `utils.ts` already exports `clamp` alias | **Yes, drop-in** | **Ready to PR** |
| C7 | Replace local `isRecord` with shared export | `doctor-legacy-config.ts:27` — functionally identical (`Boolean(v)` ≡ `v !== null` for objects) | **Yes, drop-in** | **Ready to PR** |
| C8 | Replace local `formatTokenCount` with shared | `subagent-announce-output.ts:463` — shared has variable precision for ≥10k (cosmetic diff) | Probably | Borderline — output changes |
| C9 | Replace local `formatDurationShort` with shared | `subagent-announce-output.ts:446` — returns `undefined` not `"n/a"`, omits trailing zeros | Probably | Borderline — needs `?? "n/a"` |
| ~~C10~~ | ~~Strip apiKey from model catalog~~ | #11202 already fixed — ModelCatalogEntry has no apiKey field, marker system, redaction on config.get, auth isolated to HTTP headers. Issue still Open on GH. | Verified fixed | **Comment on issue to document resolution** |

### Deep audit findings (2026-03-29) — needs verification before filing

**Data Safety — non-atomic writes (HIGH confidence, slam dunk)**

The codebase has `writeJsonAtomic` (temp+fsync+rename) and `writeTextAtomic` but several critical paths don't use them:

| # | File | What's at risk | Fix |
|---|------|---------------|-----|
| DS-1 | `infra/exec-approvals.ts:367` | All stored command approvals lost on crash (`writeFileSync`) | Use `writeJsonAtomic` |
| DS-2 | `infra/restart-sentinel.ts:72` | Lost message routing after restart (`fs.writeFile`) | Use `writeJsonAtomic` |
| DS-3 | `cli/completion-cli.ts:380` | User's `.zshrc`/`.bash_profile` corrupted (`fs.writeFile`) | Use `writeTextAtomic` |
| DS-4 | `infra/json-file.ts:16` (`saveJsonFile`) | Auth profiles + subagent registry lost on crash (`writeFileSync`). Used by auth-profiles/store.ts (5 call sites) and subagent-registry.store.ts | Replace with atomic pattern |

Best filed as one issue: "Use atomic writes consistently for crash-safety" — lists all affected files + corrective prompt.

**Correctness (HIGH confidence)**

| # | File | Bug | Impact |
|---|------|-----|--------|
| COR-1 | `command-poll-backoff.ts:39` | Off-by-one: after output reset (count=0), next no-output poll computes 0+1=1, skipping first 5s backoff tier straight to 10s | Agents wait 10s instead of 5s after commands go quiet |
| COR-2 | `session-write-lock.ts:125-172` | Async release's `fs.rm` can delete a newly-acquired lock file (race between release IO and immediate re-acquire) | Session transcript corruption (rare timing) |

**Correctness (MEDIUM confidence)**

| # | File | Bug | Impact |
|---|------|-----|--------|
| COR-3 | `auth-profiles/store.ts:461-465` | TOCTOU: write then stat mtime; concurrent writer can change mtime between ops | Stale auth credentials served from cache |
| COR-4 | `subagent-registry-state.ts:37-58` | Deleted runs resurrect from disk when persist fails (silent `saveJsonFile` failure) | Ghost subagent entries |

**Performance (HIGH confidence)**

| # | File | Issue | Hot path? | Fix effort |
|---|------|-------|-----------|-----------|
| PERF-1 | `pi-embedded-runner/run/images.ts:125-128` | 4x `new RegExp(...)` from constants on every turn | Every turn | Trivial — hoist to module level |
| PERF-2 | `pi-embedded-runner/tool-result-context-guard.ts:166-234` | Double context char estimation — creates 2 caches, iterates all messages twice | Every turn | Small — return cache from first pass |
| PERF-3 | `tool-loop-detection.ts:106-125` | SHA-256 hash on every tool call; could use string comparison or fast hash | Every tool call | Medium |
| PERF-4 | `pi-embedded-helpers/messaging-dedupe.ts:14` | Unicode emoji regex on every streaming chunk; most text is ASCII | Every chunk | Small — ASCII fast-path |
| PERF-5 | `pi-embedded-runner/run/payloads.ts:152-159` | Static billing error normalized on every turn | Every turn | Trivial — module-level const |

**Performance (MEDIUM confidence)**

| # | File | Issue | Impact |
|---|------|-------|--------|
| PERF-6 | `auth-profiles/store.ts` (8 sites) | `structuredClone` on every auth store read (3-5 deep clones per turn) | Every API call |
| PERF-7 | `pi-embedded-subscribe.handlers.tools.ts:35` | Module-level `toolStartData` Map grows unbounded if tool calls abort | Memory leak in daemons |
| PERF-8 | `pi-embedded-helpers/messaging-dedupe.ts:19-35` | O(n) linear scan with `.includes()` on every streaming chunk | Heavy messaging sessions |

### Not actionable (codebase is clean)

Security scan found zero new high-confidence issues beyond PR #56278. Specifically:
- All `catch {}` blocks are annotated with intent
- All `as any` has `oxlint-disable-next-line` suppression
- All `console.log` is in CLI entry points or gated by debug flags
- No deprecated Node.js APIs, no floating promises, no bare string throws

## Rejected

| Issue | Reason |
|-------|--------|
| Compaction preserving API keys | Intentional — operational workflow requirement |
| Agent file access to .env | By design — NemoClaw constrains this |
| 36 "dead" platform callbacks | Framework lifecycle methods — all used |
| Security email (broad leakage report) | Most findings were features, not bugs |
