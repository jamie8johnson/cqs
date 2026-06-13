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

    // Untracked files (lane-new code, gitignore-respecting).
    let untracked_raw = git_capture(
        worktree_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        "ls-files --others",
    )?;
    for field in untracked_raw.split(|&b| b == 0).filter(|f| !f.is_empty()) {
        let path = normalize_str(&String::from_utf8_lossy(field));
        records.push(DeltaRecord {
            status: DeltaStatus::Untracked,
            old: None,
            new: path,
        });
    }

    let mut delta = Delta {
        parent_head_oid: oid,
        ..Default::default()
    };

    for rec in &records {
        // Mask set: every touched origin. For renames/copies the `old` path
        // is masked too (the rename case from the program doc).
        if let Some(old) = &rec.old {
            // `R` masks old; `C` leaves old present in parent (untouched).
            if rec.status == DeltaStatus::Renamed {
                delta.masked_origins.insert(PathBuf::from(old));
            }
        }
        delta.masked_origins.insert(PathBuf::from(&rec.new));

        // Parse set: which masked origins actually get indexed. `D` masks
        // only (no content). `R`/`C` index the new path. Everything else
        // (`M`/`A`/`T`/untracked) indexes its path.
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

    let cap = overlay_max_files();
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
///   "cqs-overlay-v1\0"
///   ‖ parent_head_oid ‖ "\0"
///   ‖ for each record, sorted by (new, old):
///       status_letter ‖ "\0" ‖ old ‖ "\0" ‖ new ‖ "\0"
///       ‖ blake3(worktree file bytes)   // 32 zero bytes for D / unreadable
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
pub fn fingerprint(worktree_root: &Path, delta: &Delta) -> [u8; 32] {
    let _span = tracing::debug_span!("overlay_fingerprint").entered();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"cqs-overlay-v1\0");
    hasher.update(delta.parent_head_oid.as_bytes());
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
        // Content hash of the worktree file, or 32 zero bytes for deletions
        // / unreadable files. The deletion's *presence* in the preimage
        // (status letter + path) still distinguishes it from an absent
        // record; the zero content-hash is the documented sentinel.
        match rec.status {
            DeltaStatus::Deleted => hasher.update(&ZERO32),
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
                    Ok(digest) => hasher.update(&digest),
                    Err(_) => hasher.update(&ZERO32),
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
/// (which does) and return an error for symlinks so the caller's `Err(_)`
/// branch maps them to the ZERO32 sentinel — the same stable, non-dereferencing
/// digest already used for deletions and unreadable files. This keeps the
/// fingerprint deterministic and independent of the symlink target's contents.
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
        let fp1 = fingerprint(dir, &delta_with(recs.clone()));
        let fp2 = fingerprint(dir, &delta_with(recs));
        assert_eq!(fp1, fp2);
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
        let fp_ab = fingerprint(dir, &delta_with(vec![a.clone(), b.clone()]));
        let fp_ba = fingerprint(dir, &delta_with(vec![b, a]));
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
        let fp_two = fingerprint(dir, &delta_with(vec![a.clone(), b]));
        let fp_one = fingerprint(dir, &delta_with(vec![a]));
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
        let fp_before = fingerprint(root, &delta_with(vec![rec.clone()]));
        std::fs::write(root.join("a.rs"), b"fn edited() {}").unwrap();
        let fp_after = fingerprint(root, &delta_with(vec![rec]));
        assert_ne!(
            fp_before, fp_after,
            "content change must move the fingerprint"
        );
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
        let fp_before = fingerprint(root, &delta_with(vec![rec.clone()]));

        // Mutate the target the symlink points at.
        std::fs::write(root.join("target.rs"), b"fn edited_target() {}").unwrap();
        let fp_after = fingerprint(root, &delta_with(vec![rec.clone()]));
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
        let fp_sentinel = fingerprint(root, &delta_with(vec![del]));
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
}
