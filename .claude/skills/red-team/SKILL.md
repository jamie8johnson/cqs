# Red Team Audit

Adversarial security audit — attacker mindset, PoC-required, end-to-end attack chains.

## Arguments

- (none) — runs all 4 categories

## Process

### Setup

1. **Read `SECURITY.md`** — extract current threat model, trust boundaries, stated protections
2. **Read prior triage files** (`docs/audit-triage-v*.md`) — skip anything already triaged
3. **Read current findings** (`docs/audit-findings.md`) — skip anything already reported
4. **Enable audit mode**: `cqs audit-mode on --expires 4h -q`

### Execution

5. **Create team**: `red-team`
6. **Spawn 4 opus agents** (one per category below)
7. **Each agent prompt must include**:
   - The threat model section (below) — calibrated from SECURITY.md
   - Their category scope, goal, key question, targets, and files
   - Existing protections to verify (not re-report)
   - Prior findings to skip
   - The cqs tools block
   - The finding format and rules (below)
   - Instruction to append findings to `docs/audit-findings.md` under `## Red Team`

### Cleanup

8. **Shutdown team** after all agents complete
9. **Incorporate findings** into main triage (`docs/audit-triage.md`)
10. **Disable audit mode**: `cqs audit-mode off -q`

## Threat Model Calibration

Read `SECURITY.md` at the start of each run. Extract:

**Trust boundaries:**

| Boundary | Trust Level |
|---|---|
| Local user | Trusted — runs cqs, controls it |
| Project files | Trusted — user's code, indexed by choice |

**Stated protections** (verify these, don't re-report as missing):
1. Path traversal: `dunce::canonicalize()` + `starts_with()` — cannot read outside project root
2. FTS injection: `normalize_for_fts()` strips FTS5 operators before MATCH
3. Database corruption: `PRAGMA quick_check` on every open
4. Reference config trust: warnings logged on override
5. Symlink resolution before path check
6. Zip-slip containment in convert module
7. Parameterized SQL via sqlx
8. `validate_finite_f32()` for NaN/Infinity
9. `.clamp(1, 100)` on limit params
10. 1MB batch line limit, 10MB file read limit

**Explicitly out of scope** (NOT findings):
- Local user reading their own index.db (trusted user)
- Local privilege escalation (documented non-goal)
- TOCTOU on symlinks (documented, accepted trade-off)
- Unencrypted index.db (documented, by design)
- Side-channel attacks (documented non-goal)

## Categories

### RT-INJ: Input Injection & Command Injection

**Goal:** Craft inputs that escape their intended context — bypass sanitization, corrupt structured data, or trigger unintended execution.

**Key question:** Can AI agent input (the primary user) break out of expected boundaries?

**Targets:**
- Batch pipeline syntax (`|` chaining in `parse_pipeline`) — pipeline boundary bypass
- FTS5 query construction — `normalize_for_fts()` bypass on ANY code path reaching MATCH
- Notes text → TOML serialization — metacharacter corruption (`"""`, `[[`, `\n[note]`)
- `--ref` names used in SQL and filesystem paths — validation of `/`, `..`, `\0`
- `shell-words::split` in batch — unbalanced quotes, null bytes
- `CQS_PDF_SCRIPT` env var — any validation of script path before spawn?
- `--path` glob patterns via `globset` — ReDoS with pathological patterns

**Files:** `src/cli/batch/mod.rs`, `src/notes.rs`, `src/search.rs`, `src/project.rs`, `src/convert.rs`, `src/cli/batch/handlers.rs`

### RT-FS: Filesystem Boundary Violations

**Goal:** Bypass "cannot read files outside project root" via path traversal, symlink tricks, or indirect access.

**Key question:** Can any code path reach a file outside the project root despite canonicalize+starts_with?

**NOT in scope:** TOCTOU, index.db permissions, symlink-outside (already blocked — verify, don't re-report).

**Targets:**
- Path traversal protection completeness — is canonicalize+starts_with on ALL file-reading paths?
- `cqs convert --output <path>` — output directory escape
- Reference index path construction — ref name with `../` escaping refs directory
- Function name as path component — path separators in function names used in fs ops
- Index entries as indirect reads — stale paths serving wrong content

**Files:** `src/cli/commands/read.rs`, `src/project.rs`, `src/convert.rs`, `src/store/mod.rs`, `src/cli/batch/handlers.rs`

### RT-RES: Adversarial Robustness

**Goal:** Crash, hang, OOM, or panic with valid-syntax but adversarial inputs. Check for unprotected edge cases.

**Key question:** Can automated queries cause failure? Are all code paths resilient to edge cases?

**Two modes:** Outside-in (attack chains for resource exhaustion) + Inside-out (bare unwrap, empty/huge/unicode/malformed inputs).

**Targets — Attack Chains:**
- Pipeline fan-out bomb (`search | callers | callers | callers`)
- BFS depth bomb (unbounded gather traversal)
- Token budget extremes (0, u64::MAX)
- Graph cycle handling (mutual recursion → infinite traversal)
- Query length (100KB → tokenizer/embedding OOM)
- Batch session memory growth (unbounded caches)
- Watch event storm (rapid file creation)

**Targets — Edge Cases:**
- Bare `unwrap()`/`expect()` in non-test code
- Empty/zero inputs (empty query, `--limit 0`, empty graph)
- Unicode (multi-byte function names, emoji in queries)
- Malformed data (corrupted HNSW, invalid embeddings, bad notes.toml)
- Integer overflow (extreme caller_count, score overflow)

**Files:** `src/cli/batch/mod.rs`, `src/gather.rs`, `src/impact/mod.rs`, `src/cli/commands/task.rs`, `src/hnsw/mod.rs`, `src/watcher.rs`, `src/embedder.rs`, `src/task.rs`, `src/scout.rs`

### RT-DATA: Silent Data Corruption

**Goal:** Cause incorrect search results, corrupt index, or inconsistent output — without errors.

**Key question:** Can normal-looking operations leave the system in an inconsistent state?

**Targets:**
- HNSW/SQLite desync (orphaned entries after delete/gc)
- Concurrent index+watch (file locking vs partial writes)
- notes.toml write race (simultaneous adds)
- Embedding dimension mismatch (different model cached)
- Schema migration atomicity (crash mid-migration)
- Batch OnceLock stale cache (index mutation invalidation)
- Score ordering inconsistency (NaN breaking sort)

**Files:** `src/store/mod.rs`, `src/hnsw/mod.rs`, `src/notes.rs`, `src/indexer.rs`, `src/cli/batch/mod.rs`, `src/embedder.rs`

## Finding Format

```
#### RT-{CAT}-N: {Title}
- **Severity:** critical | high | medium | low
- **Location:** `file:line`
- **Attack vector:** Concrete input or sequence that triggers this
- **PoC:** Trace the code path with the attack input, showing each function call and why the input reaches the vulnerable point. Critical/high: step-by-step traceable. Medium/low: reasoning with code references.
- **Impact:** What state is corrupted, data exposed, or resource exhausted
- **Suggested mitigation:** ...
```

## Rules

- **Every finding must (a) bypass a stated protection, or (b) identify an unprotected gap.** "Missing validation" alone is not a finding — show the attack.
- "PoC" = trace the code path with a concrete input. Agents cannot execute live attacks.
- Report-only — do NOT fix anything.
- Verify existing protections cover all code paths (especially new code).
- Focus on new code but follow attack chains into old code.
- **Not a finding:** anything in the "explicitly out of scope" list.

## cqs Tools for Agents

Include this block in every agent prompt:

```
## cqs Available (via Bash)

- `cqs "query" --json` — semantic search (finds code by concept, not text)
- `cqs "name" --name-only --json` — definition lookup by function/type name
- `cqs read <path>` — file with notes injected as comments. Use instead of raw Read.
- `cqs read --focus <fn>` — function + type dependencies only. Saves tokens.
- `cqs dead --json` — find dead code (uncalled functions)
- `cqs callers <fn> --json` — who calls this function
- `cqs callees <fn> --json` — what this function calls
- `cqs explain <fn> --json` — function card (signature, callers, callees, similar)
- `cqs similar <fn> --json` — find duplicate/similar code
- `cqs deps <type> --json` — type dependencies: who uses this type
- `cqs context <file> --json` — module overview (chunks, callers, callees)
- `cqs gather "query" --json` — smart context: seed search + call graph BFS
- `cqs scout "task" --json` — pre-investigation: search + callers/tests + staleness + notes
- `cqs task "description" --json` — implementation brief: scout + gather + impact + placement
- `cqs impact <fn> --json` — what breaks if you change this
- `cqs test-map <fn> --json` — tests that exercise this function
- `cqs trace <source> <target> --json` — shortest call path between two functions
- `cqs review --json` — diff review: impact + notes + risk scoring
- `cqs health --json` — codebase quality snapshot: dead code, staleness, hotspots
- `cqs stale --json` — check index freshness (files changed since last index)
```
