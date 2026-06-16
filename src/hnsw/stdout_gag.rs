//! Cross-platform, best-effort suppression of process stdout (fd 1) for the
//! duration of a tightly-scoped call.
//!
//! ## Why this exists
//!
//! `hnsw_rs 0.3.4`'s `Hnsw::modify_level_scale` unconditionally `println!`s a
//! `"Current scale value : ..."` diagnostic line to the process stdout every
//! time it is called. cqs applies [`crate::hnsw::LEVEL_SCALE_FACTOR`] via that
//! method at HNSW *construction*, before any point is inserted, on every build
//! path. The line therefore lands on the process stdout for every index build.
//!
//! For commands that emit machine-readable JSON to stdout (notably `cqs gc
//! --json`, which rebuilds the HNSW after pruning and then writes its JSON
//! summary), that stray line corrupts the JSON contract. Suppressing fd 1
//! around the `modify_level_scale` call removes the leak at its source while
//! leaving everything else cqs writes to stdout untouched.
//!
//! ## Why a hand-rolled redirect, not the `gag` crate, for the production path
//!
//! The `gag` crate is *nix-only ("Currently only *nix operating systems are
//! supported"). Using it on the production suppression path would silently
//! no-op (or fail to build) on Windows — exactly the cross-platform footgun we
//! must avoid. This redirect uses `libc`'s POSIX-style fd primitives, which the
//! `libc` crate exposes on BOTH unix and Windows (the Windows bindings map them
//! onto the MSVCRT lowio `_open`/`_dup`/`_dup2`/`_close` layer; fd 1 is stdout
//! in the CRT just as on unix). It mirrors the existing libc cfg-split style in
//! `config.rs` and degrades to a graceful no-op on any other platform.
//!
//! ## Contract
//!
//! - **Best-effort**: any syscall failure yields a no-op guard. Suppression
//!   never fails the caller — at worst the diagnostic line leaks (functional,
//!   just noisy), never a panic or aborted build.
//! - **RAII restore on every exit, including panic**: restoration happens in
//!   [`Drop`], so stdout is restored even if the gagged call unwinds.
//! - **Tight scope**: the guard redirects fd 1 to the platform null device for
//!   only as long as it is held. cqs's tracing/logging goes to *stderr*
//!   (`main.rs` wires `tracing_subscriber` with `.with_writer(std::io::stderr)`),
//!   so a stdout-only gag never eats logs.
//! - **Single-threaded scope assumption**: fd 1 is process-global. The gag is
//!   held only around the microsecond `modify_level_scale` call at graph
//!   construction, before any inserts and with no concurrent stdout writers on
//!   the build path, so the global-fd swap is safe in context.

/// RAII guard that redirects the process stdout (fd 1) to the platform null
/// device while held, restoring the original fd on drop.
///
/// Construct via [`StdoutGag::new`]; it is always safe to ignore the result and
/// proceed (the guard is best-effort). Hold the returned guard across the call
/// whose stdout you want suppressed, then let it drop.
#[must_use = "the gag only suppresses stdout while the guard is held; dropping it restores stdout"]
pub(crate) struct StdoutGag {
    /// The saved duplicate of the original fd 1, restored on drop. `None` when
    /// installation failed (a no-op guard) or after restore has run.
    saved_fd: Option<RawFd>,
}

/// CRT/POSIX file descriptor type. `libc::c_int` on every platform where the
/// redirect is wired (unix + Windows); a plain `i32` placeholder elsewhere so
/// the struct still type-checks on the no-op fallback.
#[cfg(any(unix, windows))]
type RawFd = libc::c_int;
#[cfg(not(any(unix, windows)))]
type RawFd = i32;

impl StdoutGag {
    /// Install the gag: flush any pending buffered stdout (so legitimate output
    /// already queued is not trapped behind the redirect), then point fd 1 at
    /// the null device. Returns a guard that restores fd 1 on drop.
    ///
    /// On any failure — or on a platform without a supported mechanism — returns
    /// a no-op guard (stdout is left untouched). Never panics.
    pub(crate) fn new() -> Self {
        // Flush Rust's stdout buffer so anything already written goes out
        // *before* we redirect the underlying fd, and isn't swallowed.
        use std::io::Write;
        let _ = std::io::stdout().flush();
        Self {
            saved_fd: install(),
        }
    }
}

impl Drop for StdoutGag {
    fn drop(&mut self) {
        if let Some(saved) = self.saved_fd.take() {
            restore(saved);
        }
    }
}

// ── unix + windows ───────────────────────────────────────────────────────────
//
// `libc` exposes POSIX-style `open`/`dup`/`dup2`/`close` and `O_WRONLY` on both
// unix and Windows (the Windows bindings wrap the MSVCRT lowio `_open` etc.), so
// a single implementation covers both. The platform-specific detail is only the
// null-device path: "/dev/null" on unix, "NUL" on Windows.
#[cfg(any(unix, windows))]
fn install() -> Option<RawFd> {
    const STDOUT_FD: RawFd = 1;

    #[cfg(unix)]
    let devnull_path = c"/dev/null";
    #[cfg(windows)]
    let devnull_path = c"NUL";

    // Duplicate the current fd 1 so we can restore it on drop.
    let saved = unsafe { libc::dup(STDOUT_FD) };
    if saved < 0 {
        return None;
    }
    // Open the null device for writing.
    let devnull = unsafe { libc::open(devnull_path.as_ptr(), libc::O_WRONLY) };
    if devnull < 0 {
        // Nothing was redirected yet; release the saved dup and bail.
        unsafe {
            libc::close(saved);
        }
        return None;
    }
    // Point fd 1 at the null device. On failure, undo and bail.
    let rc = unsafe { libc::dup2(devnull, STDOUT_FD) };
    unsafe {
        libc::close(devnull);
    }
    if rc < 0 {
        unsafe {
            libc::close(saved);
        }
        return None;
    }
    Some(saved)
}

#[cfg(any(unix, windows))]
fn restore(saved: RawFd) {
    const STDOUT_FD: RawFd = 1;
    // Best-effort: even if dup2 fails we still close the saved fd to avoid
    // leaking it. There is nothing actionable to do on a restore failure.
    unsafe {
        libc::dup2(saved, STDOUT_FD);
        libc::close(saved);
    }
}

// ── unsupported platforms ────────────────────────────────────────────────────
//
// No supported redirect mechanism: degrade to a no-op guard. The diagnostic
// line leaks but the build stays functional, honoring the best-effort contract.
#[cfg(not(any(unix, windows)))]
fn install() -> Option<RawFd> {
    None
}

#[cfg(not(any(unix, windows)))]
fn restore(_saved: RawFd) {}
