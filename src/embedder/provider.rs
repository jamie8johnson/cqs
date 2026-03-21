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
fn ort_err<T>(e: ort::Error<T>) -> EmbedderError {
    EmbedderError::InferenceFailed(e.to_string())
}

/// Ensure ORT CUDA provider libraries are findable (Unix only)
///
/// ORT's C++ runtime resolves provider paths via `dladdr` -> `argv[0]`.
/// With static linking and PATH invocation, `argv[0]` is the bare binary
/// name (e.g., "cqs"), so ORT constructs `absolute("cqs").remove_filename()`
/// = CWD. Providers must exist there for `dlopen` to succeed.
///
/// Strategy: compute the same directory ORT will search (from argv[0]),
/// and create symlinks from the ORT cache there. Symlinks are cleaned up
/// on process exit.
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

    // Collect all symlink paths for cleanup
    let mut cleanup_paths: Vec<PathBuf> = provider_libs
        .iter()
        .map(|lib| ort_search_dir.join(lib))
        .collect();

    // Also symlink into LD_LIBRARY_PATH for other search paths
    if let Some(ld_dir) = find_ld_library_dir(&ort_lib_dir) {
        symlink_providers(&ort_lib_dir, &ld_dir, &provider_libs);
        cleanup_paths.extend(provider_libs.iter().map(|lib| ld_dir.join(lib)));
    }

    // Register cleanup for ALL symlinked paths (both directories)
    register_provider_cleanup(cleanup_paths);
}

/// Compute the directory ORT's GetRuntimePath() will resolve to.
///
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
    let triplet = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        _ => return None,
    };
    let ort_cache = cache_dir.join(format!("ort.pyke.io/dfbin/{triplet}"));

    match std::fs::read_dir(&ort_cache) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .next(),
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

/// Register atexit cleanup for provider symlinks.
/// Uses Mutex to support paths from multiple directories.
#[cfg(target_os = "linux")]
fn register_provider_cleanup(paths: Vec<PathBuf>) {
    use std::sync::Mutex;

    static CLEANUP_PATHS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    if let Ok(mut guard) = CLEANUP_PATHS.lock() {
        guard.extend(paths);
    }

    // Register atexit handler only once
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        /// Cleans up temporary files and symlinks registered for deletion during program termination.
        ///
        /// This function is registered as an exit handler and removes any files that were added to the cleanup list during execution. It safely iterates through registered paths, verifying each is a symlink before attempting deletion, and silently ignores any removal failures.
        ///
        /// # Arguments
        ///
        /// None. Accesses a global `CLEANUP_PATHS` collection to determine which files to remove.
        ///
        /// # Returns
        ///
        /// Nothing. This is an extern "C" function with no return value, suitable for use as an exit handler.
        extern "C" fn cleanup() {
            // Note: remove_file may allocate. Acceptable for CLI tool that exits normally.
            if let Ok(paths) = CLEANUP_PATHS.lock() {
                for path in paths.iter() {
                    if path.symlink_metadata().is_ok() && std::fs::read_link(path).is_ok() {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
        unsafe { libc::atexit(cleanup) };
    });
}

/// No-op on non-Linux platforms (CUDA provider libs handled differently)
#[cfg(not(target_os = "linux"))]
fn ensure_ort_provider_libs() {
    // No-op: Windows and other platforms find CUDA/TensorRT provider libraries
    // via PATH, so no symlinking is needed. The Unix version symlinks .so files
    // into ort's search directory because LD_LIBRARY_PATH may not include them.
}

/// Cached GPU provider detection result
static CACHED_PROVIDER: OnceCell<ExecutionProvider> = OnceCell::new();

/// Select the best available execution provider (cached)
///
/// Provider detection is expensive (checks CUDA/TensorRT availability).
/// Result is cached in a static OnceCell for subsequent calls.
pub(crate) fn select_provider() -> ExecutionProvider {
    *CACHED_PROVIDER.get_or_init(detect_provider)
}

/// Detect the best available execution provider
fn detect_provider() -> ExecutionProvider {
    use ort::ep::{TensorRT, CUDA};

    // Ensure provider libs are findable before checking availability
    ensure_ort_provider_libs();

    // Try CUDA first
    let cuda = CUDA::default();
    if cuda.is_available().unwrap_or(false) {
        return ExecutionProvider::CUDA { device_id: 0 };
    }

    // Try TensorRT
    let tensorrt = TensorRT::default();
    if tensorrt.is_available().unwrap_or(false) {
        return ExecutionProvider::TensorRT { device_id: 0 };
    }

    ExecutionProvider::CPU
}

/// Create an ort session with the specified provider
pub(crate) fn create_session(
    model_path: &Path,
    provider: ExecutionProvider,
) -> Result<Session, EmbedderError> {
    use ort::ep::{TensorRT, CUDA};

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
        ExecutionProvider::CPU => builder.commit_from_file(model_path).map_err(ort_err)?,
    };

    Ok(session)
}
