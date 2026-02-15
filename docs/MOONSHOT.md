# Moonshot Plan: Maximal Agent Power

## Context

cqs has 35 CLI commands, 14 in batch mode, pipeline syntax, token budgeting, and a reference system. Agents currently call 3-8 cqs commands per task and manually assemble understanding. The moonshot: **one query, complete implementation context, zero iteration**.

---

## Where We Are

**35 commands.** 14 available in batch mode. Pipeline syntax chains them. Token budgeting on 7 commands. Scout, gather, and impact exist independently but share nothing — each loads its own call graph, test chunks, and staleness data. Where is search-only (no call graph).

**Parser extracts:** Functions, methods, classes, structs, enums, traits, interfaces, constants (+ markdown sections). Call sites (callee name + line). Signatures as raw strings. **Type references** (Phase 2b Step 1): parameter types, return types, field types, trait bounds, generic parameters via tree-sitter queries across 7 languages (Rust, Python, TypeScript, Go, Java, C, SQL).

**Schema v11:** `chunks`, `calls`, `function_calls`, `notes`, `type_edges` tables. Type-level dependency tracking via `type_edges` (Phase 2b Step 2): source_chunk_id → target_type_name with edge_kind classification (Param, Return, Field, Impl, Bound, Alias). `cqs deps` command for forward/reverse queries.

**Search pipeline:** FTS5 keyword → semantic embedding → RRF fusion → HNSW acceleration → unified (code + notes). Notes are separate results merged by score — they don't influence code ranking.

**Token budgeting:** Flat greedy knapsack by score. No information-type priority. Exception: `explain` hardcodes target-first, similar-second.

**Embeddings:** 769-dim (768 E5-base-v2 + 1 sentiment), stored as BLOB in SQLite. `full_cosine_similarity()` exists for cross-store comparison. Reference system stores separate Store+HNSW per reference.

---

## Phase 1: Batch Completeness

*1-2 sessions. Close the gaps that force agents out of batch mode.*

### What's missing and why

| Command | Blocker | Fix |
|---------|---------|-----|
| `scout` | Nothing — BatchContext already has Store + Embedder + root | Add `BatchCmd::Scout` variant + dispatch handler |
| `where` | Nothing — same as scout | Add `BatchCmd::Where` variant + dispatch handler |
| `notes list` | CLI reads `docs/notes.toml` directly, not Store | Use `store.list_notes_summaries()` instead — consistent, already indexed. **Caveat:** Store data may lag if notes.toml edited but not re-indexed; add mtime freshness check. |
| `read` | Needs filesystem access + audit mode + notes injection | Add audit mode flag to BatchContext. File read via `root` for path resolution. Cache parsed notes. |
| `stale` | Needs `enumerate_files()` which takes `&Parser` | `enumerate_files()` only uses `parser.supported_extensions()` → `REGISTRY.supported_extensions()`. Refactor to accept extensions slice instead of Parser. Add lazy file set to BatchContext. |
| `health` | Same as stale — needs file set for staleness check | Same fix: lazy `OnceLock<HashSet<PathBuf>>` for file set. Shares refactored `enumerate_files()`. |

### Architecture changes

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

### New batch commands

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

### Estimated scope

~250 lines in `batch.rs` (6 new dispatch handlers + 3 new BatchContext fields). No new files, no new dependencies. ~10 integration tests.

---

## Phase 2: Deeper Understanding

*3-5 sessions. Fill the structural blind spots.*

### 2a. `cqs onboard "concept"` — guided codebase tour

**What it does:** Given a concept, produces an ordered reading list: entry point → call chain → key types → tests. One command replaces 10 minutes of manual exploration.

**Architecture:**
1. `scout(query)` for initial relevant code (reuse existing)
2. Pick highest-scored modify_target as entry point
3. BFS expansion from entry point via `bfs_expand()` + `fetch_and_assemble()` (NOT `gather()` — gather re-searches, we already have the entry point)
4. For each gathered chunk, fetch test mapping via `find_affected_tests()`
5. Order: entry point → callees (depth-first) → callers → tests
6. Token-budget the ordered list

**Reuses:** scout, gather's internal `bfs_expand()` + `fetch_and_assemble()`, impact/test-finding, token_pack. New: ordering logic + OnboardResult type.

**Prerequisite:** `bfs_expand()` and `fetch_and_assemble()` in `gather.rs` are currently private (`fn`, not `pub fn`). Must be made `pub(crate)` for reuse by onboard and later by `cqs task` (Phase 4).

**New files:** `src/onboard.rs` (~150 lines), `src/cli/commands/onboard.rs` (~80 lines).

### 2b. `cqs deps <type>` — type-level dependency graph

**This is the big one.** Requires changes at every layer.

#### Parser layer (per-language tree-sitter queries)

New query set per language — `type_queries` alongside existing `chunk_queries` and `call_queries`:

| Language | Type references to capture |
|----------|--------------------------|
| Rust | `fn(x: Type)`, `-> Type`, `struct { f: Type }`, `impl Trait for Type`, `T: Bound`, `type Alias = Type` |
| Python | Type hints: `def f(x: Type) -> Type`, `field: Type` (class body) |
| TypeScript | `function f(x: Type): Type`, `interface`, `extends`, `implements` |
| Go | `func f(x Type) Type`, `type X struct { f Type }` |
| Java | Extends, implements, parameter types, return types, field types |
| C | Function signatures, struct field types, typedef |
| SQL | Table references in queries (limited) |
| JavaScript | No static types — skip |
| Markdown | No types — skip |

**Output type:** `TypeReference { target_type_name: String, edge_kind: TypeEdgeKind, line_number: u32 }`

**TypeEdgeKind enum:** `Param`, `Return`, `Field`, `Impl`, `Bound`, `Alias`, `Extends`, `Import`

#### Schema v11

```sql
CREATE TABLE IF NOT EXISTS type_edges (
    source_chunk_id TEXT NOT NULL,
    target_type_name TEXT NOT NULL,
    edge_kind TEXT NOT NULL,
    line_number INTEGER,
    FOREIGN KEY (source_chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
);
CREATE INDEX idx_type_edges_target ON type_edges(target_type_name);
CREATE INDEX idx_type_edges_source ON type_edges(source_chunk_id);
```

Migration: v10 → v11. Framework exists in `src/store/migrations.rs` (commented template). Re-index required after migration (type edges need fresh parse).

#### Store methods

```
get_type_users(type_name) -> Vec<ChunkSummary>      // who uses this type
get_types_used_by(chunk_id) -> Vec<(String, EdgeKind)> // what types this chunk uses
get_type_graph() -> TypeGraph                         // full adjacency lists
upsert_type_edges_batch(chunk_id, Vec<TypeReference>) // index time
```

#### CLI

`cqs deps <type_name>` — lists all functions/methods that reference this type, grouped by edge kind.

`cqs deps --reverse <function>` — lists all types this function depends on.

JSON output for agent consumption. Add to batch mode. Add to PIPEABLE_COMMANDS (output has function names).

Integration with existing commands:
- `cqs impact <function>` gains `--include-types` flag: also traces type-level edges. **Note:** BFS currently only walks call edges; needs unified graph traversal over call + type edges, or two-pass approach. Non-trivial change to `bfs.rs`.
- `cqs dead` gains type-reference awareness: struct with 0 type_edges + 0 callers = more confidently dead
- `cqs related` `shared_types` upgrades from signature LIKE-matching (`search_chunks_by_signatures_batch`) to type_edges traversal — far more accurate

**Estimated scope:** ~800-1000 lines across parser queries (most effort), schema migration, store methods, CLI command. 7 language query files to author — each language has different AST structure for type references, requiring individual authoring and testing.

**All new commands should also be added to batch mode** with batch dispatch handlers, following Phase 1's pattern.

### 2c. Embedding model evaluation

Benchmark E5-base-v2 against CodeSage, UniXcoder, Nomic Code on the existing eval harness. Quantify the retrieval quality gap. If another model is significantly better on code, upgrade.

This is research, not code. Output: evaluation report with precision@K metrics.

---

## Phase 3: Semantic Memory

*2-3 sessions. The index accumulates intelligence over time.*

### 3a. Note-boosted search ranking

**Today:** Notes appear as separate results merged by score. A note saying "this module is fragile" doesn't boost code results near that module.

**Change:** In `search_filtered()` scoring loop, after scoring a code chunk, check if any note mentions match the chunk's file path or name. If match, apply multiplicative boost scaled by note sentiment.

**Where:** `src/search.rs`, in the scoring loop inside `search_filtered()`. Requires passing `list_notes_summaries()` results (cheap — no embeddings) into the search function. `path_matches_mention()` already exists in `src/note.rs`.

**Formula:** `adjusted_score = base_score * (1.0 + note_sentiment * NOTE_BOOST_FACTOR)` where `NOTE_BOOST_FACTOR = 0.15`. A note with sentiment -1 about a function reduces its ranking by 15%. Sentiment +1 boosts by 15%. **Multiple notes matching same chunk:** take strongest absolute sentiment (max of |sentiment|, preserving sign). This avoids averaging away strong signals.

~50 lines changed. No schema change.

### 3b. Auto-stale notes

Detect when notes reference deleted/renamed functions. `notes list` gains a `--check` flag that verifies each mention still exists in the index via `store.search_by_names_batch()`. Stale mentions flagged in output.

`cqs suggest` extended to detect stale notes as a pattern category.

~100 lines.

### 3c. `cqs drift` — semantic change detection

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

### 3d. `cqs patterns` — convention extraction

Analyze codebase for recurring patterns: error handling style, naming conventions, import patterns, test organization. Uses `where_to_add.rs`'s `LocalPatterns` extraction across all indexed files instead of just search results.

~300 lines. Deferred to last in phase — largest effort, least agent impact.

---

## Phase 4: One-Shot Task Context

*2-3 sessions. The moonshot.*

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
| Gather BFS | Yes | `bfs_expand()` + `fetch_and_assemble()` | Seed from scout targets, not fresh search. **Both are currently private** in `gather.rs` — make `pub(crate)` (same prereq as Phase 2a). |
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
  - `gather.rs`: make `bfs_expand()` + `fetch_and_assemble()` pub(crate) (already needed by Phase 2a)
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

## Phase 5: Reach

*Ongoing. Breadth after depth.*

| Item | What | Why |
|------|------|-----|
| C# language support | tree-sitter-c-sharp | Biggest missing language by market |
| Pre-built release binaries | GitHub Actions CI/CD | Users shouldn't compile from source |
| `cqs ref install <name>` | Pre-built reference packages | One command to add tokio/express/django |

---

## Dependency Graph

```
Phase 1 (Batch Completeness)
  └── no dependencies, can start immediately

Phase 2a (Onboard)
  └── depends on nothing new, uses existing scout + gather + impact

Phase 2b (Type Deps)
  └── independent of phases 1/2a
  └── schema v11 migration — must re-index after

Phase 2c (Embedding Eval)
  └── independent, pure research

Phase 3a (Note-boosted ranking)
  └── independent of all phases

Phase 3b (Auto-stale notes)
  └── depends on nothing new

Phase 3c (Drift)
  └── independent — uses existing reference system + cosine similarity

Phase 3d (Patterns)
  └── independent — uses existing LocalPatterns

Phase 4 (Task + Verify)
  └── benefits from Phase 1 (batch completeness for testing)
  └── benefits from Phase 2b (type deps enrich impact analysis)
  └── benefits from Phase 3a (note-boosted ranking improves scout seeds)
  └── can start without them — just better with them
```

**Recommended execution order:**
1. Phase 1 (quick wins, unblocks batch workflows)
2. Phase 2a (onboard — high agent value, moderate effort)
3. Phase 3a + 3b (note improvements — small, high leverage)
4. Phase 2b (type deps — big effort, foundational)
5. Phase 3c (drift — moderate, uses reference system)
6. Phase 4 (task — the moonshot, best after deps + notes land)
7. Phase 5 (reach — whenever)

---

## Progress Tracker

| Phase | Item | Status | Sessions |
|-------|------|--------|----------|
| 1 | Batch: scout, where, notes | Not started | 1 |
| 1 | Batch: read, stale, health | Not started | 1 |
| 2a | `cqs onboard` | Not started | 1-2 |
| 2b | Type deps: parser queries | Not started | 3-5 |
| 2b | Type deps: schema + store | Not started | 1 |
| 2b | Type deps: CLI + integration | Not started | 1 |
| 2c | Embedding model eval | Not started | 1 (research) |
| 3a | Note-boosted ranking | Not started | 0.5 |
| 3b | Auto-stale notes | Not started | 0.5 |
| 3c | `cqs drift` | Not started | 1 |
| 3d | `cqs patterns` | Not started | 1-2 |
| 4 | `cqs task` | Not started | 2-3 |
| 4 | `cqs verify` | Not started | 1-2 |
| 5 | C#, binaries, ref install | Not started | ongoing |

---

## What This Means for Agents

| State | Tool calls per task | Context efficiency |
|-------|--------------------|--------------------|
| Today | 3-8 | ~40% of window on exploration |
| After Phase 1 | 1 batch call replaces 3-5 CLIs | Pipeline chains cover most workflows |
| After Phase 2 | Type + call graph = complete structural understanding | Onboard eliminates ramp-up |
| After Phase 3 | Memory shapes retrieval, stale knowledge self-heals | Notes amplify relevant results |
| After Phase 4 | **1 call = complete implementation brief** | ~90% of window on actual work |

---

*Architecture details derived from: `src/cli/batch.rs` (BatchContext, dispatch, pipeline), `src/store/` (Store, schema v10, search pipeline), `src/parser/` (chunk extraction, call extraction, no type extraction), `src/scout.rs`, `src/gather.rs`, `src/impact/`, `src/where_to_add.rs`, `src/search.rs`, `src/note.rs`, `src/math.rs`, `src/embedder.rs`.*

*Created 2026-02-14. See `ROADMAP.md` for current sprint work.*
