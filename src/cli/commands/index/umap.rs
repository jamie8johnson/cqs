//! UMAP projection pass for `cqs index --umap`.
//!
//! Streams every chunk embedding out to `scripts/run_umap.py`, which uses
//! umap-learn to produce 2D coordinates. The script's stdout is parsed
//! line-by-line and written back to `chunks.umap_x` / `chunks.umap_y` via
//! [`Store::update_umap_coords_batch`].
//!
//! Optional pass: invoked only when the user passes `--umap`. Skipped with
//! a clear `tracing::warn!` if Python or umap-learn is unavailable so the
//! main index build still succeeds.
//!
//! The wire format between Rust and Python is documented in
//! `scripts/run_umap.py`'s module docstring; both sides keep it in sync.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use cqs::Store;

/// Baseline streaming batch size for the UMAP projection's
/// `embedding_batches` paginator at 1024-dim. At 1024-dim each batch is
/// ~4 MB; at 4096-dim it would be ~16 MB without scaling. Dim-aware scaling
/// keeps wide-dim slots from blowing heap; `CQS_UMAP_STREAM_BATCH` overrides.
const STREAM_BATCH_SIZE_BASELINE: usize = 1024;

/// Dim-aware UMAP stream batch size with env override.
fn umap_stream_batch_size(dim: usize) -> usize {
    let baseline = std::env::var("CQS_UMAP_STREAM_BATCH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(STREAM_BATCH_SIZE_BASELINE);
    cqs::limits::dim_scaled_batch(baseline, dim, 64, 8_192)
}

/// Default wall-clock ceiling for the `run_umap.py` fit subprocess, in seconds.
///
/// Generous because the fit's cost scales with corpus size: a ~17k-chunk slot
/// fits in ~20s, but a much larger corpus on a CPU-only host can legitimately
/// take minutes. The default exists only to convert a pathological *hang* (a
/// degenerate fit, a wedged BLAS thread) into a bounded skip; it is not a
/// performance target. Override with `CQS_UMAP_FIT_TIMEOUT_SECS`.
const UMAP_FIT_TIMEOUT_SECS_DEFAULT: u64 = 600;

/// Resolve the fit-subprocess wall-clock timeout, honoring
/// `CQS_UMAP_FIT_TIMEOUT_SECS`. A `0` (or unparseable/empty) value falls back
/// to the default rather than meaning "no timeout" — a zero ceiling would kill
/// every fit instantly, which is never what an operator wants.
fn umap_fit_timeout() -> std::time::Duration {
    let secs = std::env::var("CQS_UMAP_FIT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(UMAP_FIT_TIMEOUT_SECS_DEFAULT);
    std::time::Duration::from_secs(secs)
}

/// The UMAP projection script, embedded at compile time. Avoids a
/// "script not found" failure when `cqs index --umap` runs outside the
/// source tree (i.e. anywhere the installed binary is invoked). The script
/// gets written to a temp file before each invocation; the temp file is
/// dropped immediately after the subprocess exits.
const UMAP_SCRIPT: &str = include_str!("../../../../scripts/run_umap.py");

/// Owned `(chunk_id, embedding)` pairs read out of a store for projection.
type EmbeddingRows = Vec<(String, Vec<f32>)>;

/// Decision on whether the embedding read must be staged through fast local
/// disk before the projection runs.
///
/// `run_umap_projection` reads every embedding from the store via random-page
/// SQLite IO. On a slow mmap filesystem (WSL `/mnt/c` 9P / NFS / SMB) that
/// access pattern collapses — measured at hours for a ~17k-chunk slot — while
/// the same read against a fast-disk copy finishes in seconds. Staging copies
/// the live DB onto fast disk with a sequential file copy (NOT a page-walking
/// VACUUM INTO — that re-incurs the same random-page IO over v9fs), reads
/// embeddings from the copy, and writes coords back to the original (a single
/// bounded transaction — sequential write IO is fine on v9fs).
#[derive(Debug, Clone, PartialEq, Eq)]
enum StagingDecision {
    /// DB is on a fast fs — read embeddings directly, no copy.
    DirectRead,
    /// DB is on a slow fs and a fast temp dir was found — copy the DB into
    /// this dir, read from the copy.
    StageVia(PathBuf),
    /// DB is on a slow fs but no fast temp dir is available — the caller must
    /// loud-warn and skip rather than silently hang on the slow read.
    SlowNoFastDisk,
}

/// Decide how to source the embedding read for `db_path`.
///
/// Pure over `db_path` + the chosen fast temp dir, so it is unit-testable
/// without mounting a filesystem (`is_wsl_drvfs_path` is a path-shape check).
fn decide_staging(db_path: &Path) -> StagingDecision {
    if !cqs::config::is_wsl_drvfs_path(db_path) {
        return StagingDecision::DirectRead;
    }
    match pick_fast_temp_dir() {
        Some(dir) => StagingDecision::StageVia(dir),
        None => StagingDecision::SlowNoFastDisk,
    }
}

/// Pick a fast temp directory to stage the DB copy through.
///
/// Prefers `$XDG_RUNTIME_DIR` (typically a tmpfs), falling back to
/// `std::env::temp_dir()` (`/tmp`, the WSL ext4 rootfs). A candidate is
/// rejected if it is itself on a slow mmap fs — staging slow→slow would just
/// move the hang, so we return `None` and let the caller warn-and-skip rather
/// than copy onto another slow mount.
fn pick_fast_temp_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg);
        if !p.as_os_str().is_empty() {
            candidates.push(p);
        }
    }
    candidates.push(std::env::temp_dir());

    candidates.into_iter().find(|dir| {
        // The directory must exist (a stale XDG_RUNTIME_DIR pointing at a
        // missing path is useless) and must not itself be slow.
        dir.is_dir() && !cqs::config::is_wsl_drvfs_path(dir)
    })
}

/// Stream every `(id, embedding)` pair out of a store into an owned buffer.
///
/// Generic over the store mode so it can read from the original
/// `Store<ReadWrite>` (direct path) or a `Store<ReadOnly>` copy (staged path).
/// The `dim` drives the streaming batch size only.
fn collect_embeddings<Mode>(store: &Store<Mode>, dim: usize) -> Result<EmbeddingRows> {
    let stream_batch = umap_stream_batch_size(dim);
    let mut buffered: EmbeddingRows = Vec::new();
    for batch in store.embedding_batches(stream_batch) {
        let batch = batch.context("read embedding batch for UMAP")?;
        for (id, emb) in batch {
            buffered.push((id, emb.as_slice().to_vec()));
        }
    }
    Ok(buffered)
}

/// Append a sidecar suffix (`-wal` / `-shm`) to a DB path.
///
/// SQLite names sidecars by appending the suffix to the full filename, not by
/// replacing the extension: `index.db` -> `index.db-wal`, `index.db-shm`.
fn sidecar_path(db: &Path, ext: &str) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push(ext);
    PathBuf::from(s)
}

/// Sequentially copy the live DB onto fast disk and read embeddings from the
/// copy.
///
/// Returns the buffered embeddings plus the copy's `TempPath` — the caller must
/// keep the `TempPath` alive until the read is consumed, and it cleans up the
/// copy file (and its `-wal`/`-shm` sidecars, if any) on drop.
///
/// The copy is a plain sequential `std::fs::copy` of the main DB file plus its
/// `-wal`/`-shm` sidecars, NOT a `VACUUM INTO`. VACUUM INTO reads the source DB
/// page-by-page through SQLite's pager, which on a slow mmap filesystem (WSL 9P
/// / NFS / SMB) is the same random-page IO pathology the staging exists to
/// escape — measured at ~12 minutes for a ~17k-chunk slot. A sequential file
/// copy of the same 543 MB DB on v9fs takes ~3 seconds. The copy is a
/// throwaway, read-only projection source: it does not need VACUUM's rebuild or
/// backup-grade compaction.
///
/// Consistency under a concurrent daemon writer: the main DB file is copied
/// first, then the `-wal`/`-shm` sidecars. A writer committing into the WAL
/// mid-copy can leave a partial trailing frame in the copied `-wal`; SQLite's
/// WAL recovery validates frame checksums and stops at the first invalid frame,
/// so the read sees a valid (possibly slightly older) committed state, never a
/// torn/corrupt one. The narrow remaining window — a concurrent checkpoint
/// draining + truncating the source WAL between the main copy and the sidecar
/// copy — can drop a handful of just-committed rows from the copy, which for a
/// cosmetic 2D projection only means those chunks keep their prior coords. In
/// the real `cqs index --umap` flow the source is the indexer's own handle
/// under the index lock, so a competing writer is not present. The sidecars
/// are copied — rather than `wal_checkpoint(TRUNCATE)` then copy-main — to
/// avoid contending for the exclusive lock with a live daemon; the PASSIVE
/// drain below shrinks the WAL without blocking.
fn stage_and_read(
    store: &Store,
    db_path: &Path,
    fast_dir: &Path,
    dim: usize,
) -> Result<(EmbeddingRows, tempfile::TempPath)> {
    // Reserve a unique path in the fast dir. The copy below overwrites the
    // placeholder's contents; the TempPath owns cleanup of the copy (and its
    // sidecars are removed explicitly when the read is done — see below).
    let placeholder = tempfile::Builder::new()
        .prefix("cqs-umap-copy-")
        .suffix(".db")
        .tempfile_in(fast_dir)
        .with_context(|| {
            format!(
                "failed to create UMAP staging temp file in {}",
                fast_dir.display()
            )
        })?;
    let copy_path = placeholder.into_temp_path();

    // Drain the in-memory pool's buffered writes into the on-disk WAL so the
    // copied sidecars reflect committed state. PASSIVE never blocks on the
    // exclusive lock — it skips frames a concurrent reader/writer is past
    // rather than waiting — so it cannot stall against a live daemon; if it
    // can't fully drain, the sidecar copy below still captures whatever is
    // committed. A checkpoint failure is non-fatal to the copy.
    store.checkpoint_passive_best_effort();

    // Sequential copy: main DB first, then sidecars. Reading every page through
    // a sequential file copy is fast on v9fs; reading them through SQLite's
    // random-page pager (VACUUM INTO) is not.
    std::fs::copy(db_path, &copy_path).with_context(|| {
        format!(
            "failed to copy index DB {} -> {}",
            db_path.display(),
            copy_path.display()
        )
    })?;
    for ext in ["-wal", "-shm"] {
        let src_side = sidecar_path(db_path, ext);
        if src_side.exists() {
            let dst_side = sidecar_path(&copy_path, ext);
            std::fs::copy(&src_side, &dst_side).with_context(|| {
                format!(
                    "failed to copy index DB sidecar {} -> {}",
                    src_side.display(),
                    dst_side.display()
                )
            })?;
        }
    }
    tracing::info!(
        src = %db_path.display(),
        copy = %copy_path.display(),
        "UMAP: staged embedding read through fast-disk sequential copy"
    );

    // Open the copy read-only and read embeddings from it. The copy is a
    // byte-identical clone of the already-current WAL-mode DB, so a read-only
    // open needs no header write and runs no migration (the read-only open
    // path bails on a schema mismatch but here the version matches exactly).
    // Read-only keeps the projection a pure consumer — it never mutates the
    // throwaway copy, and the source DB is untouched.
    let copy_store =
        Store::<cqs::store::ReadOnly>::open_readonly(&copy_path).with_context(|| {
            format!(
                "failed to open UMAP staging copy at {}",
                copy_path.display()
            )
        })?;
    let rows = collect_embeddings(&copy_store, dim)?;
    // Drop the copy store's connections before returning so the TempPath can be
    // unlinked cleanly (no lingering fds against the copy file).
    drop(copy_store);

    // Remove the copied sidecars eagerly — the TempPath only unlinks the main
    // `.db` on drop, so without this the fast temp dir would accumulate
    // `-wal`/`-shm` orphans across repeated `--umap` runs.
    for ext in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(sidecar_path(&copy_path, ext));
    }

    Ok((rows, copy_path))
}

/// Outcome of a bounded child-process run: the wait result (or `Err` if the
/// reaping `wait()` itself failed), the captured stdout/stderr, and whether the
/// wall-clock timeout fired and killed the child.
struct ChildOutcome {
    /// `Ok(status)` once the child was reaped; `Err` only if the underlying
    /// `wait()` syscall failed (rare). When `timed_out` is true the status
    /// reflects the kill, and the caller ignores it.
    status: Result<std::process::ExitStatus>,
    stdout_buf: Vec<u8>,
    stderr_buf: Vec<u8>,
    /// True when the wall-clock deadline expired and the child was killed.
    timed_out: bool,
}

/// Run `child` to completion under a wall-clock `timeout`, capturing stdout and
/// stderr with the same byte caps the inline read used to apply.
///
/// Mechanism (std-only, no extra deps): stdout and stderr are drained on
/// dedicated threads (so a full stderr pipe can't deadlock the stdout drain),
/// and the `Child` is owned by a watchdog thread that polls `try_wait()` until
/// the child exits or the deadline passes. On the deadline it `kill()`s the
/// child; the kill closes the child's pipe write ends, so the drain threads hit
/// EOF and join. The watchdog returns the final `ExitStatus` and whether it had
/// to kill.
///
/// The poll loop (rather than a blocking `wait()` on a second thread) is what
/// keeps the watchdog cancellable: as soon as the child exits on its own the
/// next `try_wait()` observes it and the loop ends without ever touching the
/// kill path, so a healthy fit is never signalled.
fn run_child_bounded(
    mut child: std::process::Child,
    max_stdout_bytes: usize,
    timeout: std::time::Duration,
) -> ChildOutcome {
    use std::io::Read;

    // Take the pipe handles out of the child up front. They are independent
    // OS resources — owning them here lets the drain threads read while the
    // watchdog thread owns the `Child` for `try_wait`/`kill`.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_thread = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(64 * 1024);
        if let Some(s) = stdout {
            // Read one byte past the cap so the caller can detect overflow.
            let _ = s.take((max_stdout_bytes as u64) + 1).read_to_end(&mut buf);
        }
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(8 * 1024);
        if let Some(s) = stderr {
            // 1 MiB cap on stderr — operators only need the tail for diagnostics.
            let _ = s.take(1024 * 1024).read_to_end(&mut buf);
        }
        buf
    });

    // Poll for exit until the deadline; kill on expiry. The poll interval is a
    // small fixed step — cheap, and the worst-case extra wait past a natural
    // exit is one interval.
    let deadline = std::time::Instant::now() + timeout;
    let poll = std::time::Duration::from_millis(50);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // Deadline hit and the child is still running — kill it.
                    // The kill closes its stdout/stderr write ends, unblocking
                    // the drain threads. Then reap with a blocking wait().
                    let _ = child.kill();
                    timed_out = true;
                    break child.wait().map_err(|e| {
                        anyhow::anyhow!("failed to reap killed UMAP subprocess: {e}")
                    });
                }
                std::thread::sleep(poll);
            }
            Err(e) => break Err(anyhow::anyhow!("failed to wait for UMAP subprocess: {e}")),
        }
    };

    // Join the drain threads. They terminate once the child's pipes close
    // (natural exit or kill). A panicked drain thread degrades to empty output
    // rather than poisoning the whole projection.
    let stdout_buf = stdout_thread.join().unwrap_or_default();
    let stderr_buf = stderr_thread.join().unwrap_or_default();

    ChildOutcome {
        status,
        stdout_buf,
        stderr_buf,
        timed_out,
    }
}

/// Run the UMAP projection pass and write coords back to the store.
///
/// The Python script is embedded into the binary, so this works whether the
/// caller is running from the source tree or from an installed binary.
///
/// `db_path` is the on-disk path of `store`'s SQLite database. When it lives on
/// a slow mmap filesystem (WSL 9P / NFS / SMB), the embedding read is staged
/// through a fast-disk sequential copy — see [`decide_staging`] and
/// [`stage_and_read`] — because reading all embeddings via random-page SQLite
/// IO over v9fs collapses to hours. Coords are always written back to the
/// original `store`.
///
/// The fit subprocess is bounded by a wall-clock timeout
/// (`CQS_UMAP_FIT_TIMEOUT_SECS`); a pathological fit is killed and the
/// projection skipped rather than hanging the index build.
///
/// Returns the number of rows successfully updated. Empty corpora, "no Python",
/// and a fit timeout all return `Ok(0)` after logging — the index build is not
/// considered failed when the optional projection can't run.
pub(crate) fn run_umap_projection(store: &Store, db_path: &Path, quiet: bool) -> Result<usize> {
    let _span = tracing::info_span!("umap_projection").entered();

    // Materialize the embedded script to a tempfile so the Python interpreter
    // can read it. The TempPath drops at the end of this function, taking
    // the file with it — no leftover artifacts on disk.
    let mut script_file =
        tempfile::NamedTempFile::new().context("failed to create temp file for UMAP script")?;
    script_file
        .write_all(UMAP_SCRIPT.as_bytes())
        .context("failed to write UMAP script to temp file")?;
    script_file.flush().context("flush UMAP script tempfile")?;
    let script_path = script_file.into_temp_path();

    // Probe Python + umap-learn before streaming embeddings (cheap fail-fast).
    let python = match cqs::convert::find_python() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Python not found — UMAP projection skipped");
            if !quiet {
                eprintln!("  UMAP: Python not found — skipped ({e})");
            }
            return Ok(0);
        }
    };

    let probe = Command::new(&python)
        .args(["-c", "import umap, numpy"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("failed to invoke Python for UMAP dep probe")?;
    if !probe.status.success() {
        let stderr = String::from_utf8_lossy(&probe.stderr);
        tracing::warn!(
            stderr = %stderr.trim(),
            "umap-learn not installed — UMAP projection skipped (install with: pip install umap-learn)"
        );
        if !quiet {
            eprintln!("  UMAP: umap-learn not installed — skipped (pip install umap-learn)");
        }
        return Ok(0);
    }

    // Collect all (id, embedding) pairs into a binary buffer for stdin.
    // Format documented in scripts/run_umap.py; keep both in sync.
    //
    // The read of every embedding is random-page SQLite IO. On a slow mmap
    // filesystem (WSL 9P / NFS / SMB) that pattern collapses — measured at
    // hours for a ~17k-chunk slot — so stage the read through a fast-disk
    // sequential copy when the live DB is on such a mount. Coords are always
    // written back to the original `store` below (sequential write IO, fine on
    // v9fs).
    let dim = store.dim();
    let _stage_copy; // keep the TempPath alive across the read when staging
    let buffered: EmbeddingRows = match decide_staging(db_path) {
        StagingDecision::DirectRead => collect_embeddings(store, dim)?,
        StagingDecision::StageVia(fast_dir) => {
            // Copy the live DB onto fast disk, read embeddings from the copy.
            // A copy failure is not fatal: fall back to a direct (slow) read
            // with a loud warning rather than failing the whole index — but
            // warn so the operator knows why it may stall.
            match stage_and_read(store, db_path, &fast_dir, dim) {
                Ok((rows, copy_path)) => {
                    _stage_copy = copy_path;
                    rows
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        db = %db_path.display(),
                        "UMAP staging copy failed; falling back to a direct read \
                         on the slow filesystem (this may take a very long time)"
                    );
                    if !quiet {
                        eprintln!(
                            "  UMAP: could not stage to fast disk ({e}); reading directly \
                             from {} — this may stall for a long time on a slow mount. \
                             Ctrl-C and re-run from a fast disk if it hangs.",
                            db_path.display()
                        );
                    }
                    collect_embeddings(store, dim)?
                }
            }
        }
        StagingDecision::SlowNoFastDisk => {
            // DB is on a slow mmap fs and no fast temp dir is available. The
            // direct read would hang for hours (random-page IO over v9fs), so
            // skip the projection with an actionable hint rather than leave the
            // operator guessing why `cqs index --umap` never returns.
            tracing::warn!(
                db = %db_path.display(),
                "UMAP projection skipped: DB is on a slow filesystem (WSL 9P / NFS / SMB) \
                 and no fast temp disk (XDG_RUNTIME_DIR / TMPDIR) is available to stage \
                 the embedding read; the unstaged read would hang for a very long time"
            );
            if !quiet {
                eprintln!(
                    "  UMAP: skipped — {} is on a slow filesystem and no fast temp disk \
                     was found to stage through. Set XDG_RUNTIME_DIR or TMPDIR to a fast \
                     mount (tmpfs / ext4), or move the index off the slow mount, then re-run.",
                    db_path.display()
                );
            }
            return Ok(0);
        }
    };

    let n_rows = buffered.len();
    if n_rows == 0 {
        tracing::info!("UMAP projection skipped: no embeddings in corpus");
        if !quiet {
            eprintln!("  UMAP: no embeddings to project — skipped");
        }
        return Ok(0);
    }

    let id_max_len = buffered.iter().map(|(id, _)| id.len()).max().unwrap_or(0);

    // The wire format writes `n_rows`, `dim`, and `id_max_len` as
    // little-endian u32. A 64-bit host could in principle buffer more than
    // 4 billion rows / a >4 GB max id length — validate before the narrowing
    // cast so we fail loud instead of silently truncating and producing a
    // corrupt payload.
    anyhow::ensure!(
        n_rows <= u32::MAX as usize,
        "UMAP input has too many rows for wire format: {n_rows} > u32::MAX"
    );
    anyhow::ensure!(
        dim <= u32::MAX as usize,
        "UMAP embedding dim exceeds wire format: {dim} > u32::MAX"
    );
    anyhow::ensure!(
        id_max_len <= u32::MAX as usize,
        "UMAP id_max_len exceeds wire format: {id_max_len} > u32::MAX"
    );

    // The per-row size formula and the n_rows multiplication can each
    // overflow on a 64-bit host even when the individual operands fit in u32
    // (the `ensure!` block above only validates each operand). Use saturating
    // arithmetic so a pathological input bails with an explicit error instead
    // of panicking on `Vec::with_capacity` or wrapping silently and hitting
    // an out-of-bounds extend later.
    let per_row = 2usize
        .saturating_add(id_max_len)
        .saturating_add(dim.saturating_mul(4));
    let body_bytes = n_rows.saturating_mul(per_row);
    let total_capacity = body_bytes.saturating_add(12);
    anyhow::ensure!(
        total_capacity < usize::MAX,
        "UMAP payload size overflow: n_rows={} × per_row={} would exceed usize::MAX",
        n_rows,
        per_row,
    );
    let mut payload: Vec<u8> = Vec::with_capacity(total_capacity);
    payload.extend_from_slice(&(n_rows as u32).to_le_bytes());
    payload.extend_from_slice(&(dim as u32).to_le_bytes());
    payload.extend_from_slice(&(id_max_len as u32).to_le_bytes());
    for (id, emb) in &buffered {
        let id_bytes = id.as_bytes();
        if id_bytes.len() > u16::MAX as usize {
            anyhow::bail!(
                "chunk_id too long for UMAP wire format ({} bytes > 65535): {}",
                id_bytes.len(),
                id
            );
        }
        payload.extend_from_slice(&(id_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(id_bytes);
        for v in emb {
            payload.extend_from_slice(&v.to_le_bytes());
        }
    }
    drop(buffered); // free embedding memory before subprocess
    tracing::info!(
        n_rows,
        dim,
        bytes = payload.len(),
        "Invoking UMAP projection script"
    );
    if !quiet {
        eprintln!("  UMAP: projecting {n_rows} embeddings ({dim}-dim)…");
    }

    let mut child = Command::new(&python)
        .arg(&script_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn {}", script_path.display()))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .context("UMAP child process has no stdin")?;
        stdin
            .write_all(&payload)
            .context("failed to write embeddings to UMAP stdin")?;
    }
    // Drop stdin so the child sees EOF and starts the fit. `child.stdin` is an
    // Option; take it explicitly so the write end is closed before we wait.
    drop(child.stdin.take());
    drop(payload); // free wire buffer; child has it now

    // Bounded streaming read of stdout/stderr instead of
    // `wait_with_output()` (which buffers both unbounded). Stdout carries the
    // coord lines (~64 bytes per chunk × N chunks); cap at a generous ceiling
    // so pathological / hostile script output can't OOM the indexer process.
    // Default 1 GiB (sufficient for ~16M chunks at 64 bytes/line),
    // env-overridable via `CQS_UMAP_MAX_STDOUT_BYTES`.
    let max_stdout_bytes: usize = std::env::var("CQS_UMAP_MAX_STDOUT_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024 * 1024 * 1024);

    // Bound the whole fit subprocess by wall-clock time. A pathological /
    // adversarial fit (a degenerate corpus, a hung BLAS thread) otherwise
    // blocks the read below indefinitely — the staging fallbacks cover the
    // filesystem stage, but not the subprocess. On timeout the child is
    // killed, its pipes close (so the reads unblock), and we warn + return
    // Ok(0): the projection is optional, never a fatal index failure.
    let fit_timeout = umap_fit_timeout();
    let ChildOutcome {
        status,
        stdout_buf,
        stderr_buf,
        timed_out,
    } = run_child_bounded(child, max_stdout_bytes, fit_timeout);

    if timed_out {
        tracing::warn!(
            timeout_secs = fit_timeout.as_secs(),
            n_rows,
            "UMAP fit subprocess exceeded CQS_UMAP_FIT_TIMEOUT_SECS — killed; \
             skipping projection (coords left unchanged)"
        );
        if !quiet {
            eprintln!(
                "  UMAP: fit timed out after {}s ({n_rows} embeddings) — killed and \
                 skipped. Raise CQS_UMAP_FIT_TIMEOUT_SECS or project a smaller corpus.",
                fit_timeout.as_secs()
            );
        }
        return Ok(0);
    }

    let status = status.context("failed to wait for UMAP subprocess")?;
    if stdout_buf.len() > max_stdout_bytes {
        anyhow::bail!(
            "UMAP subprocess stdout exceeded CQS_UMAP_MAX_STDOUT_BYTES ({} bytes) — \
             output truncated; run with a smaller corpus or raise the cap",
            max_stdout_bytes
        );
    }

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_buf);
        anyhow::bail!(
            "UMAP projection failed (exit {}): {}",
            status,
            stderr.trim()
        );
    }

    // Echo the script's stderr at info level so per-run diagnostics surface
    // in the journal (it includes per-row coords range, which is useful
    // for spotting degenerate runs).
    let stderr = String::from_utf8_lossy(&stderr_buf);
    for line in stderr.lines() {
        tracing::info!(target: "cqs::umap", "{line}");
    }

    let stdout = String::from_utf8(stdout_buf).context("UMAP stdout is not UTF-8")?;
    let mut coords: Vec<(String, f64, f64)> = Vec::with_capacity(n_rows);
    for (lineno, line) in stdout.lines().enumerate() {
        let mut parts = line.splitn(3, '\t');
        let (Some(id), Some(x), Some(y)) = (parts.next(), parts.next(), parts.next()) else {
            anyhow::bail!(
                "UMAP stdout line {} malformed (expected 3 tab-separated fields): {}",
                lineno + 1,
                line
            );
        };
        let x: f64 = x
            .parse()
            .with_context(|| format!("UMAP stdout line {}: bad x value '{x}'", lineno + 1))?;
        let y: f64 = y
            .parse()
            .with_context(|| format!("UMAP stdout line {}: bad y value '{y}'", lineno + 1))?;
        coords.push((id.to_string(), x, y));
    }

    if coords.len() != n_rows {
        tracing::warn!(
            input = n_rows,
            output = coords.len(),
            "UMAP returned a different row count than input — partial update"
        );
    }

    let updated = store
        .update_umap_coords_batch(&coords)
        .context("failed to write UMAP coords back to store")?;
    tracing::info!(updated, total = coords.len(), "UMAP projection committed");
    if !quiet {
        eprintln!("  UMAP: wrote {updated} coordinate pairs to chunks");
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::store::ModelInfo;
    use serial_test::serial;
    use tempfile::TempDir;

    /// Spin up an empty `Store` whose dim matches a small test profile, just
    /// enough that `run_umap_projection` can hit `embedding_batches` and the
    /// empty-corpus branch. Returns the store, its db path, and the owning
    /// tempdir (kept alive by the caller).
    fn fresh_empty_store(dim: usize) -> (Store, std::path::PathBuf, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("test_umap.db");
        let mut store = Store::open(&path).expect("open store");
        store
            .init(&ModelInfo::new("test/model", dim))
            .expect("init store");
        store.set_dim(dim);
        (store, path, dir)
    }

    /// Pin the documented graceful-skip path on machines without Python. A
    /// refactor that promoted the skip to a hard error would break
    /// `cqs index --umap` on the install base without umap-learn — exactly
    /// the case the graceful skip exists for.
    ///
    /// Mutates the process-global `PATH`, so `#[serial]` to avoid races
    /// with any other test that shells out (notably the doctor / convert
    /// tests).
    #[test]
    #[serial]
    fn run_umap_projection_returns_zero_when_python_missing() {
        // Save and restore PATH; on Windows also save Path / PATHEXT.
        let saved_path = std::env::var_os("PATH");
        // Point PATH at an empty tempdir so `which` for python3/python/py
        // all fail. Tempdir lives for the duration of the test.
        let empty_dir = TempDir::new().expect("empty PATH dir");
        std::env::set_var("PATH", empty_dir.path());

        let (store, db_path, _tmp) = fresh_empty_store(8);
        let result = run_umap_projection(&store, &db_path, true);

        // Restore PATH before any assertion so a panic doesn't leak the
        // empty PATH into the rest of the suite.
        match saved_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }

        match result {
            Ok(n) => assert_eq!(n, 0, "Python-missing path must return Ok(0), got Ok({n})"),
            Err(e) => panic!(
                "Python-missing path must return Ok(0), got Err: {e:#}. \
                 The Python-not-found graceful skip is documented behavior — \
                 promoting it to a hard error breaks `cqs index --umap` on \
                 every machine without umap-learn installed."
            ),
        }
    }

    /// Empty corpus also returns Ok(0). Reachable only when Python +
    /// umap-learn are present (otherwise the earlier
    /// graceful-skip branch fires); this assertion is correct under both
    /// paths so the test is portable across the dev workstation and CI
    /// runners that lack umap-learn.
    #[test]
    fn run_umap_projection_returns_zero_for_empty_corpus() {
        let (store, db_path, _tmp) = fresh_empty_store(8);
        let result = run_umap_projection(&store, &db_path, true);
        match result {
            Ok(n) => assert_eq!(n, 0, "empty corpus must return Ok(0), got Ok({n})"),
            Err(e) => panic!(
                "empty corpus must return Ok(0), got Err: {e:#}. \
                 If this fails on a machine WITH umap-learn, the empty-corpus \
                 skip branch has regressed."
            ),
        }
    }

    /// A fast-fs DB path reads directly (no staging). The decision is pure
    /// over the path shape, so a real `/tmp` (ext4) path exercises the
    /// `DirectRead` arm without mounting anything.
    #[test]
    fn decide_staging_fast_path_reads_directly() {
        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("index.db");
        assert!(
            !cqs::config::is_wsl_drvfs_path(&db),
            "test tempdir must be on a fast fs for this assertion to be meaningful: {}",
            db.display()
        );
        assert_eq!(decide_staging(&db), StagingDecision::DirectRead);
    }

    /// A DB under a simulated WSL `/mnt/<letter>/` mount selects staging.
    /// `is_wsl_drvfs_path` is a path-shape check (no statfs), so a synthetic
    /// `/mnt/c/...` path drives the slow-fs branch on any host. With a fast
    /// temp dir available (the normal case on this workstation) the decision
    /// is `StageVia`.
    #[test]
    fn decide_staging_wsl_mount_selects_staging_when_fast_disk_available() {
        let slow_db = Path::new("/mnt/c/Projects/cqs/.cqs/slots/gemma/index.db");
        // Only meaningful when this host actually exposes a fast temp dir
        // (true on the dev workstation and CI Linux runners). If no fast disk
        // exists, the decision is SlowNoFastDisk — assert that branch instead
        // so the test is correct on every host.
        match decide_staging(slow_db) {
            StagingDecision::StageVia(dir) => {
                assert!(
                    !cqs::config::is_wsl_drvfs_path(&dir),
                    "staging dir must itself be on a fast fs, got {}",
                    dir.display()
                );
            }
            StagingDecision::SlowNoFastDisk => {
                assert!(
                    pick_fast_temp_dir().is_none(),
                    "SlowNoFastDisk only valid when no fast temp dir exists"
                );
            }
            StagingDecision::DirectRead => panic!(
                "a /mnt/<letter>/ path must be classified slow, not DirectRead — \
                 is_wsl_drvfs_path regressed on the automount-root arm"
            ),
        }
    }

    /// The fast-location selection rejects a slow temp dir. With both
    /// `XDG_RUNTIME_DIR` and `TMPDIR` pointed at a (simulated) slow WSL mount,
    /// `pick_fast_temp_dir` must return `None` rather than stage slow→slow.
    ///
    /// Mutates process-global env vars, so `#[serial]`.
    #[test]
    #[serial]
    fn pick_fast_temp_dir_rejects_slow_candidates() {
        let saved_xdg = std::env::var_os("XDG_RUNTIME_DIR");
        let saved_tmp = std::env::var_os("TMPDIR");

        // Both candidate sources point at a /mnt/<letter>/ shape, which
        // is_wsl_drvfs_path classifies slow regardless of the real fs. The
        // path need not exist — pick_fast_temp_dir's is_dir() guard then also
        // rejects it, but the slow-shape rejection is the property under test
        // (a real slow dir that DID exist would be rejected for being slow).
        std::env::set_var("XDG_RUNTIME_DIR", "/mnt/c/slow-xdg");
        std::env::set_var("TMPDIR", "/mnt/c/slow-tmp");

        let picked = pick_fast_temp_dir();

        // Restore before asserting so a panic can't leak the env into the suite.
        match saved_xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        match saved_tmp {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }

        assert_eq!(
            picked, None,
            "every candidate is on a slow mount — must refuse to stage slow→slow"
        );
    }

    /// `pick_fast_temp_dir` prefers a fast `XDG_RUNTIME_DIR` when set, and the
    /// chosen dir is never itself slow.
    ///
    /// Mutates `XDG_RUNTIME_DIR`, so `#[serial]`.
    #[test]
    #[serial]
    fn pick_fast_temp_dir_prefers_fast_xdg_runtime_dir() {
        let saved_xdg = std::env::var_os("XDG_RUNTIME_DIR");
        let fast = TempDir::new().expect("fast tempdir");
        std::env::set_var("XDG_RUNTIME_DIR", fast.path());

        let picked = pick_fast_temp_dir();

        match saved_xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }

        let picked = picked.expect("a fast XDG_RUNTIME_DIR must be selected");
        assert!(
            !cqs::config::is_wsl_drvfs_path(&picked),
            "selected dir must be fast"
        );
        assert_eq!(
            picked,
            fast.path(),
            "a fast XDG_RUNTIME_DIR must win over the temp_dir() fallback"
        );
    }

    /// The staged read produces the same rows as a direct read on the same
    /// corpus. This pins the copy path's correctness without reproducing the
    /// slow-fs hang: seed N>n_neighbors embeddings, read them directly and via
    /// a fast-disk sequential copy, and assert the two reads are identical.
    ///
    /// `stage_and_read` copies the live DB (main + `-wal`/`-shm` sidecars) and
    /// reads from the copy; `collect_embeddings` is the direct read. Equal
    /// output proves the sequential copy round-trips every embedding faithfully
    /// — including data still resident in the WAL at copy time, which the
    /// copy's read-only open replays.
    #[test]
    fn staged_read_matches_direct_read() {
        let dim = 8usize;
        let (store, db_path, _tmp) = fresh_empty_store(dim);

        // Seed a small synthetic corpus. 32 chunks is comfortably above any
        // umap n_neighbors default and large enough to span multiple
        // streaming batches at small batch sizes.
        let mut seeded: Vec<(String, Vec<f32>)> = Vec::new();
        for i in 0..32u32 {
            let id = format!("src/f{i}.rs:1:{i:08x}");
            let emb: Vec<f32> = (0..dim).map(|d| (i as f32) + (d as f32) * 0.25).collect();
            seeded.push((id, emb));
        }
        seed_chunks_with_embeddings(&store, dim, &seeded);

        let direct = collect_embeddings(&store, dim).expect("direct read");
        assert_eq!(
            direct.len(),
            seeded.len(),
            "direct read must see every seeded chunk"
        );

        let fast_dir = std::env::temp_dir();
        assert!(
            !cqs::config::is_wsl_drvfs_path(&fast_dir),
            "temp_dir must be fast for this test to stage meaningfully: {}",
            fast_dir.display()
        );
        let (staged, _copy) =
            stage_and_read(&store, &db_path, &fast_dir, dim).expect("staged read");

        // Compare by id (order-independent): both reads are rowid-ascending,
        // but sort to make the assertion robust to batch boundaries.
        let mut direct_sorted = direct.clone();
        let mut staged_sorted = staged.clone();
        direct_sorted.sort_by(|a, b| a.0.cmp(&b.0));
        staged_sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            staged_sorted, direct_sorted,
            "staged sequential-copy read must yield identical (id, embedding) rows as the direct read"
        );
    }

    /// The fast-copy staging must leave no `-wal`/`-shm` orphans in the temp
    /// dir after the read. `stage_and_read` removes the copied sidecars
    /// eagerly (the `TempPath` only owns the main `.db`); a regression there
    /// would silently accumulate files across repeated `--umap` runs.
    #[test]
    fn staged_copy_cleans_up_sidecars() {
        let dim = 8usize;
        let (store, db_path, _tmp) = fresh_empty_store(dim);
        let seeded: Vec<(String, Vec<f32>)> = (0..16u32)
            .map(|i| {
                (
                    format!("src/g{i}.rs:1:{i:08x}"),
                    (0..dim).map(|d| (i as f32) + (d as f32)).collect(),
                )
            })
            .collect();
        seed_chunks_with_embeddings(&store, dim, &seeded);

        let fast_dir = std::env::temp_dir();
        let (_rows, copy_path) =
            stage_and_read(&store, &db_path, &fast_dir, dim).expect("staged read");

        // The main copy file still exists (owned by the TempPath), but neither
        // sidecar should survive the read.
        assert!(copy_path.exists(), "copy .db must still exist before drop");
        assert!(
            !sidecar_path(&copy_path, "-wal").exists(),
            "copy -wal sidecar must be cleaned up after the read"
        );
        assert!(
            !sidecar_path(&copy_path, "-shm").exists(),
            "copy -shm sidecar must be cleaned up after the read"
        );
    }

    /// Seed `chunks` rows carrying embeddings so `embedding_batches` returns
    /// them. Builds a minimal `Chunk` per row and upserts through the store's
    /// own batch path (the same write path the indexer uses).
    fn seed_chunks_with_embeddings(store: &Store, dim: usize, rows: &[(String, Vec<f32>)]) {
        use cqs::parser::{Chunk, ChunkType, Language};
        use cqs::Embedding;
        use std::path::PathBuf;

        let batch: Vec<(Chunk, Embedding)> = rows
            .iter()
            .enumerate()
            .map(|(i, (id, emb))| {
                let content = format!("fn f{i}() {{}}");
                let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                let chunk = Chunk {
                    id: id.clone(),
                    file: PathBuf::from(format!("src/f{i}.rs")),
                    language: Language::Rust,
                    chunk_type: ChunkType::Function,
                    name: format!("f{i}"),
                    signature: format!("fn f{i}()"),
                    content,
                    doc: None,
                    line_start: 1,
                    line_end: 1,
                    byte_start: 0,
                    content_hash,
                    canonical_hash: String::new(),
                    parent_id: None,
                    window_idx: None,
                    parent_type_name: None,
                    parser_version: 0,
                };
                // Pad/truncate the synthetic embedding to the store dim so the
                // wire bytes match what `embedding_batches` expects.
                let mut v = emb.clone();
                v.resize(dim, 0.0);
                (chunk, Embedding::new(v))
            })
            .collect();
        store
            .upsert_chunks_batch(&batch, Some(1))
            .expect("seed chunks with embeddings");
    }

    /// `umap_fit_timeout` honors `CQS_UMAP_FIT_TIMEOUT_SECS`, falls back to the
    /// default when unset/empty/garbage, and rejects `0` (which would kill
    /// every fit instantly) in favor of the default.
    ///
    /// Mutates a process-global env var, so `#[serial]`.
    #[test]
    #[serial]
    fn umap_fit_timeout_honors_env_and_rejects_zero() {
        let saved = std::env::var_os("CQS_UMAP_FIT_TIMEOUT_SECS");

        std::env::remove_var("CQS_UMAP_FIT_TIMEOUT_SECS");
        assert_eq!(
            umap_fit_timeout().as_secs(),
            UMAP_FIT_TIMEOUT_SECS_DEFAULT,
            "unset must use the default"
        );

        std::env::set_var("CQS_UMAP_FIT_TIMEOUT_SECS", "42");
        assert_eq!(umap_fit_timeout().as_secs(), 42, "set value must win");

        std::env::set_var("CQS_UMAP_FIT_TIMEOUT_SECS", "0");
        assert_eq!(
            umap_fit_timeout().as_secs(),
            UMAP_FIT_TIMEOUT_SECS_DEFAULT,
            "0 must fall back to the default, not mean an instant-kill ceiling"
        );

        std::env::set_var("CQS_UMAP_FIT_TIMEOUT_SECS", "garbage");
        assert_eq!(
            umap_fit_timeout().as_secs(),
            UMAP_FIT_TIMEOUT_SECS_DEFAULT,
            "garbage must fall back to the default"
        );

        match saved {
            Some(v) => std::env::set_var("CQS_UMAP_FIT_TIMEOUT_SECS", v),
            None => std::env::remove_var("CQS_UMAP_FIT_TIMEOUT_SECS"),
        }
    }

    /// A child that exits promptly is reaped by `run_child_bounded` with its
    /// stdout captured and `timed_out == false` — the timeout path never
    /// fires for a healthy fit.
    #[cfg(unix)]
    #[test]
    fn run_child_bounded_captures_fast_child_without_timeout() {
        let child = Command::new("sh")
            .args(["-c", "printf 'hello-umap'"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fast child");

        let outcome = run_child_bounded(child, 1024 * 1024, std::time::Duration::from_secs(30));
        assert!(!outcome.timed_out, "a fast child must not be timed out");
        let status = outcome.status.expect("status must be Ok");
        assert!(status.success(), "fast child must exit 0");
        assert_eq!(
            outcome.stdout_buf, b"hello-umap",
            "stdout must be captured verbatim"
        );
    }

    /// A child that hangs past the wall-clock deadline is killed and reported
    /// with `timed_out == true`. Uses a sub-second timeout against a long
    /// `sleep` so the test itself stays fast. Pins the fit-subprocess
    /// robustness contract: a pathological fit can never hang the indexer.
    #[cfg(unix)]
    #[test]
    fn run_child_bounded_kills_hung_child_on_timeout() {
        let start = std::time::Instant::now();
        // `sleep 30` never exits within the test window; the watchdog must kill
        // it. We keep its stdout piped but it never writes, so the only way the
        // drain threads finish is the kill closing the pipe.
        let child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn hung child");

        let outcome = run_child_bounded(child, 1024 * 1024, std::time::Duration::from_millis(300));
        assert!(
            outcome.timed_out,
            "a child that outlives the deadline must be reported timed_out"
        );
        // The kill + reap must happen quickly — nowhere near the 30s sleep.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "timeout path must kill promptly, took {:?}",
            start.elapsed()
        );
    }
}
