# New Commands v1.11.0 — Implementation Plan

6 easy commands, one session. Each builds on existing infrastructure.

## 1. Query Expansion (synonym map)

**Files:** `src/search/query.rs` (or new `src/search/synonyms.rs`)

**Approach:**
- Static `phf_map` of synonyms: `"auth" → ["authentication", "authorize", "credential"]`, etc.
- In `search_filtered`, before the FTS query, expand each query token against the map
- Expansion is OR-based: `"auth middleware"` → FTS query includes `auth OR authentication OR authorize`
- Only expand single tokens, not phrases
- Start with ~30 common programming abbreviations

**Key synonyms:**
```
auth → authentication, authorize, credential, login
config → configuration, settings, preferences
err → error, failure, exception
fn → function, method, func
init → initialize, setup, constructor
parse → parsing, deserialize, decode
req → request, query
res → response, result, reply
fmt → format, formatting
db → database, storage, store
```

**Tests:** Verify expansion happens, verify no expansion for unknown terms, verify OR semantics in FTS.

## 2. `cqs affected`

**Files:** `src/cli/commands/affected.rs` (new), `src/cli/definitions.rs`, `src/cli/dispatch.rs`

**Approach:**
- Get current `git diff HEAD` (or `--base <ref>`)
- Parse with `parse_unified_diff` (existing)
- Find changed functions via `find_changed_functions` (existing)
- For each changed function: `impact()` → callers, `test_map()` → tests
- Output: changed functions table + impacted callers + test coverage summary + overall risk score

**Output format:**
```
Changed: 3 functions in 2 files
  src/math.rs: full_cosine_similarity (46 callers, 12 tests)
  src/math.rs: cosine_similarity (23 callers, 8 tests)
  src/search/query.rs: search_filtered (5 callers, 15 tests)

Impact: 74 callers, 35 tests
Risk: HIGH (>40 callers affected)

Run: cargo test test_cosine test_search test_scoring
```

**Tests:** Mock diff → verify output includes callers and tests.

## 3. `cqs brief <file>`

**Files:** `src/cli/commands/brief.rs` (new), `src/cli/definitions.rs`, `src/cli/dispatch.rs`

**Approach:**
- Load chunks for file from store
- For each chunk: name, chunk_type, count callers (from call graph), check test coverage (test_map)
- One line per function, compact format
- Same data the pre-Edit hook uses, but richer (adds test coverage)

**Output format:**
```
src/math.rs — 8 functions, 46 external callers
  fn cosine_similarity        23 callers  8 tests  ✓
  fn full_cosine_similarity   46 callers  12 tests ✓
  fn normalize_l2              3 callers  2 tests  ✓
  fn make_embedding            0 callers  0 tests  ✗ (test-only)
```

**Tests:** Verify output format, verify caller/test counts match impact/test-map.

## 4. `cqs neighbors <fn>`

**Files:** `src/cli/commands/neighbors.rs` (new), `src/cli/definitions.rs`, `src/cli/dispatch.rs`

**Approach:**
- Load the target function's embedding from store
- Brute-force cosine against all chunk embeddings (same path as `find_contrastive_neighbors`)
- Return top-K (default 5) with similarity scores
- `--json` for programmatic use

**Output format:**
```
Nearest neighbors of full_cosine_similarity:
  0.934  cosine_similarity (src/math.rs)
  0.821  dot_product (src/math.rs)
  0.756  normalize_l2 (src/math.rs)
  0.623  search_by_embedding (src/store/search.rs)
  0.601  score_candidate (src/search/scoring/candidate.rs)
```

**Tests:** Verify self-similarity = 1.0, verify ordering is descending.

## 5. `cqs doctor --fix`

**Files:** `src/cli/commands/doctor.rs` (modify existing)

**Approach:**
- Doctor already returns a list of issues with categories
- Add `--fix` flag
- For each issue type, map to a fix:
  - Stale index → `cqs index`
  - Schema mismatch → `cqs migrate` (existing)
  - Orphan chunks → `cqs gc`
  - Missing HNSW → `cqs index --force`
  - Model mismatch → warn (can't auto-fix, needs user decision)
- Run fixes in order, report what was done

**Tests:** Mock a stale index scenario, verify `--fix` runs the right remedy.

## 6. `cqs train-pairs`

**Files:** `src/cli/commands/train_pairs.rs` (new), `src/cli/definitions.rs`, `src/cli/dispatch.rs`

**Approach:**
- Extract (NL description, code) pairs from the current index
- `--contrastive`: add "Unlike X and Y" prefixes using call graph callees
- `--test-queries`: placeholder for future test-derived query extraction
- `--output <path>`: write JSONL
- `--limit N`: cap output size
- `--language <lang>`: filter by language

**Output:** JSONL with `query`, `positive`, `language`, `function_name`, `file`, `callers`, `callees` fields (same format as training data).

**Tests:** Verify output format, verify contrastive prefix generation, verify language filter.

## Implementation Order

1. Query expansion (touches search hot path — test thoroughly)
2. `cqs brief` (simplest new command, validates the pattern)
3. `cqs affected` (combines existing commands)
4. `cqs neighbors` (new brute-force path, needs perf attention)
5. `cqs doctor --fix` (modify existing command)
6. `cqs train-pairs` (new command, research tool)

## Quality Requirements (per CLAUDE.md rules)

Every new public function MUST have:
1. `tracing::info_span!` at entry (or `debug_span!` for internal helpers)
2. `tracing::warn!` on error fallback — never bare `.unwrap_or_default()`
3. `?` propagation where the caller returns `Result`
4. Tests for: happy path, empty input, error path, edge cases (zero results, NaN scores, unicode names)

**Specifically:**
- Query expansion: test empty query, query with no synonyms, query with all synonyms
- `affected`: test empty diff, diff with no functions, diff with deleted files
- `brief`: test empty file, file not in index, file with zero callers
- `neighbors`: test function not found, zero-vector embedding, self-similarity
- `doctor --fix`: test each issue type individually, test when no issues exist
- `train-pairs`: test empty index, test `--contrastive` with no call graph, test `--limit 0`

## Ecosystem Updates Per Command

For each new command:
1. CLI definition in `definitions.rs`
2. Dispatch in `dispatch.rs`
3. `--json` support
4. `tracing::info_span!` at entry
5. Proper error handling (Result propagation, warn on fallback)
6. CLAUDE.md entry
7. `.claude/skills/cqs/SKILL.md` entry
8. `.claude/skills/cqs-bootstrap/SKILL.md` portable skills list
9. CHANGELOG entry
10. Tests (happy + error + edge cases)
