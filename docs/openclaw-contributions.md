# OpenClaw Contributions

Tracking file for contributions to [openclaw/openclaw](https://github.com/openclaw/openclaw).

*Updated 2026-03-30.*

## Open PRs (7)

| PR | Title | Greptile | Notes |
|----|-------|---------|-------|
| [#57926](https://github.com/openclaw/openclaw/pull/57926) | perf: 4 hot-path optimizations (regex, billing const, cache reuse, credential stripping) | 5/5 | Consolidated from 4 closed PRs |
| [#57927](https://github.com/openclaw/openclaw/pull/57927) | fix: duplicate removal + toolStartData sweep with in-flight tracking | 4/5 | Consolidated from 2 closed PRs. Codex P1 on in-flight — addressed with runState counter |
| [#57919](https://github.com/openclaw/openclaw/pull/57919) | fix: gateway lock exit handler (closes #57032) | 4/5 | Codex P1 on async close — fixed with fsSync.closeSync(handle.fd) |
| [#57895](https://github.com/openclaw/openclaw/pull/57895) | fix: atomic writes for 3 crash-safety paths (closes #56994) | pending | Restart sentinel + exec-approvals + saveJsonFile. Shell profile reverted (symlink concern) |
| [#57024](https://github.com/openclaw/openclaw/pull/57024) | chore: remove unused deps (hono, long, uuid) | 5/5 | Lockfile regenerated |
| [#57016](https://github.com/openclaw/openclaw/pull/57016) | perf: ASCII fast-path for emoji stripping | 5/5 | Greptile P2 addressed (removed comma expression) |
| [#57000](https://github.com/openclaw/openclaw/pull/57000) | fix: poll backoff off-by-one | 3/5 | Greptile P2 addressed (clamp in getCommandPollSuggestion) |

## Closed PRs (6 — consolidated into #57926 + #57927)

| PR | Title | Reason |
|----|-------|--------|
| #56278 | fix: credential stripping in payload logs | → consolidated into #57926 |
| #56972 | fix: duplicate clamp/isRecord | → consolidated into #57927 |
| #56999 | perf: regex hoist | → consolidated into #57926 |
| #57001 | perf: billing const hoist | → consolidated into #57926 |
| #57002 | perf: cache reuse | → consolidated into #57926 |
| #57015 | fix: toolStartData sweep | → consolidated into #57927 |
| #57911 | fix: gateway lock exit handler | Auto-closed by PR limit bot → resubmitted as #57919 |
| #57913 | fix: cron log prune before read | Closed — Codex found race with append queue |

## Open Issues (6)

| Issue | Title | Status |
|-------|-------|--------|
| [#56973](https://github.com/openclaw/openclaw/issues/56973) | refactor: remove normalizeChannelId wrapper collision | Open |
| [#56974](https://github.com/openclaw/openclaw/issues/56974) | refactor: consolidate duplicated fileExists | Open |
| [#56994](https://github.com/openclaw/openclaw/issues/56994) | fix: atomic writes for crash-safety | PR #57895 covers 3 of 4 paths |
| [#57017](https://github.com/openclaw/openclaw/issues/57017) | fix: cron log OOM on large files | Updated with race analysis — needs write-queue integration |
| [#57019](https://github.com/openclaw/openclaw/issues/57019) | fix: session lock race | Amended — low/theoretical on Linux (starttime blocks fast path) |
| [#57032](https://github.com/openclaw/openclaw/issues/57032) | fix: gateway lock handle leak | PR #57919 |
| [#57036](https://github.com/openclaw/openclaw/issues/57036) | chore: remove unused deps | PR #57024 |

## Closed Issues (4)

| Issue | Reason |
|-------|--------|
| #57028 | SHA-256 for equality — not a real issue (30-entry bounded map, hash is correct) |
| #57029 | Messaging dedupe O(n) — by design (substring matching is intentional) |
| #57031 | QueuedFileWriter swallows errors — amended to low/informational (diagnostic logs only) |

## Other

| Type | Link | Description |
|------|------|-------------|
| Comment | [#11202](https://github.com/openclaw/openclaw/issues/11202) | Documented that apiKey leak is already fixed |

## Lessons

- **10-PR limit**: OpenClaw auto-closes PRs when author has >10 open. Consolidate related changes.
- **Greptile + Codex catch real issues**: ~30% of PRs get actionable feedback. Always check reviews before moving on.
- **Don't prune from read paths**: Mutating files on read races with append queues. Size-cap reads instead.
- **Exit handlers must be synchronous**: `process.on('exit')` can't run async ops. Use `fsSync.closeSync(fd)` not `handle.close()`.
- **Verify before filing**: 2 of 10 issues turned out to be non-issues after deeper investigation. Close honestly.
