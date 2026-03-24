## Summary
v1.4.0 release — ChunkType expansion, refactoring, and quality-of-life improvements.

**New ChunkTypes:**
- Extension (Swift, ObjC, F#, Scala 3)
- Constructor (10 languages)
- Coverage gaps fixed: Python/JS/TS/Solidity/Java/Erlang/Bash/R/Lua

**Refactoring:**
- 4 large files split into 23 submodules (7,645 lines reorganized)

**Improvements:**
- `--json` alias on impact/review/ci/trace
- Single-thread runtime for 27 read-only commands
- Batch/chat cache auto-invalidation on index change

## Test plan
- [x] 1867 tests pass, 0 fail
- [x] `cargo clippy` — clean
- [x] `cargo fmt` — clean

🤖 Generated with [Claude Code](https://claude.com/claude-code)
