---
name: cqs-plan
description: Task planning with scout data + task-type templates. Produces implementation checklists.
disable-model-invocation: false
argument-hint: "<task description>"
---

# Plan

Generate an implementation plan by combining `cqs scout` output with a task-type template.

## Process

1. **Classify the task** into one of the templates below based on the description.
2. **Run scout**: `cqs scout "<task description>" --json -q`
3. **Run targeted lookups** from the template's checklist — scout alone misses structural touchpoints (clap structs, dispatch arms, skill files).
4. **Produce a plan** listing every file to change, what to change, and why. Be specific about struct fields, function signatures, and match arms.

## Arguments

- `$ARGUMENTS` — task description (required)

## Templates

### Add/Replace a CLI Flag

**When:** Adding a new flag, renaming a flag, changing a flag's type (bool → enum).

**Checklist:**
1. `src/cli/mod.rs` — `Commands` enum variant. Add/modify `#[arg]` field. If enum-typed, define with `clap::ValueEnum`.
2. `src/cli/mod.rs` — `run_with()` match arm. Update destructuring and `cmd_<name>()` call.
3. `src/cli/commands/<name>.rs` — `cmd_<name>()` signature. Update branching logic.
4. `src/cli/commands/<name>.rs` — Display functions if the flag affects output format.
5. `src/store/*.rs` / `src/lib.rs` — Usually NO changes. Only if flag affects query behavior.
6. Tests: `tests/<name>_test.rs` — add case for new value. Update tests using old flag name.
7. `.claude/skills/cqs-<name>/SKILL.md` — update argument-hint and usage.
8. `README.md` — update examples if the command is featured.
9. Verify callers: `cqs callers cmd_<name> --json`

**Patterns:**
- Output format flags: `#[arg(long, value_enum, default_value_t)]`
- Display functions: `display_<name>_text()`, `display_<name>_json()`
- JSON output: `serde_json::to_string_pretty` on `#[derive(Serialize)]` structs

### Add a New CLI Command

**When:** Adding an entirely new `cqs <command>`.

**Checklist:**
1. `src/cli/mod.rs` — Add variant to `Commands` enum with args. Add `use` import for `cmd_<name>`. Add match arm in `run_with()`.
2. `src/cli/commands/<name>.rs` — New file. Implement `cmd_<name>()`. Follow existing command pattern (open store, call library, format output).
3. `src/cli/commands/mod.rs` — Add `mod <name>;` and `pub(crate) use <name>::cmd_<name>;`.
4. `src/lib.rs` or `src/<module>.rs` — Library function if logic is non-trivial. Keep CLI layer thin.
5. Tests: `tests/<name>_test.rs` — integration tests using `TestStore` or `assert_cmd`.
6. `.claude/skills/cqs-<name>/SKILL.md` — New skill file with frontmatter.
7. `.claude/skills/cqs-bootstrap/SKILL.md` — Add to portable skills list.
8. `CLAUDE.md` — Add to "Key commands" list.
9. `README.md` — Add to command reference.
10. `CONTRIBUTING.md` — Update Architecture Overview if adding new source files.

**Patterns:**
- Command files are ~50-150 lines. Store/library calls, then display.
- `find_project_root()` + `resolve_index_dir()` + `Store::open()` boilerplate.
- JSON output with `--json` flag, text output respects `--quiet`.
- Tracing span at function entry: `let _span = tracing::info_span!("cmd_<name>").entered();`

### Fix a Bug

**When:** Something produces wrong results, panics, or misbehaves.

**Checklist:**
1. **Reproduce**: Understand the exact failure mode. Get input → actual → expected.
2. **Locate**: `cqs scout "<bug description>"` to find relevant code.
3. **Trace callers**: `cqs callers <function> --json` — who calls the buggy code? Are callers also affected?
4. **Check tests**: `cqs test-map <function> --json` — do tests exist? Do they cover the failing case?
5. **Fix**: Minimal change in the library layer, not the CLI layer.
6. **Add test**: Regression test that would have caught this bug.
7. **Check impact**: `cqs impact <function> --json` — did the fix change behavior for other callers?

**Patterns:**
- Fix in `src/*.rs` (library), test in `tests/*.rs` or inline `#[cfg(test)]`.
- Use `tracing::warn!` for recoverable errors, `bail!` for unrecoverable.
- Never `.unwrap()` in library code. `?` or `match` + `tracing::warn!`.

### Add Language Support

**When:** Adding a new programming language to the parser.

**Checklist:**
1. `Cargo.toml` — Add tree-sitter grammar dependency (optional).
2. `src/language/mod.rs` — Add to `define_languages!` macro invocation.
3. `src/language/<lang>.rs` — New file with `LanguageDef`: chunk_query, call_query, extensions.
4. `Cargo.toml` features — Add `lang-<name>` feature, add to `default` and `lang-all`.
5. Tests: `tests/fixtures/<lang>/` — sample files. Parser tests in `tests/parser_test.rs`.
6. `tests/eval_test.rs` and `tests/model_eval.rs` — Add match arms.

**Patterns:**
- One-liner in `define_languages!` handles registration.
- Chunk query captures must use names from `extract_chunk`'s `capture_types`: function, struct, class, enum, trait, interface, const.
- Call query uses `@callee` capture.

### Refactor / Extract

**When:** Moving code, splitting files, extracting shared helpers.

**Checklist:**
1. **Find all call sites**: `cqs callers <function> --json` for each function being moved.
2. **Check similar code**: `cqs similar <function> --json` to find duplicates to consolidate.
3. **Plan visibility**: `pub(crate)` for cross-module, `pub` for public API, private for same-module.
4. **Move tests with code**: `#[cfg(test)] mod tests` works in submodules.
5. **Update imports**: Each file needs its own `use` statements — they don't carry across modules.
6. **Verify callers compile**: After moving, all callers must update their `use` paths.
7. `CONTRIBUTING.md` — Update Architecture Overview for structural changes.

**Patterns:**
- `impl Foo` blocks can live in separate files (Rust allows multiple).
- Trait method imports don't carry over to submodule files.
- Use `pub(crate)` for types/constants shared across submodules.

## Output Format

Present the plan as:

```
## Plan: <task summary>

### Files to Change

1. **<file>** — <what and why>
   - <specific change with code snippet>

### Tests

- <test file>: <what to test>

### Not Changed (verified)

- <file>: <why no changes needed>
```

## When Not to Use

- Trivial changes (typos, single-line fixes) — just do it
- Pure research — use `cqs gather` or `cqs scout` directly
- Audit findings — use `/audit` skill
