# Project Continuity

## Right Now

**Alpha sweep running. 8 PRs merged this session. (2026-04-12 21:45 CDT)**

Branch: `fix/eval-batch-runner` (PR #931, CI running)

### Alpha sweep in progress

α=0.5 at 40/265 queries. Batch runner mode (single cqs process per alpha). 9 alphas total. Results will fill the alpha×category matrix for routing defaults.

### Session PRs

| PR | Theme | Status |
|---|---|---|
| #910 | AC-1: SPLADE fusion score preservation | **merged** |
| #911 | Audit P2/P3 mega-batch (28 findings) | **merged** |
| #926 | Daemon: `cqs watch --serve` (3-19ms) | **merged** |
| #927 | Daemon follow-up: arg translation + tests | **merged** |
| #928 | Persistent query embedding cache | **merged** |
| #929 | Shared runtime + routing + eval + docs | **merged** |
| #930 | SPLADE routing fixes + batch eval | **merged** |
| #931 | Eval batch runner + daemon follow-up | CI running |

### Bugs found and fixed this session

1. AC-1: fused scores discarded in scoring pipeline
2. CQ-4: incomplete persist fallback (silent data loss)
3. SPLADE model loading gated on cli.splade not use_splade
4. Batch handler missing routing entirely
5. --splade flag silently disabling all adaptive routing
6. Eval batch runner pipe buffering (stdbuf doesn't work on Rust)
7. CRLF line endings in sweep script on WSL

### Clean eval results (post all fixes)

| Category | Baseline | +SPLADE α=0.7 | Delta | N |
|---|---|---|---|---|
| structural_search | 51.9% | 66.7% | +14.8pp | 27 |
| conceptual_search | 33.3% | 41.7% | +8.4pp | 36 |
| identifier_lookup | 94.0% | 96.0% | +2.0pp | 50 |
| type_filtered | 33.3% | 29.2% | −4.1pp | 24 |
| behavioral_search | 25.0% | 20.5% | −4.5pp | 44 |
| negation | 13.8% | 6.9% | −6.9pp | 29 |
| cross_language | 23.8% | 14.3% | −9.5pp | 21 |
| multi_step | 32.4% | 20.6% | −11.8pp | 34 |
| **Overall** | **42.3%** | **41.1%** | **−1.2pp** | 265 |

### After sweep

- Fill alpha×category matrix in research
- Pick per-category optimal alpha
- Add config file support ([splade.alpha] in .cqs.toml)
- Ship defaults

### Plan docs

- `docs/plans/2026-04-12-persistent-daemon.md` — shipped
- `docs/plans/2026-04-12-selective-splade-routing.md` — in progress (alpha-only design)

## Open Issues
- #909, #912-#925 (7 shipped, 6 open, 1 won't-fix), #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0, Schema: v20
- Daemon: `cqs watch --serve` (systemd, 3-19ms graph queries)
- Per-category SPLADE alpha: `resolve_splade_alpha()` in router.rs + batch handler
- --splade flag adds SPLADE without disabling adaptive routing
- Query cache: `~/.cache/cqs/query_cache.db`
- Eval: batch runner (persistent cqs batch process) with CQS_NO_DAEMON=1
- 90 audit findings fixed + 1 won't-fix
