# Project Continuity

## Right Now

**Per-category SPLADE defaults shipped. +4.9pp R@1 expected. (2026-04-13 08:45 CDT)**

### Shipped defaults (in resolve_splade_alpha)

| Category | α | R@1 | Δ vs baseline | Verified? |
|---|---|---|---|---|
| identifier_lookup | 0.9 | 98.0% | +4.0pp | sweep |
| structural_search | 0.7 | 66.7% | +14.8pp | **yes** |
| conceptual_search | 0.9 | 41.7% | +8.4pp | sweep |
| type_filtered | 0.9 | 37.5% | +4.2pp | sweep |
| behavioral_search | 0.1 | 31.8% | +6.8pp | **yes** |
| multi_step | 1.0 | 32.4% | 0 | — |
| cross_language | 1.0 | 23.8% | 0 | noise (N=21) |
| negation | 1.0 | 13.8% | 0 | — |

Expected overall: **47.2% R@1** (+4.9pp over 42.3% baseline)

### Session PRs (all merged)

#910, #911, #926, #927, #928, #929, #930, #931. PR #932 (sweep results) in CI.

### What's next

- Merge #932 (sweep results)
- PR the shipped defaults
- Config file support for [splade.alpha]
- Release v1.23.0

## Open Issues
- #909, #912-#925, #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0, Schema: v20
- Daemon: `cqs watch --serve` (3-19ms graph queries)
- Per-category SPLADE alpha defaults shipped in resolve_splade_alpha()
- Query cache: `~/.cache/cqs/query_cache.db`
- 90 audit findings fixed + 1 won't-fix
