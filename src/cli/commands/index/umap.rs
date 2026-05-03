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
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use cqs::Store;

const STREAM_BATCH_SIZE: usize = 1024;

/// The UMAP projection script, embedded at compile time. Avoids a
/// "script not found" failure when `cqs index --umap` runs outside the
/// source tree (i.e. anywhere the installed binary is invoked). The script
/// gets written to a temp file before each invocation; the temp file is
/// dropped immediately after the subprocess exits.
const UMAP_SCRIPT: &str = include_str!("../../../../scripts/run_umap.py");

/// Run the UMAP projection pass and write coords back to the store.
///
/// The Python script is embedded into the binary, so this works whether the
/// caller is running from the source tree or from an installed binary.
///
/// Returns the number of rows successfully updated. Empty corpora and
/// "no Python" both return `Ok(0)` after logging — the index build is not
/// considered failed when the optional projection can't run.
pub(crate) fn run_umap_projection(store: &Store, quiet: bool) -> Result<usize> {
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
    let dim = store.dim();
    let mut buffered: Vec<(String, Vec<f32>)> = Vec::new();
    for batch in store.embedding_batches(STREAM_BATCH_SIZE) {
        let batch = batch.context("read embedding batch for UMAP")?;
        for (id, emb) in batch {
            buffered.push((id, emb.as_slice().to_vec()));
        }
    }

    let n_rows = buffered.len();
    if n_rows == 0 {
        tracing::info!("UMAP projection skipped: no embeddings in corpus");
        if !quiet {
            eprintln!("  UMAP: no embeddings to project — skipped");
        }
        return Ok(0);
    }

    let id_max_len = buffered.iter().map(|(id, _)| id.len()).max().unwrap_or(0);

    // RB-V1.29-2: the wire format writes `n_rows`, `dim`, and `id_max_len`
    // as little-endian u32. A 64-bit host could in principle buffer more
    // than 4 billion rows / a >4 GB max id length — validate before the
    // narrowing cast so we fail loud instead of silently truncating and
    // producing a corrupt payload.
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

    let mut payload: Vec<u8> = Vec::with_capacity(12 + n_rows * (2 + id_max_len + dim * 4));
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
    drop(payload); // free wire buffer; child has it now

    let output = child
        .wait_with_output()
        .context("failed to wait for UMAP subprocess")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "UMAP projection failed (exit {}): {}",
            output.status,
            stderr.trim()
        );
    }

    // Echo the script's stderr at info level so per-run diagnostics surface
    // in the journal (it includes per-row coords range, which is useful
    // for spotting degenerate runs).
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        tracing::info!(target: "cqs::umap", "{line}");
    }

    let stdout = String::from_utf8(output.stdout).context("UMAP stdout is not UTF-8")?;
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
    /// empty-corpus branch.
    fn fresh_empty_store(dim: usize) -> (Store, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("test_umap.db");
        let mut store = Store::open(&path).expect("open store");
        store
            .init(&ModelInfo::new("test/model", dim))
            .expect("init store");
        store.set_dim(dim);
        (store, dir)
    }

    /// #1357 / TC-HAP-V1.33-8: pin the documented graceful-skip path on
    /// machines without Python. Pre-test, a future refactor that promoted
    /// the skip to a hard error would silently break `cqs index --umap` on
    /// the install base that doesn't have umap-learn — exactly the case the
    /// graceful skip exists for.
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

        let (store, _tmp) = fresh_empty_store(8);
        let result = run_umap_projection(&store, true);

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
                 The graceful-skip path at umap.rs:53-60 is documented \
                 behavior — promoting it to a hard error breaks `cqs index \
                 --umap` on every machine without umap-learn installed."
            ),
        }
    }

    /// #1357 / TC-HAP-V1.33-8: empty corpus also returns Ok(0). Reachable
    /// only when Python + umap-learn are present (otherwise the earlier
    /// graceful-skip branch fires); this assertion is correct under both
    /// paths so the test is portable across the dev workstation and CI
    /// runners that lack umap-learn.
    #[test]
    fn run_umap_projection_returns_zero_for_empty_corpus() {
        let (store, _tmp) = fresh_empty_store(8);
        let result = run_umap_projection(&store, true);
        match result {
            Ok(n) => assert_eq!(n, 0, "empty corpus must return Ok(0), got Ok({n})"),
            Err(e) => panic!(
                "empty corpus must return Ok(0), got Err: {e:#}. \
                 If this fails on a machine WITH umap-learn, the empty-corpus \
                 branch at umap.rs:94-100 has regressed."
            ),
        }
    }
}
