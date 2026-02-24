---
name: cqs
description: "Unified cqs CLI dispatcher — semantic code search, call graph navigation, impact analysis, quality tools, notes, and infrastructure. Use `/cqs <command> [args]` for any cqs operation."
disable-model-invocation: false
argument-hint: "<command> [args...]"
---

# cqs — Unified CLI Dispatcher

Parse the first argument as the command name, pass remaining arguments as flags.

Run via Bash: `cqs <command> <args> --json -q 2>/dev/null`

Present results to the user.

---

## Search & Discovery

### search `<query>` — Semantic code search
Finds functions by concept, not text. Use instead of grep/glob.

```
cqs "<query>" [flags] --json -q
```

| Flag | Description |
|------|-------------|
| `--lang <L>` | Filter by language (rust, python, typescript, javascript, go, c, java, sql, markdown) |
| `-n/--limit <N>` | Max results (default 5, max 20) |
| `-t/--threshold <N>` | Similarity threshold (default 0.3) |
| `--name-only` | Definition lookup, skips embedding |
| `--semantic-only` | Pure vector similarity, no hybrid RRF |
| `--rerank` | Cross-encoder re-ranking (slower, more accurate) |
| `-p/--path <glob>` | Path pattern filter (e.g., `src/cli/**`) |
| `--chunk-type <T>` | Filter: function, method, class, struct, enum, trait, interface, constant, section |
| `--pattern <P>` | Pattern: builder, error_swallow, async, mutex, unsafe, recursion |
| `--note-only` | Return only notes |
| `--note-weight <F>` | Note score weight 0.0-1.0 (default 1.0) |
| `--ref <name>` | Search only this reference index |
| `--tokens <N>` | Token budget (greedy knapsack by score) |
| `--expand` | Parent context (small-to-big retrieval) |
| `-C/--context <N>` | Lines of context around chunk |
| `--no-content` | File:line only, no code |
| `--no-stale-check` | Skip per-file staleness checks |

### similar `<function>` — Find similar code
Find code similar to a given function. Refactoring discovery, duplicates.

```
cqs similar "<name>" --json -q
```

### gather `<query>` — Smart context assembly
Seed search + BFS call graph expansion. "Show me everything related to X."

```
cqs gather "<query>" [flags] --json 2>/dev/null
```

| Flag | Description |
|------|-------------|
| `--expand <N>` | BFS depth (default 1, max 5) |
| `--direction <D>` | both, callers, callees (default both) |
| `-n/--limit <N>` | Max results (default 10) |
| `--tokens <N>` | Token budget |
| `--ref <name>` | Cross-index: seed from reference, bridge into project |

### where `<description>` — Placement suggestion
Where to add new code based on semantic similarity and local patterns.

```
cqs where "<description>" --json -q
```

### scout `<task>` — Pre-investigation dashboard
Search + callers/tests + staleness + notes in one call. First step for new tasks.

```
cqs scout "<task>" [-n N] [--tokens N] --json 2>/dev/null
```

### task `<description>` — Implementation brief
Scout + gather + impact + placement + notes. Waterfall token budgeting (scout 15%, code 50%, impact 15%, placement 10%, notes 10%).

```
cqs task "<description>" [-n N] [--tokens N] --json 2>/dev/null
```

### onboard `<concept>` — Guided codebase tour
Entry point + call chain + callers + types + tests. Ordered reading list.

```
cqs onboard "<concept>" [-d/--depth N] [--tokens N] --json 2>/dev/null
```

### read `<path>` — File with contextual notes
File contents with notes injected as comments. Richer than raw Read.

```
cqs read <path> [--focus <function>] --json -q
```

`--focus <function>` returns only the target function + type dependencies.

### context `<file>` — Module overview
Chunks, callers, callees, notes for a file.

```
cqs context <file> --json -q
```

### explain `<function>` — Function card
Signature, docs, callers, callees, similar in one call.

```
cqs explain "<name>" --json -q
```

---

## Call Graph

### callers `<function>` — Who calls this?
```
cqs callers "<name>" --json -q
```

### callees `<function>` — What does this call?
```
cqs callees "<name>" --json -q
```

### trace `<source>` `<target>` — Shortest call path
BFS shortest path between two functions.

```
cqs trace "<source>" "<target>" --json -q
```

### deps `<name>` — Type dependencies
Forward: who uses this type? Reverse (`--reverse`): what types does this function use?

```
cqs deps "<name>" [--reverse] --json -q
```

### related `<function>` — Co-occurrence
Functions sharing callers, callees, or types.

```
cqs related "<name>" --json -q
```

### impact `<function>` — What breaks if you change X?
Callers with snippets + affected tests via reverse BFS.

```
cqs impact "<name>" [--depth N] [--suggest-tests] --json -q
```

### impact-diff — Diff-aware impact analysis
Changed functions, affected callers, tests to re-run.

```
cqs impact-diff [--base <ref>] [--stdin] [--json] [--tokens N]
```

### test-map `<function>` — Map function to tests
Tests that exercise a function via reverse call graph.

```
cqs test-map "<name>" --json -q
```

---

## Quality & Review

### dead — Find dead code
Functions/methods with no callers.

```
cqs dead [--include-pub] [--min-confidence low|medium|high] --json -q
```

### stale — Index freshness
Files modified since last index.

```
cqs stale --json -q
```

### health — Codebase quality snapshot
Stats + dead code + staleness + hotspots + untested hotspots + notes.

```
cqs health [--json]
```

### ci — CI pipeline analysis
Review + dead code + gate logic. Exit 3 on gate fail.

```
cqs ci [--base <ref>] [--stdin] [--gate high|medium|off] [--json] [--tokens N]
```

### review — Comprehensive diff review
Impact + notes + risk scoring + staleness. More detailed than `ci` (no gate).

```
cqs review [--base <ref>] [--stdin] [--json] [--tokens N]
```

### suggest — Auto-suggest notes
Scan for dead code clusters, untested hotspots, high-risk functions.

```
cqs suggest [--apply] [--json]
```

### gc — Report staleness
Report/clean stale index entries.

```
cqs gc --json -q
```

### stats — Index statistics
Chunk counts, languages, last update.

```
cqs stats --json -q
```

---

## Notes

### notes add `<text>` — Add a note
```
cqs notes add "<text>" [--sentiment N] [--mentions a,b,c] -q
```

Sentiment: -1 (serious pain), -0.5 (notable pain), 0 (neutral), 0.5 (notable gain), 1 (major win).

### notes update `<exact text>` — Update a note
```
cqs notes update "<exact text>" [--new-text "..."] [--new-sentiment N] -q
```

### notes remove `<exact text>` — Remove a note
```
cqs notes remove "<exact text>" -q
```

### audit-mode — Toggle audit mode
Excludes notes from search/read for unbiased review.

```
cqs audit-mode [on|off] [--expires 30m] --json
```

---

## Infrastructure

### ref — Reference index management
Manage external codebases for multi-index search.

| Subcommand | Usage |
|------------|-------|
| `ref add <name> <path> [--weight 0.8]` | Index external codebase (weight 0.0-1.0) |
| `ref list` | Show all references |
| `ref update <name>` | Re-index reference |
| `ref remove <name>` | Delete reference |

### watch — File watcher
Keep index fresh automatically.

```
cqs watch [--debounce <ms>] [--no-ignore]
```

### convert `<path>` — Document conversion
PDF/HTML/CHM/MD to cleaned Markdown.

```
cqs convert <path> [--output <dir>] [--overwrite] [--dry-run] [--clean-tags <tags>]
```

Tags: `aveva`, `pdf`, `generic`.

### diff `<source>` — Semantic diff
Compare indexed snapshots. Requires references via `ref add`.

```
cqs diff "<source>" [target] [--threshold F] [--lang L] --json -q
```

### drift `<reference>` — Semantic drift detection
Embedding distance between same-named functions across snapshots.

```
cqs drift "<ref>" [--min-drift 0.1] [--lang L] [--limit N] --json
```
