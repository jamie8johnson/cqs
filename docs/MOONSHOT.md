# Moonshot Plan: Maximal Agent Power

## Context

cqs has 35 CLI commands, 15 in batch mode, pipeline syntax, token budgeting, and a reference system. Agents currently call 3-8 cqs commands per task and manually assemble understanding. The moonshot: **one query, complete implementation context, zero iteration**.

---

## Where We Are

**35 commands.** 15 available in batch mode. Pipeline syntax chains them. Token budgeting on 7 commands. Scout, gather, and impact exist independently but share nothing — each loads its own call graph, test chunks, and staleness data. Where is search-only (no call graph).

**Parser extracts:** Functions, methods, classes, structs, enums, traits, interfaces, constants (+ markdown sections). Call sites (callee name + line). Signatures as raw strings. Type references: parameter types, return types, field types, trait bounds, generic parameters via tree-sitter queries across 7 languages (Rust, Python, TypeScript, Go, Java, C, SQL).

**Schema v11:** `chunks`, `calls`, `function_calls`, `notes`, `type_edges` tables. Type-level dependency tracking via `type_edges`: source_chunk_id → target_type_name with edge_kind classification (Param, Return, Field, Impl, Bound, Alias). `cqs deps` command for forward/reverse queries.

**Search pipeline:** FTS5 keyword → semantic embedding → RRF fusion → HNSW acceleration → unified (code + notes). Notes are separate results merged by score — they don't influence code ranking.

**Token budgeting:** Flat greedy knapsack by score. No information-type priority. Exception: `explain` hardcodes target-first, similar-second.

**Embeddings:** 769-dim (768 E5-base-v2 + 1 sentiment), stored as BLOB in SQLite. `full_cosine_similarity()` exists for cross-store comparison. Reference system stores separate Store+HNSW per reference.

---

## Phase Ordering Principle

**Foundation first.** Things that change parser, schema, store, or search scoring come before things that wire existing internals into commands. Foundation layers are hard to redo later — if you skip them, future phases need reindexing, schema migrations, or architectural rework. Wiring is easy anytime.

---

## Phase 1: Foundation

*Schema, parser, search — hard to redo later.*

### 1a. Type extraction + schema v11 + `cqs deps` — DONE

Parser type extraction across 7 languages (PR #440). Schema v11 with `type_edges` table, store methods, `cqs deps` CLI + batch mode (PR #442).

**What shipped:**
- `parse_file_relationships()` returns `(Vec<FunctionCalls>, Vec<ChunkTypeRefs>)` from single tree-sitter parse
- `type_edges` table: source_chunk_id → target_type_name with edge_kind + line_number, FK CASCADE to chunks
- Store: `upsert_type_edges`, `get_type_users`, `get_types_used_by`, batch variants, stats, graph, shared_type_users, prune
- CLI: `cqs deps <type>` (forward) / `cqs deps --reverse <function>` (reverse)
- Batch: `deps` command, pipeable (forward mode)
- Stats: `type_graph` section in `cqs stats`

### 1b. Type integration into existing commands

Wire type_edges into commands that currently only walk call edges.

- `cqs related` — upgrade `shared_types` from signature LIKE-matching (`search_chunks_by_signatures_batch`) to `type_edges` traversal. Far more accurate.
- `cqs impact` — `--include-types` flag: also trace type-level edges. BFS currently only walks call edges; needs unified graph traversal over call + type edges, or two-pass approach. Non-trivial change to `bfs.rs`.
- `cqs dead` — type-reference awareness: struct with 0 type_edges + 0 callers = more confidently dead.
- Unified BFS over call + type graph for downstream consumers.

**Estimated scope:** ~300-400 lines across `related.rs`, `impact/bfs.rs`, `dead` logic.

### 1c. Note-boosted search ranking — DONE

Notes now influence code search ranking. In `search_filtered()` and `search_by_candidate_ids()`, after scoring each chunk, notes whose mentions match the chunk's file path or name apply a multiplicative boost: `adjusted_score = base_score * (1.0 + sentiment * 0.15)`. Multiple matching notes: strongest absolute sentiment wins (preserving sign).

**What shipped:**
- `note_boost()` helper in `src/search.rs` — computes per-chunk boost from note mentions
- Wired into both brute-force (`search_filtered`) and HNSW-guided (`search_by_candidate_ids`) paths
- Notes loaded once per search via `list_notes_summaries()` (cheap, no embeddings)
- 7 unit tests for boost logic (no match, file match, name match, strongest-wins)
- ~40 lines added. No schema change.

### 1d. Embedding model evaluation

Benchmark E5-base-v2 against CodeSage, UniXcoder, Nomic Code on the existing eval harness. Quantify the retrieval quality gap. If another model is significantly better on code, upgrade — this changes embedding dim and requires full reindex.

This is research, not code. Output: evaluation report with precision@K metrics.

---

## Phase 2: Features

*New commands + batch wiring. All use Phase 1 foundations.*

### 2a. Batch completeness

*Close the gaps that force agents out of batch mode.*

#### What's missing and why

| Command | Blocker | Fix |
|---------|---------|-----|
| `scout` | Nothing — BatchContext already has Store + Embedder + root | Add `BatchCmd::Scout` variant + dispatch handler |
| `where` | Nothing — same as scout | Add `BatchCmd::Where` variant + dispatch handler |
| `notes list` | CLI reads `docs/notes.toml` directly, not Store | Use `store.list_notes_summaries()` instead — consistent, already indexed. **Caveat:** Store data may lag if notes.toml edited but not re-indexed; add mtime freshness check. |
| `read` | Needs filesystem access + audit mode + notes injection | Add audit mode flag to BatchContext. File read via `root` for path resolution. Cache parsed notes. |
| `stale` | Needs `enumerate_files()` which takes `&Parser` | `enumerate_files()` only uses `parser.supported_extensions()` → `REGISTRY.supported_extensions()`. Refactor to accept extensions slice instead of Parser. Add lazy file set to BatchContext. |
| `health` | Same as stale — needs file set for staleness check | Same fix: lazy `OnceLock<HashSet<PathBuf>>` for file set. Shares refactored `enumerate_files()`. |

#### Architecture changes

```
BatchContext {
    store: Store,                              // existing
    embedder: OnceLock<Embedder>,              // existing
    hnsw: OnceLock<Option<Box<dyn VectorIndex>>>, // existing
    refs: RefCell<HashMap<String, ReferenceIndex>>, // existing
    root: PathBuf,                             // existing
    cqs_dir: PathBuf,                          // existing
+   file_set: OnceLock<HashSet<PathBuf>>,      // new: lazy, for stale/health
+   audit_mode: OnceLock<bool>,                // new: lazy, check .audit-mode file once
+   notes_cache: OnceLock<Vec<NoteEntry>>,     // new: lazy, parse docs/notes.toml once
}
```

#### New batch commands

| Command | Pipeable? | Output format |
|---------|-----------|---------------|
| `scout <query>` | Yes — outputs function names in file_groups.chunks | `ScoutResult` JSON |
| `where <description>` | No — outputs file paths, not function names | `PlacementResult` JSON |
| `read <path> [--focus <fn>]` | No — outputs file content | `{file, content, notes_injected}` |
| `stale` | No | `StaleReport` JSON |
| `health` | No | `HealthReport` JSON |
| `notes [--warnings] [--patterns]` | No | `[{text, sentiment, mentions}]` |

Pipeline additions: `scout` added to PIPEABLE_COMMANDS. `extract_names()` must walk nested `file_groups[].chunks[].name` (current implementation only checks top-level array fields — needs recursive or special-case handling). Only `modify_target` role chunks should be extracted for piping (not dependencies or test-to-update).

Note: `enumerate_files()` currently requires `&Parser` but only calls `parser.supported_extensions()` which delegates to `REGISTRY.supported_extensions()`. Refactor signature to accept `&[&str]` extensions slice — decouples from Parser, makes BatchContext lightweight.

**Estimated scope:** ~250 lines in `batch.rs` (6 new dispatch handlers + 3 new BatchContext fields). No new files, no new dependencies. ~10 integration tests.

### 2b. `cqs onboard "concept"` — guided codebase tour

**What it does:** Given a concept, produces an ordered reading list: entry point → call chain → key types → tests. One command replaces 10 minutes of manual exploration.

**Architecture:**
1. `scout(query)` for initial relevant code (reuse existing)
2. Pick highest-scored modify_target as entry point
3. BFS expansion from entry point via `bfs_expand()` + `fetch_and_assemble()` (NOT `gather()` — gather re-searches, we already have the entry point)
4. For each gathered chunk, fetch test mapping via `find_affected_tests()`
5. Order: entry point → callees (depth-first) → callers → tests
6. Token-budget the ordered list

**Reuses:** scout, gather's internal `bfs_expand()` + `fetch_and_assemble()`, impact/test-finding, token_pack. New: ordering logic + OnboardResult type.

**Prerequisite:** `bfs_expand()` and `fetch_and_assemble()` in `gather.rs` are currently private (`fn`, not `pub fn`). Must be made `pub(crate)` for reuse by onboard and later by `cqs task` (Phase 3).

**New files:** `src/onboard.rs` (~150 lines), `src/cli/commands/onboard.rs` (~80 lines).

### 2c. Auto-stale notes

Detect when notes reference deleted/renamed functions. `notes list` gains a `--check` flag that verifies each mention still exists in the index via `store.search_by_names_batch()`. Stale mentions flagged in output.

`cqs suggest` extended to detect stale notes as a pattern category.

~100 lines.

### 2d. `cqs drift` — semantic change detection

**What it does:** Compare embeddings of same-named functions across two snapshots. Surface functions where embedding distance exceeds threshold.

**Algorithm:**
1. For each function in current index, find matching name in reference index
2. Retrieve embeddings from both stores via `get_chunk_with_embedding()`
3. Compute `full_cosine_similarity()` (already exists in `src/math.rs`)
4. If similarity < threshold (default 0.95), flag as drifted
5. Sort by drift magnitude (most changed first)

**Output:** `{drifted: [{name, file, similarity, delta}], threshold, reference}`

**Prerequisite:** Reference must be a snapshot of the same codebase at an earlier point (`cqs ref add v1.0 .`), not an external library. Drift compares same-named functions across snapshots — external references have different function sets.

~200 lines. New file `src/drift.rs`, CLI in `src/cli/commands/drift.rs`.

### 2e. `cqs patterns` — convention extraction

Analyze codebase for recurring patterns: error handling style, naming conventions, import patterns, test organization. Uses `where_to_add.rs`'s `LocalPatterns` extraction across all indexed files instead of just search results.

~300 lines. Deferred to last in phase — largest effort, least agent impact.

---

## Phase 3: Moonshot

*Orchestrates everything — benefits from all prior phases.*

### `cqs task "description"` — single-call implementation brief

**What it does:** Given a task description, returns everything an agent needs: relevant code, impact analysis, placement suggestions, test requirements, risk assessment, relevant notes — in one token-budgeted response.

**Architecture:**

```
cqs task "add authentication middleware" --tokens 8000 --json

┌─────────────────────────────────────────────┐
│ 1. Shared resource loading (once)           │
│    Store, Embedder, embed_query(task),       │
│    get_call_graph(), find_test_chunks()      │
├─────────────────────────────────────────────┤
│ 2. Scout phase — relevant code + metadata   │
│    scout_with_graph() — pre-loaded graph    │
│    Output: ScoutResult                      │
├─────────────────────────────────────────────┤
│ 3. Gather phase — BFS from modify targets   │
│    bfs_expand() + fetch_and_assemble()      │
│    Seeded from scout, not fresh search      │
│    Output: GatherResult                     │
├─────────────────────────────────────────────┤
│ 4. Impact phase — what breaks              │
│    analyze_impact_with_graph() for targets  │
│    Output: Vec<ImpactResult>                │
├─────────────────────────────────────────────┤
│ 5. Where phase — placement suggestion       │
│    suggest_placement()                      │
│    Output: PlacementResult                  │
├─────────────────────────────────────────────┤
│ 6. Test-map phase — tests to run/write      │
│    find_affected_tests() with pre-loaded    │
│    Output: test names + files               │
├─────────────────────────────────────────────┤
│ 7. Adaptive token budgeting                 │
│    Per-section budget with waterfall        │
│    Output: unified JSON with token_count    │
└─────────────────────────────────────────────┘
```

**Key insight:** Today an agent calling scout + gather + impact separately loads the call graph 3 times and test chunks 2 times. Adding where adds another embedding query. `cqs task` loads everything once.

**What exists vs what's new:**

| Component | Exists? | Reuse | New work |
|-----------|---------|-------|----------|
| Semantic search | Yes | `search_filtered()` | None |
| Scout grouping | Yes | `scout()` | Need `scout_with_graph()` variant accepting pre-loaded `&CallGraph` + `&[ChunkSummary]` |
| Gather BFS | Yes | `bfs_expand()` + `fetch_and_assemble()` | Seed from scout targets, not fresh search. **Both are currently private** in `gather.rs` — make `pub(crate)` (same prereq as Phase 2b). |
| Impact analysis | Yes | `analyze_impact()` | Need `analyze_impact_with_graph()` — `compute_hints_with_graph()` already accepts pre-loaded graph |
| Where-to-add | Yes | `suggest_placement()` | None — already standalone |
| Test mapping | Yes | `find_affected_tests()` | Accepts pre-loaded `&CallGraph` but still loads test_chunks internally. Need variant accepting pre-loaded test chunks, or cache in orchestrator. |
| Notes lookup | Yes | `find_relevant_notes()` | None |
| Token packing | Yes | `token_pack()` | None — already generic |
| Adaptive budgeting | **No** | — | New: section-aware budget allocator |
| Unified output | **No** | — | New: `TaskResult` combining all sections |
| CLI command | **No** | — | New: `cmd_task()` |

**Adaptive token budgeting:**

```rust
struct BudgetAllocation {
    scout_pct: f32,     // 15% — overview/metadata (cheap)
    gather_pct: f32,    // 50% — code content (most tokens)
    impact_pct: f32,    // 15% — callers/tests
    where_pct: f32,     // 10% — placement suggestions
    notes_pct: f32,     // 10% — relevant notes
}
```

Algorithm: compute per-section budget → pack each section independently with `token_pack()` → redistribute unused budget to next section (waterfall) → always include at least 1 item from each non-empty section.

**Estimated scope:** ~500-650 lines new code:
- `src/task.rs` (~200-250 lines) — orchestrator, TaskResult, BudgetAllocation, waterfall redistribution, error handling
- `src/cli/commands/task.rs` (~100 lines) — CLI wiring, JSON output
- Refactoring for shared resources (~80 lines):
  - `gather.rs`: make `bfs_expand()` + `fetch_and_assemble()` pub(crate) (already needed by Phase 2b)
  - `scout.rs`: add `scout_with_graph()` accepting pre-loaded `&CallGraph` + `&[ChunkSummary]`
  - `impact/analysis.rs`: add `find_affected_tests_with_chunks()` accepting pre-loaded test chunks
- Tests (~100-150 lines)
- CLI registration, lib.rs re-exports, docs

### `cqs verify <diff>` — post-implementation validation

Given a diff, check against the index:
- Did the change update all affected callers?
- Are there missing test updates for changed functions?
- Does the new code follow local conventions (via where_to_add patterns)?
- Any new dead code introduced?

Builds on `impact-diff`, `dead`, and `where_to_add` pattern extraction. ~300 lines.

---

## Phase 4: Reach

*Ongoing. Breadth after depth.*

| Item | What | Why |
|------|------|-----|
| C# language support | tree-sitter-c-sharp | Biggest missing language by market |
| Pre-built release binaries | GitHub Actions CI/CD | Users shouldn't compile from source |
| `cqs ref install <name>` | Pre-built reference packages | One command to add tokio/express/django |

---

## Dependency Graph

```
Phase 1a (Type extraction + schema + deps CLI) — DONE
  └── foundation for 1b, enriches Phase 3

Phase 1b (Type integration)
  └── depends on 1a (needs type_edges table)

Phase 1c (Note-boosted ranking)
  └── independent — changes search scoring
  └── improves Phase 3 scout seeds

Phase 1d (Embedding eval)
  └── independent — pure research
  └── if model changes, full reindex required

Phase 2a (Batch completeness)
  └── no dependencies, uses existing commands

Phase 2b (Onboard)
  └── no new dependencies, uses existing scout + gather + impact
  └── prereq: make gather internals pub(crate) (also needed by Phase 3)

Phase 2c (Auto-stale notes)
  └── independent

Phase 2d (Drift)
  └── independent — uses existing reference system + cosine similarity

Phase 2e (Patterns)
  └── independent — uses existing LocalPatterns

Phase 3 (Task + Verify)
  └── benefits from Phase 1b (type deps enrich impact analysis)
  └── benefits from Phase 1c (note-boosted ranking improves scout seeds)
  └── benefits from Phase 2a (batch completeness for testing)
  └── can start without them — just better with them
```

---

## Progress Tracker

| Phase | Item | Status | Sessions |
|-------|------|--------|----------|
| 1a | Type extraction + schema v11 + deps | **Done** (PRs #440, #442) | 3 |
| 1b | Type integration (related, impact, dead) | **Done** (PR #447) | 1 |
| 1c | Note-boosted search ranking | **Done** | 0.5 |
| 1d | Embedding model eval | Not started | 1 (research) |
| 2a | Batch: scout, where, read, stale, health, notes | Not started | 1-2 |
| 2b | `cqs onboard` | **Done** (PR #457) | 1 |
| 2c | Auto-stale notes | **Done** | 0.5 |
| 2d | `cqs drift` | **Done** | 0.5 |
| 2e | `cqs patterns` | Not started | 1-2 |
| 3 | `cqs task` | Not started | 2-3 |
| 3 | `cqs verify` | Not started | 1-2 |
| 4 | C#, binaries, ref install | Not started | ongoing |

---

## What This Means for Agents

| State | Tool calls per task | Context efficiency |
|-------|--------------------|--------------------|
| Today | 3-8 | ~40% of window on exploration |
| After Phase 1 | Type + call graph = complete structural understanding | Notes shape search ranking |
| After Phase 2 | Batch covers all workflows, onboard eliminates ramp-up | Pipeline chains cover most workflows |
| After Phase 3 | **1 call = complete implementation brief** | ~90% of window on actual work |

---

*Architecture details derived from: `src/cli/batch.rs` (BatchContext, dispatch, pipeline), `src/store/` (Store, schema v11, search pipeline), `src/parser/` (chunk extraction, call extraction, type extraction), `src/scout.rs`, `src/gather.rs`, `src/impact/`, `src/where_to_add.rs`, `src/search.rs`, `src/note.rs`, `src/math.rs`, `src/embedder.rs`.*

*Created 2026-02-14. Reorganized 2026-02-15. See `ROADMAP.md` for current sprint work.*
