//! ORT execution provider detection and session creation
//!
//! Handles CUDA/TensorRT provider discovery, symlink setup for provider
//! libraries, and ONNX session creation with the best available provider.

use once_cell::sync::OnceCell;
use ort::ep::ExecutionProvider as OrtExecutionProvider;
use ort::session::Session;
use std::path::{Path, PathBuf};

use super::{EmbedderError, ExecutionProvider};

/// Convert any ort error to [`EmbedderError::InferenceFailed`] via `.to_string()`.
pub(super) fn ort_err<T>(e: ort::Error<T>) -> EmbedderError {
    EmbedderError::InferenceFailed(e.to_string())
}

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
            tracing::debug!("Failed to symlink {}: {}", lib, e);
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
    {
        use ort::ep::TensorRT;
        let tensorrt = TensorRT::default();
        if tensorrt.is_available().unwrap_or(false) {
            let provider = ExecutionProvider::TensorRT { device_id: 0 };
            tracing::info!(provider = ?provider, "Execution provider selected");
            return provider;
        }
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
            builder
                .with_execution_providers([
                    TensorRT::default().with_device_id(device_id).build(),
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
