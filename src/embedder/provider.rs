//! ORT execution provider detection and session creation
//!
//! Handles CUDA/TensorRT provider discovery, symlink setup for provider
//! libraries, and ONNX session creation with the best available provider.

use once_cell::sync::OnceCell;
use ort::ep::ExecutionProvider as OrtExecutionProvider;
use ort::session::Session;
use std::path::{Path, PathBuf};

use super::{EmbedderError, ExecutionProvider};
use crate::ort_helpers::ort_err;

/// Ensure ORT CUDA provider libraries are findable (Unix only)
/// ORT's C++ runtime resolves provider paths via `dladdr` -> `argv[0]`.
/// With static linking and PATH invocation, `argv[0]` is the bare binary
/// name (e.g., "cqs"), so ORT constructs `absolute("cqs").remove_filename()`
/// = CWD. Providers must exist there for `dlopen` to succeed.
/// Strategy: compute the same directory ORT will search (from argv[0]),
/// and create symlinks from the ORT cache there.
///
/// PB-5 / #856: we previously registered a libc::atexit handler to
/// unlink the symlinks here, but that function called Mutex::lock()
/// which is UB after the Rust allocator has been torn down (and panics
/// on poisoned mutex → unwinding into C is also UB). The symlinks are
/// now left in place on process exit; they get overwritten on the
/// next run (ORT provider resolution is deterministic per cqs version).
/// If stale-file accumulation becomes a concern, add a startup-time
/// GC pass instead of a shutdown-time one.
#[cfg(target_os = "linux")]
fn ensure_ort_provider_libs() {
    let ort_lib_dir = match find_ort_provider_dir() {
        Some(d) => d,
        None => return,
    };

    let provider_libs = [
        "libonnxruntime_providers_shared.so",
        "libonnxruntime_providers_cuda.so",
        "libonnxruntime_providers_tensorrt.so",
    ];

    // Compute the directory ORT's GetRuntimePath() will resolve to.
    // ORT does: dladdr() -> dli_fname (= argv[0] on glibc) ->
    //   std::filesystem::absolute(dli_fname).remove_filename()
    // For PATH invocation: argv[0]="cqs" -> absolute = CWD/"cqs" -> parent = CWD
    let ort_search_dir = match ort_runtime_search_dir() {
        Some(d) => d,
        None => return,
    };

    symlink_providers(&ort_lib_dir, &ort_search_dir, &provider_libs);

    // Also symlink into LD_LIBRARY_PATH for other search paths
    if let Some(ld_dir) = find_ld_library_dir(&ort_lib_dir) {
        symlink_providers(&ort_lib_dir, &ld_dir, &provider_libs);
    }
}

/// Compute the directory ORT's GetRuntimePath() will resolve to.
/// Reproduces ORT's logic: `dladdr` returns `dli_fname = argv[0]` (glibc),
/// then `std::filesystem::absolute(dli_fname).remove_filename()`.
#[cfg(target_os = "linux")]
fn ort_runtime_search_dir() -> Option<PathBuf> {
    // Read argv[0] the same way glibc's dladdr does
    let cmdline = std::fs::read("/proc/self/cmdline").ok()?;
    let argv0_end = cmdline.iter().position(|&b| b == 0)?;
    let argv0 = std::str::from_utf8(&cmdline[..argv0_end]).ok()?;

    // If argv[0] is already absolute, parent is the binary's directory
    let abs_path = if argv0.starts_with('/') {
        PathBuf::from(argv0)
    } else {
        // Relative: resolve against CWD (same as std::filesystem::absolute)
        std::env::current_dir().ok()?.join(argv0)
    };

    abs_path.parent().map(|p| p.to_path_buf())
}

/// Find the ORT provider library cache directory
#[cfg(target_os = "linux")]
fn find_ort_provider_dir() -> Option<PathBuf> {
    let cache_dir = dirs::cache_dir()?;
    let triplet = match std::env::consts::ARCH {
        "x86_64" => "x86_64-unknown-linux-gnu",
        "aarch64" => "aarch64-unknown-linux-gnu",
        _ => return None,
    };
    let ort_cache = cache_dir.join(format!("ort.pyke.io/dfbin/{triplet}"));

    match std::fs::read_dir(&ort_cache) {
        Ok(entries) => {
            // PB-31: Sort descending by name to pick the latest version deterministically
            let mut dirs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect();
            dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
            dirs.into_iter().next()
        }
        Err(e) => {
            tracing::debug!(path = %ort_cache.display(), error = %e, "ORT cache not found");
            None
        }
    }
}

/// Find a writable directory from LD_LIBRARY_PATH (excluding the ORT cache)
///
/// P3.34 — Platform scope:
/// On Linux this walks `LD_LIBRARY_PATH` (`:`-separated) and symlinks ORT
/// provider `.so` files into the runtime's search dir. On Windows and macOS
/// provider DLL/dylib resolution is delegated entirely to ORT's loader
/// (Windows: `PATH` search; macOS: `DYLD_*` paths). If a future regression
/// surfaces on those platforms, add an arm with `;`-split for `PATH` (Win)
/// or `DYLD_LIBRARY_PATH` (mac).
#[cfg(target_os = "linux")]
fn find_ld_library_dir(ort_lib_dir: &Path) -> Option<PathBuf> {
    let ld_path = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let ort_cache_str = ort_lib_dir.to_string_lossy();
    ld_path
        .split(':')
        .find(|p| !p.is_empty() && Path::new(p).is_dir() && !ort_cache_str.starts_with(p))
        .map(PathBuf::from)
}

/// Create symlinks for provider libraries in the target directory
#[cfg(target_os = "linux")]
fn symlink_providers(src_dir: &Path, target_dir: &Path, libs: &[&str]) {
    for lib in libs {
        let src = src_dir.join(lib);
        let dst = target_dir.join(lib);

        if !src.exists() {
            continue;
        }

        // Skip if symlink already points to the right place.
        // Canonicalize both paths so relative vs absolute and symlink chains
        // don't cause false mismatches (PB-10).
        if let Ok(existing) = std::fs::read_link(&dst) {
            let existing_canon = dunce::canonicalize(&existing).unwrap_or(existing);
            let src_canon = dunce::canonicalize(&src).unwrap_or_else(|_| src.clone());
            if existing_canon == src_canon {
                continue;
            }
            let _ = std::fs::remove_file(&dst);
        }

        if let Err(e) = std::os::unix::fs::symlink(&src, &dst) {
            tracing::debug!(lib = %lib, error = %e, "Failed to symlink");
        }
    }
}

/// No-op on non-Linux platforms (CUDA provider libs handled differently)
#[cfg(not(target_os = "linux"))]
fn ensure_ort_provider_libs() {
    // No-op: Windows and other platforms find CUDA/TensorRT provider libraries
    // via PATH, so no symlinking is needed. The Unix version symlinks .so files
    // into ort's search directory because LD_LIBRARY_PATH may not include them.
    tracing::debug!(
        "Provider library setup not implemented for this platform — GPU may not activate"
    );
}

/// Resolve the on-disk cache directory for TensorRT engine binaries +
/// timing tactic results, or return `None` when caching is opted out.
///
/// TRT compiles each ONNX graph into a hardware-specific engine on
/// first use — 4-5 s for SPLADE-Code (33M params), 30-90 s for
/// BGE-large (340M). Without persistent caching the daemon pays the
/// compile cost on every restart. With caching, the engine is reused
/// until any identity input (model bytes, GPU SM, TRT version) changes,
/// at which point TRT silently re-compiles and overwrites the cached
/// blob.
///
/// Cache layout under `~/.cache/cqs/trt-engine-cache/`:
/// - `TensorrtExecutionProvider_TRTKernel_*.engine` — compiled engines
/// - `TensorrtExecutionProvider.cache` — timing tactic results
///
/// Both are safe to delete; missing files cause TRT to re-compile
/// transparently. Cache directory is created on demand.
///
/// Set `CQS_TRT_ENGINE_CACHE=0` to disable persistence — the helper
/// returns `None` and `create_session` falls through to a no-cache
/// TensorRT builder. Useful when validating that a driver upgrade
/// invalidated the cache, or to force a clean compile after a known
/// regression.
fn trt_cache_dir() -> Option<PathBuf> {
    if std::env::var("CQS_TRT_ENGINE_CACHE").as_deref() == Ok("0") {
        return None;
    }
    let dir = dirs::cache_dir()?.join("cqs").join("trt-engine-cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Cached GPU provider detection result
static CACHED_PROVIDER: OnceCell<ExecutionProvider> = OnceCell::new();

/// Select the best available execution provider (cached)
/// Provider detection is expensive (checks CUDA/TensorRT availability).
/// Result is cached in a static OnceCell for subsequent calls.
pub(crate) fn select_provider() -> ExecutionProvider {
    *CACHED_PROVIDER.get_or_init(detect_provider)
}

/// Detect the best available execution provider.
///
/// Issue #956 (Phase A): probe order moves into a series of cfg-gated
/// blocks rather than a single hardcoded CUDA → TensorRT → CPU chain.
/// Each block is compiled out entirely when its `ep-*` cargo feature
/// is off, so a build with `ep-coreml` disabled has no CoreML branch
/// in the binary at all. CUDA + TensorRT are always-on today because
/// the `ort` dep enables them unconditionally on Linux/Windows.
///
/// Probe order: TensorRT → CUDA → CoreML → ROCm → CPU. TensorRT goes
/// before CUDA so an op-fallback-to-CUDA TensorRT session is preferred
/// over CUDA-only on hardware that has both. Non-NVIDIA backends are
/// last because they're only ever the right pick when CUDA is absent.
fn detect_provider() -> ExecutionProvider {
    let _span = tracing::info_span!("detect_provider").entered();

    // Ensure provider libs are findable before checking availability.
    ensure_ort_provider_libs();

    // ── NVIDIA TensorRT ────────────────────────────────────────────
    //
    // `CQS_DISABLE_TENSORRT=1` skips the TRT probe entirely (falls
    // through to CUDA). Useful when a model's ONNX graph uses ops
    // TensorRT can't compile — e.g. Gemma3's bidirectional-attention
    // head emits a plugin op TRT 10 doesn't recognise, and
    // `create_session` fails at engine build time. CUDA's op coverage
    // is broader (it falls back to ORT's own kernel for unknown ops),
    // so it accepts more graph shapes at the cost of TRT's specific
    // perf wins.
    if std::env::var("CQS_DISABLE_TENSORRT").as_deref() != Ok("1") {
        use ort::ep::TensorRT;
        let tensorrt = TensorRT::default();
        if tensorrt.is_available().unwrap_or(false) {
            let provider = ExecutionProvider::TensorRT { device_id: 0 };
            tracing::info!(provider = ?provider, "Execution provider selected");
            return provider;
        }
    } else {
        tracing::info!("CQS_DISABLE_TENSORRT=1 — skipping TensorRT probe");
    }

    // ── NVIDIA CUDA ────────────────────────────────────────────────
    {
        use ort::ep::CUDA;
        let cuda = CUDA::default();
        if cuda.is_available().unwrap_or(false) {
            let provider = ExecutionProvider::CUDA { device_id: 0 };
            tracing::info!(provider = ?provider, "Execution provider selected");
            return provider;
        }
    }

    // ── Apple CoreML ───────────────────────────────────────────────
    // Phase B will add the actual `ort::ep::CoreML` probe here once
    // the target-conditional `ort/coreml` feature is wired. For now
    // the cfg gates exist so adding the probe is a one-block change.
    #[cfg(feature = "ep-coreml")]
    {
        // TODO(#956 Phase B): replace with `ort::ep::CoreML::default()
        // .is_available()` once the macOS target adds `ort/coreml` to
        // the dep features. Today the ort crate isn't compiled with
        // CoreML support so the type doesn't exist.
        tracing::warn!(
            "ep-coreml feature is enabled but the CoreML provider isn't wired yet \
             (Phase B). Falling through to next backend."
        );
    }

    // ── AMD ROCm ───────────────────────────────────────────────────
    #[cfg(feature = "ep-rocm")]
    {
        // TODO(#956 Phase C): replace with `ort::ep::ROCm::default()
        // .is_available()` once the `ort/rocm` feature is wired and
        // tested on AMD hardware.
        tracing::warn!(
            "ep-rocm feature is enabled but the ROCm provider isn't wired yet \
             (Phase C). Falling through to next backend."
        );
    }

    // ── CPU fallback (always available) ────────────────────────────
    let provider = ExecutionProvider::CPU;
    tracing::info!(provider = ?provider, "Execution provider selected");
    provider
}

/// Create an ort session with the specified provider.
///
/// Issue #956 (Phase A): non-NVIDIA arms are cfg-gated to mirror the
/// `ExecutionProvider` enum. CUDA and TensorRT are always compiled in;
/// CoreML and ROCm arms exist only when their `ep-*` features are on,
/// which is the same condition under which their enum variants exist.
pub(crate) fn create_session(
    model_path: &Path,
    provider: ExecutionProvider,
) -> Result<Session, EmbedderError> {
    let _span = tracing::info_span!("create_session", provider = ?provider).entered();
    use ort::ep::{TensorRT, CUDA};

    tracing::info!(provider = ?provider, model_path = %model_path.display(), "Creating ONNX session");

    let mut builder = Session::builder().map_err(ort_err)?;

    let session = match provider {
        ExecutionProvider::CUDA { device_id } => builder
            .with_execution_providers([CUDA::default().with_device_id(device_id).build()])
            .map_err(ort_err)?
            .commit_from_file(model_path)
            .map_err(ort_err)?,
        ExecutionProvider::TensorRT { device_id } => {
            // TRT compiles each ONNX model into an engine on first use,
            // taking 5 s for SPLADE-Code (33M params) and 30-90 s for
            // BGE-large (340M). Persisting the compiled engine + the
            // optimization tactic timing cache to disk amortizes that
            // cost to once-per-(model, GPU, TRT version) instead of
            // once-per-daemon-restart. Cache is invalidated automatically
            // when any of those identity inputs change.
            //
            // Set `CQS_TRT_ENGINE_CACHE=0` to opt out (forces re-compile
            // every session — useful when validating that a driver
            // upgrade did invalidate the cache).
            let mut tensorrt = TensorRT::default().with_device_id(device_id);
            if let Some(cache_dir) = trt_cache_dir() {
                let cache_str = cache_dir.to_string_lossy().into_owned();
                tensorrt = tensorrt
                    .with_engine_cache(true)
                    .with_engine_cache_path(cache_str.clone())
                    .with_timing_cache(true)
                    .with_timing_cache_path(cache_str);
            }
            builder
                .with_execution_providers([
                    tensorrt.build(),
                    // Fallback to CUDA for unsupported ops
                    CUDA::default().with_device_id(device_id).build(),
                ])
                .map_err(ort_err)?
                .commit_from_file(model_path)
                .map_err(ort_err)?
        }
        // Phase B/C arms: today these are unreachable because
        // `detect_provider()` never returns the new variants — but the
        // match must stay exhaustive once the variants exist. When
        // Phase B wires `ort::ep::CoreML`, replace the `unreachable!()`
        // with the real builder call.
        #[cfg(feature = "ep-coreml")]
        ExecutionProvider::CoreML => {
            return Err(EmbedderError::InferenceFailed(
                "CoreML provider not wired yet (#956 Phase B)".to_string(),
            ));
        }
        #[cfg(feature = "ep-rocm")]
        ExecutionProvider::ROCm { .. } => {
            return Err(EmbedderError::InferenceFailed(
                "ROCm provider not wired yet (#956 Phase C)".to_string(),
            ));
        }
        ExecutionProvider::CPU => builder.commit_from_file(model_path).map_err(ort_err)?,
    };

    Ok(session)
}

#[cfg(test)]
mod tests {
    //! P3.53 — direct coverage for the post-#1120 provider split.
    //!
    //! `select_provider` and `detect_provider` were added by the
    //! ExecutionProvider feature split (issue #956 Phase A) but never
    //! gained dedicated tests. The cache-on-first-call invariant
    //! (`OnceCell` semantics) and the cfg-gated probe order are the
    //! contracts most likely to silently regress under future feature
    //! splits — pin them here.
    use super::*;

    /// Shared mutex for tests that mutate process-global env vars
    /// (`LD_LIBRARY_PATH`, `CQS_TRT_ENGINE_CACHE`). Each env-mutating
    /// test takes this lock before any `env::set_var` / `env::remove_var`
    /// and releases it after restoring the prior value, guaranteeing
    /// linearization across parallel test threads.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `select_provider` must be idempotent: subsequent calls return the
    /// same `ExecutionProvider` value (OnceCell semantics). Hardware-agnostic;
    /// only checks consistency, not which provider was selected.
    #[test]
    fn select_provider_caches_first_call() {
        let p1 = select_provider();
        let p2 = select_provider();
        // ExecutionProvider isn't Eq/PartialEq, so compare by Debug repr —
        // every variant carries enough info in Debug to detect drift.
        assert_eq!(
            format!("{p1:?}"),
            format!("{p2:?}"),
            "select_provider must return the same value on repeated calls"
        );
    }

    /// P3.16: `find_ld_library_dir` must skip empty entries (`::` and
    /// trailing `:`), reject paths whose first component matches the ORT
    /// cache, and only return entries that exist on disk. Pinned via
    /// `LD_LIBRARY_PATH = ":/tmp:"` — `/tmp` is the only non-empty,
    /// non-ORT-cache entry that's guaranteed to exist on Unix CI.
    #[cfg(target_os = "linux")]
    #[test]
    fn find_ld_library_dir_skips_empty_entries() {
        use std::env;
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = env::var_os("LD_LIBRARY_PATH");
        // SAFETY: this test holds ENV_LOCK; no other test in this module
        // touches LD_LIBRARY_PATH concurrently.
        unsafe {
            env::set_var("LD_LIBRARY_PATH", ":/tmp:");
        }
        // Use a dummy ORT cache path that doesn't overlap with any
        // realistic LD_LIBRARY_PATH entry so the cache-skip filter doesn't
        // eat `/tmp`.
        let dir = find_ld_library_dir(Path::new("/nonexistent-ort-cache"));
        assert_eq!(dir.as_deref(), Some(Path::new("/tmp")));
        unsafe {
            match prev {
                Some(p) => env::set_var("LD_LIBRARY_PATH", p),
                None => env::remove_var("LD_LIBRARY_PATH"),
            }
        }
    }

    /// P3.16: `find_ld_library_dir` must return `None` cleanly when
    /// `LD_LIBRARY_PATH` is empty / unset — silent CPU fallback is the
    /// production failure mode if this panics.
    #[cfg(target_os = "linux")]
    #[test]
    fn find_ld_library_dir_handles_unset() {
        use std::env;
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = env::var_os("LD_LIBRARY_PATH");
        unsafe {
            env::remove_var("LD_LIBRARY_PATH");
        }
        let dir = find_ld_library_dir(Path::new("/nonexistent-ort-cache"));
        assert!(dir.is_none(), "unset LD_LIBRARY_PATH must return None");
        unsafe {
            if let Some(p) = prev {
                env::set_var("LD_LIBRARY_PATH", p);
            }
        }
    }

    /// P3.16: `ort_runtime_search_dir` must succeed on a normal Unix
    /// process — `/proc/self/cmdline` is always populated. Pins that the
    /// helper doesn't crash on a malformed cmdline (no NUL terminator
    /// triggers the `position` early-return path); we can't induce that
    /// in-process so we just verify the happy path returns *some* dir.
    #[cfg(target_os = "linux")]
    #[test]
    fn ort_runtime_search_dir_resolves_for_test_binary() {
        let dir = ort_runtime_search_dir();
        // The test binary always has a cmdline; the only None path is
        // a UTF-8 failure or a missing NUL terminator, both impossible
        // for cargo's harness on Linux.
        assert!(dir.is_some(), "/proc/self/cmdline must resolve in-process");
    }

    /// `detect_provider` must always produce a valid `ExecutionProvider`.
    /// Hardware availability decides which arm fires; pin only that the
    /// function can't return without a value, and that the result formats
    /// cleanly with `Debug` (a regression that broke the derive would
    /// surface here before it reached the tracing field).
    ///
    /// We deliberately don't assert which variant is chosen — `ort` probes
    /// TensorRT / CUDA / CoreML / ROCm unconditionally at runtime regardless
    /// of our cargo features, so the answer is environment-dependent.
    #[test]
    fn detect_provider_returns_valid_variant() {
        // Probe directly — `select_provider` would memoize whatever ran
        // first in the test binary (could be from a different test).
        let p = detect_provider();
        let _ = format!("{p:?}");
    }

    /// Issue #956 Phase A enum: the always-on CPU variant must be `Copy`
    /// (the cache hands out values by reading the OnceCell), and `Debug`
    /// (every tracing event includes `provider = ?provider`).
    #[test]
    fn execution_provider_is_debug_and_copy() {
        let p = ExecutionProvider::CPU;
        let p2 = p; // exercises Copy
        let _ = format!("{p:?}");
        let _ = format!("{p2:?}");
    }

    /// `trt_cache_dir` resolves to a writable directory under the user
    /// cache root and creates it on demand. A `None` return would
    /// silently disable engine caching — the helper guarantees `Some`
    /// on a normal system, so pinning the contract here protects against
    /// a future refactor that fails-open and erases the once-per-restart
    /// compile-cost amortization.
    #[cfg(target_os = "linux")]
    #[test]
    fn trt_cache_dir_creates_directory_under_user_cache() {
        // Env-mutating tests serialize via the module-local mutex.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("CQS_TRT_ENGINE_CACHE").ok();
        // SAFETY: tests run sequentially within this guard; restored below.
        unsafe { std::env::remove_var("CQS_TRT_ENGINE_CACHE") };

        let path = trt_cache_dir().expect("trt_cache_dir must resolve on a normal system");
        let is_dir = path.is_dir();
        let suffix_ok = path.ends_with("cqs/trt-engine-cache");

        // SAFETY: see above.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CQS_TRT_ENGINE_CACHE", v),
                None => std::env::remove_var("CQS_TRT_ENGINE_CACHE"),
            }
        }

        assert!(is_dir, "expected directory at {}", path.display());
        assert!(
            suffix_ok,
            "expected path to end with cqs/trt-engine-cache, got {}",
            path.display()
        );
    }

    /// `CQS_TRT_ENGINE_CACHE=0` opts out of engine caching. The helper
    /// must return `None`, and `create_session` short-circuits to the
    /// no-cache TRT builder. Pinning the opt-out path protects the
    /// "force re-compile after driver upgrade" workflow from a
    /// regression that would silently keep using the stale cache.
    #[cfg(target_os = "linux")]
    #[test]
    fn trt_cache_dir_returns_none_when_opted_out() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("CQS_TRT_ENGINE_CACHE").ok();
        // SAFETY: tests run sequentially within this guard; restored below.
        unsafe { std::env::set_var("CQS_TRT_ENGINE_CACHE", "0") };

        let result = trt_cache_dir();

        // SAFETY: see above.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CQS_TRT_ENGINE_CACHE", v),
                None => std::env::remove_var("CQS_TRT_ENGINE_CACHE"),
            }
        }

        assert!(result.is_none(), "opt-out must yield None");
    }
}
