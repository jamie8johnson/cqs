## Summary
ChunkType coverage improvements across 11 languages:

**Extension variant (4 languages):**
- Swift: Extension for `extension Type { }` (already in main)
- Objective-C: Extension for categories `@interface Type (Category)`
- F#: Extension for type extensions `type MyType with ...`
- Scala 3: Extension for `extension (x: Type) { ... }`
- Infrastructure: wired `@extension` capture in parser chunk.rs and mod.rs

**Coverage gaps fixed (7 languages):**
- Python: UPPER_CASE module-level constants as Constant
- JavaScript: `const` declarations (non-function) as Constant
- TypeScript: `const` declarations (non-function) as Constant
- Solidity: `event` declarations use Event ChunkType (was Property)
- Java: `static final` fields promoted to Constant (was Property)
- Erlang: `-define()` preprocessor macros as Macro
- Bash: `readonly` declarations as Constant

**Also:** Roadmap updates for contrastive summaries, algorithm detection, Constructor variant

## Test plan
- [x] 1837 tests pass, 0 fail (+12 new tests)
- [x] `cargo fmt` — clean
- [x] ObjC: 3 new tests (category interface, category implementation, regular class stays Class)
- [x] F#: 1 new test (type extension)
- [x] Scala: 1 new test (extension definition)
- [x] Python: 1 new test (UPPER_CASE constants)
- [x] JS/TS: 2 new tests (const declarations)
- [x] Solidity/Java/Erlang/Bash: 4 new tests

🤖 Generated with [Claude Code](https://claude.com/claude-code)
