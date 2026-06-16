//! Worktree search overlay — dirty-delta discovery, fingerprint, and the
//! [`WorktreeOverlay`] type (Result-Trust program §3, plan
//! `docs/plans/2026-06-12-worktree-overlay-implementation.md`).
//!
//! ## Why this exists
//!
//! Lane agents work in `.claude/worktrees/<agent>` (or out-of-tree)
//! checkouts. Reads resolve to the **parent** index (deliberate, #1254), so
//! every search reflects main's state, not the lane's edits. The overlay is
//! an ephemeral, query-time index of *only the worktree's dirty delta* that
//! shadows the parent index for changed origins, so a lane sees its own
//! edits without a full per-worktree reindex.
//!
//! ## What lives here (the crate-portable pieces)
//!
//! - [`discover_delta`] — runs `git` against the worktree + parent HEAD and
//!   returns the [`Delta`]: the set of touched origins (the *mask*) and the
//!   subset to actually parse+embed (the *parse set*).
//! - [`parse_name_status_z`] — the `-z --find-renames` name-status parser,
//!   including the two-entry expansion for `R`/`C` records.
//! - [`fingerprint`] — the blake3 cache identity over the delta's content.
//! - [`WorktreeOverlay`] / [`OverlayStats`] — the built overlay struct.
//!
//! The actual *build* (open an in-memory store, parse+embed via the
//! incremental pipeline) lives bin-side in `src/cli/worktree_overlay_build.rs`
//! because the pipeline entry (`reindex_files`) is a bin-crate function. This
//! module is deliberately embedder-free so its delta/fingerprint tests run
//! without loading a model.
//!
//! ## Shadow semantics (origin-level, not `(origin, name)`)
//!
//! `masked_origins` holds **every** delta-touched origin — modified, added,
//! deleted, renamed (both old and new path), and even binary/unparseable
//! ones. A parent hit is masked iff its origin is in this set,
//! unconditionally. This is correct where `(origin, name)` shadowing fails:
//! a function deleted from a still-present file leaves a parent hit with no
//! overlay counterpart; origin-level masking still drops it. See the plan's
//! §4 failure-mode table.
//!
//! ## Accepted residual gap
//!
//! If the **parent working tree itself is dirty** relative to parent HEAD,
//! the parent index may already diverge from `<parent_head_oid>`; the watch
//! daemon closes that window within a tick and the existing `stale_origins`
//! machinery covers it. Out of scope for the overlay.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::store::{ReadWrite, Store};

/// Default ceiling on delta size. A lane this far from main is a rebase
/// problem, not an overlay problem — past the cap we skip the overlay and
/// serve the parent index honestly. Override via `CQS_OVERLAY_MAX_FILES`.
pub const DEFAULT_OVERLAY_MAX_FILES: usize = 500;

/// Resolve the delta-size cap honoring `CQS_OVERLAY_MAX_FILES`. Zero falls
/// back to the default (disabling the guard entirely would let a giant
/// rebase delta build a multi-second overlay on the query path).
pub fn overlay_max_files() -> usize {
    std::env::var("CQS_OVERLAY_MAX_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_OVERLAY_MAX_FILES)
}

/// Errors from overlay delta discovery / fingerprinting.
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    /// A `git` subprocess failed to spawn or exited non-zero.
    #[error("git {context} failed: {message}")]
    Git { context: String, message: String },

    /// The parent HEAD OID was not a 40-char (or 64-char SHA-256) hex string.
    #[error("unexpected parent HEAD oid: {0:?}")]
    BadHeadOid(String),

    /// The delta exceeded [`overlay_max_files`]; the caller should skip the
    /// overlay and report `skipped-delta-too-large`.
    #[error("delta too large: {count} files exceeds cap {cap}")]
    DeltaTooLarge { count: usize, cap: usize },

    /// I/O error reading a worktree file (for the fingerprint).
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Store construction / pipeline error (surfaced by the bin-side builder).
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),

    /// The parse+embed pipeline (`reindex_files`) failed while building the
    /// overlay. Distinct from [`OverlayError::Git`] so the message doesn't
    /// misattribute a pipeline failure to a git invocation.
    #[error("overlay build failed: {0}")]
    Build(String),
}

/// Git name-status record kinds we care about (subset of git's status
/// letters, plus the rename/copy score). Typechange (`T`) is folded into
/// `Modified` — for indexing purposes a file whose mode flipped is just a
/// changed origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaStatus {
    /// `M` (modified) or `T` (typechange).
    Modified,
    /// `A` — added (tracked).
    Added,
    /// `D` — deleted. Masks the origin; contributes no parse-set entry.
    Deleted,
    /// `R` — rename. `old` masked only; `new` masked + parsed.
    Renamed,
    /// `C` — copy. `new` masked + parsed; `old` untouched (still in parent).
    Copied,
    /// Untracked file (from `ls-files --others`). Masks (harmless — parent
    /// has no such origin) and parses.
    Untracked,
}

/// One delta record. `old` is `Some` only for `R`/`C`. Paths are
/// repo-relative, forward-slash, `normalize_path`'d.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaRecord {
    pub status: DeltaStatus,
    /// Renamed/copied source path; `None` for non-rename records.
    pub old: Option<String>,
    /// The current path (target for renames/copies, the path otherwise).
    pub new: String,
}

/// The discovered worktree delta.
#[derive(Debug, Clone, Default)]
pub struct Delta {
    /// Parsed records, in discovery order.
    pub records: Vec<DeltaRecord>,
    /// EVERY origin touched by the delta (the mask set), `normalize_path`'d.
    pub masked_origins: HashSet<PathBuf>,
    /// The subset of origins to actually parse+embed (regular files, a
    /// supported extension, under the size cap), `normalize_path`'d
    /// repo-relative paths suitable for `reindex_files`.
    pub parse_set: Vec<PathBuf>,
    /// The parent HEAD OID this delta was computed against.
    pub parent_head_oid: String,
}

/// Stats for `_meta.worktree_overlay` + debug logging.
#[derive(Debug, Clone, Default)]
pub struct OverlayStats {
    /// Number of files in the delta (mask set size).
    pub files_in_delta: usize,
    /// Number of chunks the overlay store indexed.
    pub chunks_indexed: usize,
    /// Wall-clock build time in milliseconds.
    pub build_ms: u128,
}

/// The built overlay: an in-memory store of the dirty delta plus the mask
/// set and the cache identity.
pub struct WorktreeOverlay {
    /// In-memory store holding the parsed+embedded dirty delta. Searched via
    /// `search_filtered_with_index(..., None)` (brute force); never persisted.
    pub store: Store<ReadWrite>,
    /// EVERY origin touched by the delta (the mask set), `normalize_path`'d.
    pub masked_origins: HashSet<PathBuf>,
    /// Dirty-state fingerprint — the cache identity (see [`fingerprint`]).
    pub fingerprint: [u8; 32],
    /// The worktree root this overlay was built for.
    pub worktree_root: PathBuf,
    /// Build/size stats for `_meta` + debug log.
    pub stats: OverlayStats,
}

impl std::fmt::Debug for WorktreeOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Store` is not Debug; summarize instead of deriving.
        f.debug_struct("WorktreeOverlay")
            .field("masked_origins", &self.masked_origins.len())
            .field("fingerprint", &hex32(&self.fingerprint))
            .field("worktree_root", &self.worktree_root)
            .field("stats", &self.stats)
            .finish()
    }
}

impl WorktreeOverlay {
    /// Merge this overlay into a `SearchResult`-level parent result set — the
    /// seed-search analogue of the binary's `UnifiedResult`-level
    /// `apply_overlay` (Part A). Used by the `scout` / `gather` / `task`
    /// seed sites, whose retrieval consumes `Vec<SearchResult>` directly rather
    /// than the `UnifiedResult` the search surface produces.
    ///
    /// Three steps, mirroring `apply_overlay` so the two surfaces shadow the
    /// parent index identically:
    ///
    /// 1. **Mask**: drop every parent hit whose `chunk.file` is in
    ///    `masked_origins`. Origin-level, name-agnostic — a function deleted
    ///    from a still-present file (its origin is in the delta but no overlay
    ///    chunk shares its name) is correctly dropped, where `(origin, name)`
    ///    shadowing would resurrect it.
    /// 2. **Fan out**: search the overlay store with the *same* `query_embedding`
    ///    + `filter` at `limit` / `threshold`, brute-force (`index = None` — the
    ///    overlay holds at most a few hundred chunks). A fan-out failure is
    ///    logged and degrades to the masked parent set (never a hard error on
    ///    the query path).
    /// 3. **Merge**: concatenate, sort by score descending (id tiebreak — the
    ///    same total order `reference::merge_results` uses), and truncate to
    ///    `limit`.
    ///
    /// Records the `Active { files, chunks }` envelope meta whenever it runs
    /// (including the all-masked / no-overlay-hit empty case — the overlay still
    /// shaped the answer). The BFS / call-graph expansion downstream of the seed
    /// stays on parent-truth in Part A; the seed sites emit a `seed-only`
    /// `_meta.overlay_graph` marker to make that honest.
    pub fn merge_seed_results(
        &self,
        parent: Vec<crate::store::SearchResult>,
        query_embedding: &crate::Embedding,
        filter: &crate::store::SearchFilter,
        limit: usize,
        threshold: f32,
    ) -> Vec<crate::store::SearchResult> {
        let _span = tracing::info_span!(
            "overlay_merge_seed_results",
            masked = self.masked_origins.len(),
            chunks = self.stats.chunks_indexed
        )
        .entered();

        // 1. Mask: drop parent hits whose origin is in the delta.
        let mut merged: Vec<crate::store::SearchResult> = parent
            .into_iter()
            .filter(|sr| !self.masked_origins.contains(&sr.chunk.file))
            .collect();

        // 2. Fan out over the overlay store (brute force; best-effort).
        match self
            .store
            .search_filtered_with_index(query_embedding, filter, limit, threshold, None)
        {
            Ok(hits) => merged.extend(hits),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "overlay seed search failed; serving masked parent seeds only"
                );
            }
        }

        // 3. Merge by score (highest first), id tiebreak — the same total order
        //    `reference::merge_results` uses. Truncate to the requested limit.
        merged.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.chunk.id.cmp(&b.chunk.id))
        });
        merged.truncate(limit);

        set_overlay_meta(OverlayMeta::Active {
            files: self.stats.files_in_delta,
            chunks: self.stats.chunks_indexed,
        });

        merged
    }

    /// Merge the overlay's call-graph callers into a parent `callers(X)` result.
    ///
    /// `function_calls.file` is the CALLER's call-site origin (there is no
    /// callee-origin column — the callee is identified by name), so the merge is
    /// a single-call mask plus a union:
    ///
    /// 1. **Mask**: drop every parent caller whose `file` (its call-site origin)
    ///    is in `masked_origins`. That caller's out-edge to X may have changed or
    ///    vanished in the worktree, so its parent row is no longer authoritative.
    ///    A caller deleted from a delta file thus drops out with no overlay
    ///    replacement (the count falls); a caller whose body changed is replaced
    ///    by its overlay row below.
    /// 2. **Union**: append every overlay caller. By construction every chunk in
    ///    the overlay store comes from a masked origin, so every overlay caller
    ///    row is from a masked origin — the mask above already removed any
    ///    parent counterpart, so the union cannot double-count. An added caller
    ///    in a worktree-new file appears only here (the count rises).
    ///
    /// The result is the parent callers minus the suspect (masked-origin) ones,
    /// plus the worktree's fresh view of those same origins. Edge-kind filtering
    /// and the `--limit` cap stay in the core, applied to the merged set.
    pub fn merge_callers(
        &self,
        parent: Vec<crate::store::CallerInfo>,
        overlay: Vec<crate::store::CallerInfo>,
    ) -> Vec<crate::store::CallerInfo> {
        let _span = tracing::info_span!(
            "overlay_merge_callers",
            masked = self.masked_origins.len(),
            parent = parent.len(),
            overlay = overlay.len()
        )
        .entered();
        let mut merged: Vec<crate::store::CallerInfo> = parent
            .into_iter()
            .filter(|c| !self.masked_origins.contains(&c.file))
            .collect();
        merged.extend(overlay);
        merged
    }

    /// Merge the overlay's call-graph callees into a parent `callees(X)` result.
    ///
    /// The masking key is asymmetric to `merge_callers`. A callee row carries no
    /// file (`function_calls` records the callee by NAME), so there is no
    /// per-row origin to mask against. What governs authority is X's DEFINITION
    /// file — a property of the query target, not of any row:
    ///
    /// - **`x_def_masked == true`** (X's body lives in a delta-touched file, so
    ///   its entire out-edge set is suspect): drop ALL parent callee rows and
    ///   serve the overlay's callee rows for X. The overlay reflects X's
    ///   worktree body — added calls appear, deleted calls vanish.
    /// - **`x_def_masked == false`** (X's body is unchanged): the parent callees
    ///   are authoritative and there is nothing to mask (callees are name-only).
    ///   Return them untouched and do NOT union the overlay — X was not edited,
    ///   so the overlay holds no callee rows for it, and unioning could only
    ///   inject rows from a stale or unrelated overlay scan.
    ///
    /// The caller (the core) resolves `x_def_masked` via
    /// [`WorktreeOverlay::callee_target_def_masked`]. Edge-kind filtering and the
    /// cap stay in the core.
    ///
    /// Accepted fidelity loss when a name is multiply-defined: the bare-name
    /// callee query aggregates the out-edges of EVERY definition of X (callees
    /// carry no def-origin to scope by). If one definition lives in the delta and
    /// another does not, `x_def_masked` is true and the parent rows — including
    /// the UNEDITED definition's callees — are all dropped in favour of the
    /// overlay's, which only covers the edited file. This over-masks in the safe
    /// direction (never serves a stale edge for the edited X; only loses some
    /// valid edges of the co-named unedited X) and is forced by the schema. The
    /// def-origin-scoped path that could separate them is the `Type::method`
    /// query, which stays on parent-truth in PR1.
    pub fn merge_callees(
        &self,
        parent: Vec<crate::store::CalleeInfo>,
        overlay: Vec<crate::store::CalleeInfo>,
        x_def_masked: bool,
    ) -> Vec<crate::store::CalleeInfo> {
        let _span = tracing::info_span!(
            "overlay_merge_callees",
            masked = self.masked_origins.len(),
            parent = parent.len(),
            overlay = overlay.len(),
            x_def_masked
        )
        .entered();
        if x_def_masked {
            overlay
        } else {
            parent
        }
    }

    /// Whether X's DEFINITION lives in a delta-touched file — the masking key
    /// for [`WorktreeOverlay::merge_callees`]. True when either (a) any of X's
    /// parent-store definition origins is in `masked_origins` (X's def file was
    /// modified, deleted, or is the masked old path of a rename), or (b) the
    /// overlay store itself defines X (X was added in the worktree, or moved into
    /// a delta file, so the parent has no def at the new path). Either condition
    /// means X's body is suspect and its out-edges must be served from the
    /// overlay.
    ///
    /// `parent_def_origins` is X's definition origins read from the parent store
    /// (e.g. `Store::get_chunks_by_name(X)` projected to `.file`); the overlay
    /// check is done here against the overlay store so the predicate is
    /// self-contained and unit-testable.
    pub fn callee_target_def_masked<I>(&self, name: &str, parent_def_origins: I) -> bool
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let _span = tracing::debug_span!("callee_target_def_masked", name).entered();
        // (a) X's parent def-origin was touched by the delta.
        for origin in parent_def_origins {
            if self.masked_origins.contains(&origin) {
                return true;
            }
        }
        // (b) The overlay store itself defines X (worktree-added / moved-in X,
        // whose parent def-origin is absent or is a masked old path). A store
        // error degrades to `false` — the parent callees stay authoritative,
        // which is the safe default (it never injects unrelated overlay rows).
        match self.store.get_chunks_by_name(name) {
            Ok(chunks) => chunks.iter().any(|c| self.masked_origins.contains(&c.file)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "overlay get_chunks_by_name failed; treating X def as unmasked"
                );
                false
            }
        }
    }

    /// Extract the overlay-ORIGIN seeds from a merged seed set, keyed by name.
    ///
    /// An overlay-origin seed is one whose `chunk.file` is in `masked_origins`
    /// — i.e. it came from the overlay fan-out (a worktree-added, -renamed, or
    /// -modified origin), not the masked-survivor parent tail. (A parent
    /// survivor never has its file in `masked_origins`; `merge_seed_results`
    /// drops those first.)
    ///
    /// `gather`/`task` PREFER these over a by-name re-fetch when assembling
    /// depth-0 chunks: the parent store has no chunk for an overlay-only name
    /// (a worktree-added function), and serves STALE content for a modified
    /// origin's name. Surfacing the overlay seed itself makes gather/task as
    /// honest about the worktree delta as `scout` (which consumes the merged
    /// `SearchResult`s directly) — the `seed-only` `_meta.overlay_graph` marker
    /// then tells the truth: seeds (and their chunks) overlaid, expansion
    /// parent-truth. The expansion (BFS) stays parent-truth in Part A.
    ///
    /// Name-keyed: a name-collision between an overlay seed and an
    /// equally-named parent BFS-expanded node resolves to the overlay version
    /// only at depth 0 (`fetch_and_assemble` consults this map for depth-0
    /// chunks only), matching where `merge_seed_results` placed it.
    pub fn overlay_origin_seeds(
        &self,
        merged: &[crate::store::SearchResult],
    ) -> std::collections::HashMap<String, crate::store::SearchResult> {
        let _span = tracing::info_span!(
            "overlay_origin_seeds",
            merged = merged.len(),
            masked = self.masked_origins.len()
        )
        .entered();
        let mut out = std::collections::HashMap::new();
        for sr in merged {
            if self.masked_origins.contains(&sr.chunk.file) {
                // Last writer wins on a name collision among overlay seeds —
                // `merge_seed_results` already sorted by score desc, so the
                // entry().or_insert keeps the highest-scoring overlay hit.
                out.entry(sr.chunk.name.clone())
                    .or_insert_with(|| sr.clone());
            }
        }
        out
    }

    /// Fetch overlay-store chunks for `names` by EXACT name, restricted to
    /// overlay-origin chunks, as a name → `SearchResult` map.
    ///
    /// `task`'s gather phase derives its targets as bare names from the scout
    /// output (no `SearchResult` survives), then re-materializes chunks by name
    /// from the PARENT store — which silently drops a worktree-added target and
    /// serves stale content for a worktree-modified one. This recovers the
    /// overlay chunk for any such target so the task `code` section carries the
    /// worktree content, matching the overlaid scout phase that surfaced the
    /// target in the first place.
    ///
    /// Exact-name (`get_chunks_by_names_batch`) rather than the FTS-fuzzy
    /// `search_by_names_batch`: a target is an exact symbol name from scout, and
    /// the overlay store is tiny, so a fuzzy match would only invite
    /// false-positive overlay chunks. Score is a sentinel `1.0` (these are
    /// pinned targets, not re-ranked hits). Best-effort: a store error logs and
    /// degrades to an empty map (the parent re-fetch then runs unchanged).
    pub fn overlay_seed_chunks_for_names(
        &self,
        names: &[&str],
    ) -> std::collections::HashMap<String, crate::store::SearchResult> {
        let _span = tracing::info_span!(
            "overlay_seed_chunks_for_names",
            count = names.len(),
            masked = self.masked_origins.len()
        )
        .entered();
        let mut out = std::collections::HashMap::new();
        let by_name = match self.store.get_chunks_by_names_batch(names) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "overlay seed-by-name fetch failed; task gather falls back to parent re-fetch"
                );
                return out;
            }
        };
        for (name, chunks) in by_name {
            // Keep only chunks whose origin is in the delta (overlay-origin) —
            // the overlay store holds ONLY delta files, so this is belt-and-
            // suspenders, but it keeps the invariant explicit and survives any
            // future overlay-store contents change.
            if let Some(chunk) = chunks
                .into_iter()
                .find(|c| self.masked_origins.contains(&c.file))
            {
                out.insert(name, crate::store::SearchResult::new(chunk, 1.0));
            }
        }
        out
    }
}

// ─── `_meta.worktree_overlay` envelope state ────────────────────────────────
//
// The overlay's outcome for a single search is surfaced as the skip-when-default
// `_meta.worktree_overlay` envelope field. Unlike `worktree_stale` (a genuine
// once-per-process fact — a CLI process either resolved to a worktree or it did
// not), the overlay outcome is *per query*: the daemon serves many searches from
// one process and must not leak one query's overlay state into the next. So this
// is a thread-local cell the search path sets explicitly per invocation, read by
// the JSON envelope, and cleared at the start of each search.

/// The `_meta.worktree_overlay` outcome for one search. Serializes to the wire
/// shape the plan §7.5 pins: an `{files, chunks}` object when the overlay is
/// active, or one of the skip-reason strings when it was eligible but skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayMeta {
    /// Overlay merged into results. `files` = mask-set size, `chunks` = overlay
    /// chunks indexed. Wire shape: `{"files": N, "chunks": M}`.
    Active { files: usize, chunks: usize },
    /// Overlay was requested + eligible but no daemon answered, so the CLI-direct
    /// path served the parent index. Wire shape: `"skipped-no-daemon"`.
    SkippedNoDaemon,
    /// Overlay was requested but the worktree delta exceeded
    /// [`overlay_max_files`]. Wire shape: `"skipped-delta-too-large"`.
    SkippedDeltaTooLarge,
}

impl OverlayMeta {
    /// Render to the `_meta.worktree_overlay` JSON value (object for the active
    /// case, string for the skip cases).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            OverlayMeta::Active { files, chunks } => {
                serde_json::json!({ "files": files, "chunks": chunks })
            }
            OverlayMeta::SkippedNoDaemon => serde_json::Value::String("skipped-no-daemon".into()),
            OverlayMeta::SkippedDeltaTooLarge => {
                serde_json::Value::String("skipped-delta-too-large".into())
            }
        }
    }
}

thread_local! {
    /// Per-query overlay outcome for the current thread, read by the JSON
    /// envelope and overwritten/cleared by the search path each invocation.
    static OVERLAY_META: std::cell::RefCell<Option<OverlayMeta>> =
        const { std::cell::RefCell::new(None) };
}

/// Record the overlay outcome for the current search so the JSON envelope can
/// surface it as `_meta.worktree_overlay`. The search path calls this exactly
/// once per query (after deciding active / skip); the envelope reads it via
/// [`take_overlay_meta`].
pub fn set_overlay_meta(meta: OverlayMeta) {
    OVERLAY_META.with(|cell| *cell.borrow_mut() = Some(meta));
}

/// Clear any overlay outcome left over from a previous query on this thread.
/// Called at the start of each search so a daemon worker thread never leaks one
/// query's overlay state into the next (the default-OFF, no-overlay case must
/// emit no `worktree_overlay` key).
pub fn clear_overlay_meta() {
    OVERLAY_META.with(|cell| *cell.borrow_mut() = None);
}

/// Read (and clear) the overlay outcome recorded for the current search.
/// `None` when no overlay was requested/eligible — the envelope then omits the
/// `worktree_overlay` key entirely (skip-when-default). Clears on read so the
/// next query starts from a clean slate even if it never sets the cell.
pub fn take_overlay_meta() -> Option<OverlayMeta> {
    OVERLAY_META.with(|cell| cell.borrow_mut().take())
}

/// Run `git -C <dir> <args...>` capturing stdout bytes. `-z` outputs embed
/// NUL separators, so stdout is returned raw rather than as a `String`.
fn git_capture(dir: &Path, args: &[&str], context: &str) -> Result<Vec<u8>, OverlayError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| OverlayError::Git {
            context: context.to_string(),
            message: format!("failed to spawn git: {e}. Is git installed?"),
        })?;
    if !output.status.success() {
        return Err(OverlayError::Git {
            context: context.to_string(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(output.stdout)
}

/// Authoritative set of worktree paths registered with the served project,
/// from `git -C <served_root> worktree list --porcelain`.
///
/// **Security primitive (the overlay-root registration gate).** Earlier
/// attempts to validate an `--overlay-root` by parsing the *worktree's own*
/// `.git` link / back-pointer files were defeated at the symlink-following
/// path layer (a `.git` symlink to a real registered worktree makes both the
/// forward gitdir resolution and the `<gitdir>/gitdir` back-pointer follow
/// through to the real worktree, so the masquerade passes while the daemon
/// still enumerates the ATTACKER tree's files). The fix queries git's OWN
/// registry, rooted at `served_root` — a path the daemon controls, not the
/// attacker's tree. A symlink masquerade is invisible to that registry, so
/// membership is the unforgeable gate; the caller requires
/// `canonicalize(overlay_root)` to be a member.
///
/// Each `worktree <path>` line is canonicalized (so the caller compares real
/// paths); entries that fail to canonicalize (a registered worktree whose
/// directory was removed out from under git) are dropped. Returns
/// [`OverlayError::Git`] when git can't be spawned or exits non-zero — the
/// caller rejects loudly (same wire-error posture as the rest of the overlay
/// path, which already shells `git -C` via [`discover_delta`]).
pub fn registered_worktrees(served_root: &Path) -> Result<Vec<PathBuf>, OverlayError> {
    let _span = tracing::info_span!("overlay_registered_worktrees").entered();
    let raw = git_capture(
        served_root,
        &["worktree", "list", "--porcelain"],
        "worktree list",
    )?;
    let text = String::from_utf8_lossy(&raw);
    // Porcelain format: stanzas separated by blank lines, each beginning with
    // `worktree <abs-path>`. We only need the path lines; everything else
    // (HEAD, branch, bare, detached, locked, prunable) is irrelevant to the
    // membership check.
    let worktrees = text
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        // Canonicalize so the membership check compares real paths (the caller
        // canonicalizes the requested overlay_root the same way). A registered
        // worktree whose directory has been deleted canonicalizes to an error
        // and is dropped — it can't be a live overlay root anyway.
        .filter_map(|p| dunce::canonicalize(p).ok())
        .collect();
    Ok(worktrees)
}

/// `git -C <parent_root> rev-parse HEAD` → validated OID string.
///
/// `parent_root` is the parent project root (the daemon's served root / the
/// CLI's resolved root) — the corpus the parent index approximates. The OID
/// is validated to be a plain hex SHA so it can be interpolated as a diff
/// base with no option-injection surface.
pub fn parent_head_oid(parent_root: &Path) -> Result<String, OverlayError> {
    let _span = tracing::info_span!("overlay_parent_head_oid").entered();
    let out = git_capture(parent_root, &["rev-parse", "HEAD"], "rev-parse HEAD")?;
    let oid = String::from_utf8_lossy(&out).trim().to_string();
    // SHA-1 = 40 hex, SHA-256 = 64 hex. Reject anything else (option text,
    // "HEAD" on an unborn branch, stray output) before it becomes a ref arg.
    let valid_len = oid.len() == 40 || oid.len() == 64;
    if !valid_len || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        tracing::warn!(oid = %oid, "rev-parse HEAD returned a non-OID value");
        return Err(OverlayError::BadHeadOid(oid));
    }
    Ok(oid)
}

/// Parse `git diff --name-status -z` (or `ls-files -z`) NUL-delimited output.
///
/// Records for `diff --name-status -z`:
///   `M\0path\0`, `A\0path\0`, `D\0path\0`, `T\0path\0`,
///   `R<score>\0old\0new\0`, `C<score>\0old\0new\0`.
///
/// The status letter for `R`/`C` carries a similarity score (e.g. `R100`)
/// and is followed by **two** NUL-delimited paths (old, then new). Every
/// other status is a single status field followed by one path. This shape
/// is why a naive split-on-NUL is wrong: the field count per record is
/// status-dependent.
///
/// `find_renames` is assumed (the caller passes `--find-renames`); without
/// it git emits `D`+`A` pairs instead, which this parser also handles
/// correctly (they fall out as separate single-path records).
pub fn parse_name_status_z(raw: &[u8]) -> Vec<DeltaRecord> {
    // Split on NUL into owned, lossy-decoded fields. Git paths are bytes;
    // we normalize to UTF-8 (lossy) because the rest of cqs keys origins by
    // String paths. This matches the parent index's own path handling:
    // chunk origins are stored via `normalize_path`, which is `to_string_lossy`-
    // based (`src/lib.rs`), so a non-UTF-8 path lossy-decodes to the SAME
    // replacement-char string on both sides — masking stays byte-exact and
    // correct even for the (astronomically rare) non-UTF-8 source path. Never
    // a panic.
    let fields: Vec<String> = raw
        .split(|&b| b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();

    let mut records = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let status_field = &fields[i];
        let letter = status_field.chars().next().unwrap_or(' ');
        match letter {
            'R' | 'C' => {
                // status, old, new — three fields.
                if i + 2 >= fields.len() {
                    // Truncated record; stop rather than mis-pair. Fail loud:
                    // a truncated -z stream means a malformed git output, not
                    // a clean end.
                    tracing::warn!(
                        status = %status_field,
                        "truncated rename/copy record in -z name-status — dropping tail"
                    );
                    break;
                }
                let old = normalize_str(&fields[i + 1]);
                let new = normalize_str(&fields[i + 2]);
                let status = if letter == 'R' {
                    DeltaStatus::Renamed
                } else {
                    DeltaStatus::Copied
                };
                records.push(DeltaRecord {
                    status,
                    old: Some(old),
                    new,
                });
                i += 3;
            }
            'M' | 'A' | 'D' | 'T' => {
                if i + 1 >= fields.len() {
                    tracing::warn!(
                        status = %status_field,
                        "truncated single-path record in -z name-status — dropping tail"
                    );
                    break;
                }
                let new = normalize_str(&fields[i + 1]);
                let status = match letter {
                    'A' => DeltaStatus::Added,
                    'D' => DeltaStatus::Deleted,
                    // `T` (typechange) folds into Modified for indexing.
                    _ => DeltaStatus::Modified,
                };
                records.push(DeltaRecord {
                    status,
                    old: None,
                    new,
                });
                i += 2;
            }
            _ => {
                // Unknown status letter (e.g. `U` unmerged during a conflict).
                // Treat the following field as a single path masked as
                // Modified — conservatively masking an origin is always safe
                // (it just means the overlay is the authority for it). If
                // there's no following field, stop.
                tracing::warn!(
                    status = %status_field,
                    "unrecognized status letter in -z name-status — masking its path as modified"
                );
                if i + 1 >= fields.len() {
                    tracing::warn!(
                        status = %status_field,
                        "truncated unknown-status record in -z name-status — dropping tail"
                    );
                    break;
                }
                let new = normalize_str(&fields[i + 1]);
                records.push(DeltaRecord {
                    status: DeltaStatus::Modified,
                    old: None,
                    new,
                });
                i += 2;
            }
        }
    }
    records
}

/// Normalize a git-reported path (forward-slash repo-relative) the same way
/// chunk origins are stored, so set membership is byte-exact cross-platform.
fn normalize_str(path: &str) -> String {
    crate::normalize_slashes(path)
}

/// Discover the worktree's dirty delta against the parent's HEAD.
///
/// Runs three git invocations (all `git -C <root>`; the daemon's own cwd is
/// the parent project, so process cwd is never relied on):
///   1. `rev-parse HEAD` against `parent_root` (the diff base — see plan
///      correction #2: parent HEAD, NOT merge-base, so a lane-behind-main
///      divergence is captured too).
///   2. `diff --name-status -z --find-renames <oid>` against `worktree_root`
///      (committed AND uncommitted tracked changes in one shot).
///   3. `ls-files --others --exclude-standard -z` against `worktree_root`
///      (untracked, gitignore-respecting).
///
/// Builds `masked_origins` (every touched origin) and `parse_set` (the
/// subset worth indexing — regular files, supported extension, under the
/// size cap). Enforces [`overlay_max_files`].
pub fn discover_delta(worktree_root: &Path, parent_root: &Path) -> Result<Delta, OverlayError> {
    let _span = tracing::info_span!("overlay_discover_delta").entered();

    let oid = parent_head_oid(parent_root)?;

    // Tracked delta (committed + uncommitted) vs parent HEAD.
    let tracked_raw = git_capture(
        worktree_root,
        &["diff", "--name-status", "-z", "--find-renames", &oid],
        "diff --name-status",
    )?;
    let mut records = parse_name_status_z(&tracked_raw);

    // Size cap, enforced incrementally so an oversized delta bails BEFORE the
    // filesystem-heavy parse-set scan (`is_parse_candidate` stats every file).
    // The cap is measured on the authoritative quantity — the deduped mask set
    // — so the early bail rejects exactly what the final backstop would.
    let cap = overlay_max_files();
    let mut delta = Delta {
        parent_head_oid: oid,
        ..Default::default()
    };

    // Helper: fold one record's touched origins into the mask set.
    fn mask_record(masked: &mut HashSet<PathBuf>, rec: &DeltaRecord) {
        // For renames/copies the `old` path is masked too (the rename case
        // from the program doc). `R` masks old; `C` leaves old present in
        // parent (untouched).
        if let Some(old) = &rec.old {
            if rec.status == DeltaStatus::Renamed {
                masked.insert(PathBuf::from(old));
            }
        }
        masked.insert(PathBuf::from(&rec.new));
    }

    // Mask the tracked records, then check the cap before spending a git
    // invocation on the untracked list.
    for rec in &records {
        mask_record(&mut delta.masked_origins, rec);
    }
    let count = delta.masked_origins.len();
    if count > cap {
        tracing::warn!(count, cap, "overlay delta exceeds cap — skipping");
        return Err(OverlayError::DeltaTooLarge { count, cap });
    }

    // Untracked files (lane-new code, gitignore-respecting). Fold each into the
    // mask set as it arrives and bail the moment the cap is crossed, so a giant
    // untracked tree never builds the full set or the parse set.
    let untracked_raw = git_capture(
        worktree_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        "ls-files --others",
    )?;
    for field in untracked_raw.split(|&b| b == 0).filter(|f| !f.is_empty()) {
        let path = normalize_str(&String::from_utf8_lossy(field));
        let rec = DeltaRecord {
            status: DeltaStatus::Untracked,
            old: None,
            new: path,
        };
        mask_record(&mut delta.masked_origins, &rec);
        records.push(rec);
        let count = delta.masked_origins.len();
        if count > cap {
            tracing::warn!(count, cap, "overlay delta exceeds cap — skipping");
            return Err(OverlayError::DeltaTooLarge { count, cap });
        }
    }

    // Under the cap: build the parse set (the subset of masked origins worth
    // indexing). `D` masks only (no content). `R`/`C` index the new path.
    // Everything else (`M`/`A`/`T`/untracked) indexes its path.
    for rec in &records {
        let parse_candidate = match rec.status {
            DeltaStatus::Deleted => None,
            _ => Some(&rec.new),
        };
        if let Some(rel) = parse_candidate {
            if is_parse_candidate(worktree_root, rel) {
                delta.parse_set.push(PathBuf::from(rel));
            }
        }
    }

    // De-dupe the parse set: a file that appears as both a rename target and
    // (pathologically) another record should be parsed once.
    delta.parse_set.sort();
    delta.parse_set.dedup();

    delta.records = records;

    // Backstop: the incremental checks above already guarantee this, but keep
    // the authoritative final check so the invariant is enforced at one point.
    let count = delta.masked_origins.len();
    if count > cap {
        tracing::warn!(count, cap, "overlay delta exceeds cap — skipping");
        return Err(OverlayError::DeltaTooLarge { count, cap });
    }

    tracing::info!(
        masked = delta.masked_origins.len(),
        parse = delta.parse_set.len(),
        "overlay delta discovered"
    );
    Ok(delta)
}

/// Parse-set membership: a delta origin is worth indexing iff it is a
/// regular file (not a symlink — git reports mode-120000 entries and the
/// walker never follows links), has a supported extension, and is under the
/// discovery size cap. Mirrors `enumerate_files_iter`'s gates so the overlay
/// indexes exactly what `cqs index` would.
fn is_parse_candidate(worktree_root: &Path, rel: &str) -> bool {
    let abs = worktree_root.join(rel);
    // `symlink_metadata` does not follow links — a symlink reports
    // `is_symlink()` true and we skip it (also skips broken links).
    let meta = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(_) => return false, // gone / unreadable — mask only.
    };
    if !meta.is_file() {
        return false; // symlink, dir, fifo, etc.
    }
    if meta.len() > crate::max_file_size() {
        return false;
    }
    // Supported extension per the same registry `cqs index` uses.
    let ext = match abs.extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return false,
    };
    crate::language::REGISTRY
        .supported_extensions()
        .any(|s| s.eq_ignore_ascii_case(ext))
}

/// Compute the dirty-state fingerprint — the overlay's cache identity.
///
/// Domain-separated preimage (plan §6):
/// ```text
/// blake3(
///   "cqs-overlay-v2\0"
///   ‖ parent_head_oid ‖ "\0"
///   ‖ notes_revision ‖ "\0"
///   ‖ for each record, sorted by (new, old):
///       status_letter ‖ "\0" ‖ old ‖ "\0" ‖ new ‖ "\0"
///       ‖ blake3(worktree file bytes)
///         // 32 zero bytes for a genuine deletion / a non-dereferenced
///         // symlink (both deterministic, intentional sentinels); a UNIQUE
///         // non-zero, non-repeating sentinel for a TRANSIENT read error,
///         // so a file unreadable at both build and re-validation never
///         // self-matches into a stale cache hit.
///       ‖ "\0"
/// )
/// ```
///
/// Content hashes (not mtimes): mtime granularity misses same-second edits
/// and WSL/NTFS mtime behavior is exactly where this feature lives; blake3
/// over <20 small files is sub-millisecond. `parent_head_oid` in the
/// preimage means a parent commit (the usual index-rebuild cause)
/// automatically rebuilds the overlay. Sorting makes the fingerprint
/// order-independent across git output orderings.
///
/// `notes_revision` is a digest of the parent's notes (see
/// `Store::notes_revision`). Notes participate in the overlay's *state* (they
/// are copied into the shadow store so its `note_boost` matches the parent),
/// so they must participate in the overlay's *cache identity* too: a parent
/// notes mutation flips this token, the fingerprint differs, and the LRU
/// treats the cached overlay as a miss and rebuilds with the fresh notes —
/// invalidation by content-identity, not the deferrable overlay-clear. Both
/// production fingerprint sites (build and re-validation) must pass the same
/// token so a notes-unchanged re-validation still matches the cached build.
pub fn fingerprint(worktree_root: &Path, delta: &Delta, notes_revision: &[u8; 32]) -> [u8; 32] {
    let _span = tracing::debug_span!("overlay_fingerprint").entered();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"cqs-overlay-v2\0");
    hasher.update(delta.parent_head_oid.as_bytes());
    hasher.update(b"\0");
    hasher.update(notes_revision);
    hasher.update(b"\0");

    // Sort records by (new, old) so git ordering doesn't change the result.
    let mut sorted: Vec<&DeltaRecord> = delta.records.iter().collect();
    sorted.sort_by(|a, b| {
        a.new
            .cmp(&b.new)
            .then_with(|| a.old.as_deref().cmp(&b.old.as_deref()))
    });

    const ZERO32: [u8; 32] = [0u8; 32];
    for rec in sorted {
        hasher.update(&[status_letter(&rec.status)]);
        hasher.update(b"\0");
        hasher.update(rec.old.as_deref().unwrap_or("").as_bytes());
        hasher.update(b"\0");
        hasher.update(rec.new.as_bytes());
        hasher.update(b"\0");
        // Content hash of the worktree file. A genuine deletion folds the
        // ZERO32 deletion sentinel; the deletion's *presence* in the preimage
        // (status letter + path) still distinguishes it from an absent record.
        // A symlink intentionally maps to ZERO32 too (not dereferenced — see
        // `content_digest`). A TRANSIENT read error gets a UNIQUE sentinel
        // (`transient_error_sentinel`) so it can never collide with the
        // deletion sentinel NOR self-match across two recomputes — the latter
        // is what forces a cache MISS instead of serving a content-blind hash
        // as if it were proven-unchanged.
        match rec.status {
            DeltaStatus::Deleted => {
                hasher.update(&ZERO32);
            }
            _ => {
                let abs = worktree_root.join(&rec.new);
                // Stream the file through blake3 rather than slurping it into
                // a Vec: the parse-set size cap gates only what gets indexed,
                // not what the fingerprint hashes — an oversize artifact in
                // the delta would otherwise be RAM-loaded on every recompute.
                // `update_reader` over a plain `File` yields the same digest
                // as `blake3::hash(&bytes)` would, so the fingerprint value is
                // unchanged for in-bounds files.
                match content_digest(&abs) {
                    Ok(digest) => {
                        hasher.update(&digest);
                    }
                    // A symlink is the one *intentional* error: it is excluded
                    // from the parse set and must contribute a stable,
                    // non-dereferencing digest. `content_digest` flags it with
                    // `InvalidInput`; keep its documented ZERO32 contract.
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
                        hasher.update(&ZERO32);
                    }
                    // Any other error is a transient/unintentional I/O failure
                    // (ENOENT race, EACCES flip, EIO, partial write). Folding
                    // ZERO32 here would (a) make a modified-but-unreadable file
                    // alias a clean deletion, and (b) let an unreadable-at-both
                    // file self-match into a stale hit. Fold a unique sentinel
                    // instead so the fingerprint cannot prove the file
                    // unchanged → the cache-compare diverges → forced rebuild.
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %abs.display(),
                            "overlay fingerprint: transient read error — folding a unique sentinel to force a cache miss (recompute), not the deletion sentinel"
                        );
                        hasher.update(&transient_error_sentinel());
                    }
                }
            }
        };
        hasher.update(b"\0");
    }

    *hasher.finalize().as_bytes()
}

/// Stream a file's bytes through blake3 without loading it into memory,
/// returning the 32-byte content digest. Equivalent to
/// `blake3::hash(&std::fs::read(path)?)` but bounded-memory for the case
/// where an oversize artifact lands in the delta.
///
/// Symlinks are NOT dereferenced. `is_parse_candidate` gates on
/// `symlink_metadata` and excludes symlinks from the parse set, so a symlink
/// never contributes searchable content; the fingerprint must agree. We probe
/// with `symlink_metadata` (which does not follow links) before `File::open`
/// (which does) and return an `InvalidInput` error for symlinks. The caller
/// keys off the error *kind*: `InvalidInput` is the intentional symlink case
/// and maps to the stable ZERO32 sentinel (deterministic, target-independent);
/// any OTHER error kind is a transient I/O failure and the caller folds a
/// unique non-repeating sentinel instead (see `fingerprint`), so a transient
/// failure can never alias a deletion nor self-match into a stale cache hit.
fn content_digest(path: &Path) -> std::io::Result<[u8; 32]> {
    if std::fs::symlink_metadata(path)?.is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "symlink: not dereferenced for fingerprint",
        ));
    }
    let file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)?;
    Ok(*hasher.finalize().as_bytes())
}

/// A 32-byte sentinel for a record whose worktree content was *transiently*
/// unreadable (an I/O error that is neither a deletion nor an intentional
/// symlink). It is folded into the content slot in place of the real digest.
///
/// The sentinel must satisfy two non-collision properties the deletion ZERO32
/// cannot:
/// 1. It is never all-zero, so a modified-but-unreadable file never aliases a
///    clean deletion of the same path.
/// 2. It is **unique per call** (never repeats), so two recomputes of the same
///    fingerprint over a file that is unreadable at *both* build and
///    re-validation produce different digests. The cache-compare then diverges
///    and forces a rebuild rather than serving a content-blind hash as if it
///    were proven-unchanged. A merely-distinct-but-deterministic sentinel would
///    still self-match in the unreadable-at-both case — uniqueness is what makes
///    the cache treat it as "always stale".
///
/// Uniqueness source: a process-lifetime monotonic counter (distinguishes two
/// calls within the same clock tick / same process — the daemon LRU re-validates
/// in the same process that built) XOR-domain-separated with the wall-clock
/// nanos (distinguishes across process restarts). The leading byte is forced
/// non-zero as a belt-and-braces guard against an all-zero coincidence.
fn transient_error_sentinel() -> [u8; 32] {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Tag-prefix + two distinct entropy sources so the value can never be
    // ZERO32 and never repeats within the process.
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"cqs-overlay-transient-read-error\0");
    hasher.update(&seq.to_le_bytes());
    hasher.update(&nanos.to_le_bytes());
    let mut out = *hasher.finalize().as_bytes();
    // Belt-and-braces: guarantee non-zero leading byte so this can never be
    // mistaken for the ZERO32 deletion/symlink sentinel even on a 1-in-2^256
    // hash coincidence.
    out[0] |= 0x01;
    out
}

/// Git status letter for a record, for the fingerprint preimage.
fn status_letter(status: &DeltaStatus) -> u8 {
    match status {
        DeltaStatus::Modified => b'M',
        DeltaStatus::Added => b'A',
        DeltaStatus::Deleted => b'D',
        DeltaStatus::Renamed => b'R',
        DeltaStatus::Copied => b'C',
        DeltaStatus::Untracked => b'U',
    }
}

/// Lowercase-hex render of a 32-byte digest (for Debug / logs).
fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a NUL-delimited name-status payload from `(status, old, new)`
    /// triples, mirroring `git diff --name-status -z` byte layout.
    fn z_payload(records: &[(&str, Option<&str>, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (status, old, new) in records {
            out.extend_from_slice(status.as_bytes());
            out.push(0);
            if let Some(o) = old {
                out.extend_from_slice(o.as_bytes());
                out.push(0);
            }
            out.extend_from_slice(new.as_bytes());
            out.push(0);
        }
        out
    }

    /// Run a git subcommand in `dir`, asserting success (test helper).
    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `registered_worktrees` returns the served project AND its linked
    /// worktrees (canonicalized) — the authoritative membership set the overlay
    /// gate checks. A bare sibling dir is NOT a member.
    #[test]
    fn registered_worktrees_lists_main_and_linked() {
        let dir = tempfile::TempDir::new().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        run_git(&main, &["init", "-q", "-b", "main"]);
        run_git(&main, &["config", "user.email", "t@e.com"]);
        run_git(&main, &["config", "user.name", "T"]);
        run_git(&main, &["add", "-A"]);
        run_git(&main, &["commit", "-q", "-m", "init"]);

        let wt = dir.path().join("wt");
        run_git(
            &main,
            &["worktree", "add", "-q", "-b", "lane", wt.to_str().unwrap()],
        );

        let listed = registered_worktrees(&main).expect("worktree list");
        let canon_main = dunce::canonicalize(&main).unwrap();
        let canon_wt = dunce::canonicalize(&wt).unwrap();
        assert!(
            listed.contains(&canon_main),
            "served main must be listed: {listed:?}"
        );
        assert!(
            listed.contains(&canon_wt),
            "linked worktree must be listed: {listed:?}"
        );

        // A bare sibling that is NOT a worktree is absent from the registry.
        let bare = dir.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let canon_bare = dunce::canonicalize(&bare).unwrap();
        assert!(
            !listed.contains(&canon_bare),
            "a non-worktree dir must NOT be a registry member"
        );
    }

    /// `registered_worktrees` errors loudly (not silently empty) when git can't
    /// run against the path — a non-git directory has no `git worktree list`,
    /// so the gate rejects rather than accepting on an empty set.
    #[test]
    fn registered_worktrees_errors_on_non_git_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let res = registered_worktrees(dir.path());
        assert!(
            matches!(res, Err(OverlayError::Git { .. })),
            "non-git dir must produce a Git error, got {res:?}"
        );
    }

    #[test]
    fn parse_z_modified_added_deleted_typechange() {
        let raw = z_payload(&[
            ("M", None, "src/a.rs"),
            ("A", None, "src/b.rs"),
            ("D", None, "src/c.rs"),
            ("T", None, "src/d.rs"),
        ]);
        let recs = parse_name_status_z(&raw);
        assert_eq!(recs.len(), 4);
        assert_eq!(recs[0].status, DeltaStatus::Modified);
        assert_eq!(recs[0].new, "src/a.rs");
        assert_eq!(recs[1].status, DeltaStatus::Added);
        assert_eq!(recs[2].status, DeltaStatus::Deleted);
        // Typechange folds into Modified.
        assert_eq!(recs[3].status, DeltaStatus::Modified);
        assert_eq!(recs[3].new, "src/d.rs");
    }

    #[test]
    fn parse_z_rename_two_entry_expansion() {
        // `R100\0old\0new\0` — the rename score rides the status field and
        // the record spans TWO paths.
        let raw = z_payload(&[("R100", Some("src/old.rs"), "src/new.rs")]);
        let recs = parse_name_status_z(&raw);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].status, DeltaStatus::Renamed);
        assert_eq!(recs[0].old.as_deref(), Some("src/old.rs"));
        assert_eq!(recs[0].new, "src/new.rs");
    }

    #[test]
    fn parse_z_copy_two_entry_expansion() {
        let raw = z_payload(&[("C75", Some("src/src.rs"), "src/copy.rs")]);
        let recs = parse_name_status_z(&raw);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].status, DeltaStatus::Copied);
        assert_eq!(recs[0].old.as_deref(), Some("src/src.rs"));
        assert_eq!(recs[0].new, "src/copy.rs");
    }

    #[test]
    fn parse_z_mixed_rename_then_modified_does_not_desync() {
        // A rename (3 fields) immediately followed by a modify (2 fields):
        // the variable field-count is exactly where a naive split desyncs.
        let raw = z_payload(&[("R100", Some("a.rs"), "b.rs"), ("M", None, "c.rs")]);
        let recs = parse_name_status_z(&raw);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].status, DeltaStatus::Renamed);
        assert_eq!(recs[0].new, "b.rs");
        assert_eq!(recs[1].status, DeltaStatus::Modified);
        assert_eq!(recs[1].new, "c.rs");
    }

    #[test]
    fn parse_z_empty_input() {
        assert!(parse_name_status_z(b"").is_empty());
    }

    #[test]
    fn parse_z_truncated_rename_stops_cleanly() {
        // `R100\0old\0` with no `new` — must not panic or mis-pair.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"R100\0old.rs\0");
        let recs = parse_name_status_z(&raw);
        assert!(recs.is_empty(), "truncated rename yields no record");
    }

    /// A fixed notes-revision token for the fingerprint-structure tests below.
    /// These tests exercise the record/content terms of the preimage; the notes
    /// term is held constant here and varied in the overlay-build tests that own
    /// a parent `Store`. (Value is arbitrary but non-zero so a regression that
    /// dropped the notes term would still alter every digest.)
    const TEST_NOTES_REV: [u8; 32] = [7u8; 32];

    /// Build a minimal `Delta` with known records (no git, no FS) for
    /// fingerprint determinism / order-independence tests.
    fn delta_with(records: Vec<DeltaRecord>) -> Delta {
        Delta {
            records,
            parent_head_oid: "a".repeat(40),
            ..Default::default()
        }
    }

    #[test]
    fn fingerprint_is_deterministic() {
        // All records are deletions → content hash is the zero sentinel, so
        // no FS access is needed and the result is fully determined by the
        // record list. Same input twice → same digest.
        let dir = std::path::Path::new("/nonexistent");
        let recs = vec![
            DeltaRecord {
                status: DeltaStatus::Deleted,
                old: None,
                new: "src/a.rs".into(),
            },
            DeltaRecord {
                status: DeltaStatus::Deleted,
                old: None,
                new: "src/b.rs".into(),
            },
        ];
        let fp1 = fingerprint(dir, &delta_with(recs.clone()), &TEST_NOTES_REV);
        let fp2 = fingerprint(dir, &delta_with(recs), &TEST_NOTES_REV);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn fingerprint_folds_in_notes_revision() {
        // Same worktree delta, different notes-revision tokens → different
        // fingerprints. This is the structural property the notes-coherence
        // hazard needed: a parent notes mutation (which flips the token) must
        // move the cache identity so the overlay LRU rebuilds with the fresh
        // notes. The inverse — identical token → identical fingerprint — guards
        // against a notes-unchanged re-validation spuriously rebuilding (the
        // over-invalidation trap).
        let dir = std::path::Path::new("/nonexistent");
        let rec = DeltaRecord {
            status: DeltaStatus::Deleted,
            old: None,
            new: "src/a.rs".into(),
        };
        let token_a = [1u8; 32];
        let token_b = [2u8; 32];
        let fp_a = fingerprint(dir, &delta_with(vec![rec.clone()]), &token_a);
        let fp_a2 = fingerprint(dir, &delta_with(vec![rec.clone()]), &token_a);
        let fp_b = fingerprint(dir, &delta_with(vec![rec]), &token_b);
        assert_eq!(
            fp_a, fp_a2,
            "same delta + same notes token → same fingerprint"
        );
        assert_ne!(
            fp_a, fp_b,
            "same delta + different notes token → different fingerprint"
        );
    }

    #[test]
    fn fingerprint_is_order_independent() {
        // Same records, opposite git output order → identical fingerprint
        // (records are sorted in the preimage).
        let dir = std::path::Path::new("/nonexistent");
        let a = DeltaRecord {
            status: DeltaStatus::Deleted,
            old: None,
            new: "src/a.rs".into(),
        };
        let b = DeltaRecord {
            status: DeltaStatus::Deleted,
            old: None,
            new: "src/b.rs".into(),
        };
        let fp_ab = fingerprint(
            dir,
            &delta_with(vec![a.clone(), b.clone()]),
            &TEST_NOTES_REV,
        );
        let fp_ba = fingerprint(dir, &delta_with(vec![b, a]), &TEST_NOTES_REV);
        assert_eq!(fp_ab, fp_ba);
    }

    #[test]
    fn fingerprint_changes_when_record_set_changes() {
        // Reverting an edit (dropping a record) must change the fingerprint
        // so a stale overlay rebuilds without it.
        let dir = std::path::Path::new("/nonexistent");
        let a = DeltaRecord {
            status: DeltaStatus::Deleted,
            old: None,
            new: "src/a.rs".into(),
        };
        let b = DeltaRecord {
            status: DeltaStatus::Deleted,
            old: None,
            new: "src/b.rs".into(),
        };
        let fp_two = fingerprint(dir, &delta_with(vec![a.clone(), b]), &TEST_NOTES_REV);
        let fp_one = fingerprint(dir, &delta_with(vec![a]), &TEST_NOTES_REV);
        assert_ne!(fp_two, fp_one);
    }

    #[test]
    fn fingerprint_reflects_file_content() {
        // Two deltas with identical record metadata but different on-disk
        // content for the modified file → different fingerprints.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.rs"), b"fn original() {}").unwrap();
        let rec = DeltaRecord {
            status: DeltaStatus::Modified,
            old: None,
            new: "a.rs".into(),
        };
        let fp_before = fingerprint(root, &delta_with(vec![rec.clone()]), &TEST_NOTES_REV);
        std::fs::write(root.join("a.rs"), b"fn edited() {}").unwrap();
        let fp_after = fingerprint(root, &delta_with(vec![rec]), &TEST_NOTES_REV);
        assert_ne!(
            fp_before, fp_after,
            "content change must move the fingerprint"
        );
    }

    #[test]
    fn transient_error_sentinel_is_unique_and_nonzero() {
        // The transient-error sentinel must never be all-zero (so it can't
        // alias the deletion ZERO32) and must never repeat (so an
        // unreadable-at-both file forces a cache miss instead of self-matching).
        const ZERO32: [u8; 32] = [0u8; 32];
        let s1 = transient_error_sentinel();
        let s2 = transient_error_sentinel();
        assert_ne!(s1, ZERO32, "sentinel must not be the deletion sentinel");
        assert_ne!(s2, ZERO32, "sentinel must not be the deletion sentinel");
        assert_ne!(s1, s2, "sentinel must be unique per call (forces a miss)");
    }

    #[test]
    fn fingerprint_transient_read_error_forces_cache_miss() {
        // A Modified record whose worktree file is unreadable (here: absent, so
        // `symlink_metadata` errors with NotFound — a transient I/O error, NOT
        // the intentional symlink InvalidInput) must fold the UNIQUE transient
        // sentinel. Two recomputes of the same delta therefore differ, so the
        // re-validation cache-compare diverges and forces a rebuild rather than
        // serving a content-blind hash as if the file were proven-unchanged.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        // Note: no file written at "missing.rs" → content_digest errors NotFound.
        let rec = DeltaRecord {
            status: DeltaStatus::Modified,
            old: None,
            new: "missing.rs".into(),
        };
        let fp1 = fingerprint(root, &delta_with(vec![rec.clone()]), &TEST_NOTES_REV);
        let fp2 = fingerprint(root, &delta_with(vec![rec]), &TEST_NOTES_REV);
        assert_ne!(
            fp1, fp2,
            "an unreadable modified file must produce a different fingerprint on \
             each recompute so the cache treats it as always-stale (forced miss)"
        );
    }

    #[test]
    fn fingerprint_transient_error_distinct_from_deletion() {
        // The core bug: a modified-but-unreadable file must NOT hash to
        // the same content slot as a clean deletion of the same path. Because
        // the transient sentinel is unique per call, the Modified-unreadable
        // fingerprint differs from BOTH the deletion fingerprint and itself —
        // the sentinel-collision with deletion can never occur.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let modified = DeltaRecord {
            status: DeltaStatus::Modified,
            old: None,
            new: "missing.rs".into(),
        };
        let deleted = DeltaRecord {
            status: DeltaStatus::Deleted,
            old: None,
            new: "missing.rs".into(),
        };
        let fp_mod = fingerprint(root, &delta_with(vec![modified]), &TEST_NOTES_REV);
        let fp_del = fingerprint(root, &delta_with(vec![deleted]), &TEST_NOTES_REV);
        assert_ne!(
            fp_mod, fp_del,
            "an unreadable modification must not alias a clean deletion"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fingerprint_unreadable_perms_forces_cache_miss() {
        // Realistic transient case: a file that exists but is unreadable
        // (permissions stripped → EACCES on File::open). EACCES is not
        // InvalidInput, so it takes the transient-sentinel arm and forces a
        // cache miss on recompute, just like the absent-file case above.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let path = root.join("locked.rs");
        std::fs::write(&path, b"fn secret() {}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let rec = DeltaRecord {
            status: DeltaStatus::Modified,
            old: None,
            new: "locked.rs".into(),
        };
        let fp1 = fingerprint(root, &delta_with(vec![rec.clone()]), &TEST_NOTES_REV);
        let fp2 = fingerprint(root, &delta_with(vec![rec]), &TEST_NOTES_REV);
        // Probe whether the read actually failed BEFORE restoring perms: if the
        // suite runs as root, EACCES is bypassed and the file reads fine (a
        // deterministic digest), so we only assert the miss when the read truly
        // failed.
        let read_failed = content_digest(&path).is_err();
        // Restore perms so TempDir cleanup can remove the file.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        if read_failed {
            assert_ne!(
                fp1, fp2,
                "an unreadable (perms) modified file must force a cache miss"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn fingerprint_does_not_dereference_symlink() {
        // A symlinked delta entry must contribute a stable, non-dereferencing
        // digest: changing the link TARGET's content must not move the
        // fingerprint. `content_digest` probes with `symlink_metadata` and maps
        // symlinks to the ZERO32 sentinel, matching `is_parse_candidate`'s
        // exclusion of symlinks from the parse set.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("target.rs"), b"fn original() {}").unwrap();
        std::os::unix::fs::symlink(root.join("target.rs"), root.join("link.rs")).unwrap();

        let rec = DeltaRecord {
            status: DeltaStatus::Modified,
            old: None,
            new: "link.rs".into(),
        };
        let fp_before = fingerprint(root, &delta_with(vec![rec.clone()]), &TEST_NOTES_REV);

        // Mutate the target the symlink points at.
        std::fs::write(root.join("target.rs"), b"fn edited_target() {}").unwrap();
        let fp_after = fingerprint(root, &delta_with(vec![rec.clone()]), &TEST_NOTES_REV);
        assert_eq!(
            fp_before, fp_after,
            "symlink target change must NOT move the fingerprint"
        );

        // And the symlink's digest is the ZERO32 sentinel: a deletion record
        // for the same path (which hashes ZERO32) yields the same fingerprint.
        let del = DeltaRecord {
            status: DeltaStatus::Deleted,
            old: None,
            new: "link.rs".into(),
        };
        let fp_sentinel = fingerprint(root, &delta_with(vec![del]), &TEST_NOTES_REV);
        // Only the status letter differs in the preimage; re-derive with a
        // Modified record over a path whose content_digest errors (the symlink)
        // to confirm the sentinel path is taken.
        assert_eq!(
            content_digest(&root.join("link.rs"))
                .err()
                .map(|e| e.kind()),
            Some(std::io::ErrorKind::InvalidInput),
            "symlink must yield an error so the caller maps it to ZERO32"
        );
        // Sanity: a Modified symlink and a Deleted path share the same ZERO32
        // content slot but differ by status letter, so their fingerprints must
        // differ — proves the symlink wasn't dereferenced into real bytes.
        assert_ne!(fp_before, fp_sentinel);
    }

    #[test]
    #[serial_test::serial]
    fn overlay_max_files_env_override() {
        let prev = std::env::var("CQS_OVERLAY_MAX_FILES").ok();

        // Default when unset.
        std::env::remove_var("CQS_OVERLAY_MAX_FILES");
        assert_eq!(overlay_max_files(), DEFAULT_OVERLAY_MAX_FILES);

        // A positive override is honored verbatim.
        std::env::set_var("CQS_OVERLAY_MAX_FILES", "42");
        assert_eq!(overlay_max_files(), 42);

        // Zero falls back to the default (the guard must never be disabled —
        // an unbounded delta would build a multi-second overlay on the query
        // path).
        std::env::set_var("CQS_OVERLAY_MAX_FILES", "0");
        assert_eq!(overlay_max_files(), DEFAULT_OVERLAY_MAX_FILES);

        // Garbage falls back to the default too.
        std::env::set_var("CQS_OVERLAY_MAX_FILES", "not-a-number");
        assert_eq!(overlay_max_files(), DEFAULT_OVERLAY_MAX_FILES);

        match prev {
            Some(v) => std::env::set_var("CQS_OVERLAY_MAX_FILES", v),
            None => std::env::remove_var("CQS_OVERLAY_MAX_FILES"),
        }
    }

    // ── merge_seed_results: the SearchResult-level seed overlay (Part A) ──
    //
    // Mirrors the `overlay_merge` unit module in `query.rs`, but at the
    // `SearchResult` granularity the scout/gather/task seed sites consume.
    // Embedder-free: chunks are seeded with hand-built one-hot embeddings, so
    // the brute-force overlay search runs without loading a model.
    mod merge_seed_results {
        use super::*;
        use crate::parser::{Chunk, ChunkType, Language};
        use crate::store::{ChunkSummary, ModelInfo, SearchResult};
        use crate::{Embedding, SearchFilter};

        /// One-hot embedding (`1.0` at `slot`). Distinct slots are near-
        /// orthogonal, so a query at `slot` retrieves only the chunk seeded
        /// there from the brute-force overlay store.
        pub(super) fn one_hot(slot: usize) -> Embedding {
            let mut v = vec![0.0_f32; crate::EMBEDDING_DIM];
            v[slot] = 1.0;
            Embedding::new(v)
        }

        /// A project-side seed `SearchResult` for `(file, name, score)`.
        pub(super) fn project_result(file: &str, name: &str, score: f32) -> SearchResult {
            let summary = ChunkSummary {
                id: format!("{file}:{name}"),
                file: PathBuf::from(file),
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                name: name.to_string(),
                signature: format!("fn {name}()"),
                content: format!("fn {name}() {{}}"),
                doc: None,
                line_start: 1,
                line_end: 2,
                content_hash: blake3::hash(name.as_bytes()).to_hex().to_string(),
                window_idx: None,
                parent_id: None,
                parent_type_name: None,
                parser_version: 0,
                vendored: false,
            };
            SearchResult::new(summary, score)
        }

        /// Build a `WorktreeOverlay` whose in-memory store holds one chunk per
        /// `(file, name, slot)` triple, with `masked_origins` exactly `masked`.
        pub(super) fn overlay_with(
            seeds: &[(&str, &str, usize)],
            masked: &[&str],
        ) -> WorktreeOverlay {
            let mut store = Store::open_memory().expect("open_memory");
            store.init(&ModelInfo::default()).expect("init store");
            store.set_dim(crate::EMBEDDING_DIM);
            for (file, name, slot) in seeds {
                let chunk = Chunk {
                    id: format!("{file}:{name}"),
                    file: PathBuf::from(file),
                    language: Language::Rust,
                    chunk_type: ChunkType::Function,
                    name: name.to_string(),
                    signature: format!("fn {name}()"),
                    content: format!("fn {name}() {{}}"),
                    doc: None,
                    line_start: 1,
                    line_end: 2,
                    byte_start: 0,
                    content_hash: blake3::hash(name.as_bytes()).to_hex().to_string(),
                    canonical_hash: String::new(),
                    parent_id: None,
                    window_idx: None,
                    parent_type_name: None,
                    parser_version: 0,
                };
                store
                    .upsert_chunks_batch(&[(chunk, one_hot(*slot))], Some(0))
                    .expect("seed overlay chunk");
            }
            WorktreeOverlay {
                store,
                masked_origins: masked.iter().map(PathBuf::from).collect(),
                fingerprint: [0u8; 32],
                worktree_root: PathBuf::from("/wt"),
                stats: OverlayStats {
                    files_in_delta: masked.len(),
                    chunks_indexed: seeds.len(),
                    build_ms: 0,
                },
            }
        }

        fn names(results: &[SearchResult]) -> Vec<String> {
            results.iter().map(|r| r.chunk.name.clone()).collect()
        }

        /// A worktree-ADDED file surfaces as a seed: the overlay holds a chunk
        /// the parent index never had (a new file), and `merge_seed_results`
        /// merges it into the parent seed set. This is the Part A headline — the
        /// scout/gather seed reflects the worktree's new file.
        #[test]
        fn added_file_surfaces_as_seed() {
            clear_overlay_meta();
            // Overlay has a brand-new file `src/new.rs` at slot 0; its origin is
            // in the delta (added). Parent seeds have an unrelated hit.
            let overlay = overlay_with(&[("src/new.rs", "fresh_fn", 0)], &["src/new.rs"]);
            let parent = vec![project_result("src/old.rs", "existing_fn", 0.5)];
            let merged =
                overlay.merge_seed_results(parent, &one_hot(0), &SearchFilter::default(), 10, 0.0);
            let got = names(&merged);
            assert!(
                got.contains(&"fresh_fn".to_string()),
                "worktree-added file must surface as a seed; got {got:?}"
            );
            assert!(
                got.contains(&"existing_fn".to_string()),
                "untouched parent seed must survive; got {got:?}"
            );
        }

        /// Origin-level masking: a function deleted from a still-present edited
        /// file (its origin is in the delta, no overlay chunk shares its name)
        /// is dropped from the seed set — the `(origin, name)`-shadowing
        /// falsifier `apply_overlay` also guards, at SearchResult granularity.
        #[test]
        fn masks_dead_function_in_edited_file() {
            clear_overlay_meta();
            let overlay = overlay_with(&[("src/a.rs", "live_fn", 0)], &["src/a.rs"]);
            let parent = vec![
                project_result("src/a.rs", "dead_fn", 0.9),
                project_result("src/b.rs", "untouched", 0.5),
            ];
            // Query a slot the overlay can't answer (slot 5) so the overlay leg
            // is empty and can't reintroduce `dead_fn`.
            let merged =
                overlay.merge_seed_results(parent, &one_hot(5), &SearchFilter::default(), 10, 0.0);
            let got = names(&merged);
            assert!(
                !got.contains(&"dead_fn".to_string()),
                "deleted function in an edited file must be masked; got {got:?}"
            );
            assert!(
                got.contains(&"untouched".to_string()),
                "a hit in an unmodified file must survive; got {got:?}"
            );
        }

        /// The merge records the `Active {files, chunks}` envelope meta whenever
        /// it runs (the seed sites' `_meta.worktree_overlay` source).
        #[test]
        fn records_active_meta() {
            clear_overlay_meta();
            let overlay = overlay_with(&[("src/x.rs", "f", 0)], &["src/x.rs"]);
            let _ = overlay.merge_seed_results(
                vec![project_result("src/y.rs", "g", 0.5)],
                &one_hot(0),
                &SearchFilter::default(),
                10,
                0.0,
            );
            match take_overlay_meta() {
                Some(OverlayMeta::Active { files, chunks }) => {
                    assert_eq!(files, 1, "files_in_delta surfaced");
                    assert_eq!(chunks, 1, "chunks_indexed surfaced");
                }
                other => panic!("expected Active overlay meta, got {other:?}"),
            }
        }

        /// Merge ordering: a higher-scoring overlay hit outranks a lower-scoring
        /// parent hit, and the result truncates to `limit`.
        #[test]
        fn merges_by_score_and_truncates() {
            clear_overlay_meta();
            let overlay = overlay_with(&[("src/new.rs", "high", 0)], &["src/new.rs"]);
            let parent = vec![
                project_result("src/old.rs", "mid", 0.6),
                project_result("src/old2.rs", "low", 0.3),
            ];
            // The overlay's one-hot self-match scores ~1.0 (> 0.6 > 0.3).
            let merged =
                overlay.merge_seed_results(parent, &one_hot(0), &SearchFilter::default(), 2, 0.0);
            assert_eq!(merged.len(), 2, "truncated to limit=2");
            assert_eq!(merged[0].chunk.name, "high", "overlay hit ranks first");
            assert_eq!(merged[1].chunk.name, "mid", "lowest-scoring hit dropped");
        }
    }

    // ── overlay-origin seed surfacing (gather/task depth-0 preference) ──
    //
    // The seed sites PREFER these chunks at depth 0 so gather/task surface an
    // overlay-origin seed's worktree content (matching scout). Reuses the
    // `merge_seed_results` module's embedder-free fixtures via re-import.
    mod overlay_origin_seeds {
        use super::merge_seed_results::*;
        use super::*;
        use crate::store::SearchResult;

        /// `overlay_origin_seeds` keeps ONLY the seeds whose origin is in the
        /// delta (overlay-origin), keyed by name — the parent survivors are
        /// dropped (they're served from the parent store by name, unchanged).
        #[test]
        fn keeps_only_overlay_origin_keyed_by_name() {
            let overlay = overlay_with(&[("src/new.rs", "fresh_fn", 0)], &["src/new.rs"]);
            // `merged` is what `merge_seed_results` would hand back: an overlay
            // hit (origin in delta) plus an untouched parent survivor.
            let merged = vec![
                project_result("src/new.rs", "fresh_fn", 0.95),
                project_result("src/old.rs", "existing_fn", 0.5),
            ];
            let map = overlay.overlay_origin_seeds(&merged);
            assert!(
                map.contains_key("fresh_fn"),
                "overlay-origin seed must be kept; got {:?}",
                map.keys().collect::<Vec<_>>()
            );
            assert!(
                !map.contains_key("existing_fn"),
                "parent-survivor seed must NOT be in the overlay map; got {:?}",
                map.keys().collect::<Vec<_>>()
            );
            // The kept entry carries the overlay `SearchResult` (worktree
            // content), not a placeholder.
            assert_eq!(map["fresh_fn"].chunk.file, PathBuf::from("src/new.rs"));
        }

        /// On a name collision among overlay seeds, the highest-scoring one wins
        /// (`merge_seed_results` already sorted by score desc, so first-writer-
        /// wins on the pre-sorted slice keeps the top hit).
        #[test]
        fn name_collision_keeps_first_pre_sorted() {
            let overlay = overlay_with(&[("src/a.rs", "dup", 0)], &["src/a.rs", "src/b.rs"]);
            // Two overlay-origin hits with the same name, score-desc order.
            let merged = vec![
                project_result("src/a.rs", "dup", 0.9),
                project_result("src/b.rs", "dup", 0.4),
            ];
            let map = overlay.overlay_origin_seeds(&merged);
            assert_eq!(map.len(), 1, "one entry per name");
            assert_eq!(
                map["dup"].chunk.file,
                PathBuf::from("src/a.rs"),
                "highest-scoring (first in pre-sorted slice) overlay hit wins"
            );
        }

        /// `overlay_seed_chunks_for_names` (task's by-exact-name path) recovers a
        /// worktree-added function's chunk from the overlay store, restricted to
        /// overlay-origin names. A name the overlay store doesn't hold is absent.
        #[test]
        fn by_name_recovers_overlay_chunk() {
            let overlay = overlay_with(&[("src/new.rs", "fresh_fn", 0)], &["src/new.rs"]);
            let map: std::collections::HashMap<String, SearchResult> =
                overlay.overlay_seed_chunks_for_names(&["fresh_fn", "not_in_overlay"]);
            assert!(
                map.contains_key("fresh_fn"),
                "task by-name fetch must recover the overlay-added chunk; got {:?}",
                map.keys().collect::<Vec<_>>()
            );
            assert!(
                !map.contains_key("not_in_overlay"),
                "a name absent from the overlay store must not appear; got {:?}",
                map.keys().collect::<Vec<_>>()
            );
            assert_eq!(map["fresh_fn"].chunk.file, PathBuf::from("src/new.rs"));
            assert!(
                map["fresh_fn"].chunk.content.contains("fresh_fn"),
                "recovered chunk carries the overlay (worktree) content"
            );
        }
    }

    // ── call-graph merge (callers / callees, #1858 Part B) ──────────────
    //
    // `merge_callers` / `merge_callees` operate on `CallerInfo` / `CalleeInfo`
    // vecs, so the merge LOGIC is tested with hand-built rows (no store). The
    // def-origin predicate `callee_target_def_masked` queries the overlay store,
    // so its overlay-side leg reuses the `overlay_with` chunk-seeding fixture.
    mod merge_call_graph {
        use super::merge_seed_results::overlay_with;
        use super::*;
        use crate::parser::CallEdgeKind;
        use crate::store::{CalleeInfo, CallerInfo};

        fn caller(file: &str, name: &str) -> CallerInfo {
            CallerInfo {
                name: name.to_string(),
                file: PathBuf::from(file),
                line: 1,
                edge_kind: CallEdgeKind::Call,
            }
        }

        fn callee(name: &str) -> CalleeInfo {
            CalleeInfo {
                name: name.to_string(),
                line: 1,
                edge_kind: CallEdgeKind::Call,
            }
        }

        fn caller_names(rows: &[CallerInfo]) -> Vec<String> {
            rows.iter().map(|c| c.name.clone()).collect()
        }

        fn callee_names(rows: &[CalleeInfo]) -> Vec<String> {
            rows.iter().map(|c| c.name.clone()).collect()
        }

        /// An overlay holding only the mask set (no chunks needed for the
        /// caller-merge logic — `merge_callers` masks by row file).
        fn overlay_masking(masked: &[&str]) -> WorktreeOverlay {
            overlay_with(&[], masked)
        }

        /// callers — deleted caller: a parent caller in a delta-touched file with
        /// NO overlay replacement drops out (the count falls). Calibration: the
        /// mask is what makes this fail without the merge — without dropping the
        /// masked-origin parent row, `dead_caller` would survive.
        #[test]
        fn callers_deleted_caller_count_drops() {
            // `src/edited.rs` is in the delta; the worktree deleted the call to X
            // there, so the overlay has NO caller from that origin.
            let overlay = overlay_masking(&["src/edited.rs"]);
            let parent = vec![
                caller("src/edited.rs", "dead_caller"),
                caller("src/stable.rs", "live_caller"),
            ];
            let merged = overlay.merge_callers(parent, Vec::new());
            let got = caller_names(&merged);
            assert!(
                !got.contains(&"dead_caller".to_string()),
                "a caller in a delta file with no overlay replacement must drop; got {got:?}"
            );
            assert!(
                got.contains(&"live_caller".to_string()),
                "a caller in an untouched file must survive; got {got:?}"
            );
            assert_eq!(merged.len(), 1, "count fell from 2 to 1");
        }

        /// callers — added caller: a worktree-new caller (overlay-only, from a
        /// masked origin) raises the count.
        #[test]
        fn callers_added_caller_count_rises() {
            let overlay = overlay_masking(&["src/new.rs"]);
            let parent = vec![caller("src/stable.rs", "existing_caller")];
            let overlay_rows = vec![caller("src/new.rs", "fresh_caller")];
            let merged = overlay.merge_callers(parent, overlay_rows);
            let got = caller_names(&merged);
            assert!(
                got.contains(&"fresh_caller".to_string()),
                "a worktree-added caller must surface; got {got:?}"
            );
            assert!(
                got.contains(&"existing_caller".to_string()),
                "an untouched parent caller must survive; got {got:?}"
            );
            assert_eq!(merged.len(), 2, "count rose from 1 to 2");
        }

        /// callers — modified caller: the parent row from the delta file is
        /// dropped and the overlay's fresh row from the SAME origin replaces it
        /// (no double-count).
        #[test]
        fn callers_modified_caller_replaced_not_duplicated() {
            let overlay = overlay_masking(&["src/edited.rs"]);
            // Parent and overlay both have a caller named `caller_fn` in the
            // edited file — the overlay row is the authoritative one.
            let parent = vec![caller("src/edited.rs", "caller_fn")];
            let overlay_rows = vec![caller("src/edited.rs", "caller_fn")];
            let merged = overlay.merge_callers(parent, overlay_rows);
            assert_eq!(
                merged.len(),
                1,
                "the masked parent row is replaced by the overlay row, not duplicated"
            );
        }

        /// callees — X edited (def-origin masked): the parent callee set is
        /// dropped wholesale and the overlay's callees for X take over.
        #[test]
        fn callees_x_edited_served_from_overlay() {
            let overlay = overlay_masking(&["src/x.rs"]);
            let parent = vec![callee("old_call"), callee("removed_call")];
            let overlay_rows = vec![callee("new_call"), callee("kept_call")];
            let merged = overlay.merge_callees(parent, overlay_rows, /* x_def_masked */ true);
            let got = callee_names(&merged);
            assert_eq!(
                got,
                vec!["new_call".to_string(), "kept_call".to_string()],
                "X's body changed: parent callees dropped, overlay callees served; got {got:?}"
            );
        }

        /// callees — X unedited (def-origin NOT masked): parent callees are
        /// authoritative and the overlay is NOT unioned (no spurious rows).
        #[test]
        fn callees_x_unedited_parent_authoritative_no_overlay_union() {
            let overlay = overlay_masking(&["src/other.rs"]);
            let parent = vec![callee("real_call")];
            // Even if the overlay scan returned rows, an unedited X must ignore
            // them — passing a non-empty overlay vec proves the no-union path.
            let overlay_rows = vec![callee("spurious_overlay_call")];
            let merged = overlay.merge_callees(parent, overlay_rows, /* x_def_masked */ false);
            let got = callee_names(&merged);
            assert_eq!(
                got,
                vec!["real_call".to_string()],
                "unedited X: parent callees authoritative, overlay NOT unioned; got {got:?}"
            );
        }

        /// `callee_target_def_masked` — (a) parent def-origin in the delta: X is
        /// defined in a modified file, so its parent def-origin is masked.
        #[test]
        fn def_masked_by_parent_origin_in_delta() {
            let overlay = overlay_masking(&["src/x.rs"]);
            // X's parent definition lives in the masked file.
            assert!(
                overlay.callee_target_def_masked("X", [PathBuf::from("src/x.rs")]),
                "X defined in a delta file ⇒ def masked"
            );
        }

        /// `callee_target_def_masked` — (b) overlay defines X: X is worktree-added
        /// (the parent has no def-origin for it), but the overlay store holds a
        /// chunk named X from a masked origin.
        #[test]
        fn def_masked_by_overlay_definition() {
            // Overlay store has a chunk `added_fn` at masked `src/new.rs`.
            let overlay = overlay_with(&[("src/new.rs", "added_fn", 0)], &["src/new.rs"]);
            // No parent def-origins (worktree-added), but the overlay defines it.
            assert!(
                overlay.callee_target_def_masked("added_fn", std::iter::empty()),
                "a worktree-added X (overlay defines it) ⇒ def masked"
            );
        }

        /// `callee_target_def_masked` — unedited X: parent def-origin is outside
        /// the delta AND the overlay doesn't define X ⇒ NOT masked.
        #[test]
        fn def_not_masked_for_unedited_target() {
            let overlay = overlay_masking(&["src/unrelated.rs"]);
            assert!(
                !overlay.callee_target_def_masked("X", [PathBuf::from("src/stable.rs")]),
                "X defined in an untouched file, absent from overlay ⇒ NOT masked"
            );
        }

        /// `callee_target_def_masked` — rename: X's parent def-origin is the OLD
        /// path (still in the parent index), which `discover_delta` masks for a
        /// rename, so the predicate fires on the old path.
        #[test]
        fn def_masked_for_renamed_def_file_old_path() {
            // Rename masks both old and new paths; the parent index still has X
            // at the old path.
            let overlay = overlay_masking(&["src/old.rs", "src/new.rs"]);
            assert!(
                overlay.callee_target_def_masked("X", [PathBuf::from("src/old.rs")]),
                "renamed def file: the masked old path fires the predicate"
            );
        }
    }
}
