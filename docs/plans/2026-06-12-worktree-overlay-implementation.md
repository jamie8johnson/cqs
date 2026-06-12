# Plan: Worktree Search Overlay — Result-Trust Program §3 (#1821)

Status: PROPOSED (design-time review pass — this doc IS the fable review the
program doc requested for the overlay's shadow semantics).
Parent design: `docs/plans/2026-06-12-edge-provenance-dead-verdicts-worktree-overlay.md` §3.
Lands AFTER: lane-callers-trust-order and lane-rank-provenance (§4).
Interaction notes in §10.

---

## 0. Design corrections to the program doc

This pass found four places where §3 as written is wrong or underspecified.
They are corrected throughout; flagged here so the deltas are reviewable.

1. **The `(origin, name)` shadow key is wrong.** Shadowing must be
   **origin-level**, not `(origin, name)`-level. The killer case: a worktree
   edit deletes function `foo` from `src/a.rs`. The origin exists in both
   stores; the name exists only in the parent. Under `(origin, name)`
   shadowing the parent's `foo` hit has no overlay counterpart and **survives
   the merge** — the agent is shown a function that does not exist in its
   checkout, with fresh confidence. Correct rule: *any parent hit whose
   origin is in the worktree delta set is masked, unconditionally*. The
   overlay store is the sole authority for changed origins. This also makes
   binary-rewritten and unparseable changed files correct for free (they mask
   even though they contribute zero overlay chunks). See §4.

2. **Diff base must be the parent's HEAD, not the merge-base.** The doc says
   "git diff --name-status <merge-base>". The parent index reflects the
   parent checkout (≈ parent HEAD). A file changed *on main after the fork
   point* but untouched in the worktree is absent from a merge-base diff —
   yet the worktree genuinely contains the older content, and the parent
   index would serve the newer one. Diffing the worktree's working tree
   against the **parent's current HEAD commit** captures both directions
   (lane edits and lane-behind-main divergence) in one invocation, and
   subsumes "plus uncommitted changes" (a `git diff <commit>` with no second
   ref diffs against the working tree). See §3.

3. **`:memory:` + the existing Store pool is a trap.** Every `Store` open
   path (`open_with_config_impl`, `src/store/mod.rs:1175`) builds an sqlx
   pool with `idle_timeout` 30s (`src/store/mod.rs:1263-1268`) and, on the
   ReadWrite path, `max_connections` up to 8 (`src/store/mod.rs:1008-1015`).
   With SQLite `:memory:`, **each pooled connection is a separate empty
   database**, and the idle reaper **destroys the database** when it closes
   the last connection. "Schema-create-on-open works for `:memory:`" is true
   only for the first connection's lifetime. A dedicated constructor is
   required. See §5.

4. **The #1739 epoch machinery is the wrong cache precedent; the refs LRU is
   the right one.** The doc says "invalidation epoch machinery from #1739
   applies unchanged". The epoch-guarded cells (`invalidation_epoch`,
   `src/cli/batch/context.rs:171`) invalidate caches *derived from the parent
   index*. The overlay's corpus is the worktree's files — the parent index
   rebuilding does not stale it; the worktree's dirty state changing does.
   That is exactly the `ReferenceIndex` situation, and the codebase already
   has the right pattern: the shared refs LRU with **per-entry staleness**
   (`refs: Arc<Mutex<LruCache<String, Arc<ReferenceIndex>>>>`,
   `src/cli/batch/context.rs:191`; `ReferenceIndex::is_stale`,
   `src/reference.rs:177`; live-LRU access from views via
   `get_ref_via_refs_lru`, `src/cli/batch/view.rs:128`). The overlay mirrors
   refs: live shared LRU keyed by worktree root, per-entry fingerprint
   staleness, plus a new slot-mask bit so explicit `refresh`/invalidation
   clears it. See §6.

---

## 1. Verified seams (signatures as of main @ e132bbba)

| Seam | Location | Verified shape |
|---|---|---|
| Worktree detection | `src/worktree.rs:246` | `parent_index_boundary_crossed(cwd: &Path, project_root: &Path) -> Option<PathBuf>` — **nested** worktrees only (resolved root must be a strict ancestor). Out-of-tree worktrees are detected by `lookup_main_cqs_dir` (`src/worktree.rs:130`) via `resolve_index_dir` (`src/lib.rs:306,325`), which sets the process-global stale flag (`record_worktree_stale`, `src/worktree.rs:322`). |
| Multi-store merge | `src/reference.rs:339` | `merge_results(primary: Vec<UnifiedResult>, refs: Vec<(String, Vec<SearchResult>)>, limit) -> Vec<TaggedResult>` — sorts by score desc with id tiebreak, dedups by `content_hash` (`:374-389`), truncates. |
| Reference fan-out | `src/reference.rs:239` | `search_reference(ref_idx, query_embedding, filter, limit, threshold, apply_weight)` → `store.search_filtered_with_index(..., index: Option<&dyn VectorIndex>)`. |
| SearchCtx trait | `src/cli/commands/search/search_ctx.rs:82` | `references() -> Result<Vec<Arc<ReferenceIndex>>>` at `:130` is the documented multi-store seam. CLI impl `:142`; daemon impl `src/cli/batch/view.rs:938`. |
| Prepared-query reuse | `src/cli/commands/search/query.rs:371` | `prepare_query(ctx, args: &QueryArgs, surface: ProjectSurface) -> Result<Prepared<'_>>`; `Prepared::{ShortCircuit, Dense(Box<PreparedQuery>)}` (`:286`); `PreparedQuery` holds `query_embedding`, `filter`, `reranker`, … (`:306`). Single-store consumer `query_core` (`:339`) → `retrieve_project` (`:637`); multi-store `merge_references` (`:741`), `retrieve_ref_scoped` (`:789`). `retrieve_project` is **the shared hook**: plain CLI path, `--include-refs` CLI path (`:1030`), and daemon `dispatch_search` (`src/cli/batch/handlers/search.rs:143`) + `dispatch_search_with_refs` (`:271`) all flow through it or through `query_core` which calls it. |
| Daemon cache cells | `src/cli/batch/context.rs:58-67` (slot bits, last is `CROSS_PROJECT: 1 << 8`), `:171` epoch, `:184-191` refs LRU (cap 2, with rationale comment), `:254` `last_staleness_check` debounce precedent, `:1450` checkout-epoch capture. |
| Incremental pipeline | `src/cli/watch/reindex.rs:492` | `reindex_files(root, store: &Store, files: &[PathBuf], parser, embedder, global_cache: Option<&EmbeddingCache>, quiet) -> Result<(usize, Vec<String>)>` — parses a file list (relative paths against `root`), extracts chunks + calls + type refs, embeds (cache-first), upserts chunks/FTS/call tables, handles deleted files. Currently `pub(super)`. |
| Store open/init | `src/store/mod.rs:983` (`open`), `:1107` (`open_readonly_small`), `:1150` (`open_readonly_after_init`), `:1488` (`init(&ModelInfo)` applies `schema.sql` — schema creation is `init`, **not** open). Search methods are generic over `Mode` (`run_project_search<Mode>`, `query.rs:686`), so a `Store<ReadWrite>` is searchable directly — no typestate erasure needed. |
| Daemon wire | `src/cli/dispatch.rs:481` `try_daemon_query`: request is `{"command", "args"}` (`:599`) — **no cwd on the wire today**. Translation: `translate_cli_args_to_batch` (`src/daemon_translate.rs:215`) + `cli_arg_spec()` (`dispatch.rs:459`) + `PROCESS_LOCAL_ARG_IDS` strip list (`dispatch.rs:403`). Text-mode invocations never forward (`dispatch.rs:512`). |
| Git invocation precedent | `run_git_diff` (`src/cli/commands/mod.rs:586`) — ref-validation rules (leading `-`, control chars, 255-char cap). |
| Worktree `_meta` | `EnvelopeMeta` `worktree_stale`/`worktree_name` (`src/cli/json_envelope.rs:65-82`). |

---

## 2. Architecture overview

A new library type, built per worktree on demand, cached in the daemon:

```rust
// src/worktree_overlay.rs (new)
pub struct WorktreeOverlay {
    /// In-memory store holding the parsed+embedded dirty delta.
    /// Store<ReadWrite> — search methods are Mode-generic; never persisted.
    pub store: Store<ReadWrite>,
    /// EVERY origin touched by the delta, relative + normalize_path()'d:
    /// old paths of renames, deleted paths, modified/added paths —
    /// including unparseable/binary/unsupported ones. The mask set.
    pub masked_origins: HashSet<PathBuf>,
    /// Dirty-state fingerprint (§6). Cache identity.
    pub fingerprint: [u8; 32],
    pub worktree_root: PathBuf,
    /// files-in-delta / chunks-indexed / build-millis, for _meta + debug log.
    pub stats: OverlayStats,
}
```

The overlay joins search inside the **shared core** (`retrieve_project` +
the two FTS short-circuit branches of `prepare_query`), reached through a new
`SearchCtx` accessor — so CLI-direct and daemon get identical merge logic by
construction, and only *resolution* of the overlay differs per surface
(daemon: build+cache; CLI-direct: `None` + warn, per the doc's phase-1
mitigation).

```rust
// search_ctx.rs — new trait method, default None so the existing
// CountingCtx test mock (query.rs:1352) keeps compiling.
fn overlay(&self) -> Option<Arc<cqs::worktree_overlay::WorktreeOverlay>> { None }
```

Merge semantics (one function, `apply_overlay`, in `query.rs` next to
`merge_references`):

1. **Mask**: drop every project result whose `chunk.file ∈ masked_origins`.
2. **Fan out**: search the overlay store with the same `PreparedQuery`
   embedding + filter (dense path, `apply_weight = false`, weight 1.0 — the
   worktree is the project, not an external reference; no 0.8 demotion).
3. **Merge**: reuse `reference::merge_results` with the overlay results as a
   `("worktree", Vec<SearchResult>)` leg, then fold `TaggedResult` back to
   `Vec<UnifiedResult>`, recording which survivors came from the overlay
   into the output for `_meta.worktree_overlay`.

---

## 3. Delta discovery (exact invocations)

All commands run with `git -C <worktree_root>` (the daemon's own cwd is the
parent project; never rely on process cwd). No user-controlled refs are
interpolated — the only external input is `worktree_root`, validated per §8.

1. **Parent HEAD OID** (the corpus the parent index approximates):
   ```
   git -C <parent_root> rev-parse HEAD
   ```
   `parent_root` = the daemon's project root / the CLI's resolved root.

2. **Tracked delta — committed AND uncommitted in one shot** (working tree
   vs parent HEAD; see correction #2):
   ```
   git -C <worktree_root> diff --name-status -z --find-renames <parent_head_oid>
   ```
   `-z` parsing: records are `M\0path\0`, `A\0path\0`, `D\0path\0`,
   `R<score>\0old\0new\0`, `C<score>\0old\0new\0`, `T\0path\0` (typechange).
   - `M`/`A`/`T` → path joins `masked_origins` and the parse set.
   - `D` → path joins `masked_origins` only (this IS the deletion mask).
   - `R` → **old path joins `masked_origins` only; new path joins both.**
     One status, two entries — the rename case from the program doc falls
     out of the general rule.
   - `C` → new path joins both; old path untouched (still present in parent).

3. **Untracked files** (lane-new code; respects .gitignore):
   ```
   git -C <worktree_root> ls-files --others --exclude-standard -z
   ```
   Each path joins `masked_origins` (harmless — parent has no such origin)
   and the parse set.

4. **Parse-set filter** (which masked origins actually get indexed): regular
   files only (`symlink_metadata`, skip symlinks — git reports mode-120000
   entries; the walker never follows links, `src/lib.rs:830`), extension in
   the same supported set `cqs index` passes to `enumerate_files_iter`
   (`src/lib.rs:795`), under the size caps in `cqs::limits`. Binary or
   unparseable survivors cost nothing: `reindex_files` already tolerates
   per-file parse failure, and masking does not depend on parse success
   (correction #1 makes this safe).

5. **Size guard**: if the delta exceeds `CQS_OVERLAY_MAX_FILES` (default
   500), skip the overlay with `_meta.worktree_overlay = "skipped-delta-too-large"`
   and a warn. A lane that far off main is a rebase problem, not an overlay
   problem.

Path normalization: git emits forward-slash repo-relative paths; chunk
origins are stored via `cqs::normalize_path` relative paths
(`src/cli/watch/reindex.rs` rewrite block). Normalize both sides with
`normalize_path` before set membership so masking is byte-exact on every
platform.

---

## 4. Shadow semantics — failure-mode table

Rule (restated): `masked_origins` = all delta-touched origins. Parent hit
masked iff its origin is in the set. Overlay store is the only source of
hits for those origins. No name-level logic anywhere.

| # | Scenario | Correct behavior | Mechanism | Pinning test |
|---|---|---|---|---|
| 1 | Function **renamed** within a file | Old name not returned for that file; new name found | Origin masked (M); overlay has only the new chunk | `overlay_rename_within_file`: `cqs "old_fn" --name-only` excludes the file; `cqs "new_fn" --name-only` hits the overlay version |
| 2 | **File renamed** (`R`) | No hit under old path; hits under new path | Old path masked, new path masked+indexed (§3 step 2) | `overlay_file_rename_masks_old_origin` |
| 3 | Function **deleted** from a modified file (name exists only in parent) | The dead parent hit is masked even though no overlay chunk shares its name | **Origin-level masking** (correction #1 — `(origin,name)` would fail here) | `overlay_masks_dead_function_in_modified_file` — the adversarial test; must fail under an `(origin,name)` implementation |
| 4 | **File deleted** (`D`) | Zero hits from that origin | Masked, no overlay content | `overlay_deleted_file_fully_masked` (program-doc gate criterion) |
| 5 | File changed then **reverted** | Byte-identical to non-overlay search | Working tree == parent HEAD → not in delta → unmasked; fingerprint changed when reverted → stale overlay rebuilds without it | `overlay_reverted_file_unmasked` + `overlay_unchanged_results_byte_identical` (gate criterion) |
| 6 | **Worktree-only new file** (untracked or `A`) | Searchable; ranks alongside parent results | Indexed in overlay; no parent counterpart to mask | `overlay_new_file_searchable` |
| 7 | **Case-only rename** on NTFS/WSL (`R Foo.rs → foo.rs`) | Old-case origin masked; new-case indexed and readable | Byte-exact masking on git-reported old path (which matches the parent index's stored case — both derive from the tracked path); file read for indexing succeeds either way on case-insensitive FS | `overlay_case_only_rename` (cfg-gated to case-insensitive hosts; on case-sensitive Linux CI it degenerates to test #2 — still asserted) |
| 8 | **.gitignore'd worktree file** | Absent from both stores — consistent absence, no overlay leak | `--exclude-standard` excludes it from delta; the parent index also never had it (indexing walks with `git_ignore(true)`, `src/lib.rs:828`) | `overlay_ignores_gitignored_files` |
| 9 | **Symlink** changed/added | Not indexed, not crashed on | Filtered from parse set (§3 step 4); if it shadows a tracked path it is still masked | `overlay_skips_symlinks` |
| 10 | File rewritten as **binary / unparseable** | Parent version masked; origin contributes no hits | Mask is delta-membership, not parse success | `overlay_binary_rewrite_masks_parent` |
| 11 | File changed **on main only** (lane behind) | Worktree's older content is served, parent's newer content masked | Parent-HEAD diff base (correction #2) puts it in the delta | `overlay_lane_behind_main_serves_old_content` |
| 12 | Unchanged-content chunk inside a changed file | Exactly one hit (overlay's), identical content | Parent copy masked; overlay copy survives; `merge_results` content-hash dedup never fires (`reference.rs:374`) | covered by #5's byte-identical assertion at chunk granularity |

Residual accepted gap (document in the module header + plan doc): if the
**parent working tree itself is dirty** relative to parent HEAD, the parent
index may already diverge from `<parent_head_oid>`; the watch daemon closes
that window within a tick, and the existing `stale_origins` machinery
(`handlers/search.rs:228`) covers it. Out of scope for the overlay.

---

## 5. Overlay store construction

1. **New constructor** `Store::<ReadWrite>::open_memory()`
   (`src/store/mod.rs`, beside `open` at `:983`):
   - `SqliteConnectOptions::new().filename(":memory:")`
   - `max_connections: 1`, **`min_connections: 1`** (`SqlitePoolOptions`),
     **`idle_timeout: None`** — pins one connection alive for the store's
     lifetime, defusing both `:memory:` hazards (correction #3).
   - `journal_mode`: leave the WAL setting (SQLite silently uses `MEMORY`
     journal for in-memory DBs); skip the umask block (no files).
   - Unit test: assert the pool was built with `idle_timeout == None` and
     `min_connections == 1`, plus a functional roundtrip.
2. **Schema**: `store.init(&ModelInfo { name: <daemon model name>, dimensions: <model dim> })`
   (`src/store/mod.rs:1488`) — `init` applies `schema.sql` at
   `CURRENT_SCHEMA_VERSION`, creating chunks/FTS/call tables. ModelInfo
   comes from `BatchContext.model_config` (`src/cli/batch/context.rs:234`)
   so overlay embeddings are dimension-compatible with the prepared query
   embedding by construction.
3. **Pipeline entry**: promote `reindex_files`
   (`src/cli/watch/reindex.rs:492`) from `pub(super)` to `pub(crate)` (or
   move its core to `src/cli/pipeline/`) and call it with
   `root = worktree_root`, `files = parse set`, the daemon's parser +
   embedder, and `global_cache = Some(parent embeddings cache)`. It already:
   parses relative-path lists, rewrites chunk ids/paths to relative, embeds
   cache-first, upserts chunks + FTS + call tables, and tolerates per-file
   failures. The full 3-stage `run_index_pipeline`
   (`src/cli/pipeline/mod.rs:54`) is deliberately NOT used — thread spawn +
   progress machinery is wrong-sized for <20 files.
   - The deleted-file branch inside `reindex_files` (delete_by_origin on
     missing files) is a no-op for the overlay (we never pass D paths).
   - Cache note: passing the parent's `embeddings_cache.db` means overlay
     builds **write** content-addressed cache rows from worktree content
     into the parent's cache. This is intentional (repeat builds across
     fingerprints become cheap; cache is not index truth; #1814 guards CLI
     write *commands*, not daemon cache maintenance) — but it is a behavior
     decision: record it in the PR description and module docs.
4. **No HNSW**: `WorktreeOverlay` searches via
   `store.search_filtered_with_index(embedding, filter, limit, threshold, None)`
   (`src/search/query.rs:720`) — brute-force over a few hundred chunks is
   microseconds; building an HNSW would cost more than it saves.
5. **Embedder availability** (program-doc mitigation, kept): phase 1 builds
   overlays **only on the daemon path**. CLI-direct (`cmd_query`) detects
   overlay-eligibility, and when no daemon answered, prints the warn
   `"overlay skipped: daemon not running (results reflect the parent index)"`
   and sets `_meta.worktree_overlay = "skipped-no-daemon"`. Note: because
   `try_daemon_query` forwards JSON invocations only (`dispatch.rs:512`),
   text-mode searches never get the overlay in phase 1 — acceptable (agents
   use `--json`); document in the flag help.

---

## 6. Cache keying, fingerprint, eviction

**Cell** (mirrors refs LRU, correction #4), in `BatchContext`:

```rust
// context.rs, beside refs (:191)
pub(super) overlays: Arc<Mutex<lru::LruCache<PathBuf, Arc<WorktreeOverlay>>>>,
```
- LRU cap 4 (refs cap is 2 with a 50-200MB rationale comment at `:184`;
  overlays are ~1-10MB each — a few hundred chunks × 4KB embedding).
- New slot bit `OVERLAYS: u16 = 1 << 9` (`context.rs:58-67`), included in
  `slot::ALL` so `invalidate_mutable_caches` (`:694`) and explicit `refresh`
  (`handlers/misc.rs:390`) clear it — the operator escape hatch. The
  epoch-guarded publish protocol is NOT used (live LRU access like
  `get_ref_via_refs_lru`, `view.rs:128`); per-entry staleness below is the
  invalidator.

**Fingerprint** (exact definition):

```
fp = blake3(
  "cqs-overlay-v1\0"
  ‖ parent_head_oid ‖ "\0"
  ‖ for each delta record, sorted by (new_path, old_path):
      status_letter ‖ "\0" ‖ old_path ‖ "\0" ‖ new_path ‖ "\0"
      ‖ blake3(worktree file bytes)   // 32 zero bytes for D / unreadable
      ‖ "\0"
)
```

Content hashes — not mtimes — because mtime granularity misses same-second
edits and WSL/NTFS mtime behavior is exactly where this feature lives;
blake3 over <20 small files is sub-millisecond. `parent_head_oid` in the
preimage means a parent commit (the usual cause of an index rebuild)
automatically rebuilds the overlay.

**Staleness window / lookup protocol** (per query):
1. Debounce: a `Cell<Option<Instant>>` per overlay entry — if the entry was
   validated within `CQS_OVERLAY_FP_DEBOUNCE_MS` (default 2000, same class
   as `last_staleness_check`, `context.rs:254`), reuse without re-running
   git. The window bounds worst-case staleness at ~2s, matching the existing
   index-staleness probe philosophy.
2. Else recompute the fingerprint (2 git spawns + hashing); on match, touch
   the debounce stamp; on mismatch, rebuild outside the LRU lock and swap in
   (the `get_ref_via_refs_lru` load-outside-lock shape, `view.rs:147-170`).
3. Concurrent builders: first-wins via a per-key build mutex or accept the
   duplicate build (cheap) — implementer's choice, but the LRU `put` must be
   last-write-wins with identical fingerprints (idempotent).

**Repeat-query cache hit is a gate criterion** — pin with a test that runs
two daemon searches and asserts `stats.build_ms` is reported once (or via a
build-count probe).

---

## 7. Query path integration

All inside the shared core so CLI==daemon parity is by construction:

1. **`SearchCtx::overlay()`** (default `None`) as in §2.
   - CLI impl (`search_ctx.rs:142`): returns `None` in phase 1 (daemon-only
     builds); the *eligibility detection + warn* lives in `cmd_query`.
   - `BatchView` impl (`view.rs`): resolves `overlay_root` from the request
     (§8), validates it, then `get_overlay_via_lru(...)` per §6.
2. **Dense path**: at the end of `retrieve_project` (`query.rs:637`), after
   the rerank step (`:670-675`):
   ```rust
   if let Some(ov) = ctx.overlay() {
       results = apply_overlay(args, prepared, results, &ov)?;
   }
   ```
   `apply_overlay`: mask → overlay search with `prepared.query_embedding` +
   `prepared.filter` at `args.threshold`/`args.limit` → `merge_results`
   (`reference.rs:339`) with leg name `"worktree"` → fold back to
   `Vec<UnifiedResult>`. Overlay hits are NOT cross-encoded in phase 1 —
   the exact precedent of `--include-refs` + `--rerank` (warned at
   `query.rs:1022-1029`); reuse that warn wording.
   - **Under-fill guard**: when an overlay is active, over-fetch the project
     search 2x (bump `prepared.search_limit` consumption or post-mask refill)
     — masking can hollow out the top-k; mirrors the `apply_weight`
     over-fetch rationale at `reference.rs:257-267`.
3. **Name-only / FTS short-circuit paths** (`prepare_query`, `query.rs:381`
   and `:417`): when `ctx.overlay()` is `Some`, run `search_by_name` against
   both stores, mask parent hits, merge by score, truncate. Ordering matters
   for the NameOnly fallback: apply the mask **before** the
   `results.is_empty()` check at `:419` so an all-masked FTS hit set falls
   through to dense (where the overlay leg can still answer).
4. **Ranking comparability**: overlay scores come from the same embedder
   (daemon's), the same `SearchFilter` (carries `name_boost`, `query_text`,
   `enable_rrf` — `query.rs:518-532`), through `search_filtered_with_index`
   — i.e., **exactly the scoring the reference leg gets today**
   (`reference.rs:268`), at weight 1.0. The known asymmetries (project side
   may be SPLADE-α-fused via `search_hybrid`, `search/query.rs:501`; RRF
   scores are rank-scaled, `rrf_fuse` at `:392`) are the same ones
   `--include-refs` already lives with; both legs honor `enable_rrf`
   per-store so RRF-mode scores are like-for-like. No new weighting knob.
   Pin with a ranking test: a trivially-edited chunk (comment change) ranks
   within ±1 position of where its parent version ranked without the overlay.
5. **`_meta`**: results envelope gains skip-when-default
   `_meta.worktree_overlay`: `{"files": N, "chunks": M}` when active,
   `"skipped-no-daemon"` / `"skipped-delta-too-large"` when degraded, absent
   otherwise. `worktree_stale: true` (`json_envelope.rs:65`) **stays** —
   it describes the index origin and still governs every non-overlay
   command; the agent-def revision (program family acceptance) will key on
   the pair.

---

## 8. Flag surface and daemon protocol

- **`--overlay`** on `SearchArgs` (`src/cli/args.rs:103`) — `SearchArgs` is
  already the single struct shared by CLI `search` and batch `search`, so
  one addition covers both parsers (the house daemon-duplication rule is
  satisfied structurally here).
- **`CQS_WORKTREE_OVERLAY=1`** — equivalent opt-in, resolved at the adapter
  boundary like `CQS_FORCE_BASE_INDEX` (`query.rs:180`); flag OR env
  activates. Default off.
- **`--overlay-root <path>`** — hidden (`#[arg(long, hide = true)]`)
  wire-only flag on `SearchArgs`. The daemon's cwd is the parent project and
  the request is `{"command","args"}` with no cwd (`dispatch.rs:599`), so
  the client must say which worktree. Resolution:
  - CLI computes `overlay_root` when overlay is requested:
    `enclosing_git_root(cwd)` (`worktree.rs:205`) when it differs from the
    resolved root AND either `parent_index_boundary_crossed(cwd, root)` is
    `Some` (nested lanes) or `lookup_main_cqs_dir` reported
    `WorktreeUseMain` (out-of-tree worktrees). Wrap this disjunction as
    `worktree::overlay_root(cwd, resolved_root) -> Option<PathBuf>` next to
    the existing predicates — both detection halves already exist.
  - On daemon forward, the client appends `--overlay-root <abs>` to the
    translated args (a deliberate post-translate append in
    `try_daemon_query`, beside the `request` build at `dispatch.rs:599`;
    keep `overlay` itself forwardable, do not add it to
    `PROCESS_LOCAL_ARG_IDS` at `:403`).
  - **Daemon-side validation** (this is a security seam — an unvalidated
    path is an arbitrary-directory read+embed primitive over the socket):
    canonicalize via `dunce::canonicalize`, then require
    `resolve_main_project_dir(overlay_root) == ctx.root`
    (`worktree.rs:54`) — i.e., the path is a real worktree *of this
    project*. Reject otherwise with a wire error. Follow the
    `run_git_diff` input-validation posture (`commands/mod.rs:586`).
- Default-on flip for worktree CWDs is a later, separate PR after lane soak
  (per program doc) — out of scope here, but the detection call is already
  in place so the flip is a one-line default change plus tests.

---

## 9. Scope guards — phase-1 command list

**Overlay applies to (exactly):**
- bare `cqs "<query>" --json` (daemon `search`) and `cqs search …` — dense,
  hybrid, `--name-only`, all filters;
- the project half of `--include-refs` (it flows through
  `retrieve_project`).

**Explicitly excluded (and how exclusion is enforced):**
- `--ref`-scoped search: `ProjectSurface::Skip` (`query.rs:984`) — searches
  an external store; worktree state is irrelevant; `retrieve_ref_scoped`
  never calls `apply_overlay`.
- `scout` / `gather` / `task`: their seed retrieval bypasses `query_core`
  entirely (`scout_core` calls `store.search_filtered` directly,
  `src/scout.rs:204`; gather at `src/gather.rs:650`) — no code change needed
  to exclude; one test pins that `cqs scout` from a worktree emits no
  `worktree_overlay` meta.
- `similar` / `related` / `neighbors` / `where` / `onboard`: same — not
  routed through `retrieve_project`.
- **All graph commands** (`callers`, `callees`, `impact`, `test-map`,
  `trace`, `explain`, `dead`, `review`, `ci`, `diff`, `drift`): stay
  parent-truth with the existing `worktree_stale` hint
  (`json_envelope.rs:65`). The overlay's call tables (written by
  `reindex_files` as a side effect) are deliberately **not** merged — call
  graph shadowing has subtraction semantics (a deleted caller must reduce
  parent counts) that phase 2 owns. No partial overlay: a graph answer that
  is half-worktree would be a new calibration lie.

Because the hook is `retrieve_project` + the FTS short-circuits, the
exclusion list is enforced by architecture, not by per-command flags — the
only commands that *can* see an overlay are the ones in the include list.

---

## 10. Lane interactions (land-after constraints)

- **lane-rank-provenance (§4, `rank_signals`)**: overlay hits flow through
  the same store retrieval + fusion primitives where the ScoreSignal fold
  records signals, so they will carry `rank_signals` for free — but add one
  explicit parity test after §4 merges: an overlay hit's JSON carries
  `rank_signals` with the same vocabulary as a project hit. All
  search-JSON shape assertions in this plan's tests must be written
  tolerant of the additional skip-when-default `rank_signals` key (assert
  presence/values of overlay keys, never exact-object equality).
- **lane-callers-trust-order**: changes `callers` output ordering — zero
  file overlap with phase 1 (graph commands excluded), sequencing-only:
  rebase, rerun the full `--tests` sweep.

---

## 11. Gate criteria → concrete test list

**Worktree fixture mechanics.** Existing worktree tests fake the layout by
writing `.git` *files* by hand (`tests/cli_worktree_parent_index_guard_test.rs:33-58`)
— sufficient for detection tests but NOT for delta discovery, which needs
real git. The real-git pattern exists in `tests/cli_blame_test.rs:27-60`
(`git init -q` + `config user.*` + commit in a TempDir). New shared helper in
`tests/common/`:

```
fn worktree_fixture() -> (TempDir, PathBuf /*parent*/, PathBuf /*wt*/)
  git init parent; commit corpus; cqs init+index parent (slow-tests gate);
  git -C parent worktree add ../wt -b lane   (or .claude/worktrees/wt for the nested shape)
```

Tests (names from §4's table plus the program-doc gates):

1. `overlay_masks_dead_function_in_modified_file` — the adversarial
   `(origin,name)` falsifier (must be in the FIRST behavior PR).
2. `overlay_rename_within_file` — old name absent, new name found
   (program-doc gate, both dense and `--name-only`).
3. `overlay_file_rename_masks_old_origin`.
4. `overlay_deleted_file_fully_masked`.
5. `overlay_unchanged_results_byte_identical` — unchanged-file results
   byte-identical to a no-overlay run (program-doc gate).
6. `overlay_reverted_file_unmasked` + fingerprint-changed-on-revert unit test.
7. `overlay_new_file_searchable` (tracked-A and untracked variants).
8. `overlay_case_only_rename` (cfg-gated; degenerates to #3 on Linux).
9. `overlay_ignores_gitignored_files`.
10. `overlay_skips_symlinks` (unix-gated).
11. `overlay_binary_rewrite_masks_parent`.
12. `overlay_lane_behind_main_serves_old_content` (correction #2 pin).
13. `overlay_repeat_query_cache_hit` — second query reuses the build
    (program-doc gate); plus `overlay_fingerprint_invalidates_on_edit`.
14. `overlay_no_worktree_no_overlay` — flag on, regular repo → no-op,
    byte-identical output (program-doc recall-gate insurance).
15. `overlay_cli_direct_degrades_honestly` — no daemon →
    `_meta.worktree_overlay == "skipped-no-daemon"` + stderr warn.
16. `overlay_daemon_rejects_foreign_root` — `--overlay-root` outside the
    project → wire error (security pin).
17. `overlay_parity_cli_daemon` — once CLI-direct builds land (phase 1.5+),
    CLI==daemon identical; in phase 1 this pins the *daemon* JSON shape and
    the CLI's skip meta instead.
18. Unit: `open_memory` pool-config pin (min_connections=1, idle_timeout
    None, max_connections=1) + roundtrip; `-z` name-status parser (R/C/D/M/T
    records, embedded-rename two-entry expansion); fingerprint determinism +
    order-independence.
19. `overlay_scout_not_overlaid` — scope-guard pin (§9).
20. Run `/recall-gate` after the query-path PR merges (retrieval-adjacent by
    the house rule, even though `overlay_no_worktree_no_overlay` says it
    cannot move).

---

## 12. Sequencing — PR slices

**PR-1: plumbing (no behavior change).** `Store::open_memory`
(`src/store/mod.rs`); `src/worktree_overlay.rs` (delta discovery, `-z`
parser, fingerprint, `build_overlay` via promoted `reindex_files`);
`worktree::overlay_root` predicate. Unit + fixture tests #6(unit), #18, plus
delta-discovery integration tests over the worktree fixture. Blast radius:
near zero — new module + one visibility promotion + one additive
constructor; nothing on the query path. Merges independently.

**PR-2: query-path integration, CLI surface, flag.** `SearchCtx::overlay()`
default-None; `apply_overlay` + mask-before-empty-check in `prepare_query`;
`--overlay`/env on `SearchArgs`; CLI degradation warn + `_meta`. Tests
#1-#12, #14, #15, #19. Blast radius: `query.rs` (the shared core — the
highest-risk file in this plan; the byte-identical-when-inactive test #14 is
the regression fence), `search_ctx.rs`, `args.rs`. Depends on PR-1.

**PR-3: daemon path.** `overlays` LRU cell + `OVERLAYS` slot bit
(`context.rs`); `BatchView::overlay()` with validation; `--overlay-root`
wire append in `try_daemon_query`; debounced fingerprint staleness. Tests
#13, #16, #17, daemon-side reruns of #1-#5. Blast radius: `context.rs` /
`view.rs` cache machinery (medium — the slot-mask contiguity test extends
mechanically), `dispatch.rs` forwarding. Depends on PR-2. PR-2 and PR-3
could fold into one if review bandwidth prefers fewer round-trips, but the
core-merge logic deserves isolated review (it is "the whole feature").

**PR-4 (later, separate): docs + agent-def clause deletion + default-on
flip after lane soak.** Per program doc; not specified here.

---

## 13. Risks (beyond the program doc's two)

1. **sqlx pool vs `:memory:`** (correction #3) — silent empty-DB or
   vanishing-DB failure modes; mitigated by `open_memory` + config pin test.
2. **Daemon wire path validation** — `--overlay-root` must be proven a
   worktree of the served project before any file is read (§8); otherwise
   the socket becomes a read-and-embed primitive over arbitrary directories.
3. **Overlay builds write to the parent's embedding cache** — intentional
   but a cross-boundary write triggered by a read command; documented
   decision, content-addressed and rebuildable.
4. **WSL subprocess latency** — two git spawns per fingerprint check on
   /mnt/c can be tens of ms; the 2s debounce bounds it to once per burst,
   and `stats.build_ms` at debug level (program-doc gate) keeps it observable.
5. **Mask-induced under-fill** — top-k hollowed out by masking; 2x
   over-fetch when overlay active (§7.2).
6. **Lane far behind main** — parent-HEAD diff base (correction #2) makes
   the delta grow with main's progress, not just the lane's; the 500-file
   cap converts the pathological case into an honest skip rather than a
   multi-second build.
7. **Shared-core regression surface** — `apply_overlay` sits inside
   `retrieve_project`, which every search on both surfaces traverses; test
   #14 (inactive ⇒ byte-identical) is the non-negotiable fence and should be
   written first in PR-2 (TDD).
8. **Note boosts and notes search** — the overlay store has no notes;
   note-boost signals keep coming from the parent store's scoring path.
   Correct (notes are project-level priors) but worth one assertion that a
   note-boosted parent hit on a *masked* origin does not resurrect.
