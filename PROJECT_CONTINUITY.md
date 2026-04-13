# Project Continuity

## Right Now

**Alpha sweep complete. α=0.9 is the optimal default. 9 PRs merged this session. (2026-04-13 00:40 CDT)**

### Sweep result: α=0.9 → +1.1pp R@1

11-point alpha sweep (0.0-1.0) × 265 queries × 8 categories. α=0.9 (90% dense, 10% sparse) is the only alpha that beats baseline overall: 43.4% vs 42.3%.

Per-category at α=0.9: identifier +4pp, conceptual +8.4pp, structural +11pp, type_filtered +4.2pp. Multi_step/cross_language/negation: SPLADE off (1.0) is best.

### Session PRs (all merged)

#910, #911, #926, #927, #928, #929, #930, #931. PR #932 (sweep results) in progress.

### What's next

- Ship α=0.9 as default (change hardcoded 1.0 → 0.9 in resolve_splade_alpha)
- Config file support for [splade.alpha] per-category overrides
- Release v1.23.0

## Open Issues
- #909, #912-#925, #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0, Schema: v20
- Daemon: `cqs watch --serve` (3-19ms graph queries)
- Per-category SPLADE alpha: `resolve_splade_alpha()`, default 1.0 (ship 0.9 pending)
- Query cache: `~/.cache/cqs/query_cache.db`
- 90 audit findings fixed + 1 won't-fix
