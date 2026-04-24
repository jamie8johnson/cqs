//! Document-to-Markdown conversion pipeline.
//!
//! Converts PDF, HTML, and CHM documents to cleaned Markdown files
//! suitable for indexing by the Markdown parser.
//!
//! ## Supported Formats
//!
//! | Format | Engine | External Dependencies |
//! |--------|--------|-----------------------|
//! | PDF | Python `pymupdf4llm` | `python3`, `pip install pymupdf4llm` |
//! | HTML/HTM | Rust `fast_html2md` | None |
//! | CHM | `7z` + `fast_html2md` | `p7zip-full` |
//! | Web Help | `fast_html2md` (multi-page) | None |
//!
//! ## Pipeline
//!
//! 1. Detect format from file extension
//! 2. Convert to raw Markdown (format-specific engine)
//! 3. Apply cleaning rules (tag-filtered, extensible)
//! 4. Extract title and generate kebab-case filename
//! 5. Write .md file with collision-safe naming

#[cfg(feature = "convert")]
pub mod chm;
#[cfg(feature = "convert")]
pub mod cleaning;
#[cfg(feature = "convert")]
pub mod html;
pub mod naming;
#[cfg(feature = "convert")]
pub mod pdf;
#[cfg(feature = "convert")]
pub mod webhelp;

#[cfg(feature = "convert")]
use std::path::{Path, PathBuf};

#[cfg(feature = "convert")]
use anyhow::Context;

/// Find a working Python interpreter.
/// Tries `python3`, `python`, `py` in order. Validates that the candidate
/// exits cleanly with `--version` to avoid running unrelated binaries.
///
/// Shared by PDF conversion and model export (PB-4).
///
/// SEC-V1.25-10: Rejects any binary whose resolved location is in a
/// user-writable directory (e.g. /tmp, /var/tmp, or a world-writable dir).
/// This blocks PATH injection where an attacker drops `python3` in a
/// writable directory earlier in PATH. Callers on systems without a
/// suitable binary get a clear error.
pub fn find_python() -> anyhow::Result<String> {
    for name in &["python3", "python", "py"] {
        let Some(resolved) = resolve_on_path(name) else {
            continue;
        };
        if !is_safe_executable_path(&resolved) {
            tracing::warn!(
                candidate = name,
                path = %resolved.display(),
                "Refusing Python candidate in user-writable directory (SEC-V1.25-10)"
            );
            continue;
        }
        match std::process::Command::new(&resolved)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {
                return Ok(resolved.to_string_lossy().to_string());
            }
            _ => continue,
        }
    }
    let install_hint = if cfg!(windows) {
        "Install Python from https://python.org/downloads/ or `winget install Python.Python.3.12`"
    } else if cfg!(target_os = "macos") {
        "macOS: `brew install python`"
    } else {
        "Linux: `sudo apt install python3` (Debian/Ubuntu) or `sudo dnf install python3` (Fedora)"
    };
    anyhow::bail!(
        "Python not found (or only available in a user-writable directory that cqs refuses to trust). \
         {install_hint}"
    )
}

/// Walk PATH looking for `name`; return the first entry that exists and is a file.
/// Applies platform-specific PATHEXT handling on Windows.
#[cfg(feature = "convert")]
pub(crate) fn resolve_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    #[cfg(windows)]
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".EXE;.BAT;.CMD;.COM".to_string())
        .split(';')
        .map(|s| s.to_string())
        .collect();
    for dir in std::env::split_paths(&path_env) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        for ext in &exts {
            let mut with_ext = candidate.clone();
            let filename = match with_ext.file_name().and_then(|f| f.to_str()) {
                Some(f) => format!("{}{}", f, ext),
                None => continue,
            };
            with_ext.set_file_name(filename);
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

/// Fallback when the `convert` feature is disabled: simple PATH walker, no PATHEXT handling.
#[cfg(not(feature = "convert"))]
fn resolve_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Reject a resolved binary path if it is in a user-writable directory.
///
/// SEC-V1.25-10 + audit P2 #35 (cross-platform):
///   * Paths under `/tmp/`, `/var/tmp/`, the OS temp dir (`std::env::temp_dir()`),
///     or the user cache dir (`dirs::cache_dir()`) are rejected outright. The
///     comparison uses `Path::starts_with()` (component-wise) so Windows paths
///     under `%TEMP%` / `%LOCALAPPDATA%\Temp` are caught too.
///   * On Unix, paths whose parent directory is group- or world-writable
///     (mode bits `022`) are rejected — this catches PATH entries like
///     `$HOME/.local/bin` that have been accidentally made writable.
///   * On non-Unix (Windows), a python interpreter (`python*`) is additionally
///     restricted to a small allowlist of trusted install roots. See
///     [`is_safe_python_path_windows`].
///   * A binary that cannot be canonicalized is treated as unsafe.
pub(crate) fn is_safe_executable_path(p: &std::path::Path) -> bool {
    let canonical = match p.canonicalize() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %p.display(), error = %e, "cannot canonicalize binary path, treating as unsafe");
            return false;
        }
    };

    // Build the set of dangerous parent prefixes at runtime. Component-wise
    // `Path::starts_with` is used (not raw substring) so platform-correct
    // separators are handled.
    for prefix in dangerous_parent_dirs() {
        if canonical.starts_with(&prefix) {
            return false;
        }
    }

    #[cfg(unix)]
    if let Some(parent) = canonical.parent() {
        if let Ok(md) = std::fs::metadata(parent) {
            use std::os::unix::fs::PermissionsExt;
            if md.permissions().mode() & 0o022 != 0 {
                return false;
            }
        }
    }

    // On non-Unix (Windows): if this looks like a python interpreter, force
    // it through the allowlist. We have no DACL inspection here — the
    // allowlist is the only positive signal that the binary belongs to a
    // legitimate install.
    #[cfg(not(unix))]
    {
        if is_python_basename(&canonical) && !is_safe_python_path_windows(&canonical) {
            return false;
        }
    }

    true
}

/// Collects the set of parent directory prefixes that should never be allowed
/// to host a trusted executable. Includes:
///   * Hard-coded `/tmp/` and `/var/tmp/` (Unix conventions; cheap to check
///     even on Windows).
///   * Whatever `std::env::temp_dir()` resolves to at runtime (covers
///     `$TMPDIR` on Unix, `%TEMP%` / `%TMP%` / `C:\Windows\Temp` on Windows).
///   * `dirs::cache_dir()` (covers `$XDG_CACHE_HOME` /
///     `~/Library/Caches` / `%LOCALAPPDATA%`).
///
/// Each prefix is canonicalized when possible so the eventual
/// `Path::starts_with` comparison is on canonical-vs-canonical paths.
fn dangerous_parent_dirs() -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::with_capacity(4);
    for p in [
        std::path::Path::new("/tmp"),
        std::path::Path::new("/var/tmp"),
    ] {
        out.push(p.to_path_buf());
        if let Ok(c) = p.canonicalize() {
            out.push(c);
        }
    }
    let temp = std::env::temp_dir();
    if let Ok(c) = temp.canonicalize() {
        out.push(c);
    } else {
        out.push(temp);
    }
    if let Some(cache) = dirs::cache_dir() {
        if let Ok(c) = cache.canonicalize() {
            out.push(c);
        } else {
            out.push(cache);
        }
    }
    out
}

/// True if the file's basename matches `python`, `python3`, `python.exe`, etc.
#[cfg(not(unix))]
fn is_python_basename(p: &std::path::Path) -> bool {
    let Some(file) = p.file_name().and_then(|f| f.to_str()) else {
        return false;
    };
    let lower = file.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    stem == "python" || stem == "python3" || stem == "py" || stem.starts_with("python3.")
}

/// Windows allowlist for python interpreters. A canonical path is accepted
/// only if it lives under one of:
///   * `C:\Python*` (any drive letter)
///   * `C:\Program Files\Python*` (or `Program Files (x86)`)
///   * `%LOCALAPPDATA%\Programs\Python\*`
///
/// This is intentionally narrow. Anything outside these roots — `%TEMP%`,
/// downloads, shared workspaces — is rejected. Operators with custom
/// install locations should symlink (or PATH-prefix) into one of the
/// approved roots.
#[cfg(not(unix))]
fn is_safe_python_path_windows(canonical: &std::path::Path) -> bool {
    let lossy = canonical.to_string_lossy().to_ascii_lowercase();

    // C:\PythonNN, D:\PythonNN, ...
    // The component immediately under the drive letter must start with "python".
    let mut comps = canonical.components();
    if let Some(std::path::Component::Prefix(_)) = comps.next() {
        // skip RootDir
        let _ = comps.next();
        if let Some(std::path::Component::Normal(first)) = comps.next() {
            if let Some(s) = first.to_str() {
                if s.to_ascii_lowercase().starts_with("python") {
                    return true;
                }
            }
        }
    }

    // C:\Program Files\Python311\python.exe (and (x86) variant)
    if lossy.contains("\\program files\\python") || lossy.contains("\\program files (x86)\\python")
    {
        return true;
    }

    // %LOCALAPPDATA%\Programs\Python\PythonNN\python.exe
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let prefix = format!("{}\\programs\\python\\", local.to_ascii_lowercase());
        if lossy.starts_with(&prefix) {
            return true;
        }
    }

    false
}

/// Document format detected from file extension.
#[cfg(feature = "convert")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFormat {
    Pdf,
    Html,
    Chm,
    /// Markdown passthrough — no conversion, just cleaning + renaming.
    Markdown,
    /// Web help site — multi-page HTML directory merged into one document.
    WebHelp,
}

/// Converter function signature: takes a file path, returns raw Markdown.
#[cfg(feature = "convert")]
type FileConverter = fn(&Path) -> anyhow::Result<String>;

/// Static descriptor for a document format.
#[cfg(feature = "convert")]
struct FormatEntry {
    variant: DocFormat,
    display_name: &'static str,
    extensions: &'static [&'static str],
    /// Converter function for file-based formats. `None` for directory formats.
    converter: Option<FileConverter>,
}

/// All supported document formats. One row per format.
/// To add a new file-based format:
/// 1. Add a variant to [`DocFormat`]
/// 2. Add a row here with extensions and converter function
/// 3. Create the converter module (e.g., `epub.rs`) with `pub fn epub_to_markdown(path: &Path) -> Result<String>`
/// 4. Add `pub mod epub;` next to the other module declarations above
#[cfg(feature = "convert")]
static FORMAT_TABLE: &[FormatEntry] = &[
    FormatEntry {
        variant: DocFormat::Pdf,
        display_name: "PDF",
        extensions: &["pdf"],
        converter: Some(pdf::pdf_to_markdown),
    },
    FormatEntry {
        variant: DocFormat::Html,
        display_name: "HTML",
        extensions: &["html", "htm"],
        converter: Some(html::html_file_to_markdown),
    },
    FormatEntry {
        variant: DocFormat::Chm,
        display_name: "CHM",
        extensions: &["chm"],
        converter: Some(chm::chm_to_markdown),
    },
    FormatEntry {
        variant: DocFormat::Markdown,
        display_name: "Markdown",
        extensions: &["md", "markdown"],
        converter: Some(markdown_passthrough),
    },
    FormatEntry {
        variant: DocFormat::WebHelp,
        display_name: "WebHelp",
        extensions: &[],
        converter: None,
    },
];

/// Passthrough converter for Markdown files — reads as-is, no transformation.
///
/// SHL-V1.29-10: size cap shared with `html::html_file_to_markdown` via
/// `crate::limits::convert_file_size()` (env `CQS_CONVERT_MAX_FILE_SIZE`)
/// instead of the prior local `MAX_FILE_SIZE = 100 MB` constant.
#[cfg(feature = "convert")]
fn markdown_passthrough(path: &Path) -> anyhow::Result<String> {
    let _span = tracing::info_span!("markdown_passthrough", path = %path.display()).entered();
    let max_bytes = crate::limits::convert_file_size();
    let meta = std::fs::metadata(path)
        .map_err(|e| anyhow::anyhow!("Failed to stat {}: {}", path.display(), e))?;
    if meta.len() > max_bytes {
        anyhow::bail!(
            "File {} exceeds {} MB size limit",
            path.display(),
            max_bytes / 1024 / 1024,
        );
    }
    std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))
}

#[cfg(feature = "convert")]
impl std::fmt::Display for DocFormat {
    /// Formats the enum variant as a human-readable string.
    /// This method implements the Display trait by looking up the variant in a format table and writing its corresponding display name to the formatter. If the variant is not found in the table, it defaults to "Unknown".
    /// # Arguments
    /// * `f` - The formatter to write the display name into
    /// # Returns
    /// A `std::fmt::Result` indicating whether the formatting succeeded or failed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = FORMAT_TABLE
            .iter()
            .find(|e| e.variant == *self)
            .map(|e| e.display_name)
            .unwrap_or("Unknown");
        write!(f, "{}", name)
    }
}

/// Options controlling the conversion pipeline.
#[cfg(feature = "convert")]
pub struct ConvertOptions {
    pub output_dir: PathBuf,
    pub overwrite: bool,
    pub dry_run: bool,
    /// Cleaning rule tags to apply (empty = all rules).
    pub clean_tags: Vec<String>,
}

/// Result of converting a single document.
#[cfg(feature = "convert")]
pub struct ConvertResult {
    pub source: PathBuf,
    pub output: PathBuf,
    pub format: DocFormat,
    pub title: String,
    pub sections: usize,
}

/// Detect document format from file extension.
/// Looks up the extension in [`FORMAT_TABLE`]. Returns `None` for unsupported
/// extensions and for directory-based formats (which have no file extension).
#[cfg(feature = "convert")]
pub fn detect_format(path: &Path) -> Option<DocFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    FORMAT_TABLE
        .iter()
        .find(|entry| entry.extensions.contains(&ext.as_str()))
        .map(|entry| entry.variant)
}

/// Convert a file or directory to Markdown.
/// If `path` is a directory, converts all supported files recursively.
/// Returns a result per successfully converted document.
#[cfg(feature = "convert")]
pub fn convert_path(path: &Path, opts: &ConvertOptions) -> anyhow::Result<Vec<ConvertResult>> {
    let _span = tracing::info_span!("convert_path", path = %path.display()).entered();

    if path.is_dir() {
        convert_directory(path, opts)
    } else {
        convert_file(path, opts).map(|r| vec![r])
    }
}

/// Convert a single document file to cleaned Markdown.
#[cfg(feature = "convert")]
fn convert_file(path: &Path, opts: &ConvertOptions) -> anyhow::Result<ConvertResult> {
    let _span = tracing::info_span!("convert_file", path = %path.display()).entered();

    let format = detect_format(path)
        .ok_or_else(|| anyhow::anyhow!("Unsupported format: {}", path.display()))?;

    // Step 1: Convert to raw markdown via FORMAT_TABLE dispatch.
    // Safety: `format` comes from `detect_format()` which looks up FORMAT_TABLE,
    // so the variant is guaranteed present.
    let entry = FORMAT_TABLE
        .iter()
        .find(|e| e.variant == format)
        .ok_or_else(|| anyhow::anyhow!("Unsupported format {:?}", format))?;

    let raw_markdown = match entry.converter {
        Some(convert_fn) => convert_fn(path)?,
        None => anyhow::bail!(
            "{} is a directory format — use convert_path() on the directory",
            entry.display_name
        ),
    };

    // Step 2: Clean conversion artifacts
    let tag_refs: Vec<&str> = opts.clean_tags.iter().map(|s| s.as_str()).collect();
    let cleaned = cleaning::clean_markdown(&raw_markdown, &tag_refs);

    // Step 3: Extract title and generate filename
    let title = naming::extract_title(&cleaned, path);
    let filename = naming::title_to_filename(&title);
    let filename = naming::resolve_conflict(&filename, path, &opts.output_dir);

    // Step 4: Count sections for reporting
    let sections = cleaned.lines().filter(|l| l.starts_with('#')).count();

    finalize_output(path, &cleaned, &filename, &title, sections, format, opts)
}

/// Shared post-processing: write cleaned Markdown with overwrite guards and error context.
/// Used by both `convert_file()` and `convert_webhelp()` to avoid duplicating
/// the output directory creation, overwrite guard, and fs::write logic.
#[cfg(feature = "convert")]
fn finalize_output(
    source: &Path,
    cleaned: &str,
    filename: &str,
    title: &str,
    sections: usize,
    format: DocFormat,
    opts: &ConvertOptions,
) -> anyhow::Result<ConvertResult> {
    let output_path = opts.output_dir.join(filename);

    if !opts.dry_run {
        std::fs::create_dir_all(&opts.output_dir).with_context(|| {
            format!(
                "Failed to create output directory: {}",
                opts.output_dir.display()
            )
        })?;

        // Guard: don't overwrite the source file
        if let (Ok(src), Ok(dst)) = (
            dunce::canonicalize(source),
            dunce::canonicalize(&output_path).or_else(|_| {
                // Output doesn't exist yet — canonicalize the parent + filename
                dunce::canonicalize(&opts.output_dir).map(|d| d.join(filename))
            }),
        ) {
            if src == dst {
                tracing::warn!(path = %source.display(), "Skipping: output would overwrite source");
                anyhow::bail!(
                    "Output would overwrite source file: {} (use a different --output directory)",
                    source.display()
                );
            }
        }

        if opts.overwrite {
            std::fs::write(&output_path, cleaned).with_context(|| {
                format!("Failed to write output file: {}", output_path.display())
            })?;
        } else {
            // Atomic create — avoids TOCTOU race between exists() check and write
            use std::io::Write;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&output_path)
            {
                Ok(mut f) => {
                    f.write_all(cleaned.as_bytes()).with_context(|| {
                        format!("Failed to write output file: {}", output_path.display())
                    })?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    anyhow::bail!(
                        "Output file already exists: {} (use --overwrite to replace)",
                        output_path.display()
                    );
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e).context(format!(
                        "Failed to write output file: {}",
                        output_path.display()
                    )));
                }
            }
        }
        tracing::info!(
            source = %source.display(),
            output = %output_path.display(),
            title = %title,
            sections = sections,
            "Converted document"
        );
    }

    Ok(ConvertResult {
        source: source.to_path_buf(),
        output: output_path,
        format,
        title: title.to_string(),
        sections,
    })
}

/// Convert all supported documents in a directory (recursive).
/// Detects web help sites (directories with `content/` + HTML) and converts
/// them as single merged documents instead of individual HTML files.
#[cfg(feature = "convert")]
fn convert_directory(dir: &Path, opts: &ConvertOptions) -> anyhow::Result<Vec<ConvertResult>> {
    let _span = tracing::info_span!("convert_directory", dir = %dir.display()).entered();

    // If this directory itself is a web help site, convert as one document
    if webhelp::is_webhelp_dir(dir) {
        return convert_webhelp(dir, opts).map(|r| vec![r]);
    }

    let mut results = Vec::new();

    // Find immediate subdirectories that are web help sites
    let mut webhelp_dirs: Vec<PathBuf> = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.filter_map(|e| match e {
                Ok(entry) => Some(entry),
                Err(err) => {
                    tracing::warn!(error = %err, "Skipping directory entry due to read_dir error");
                    None
                }
            }) {
                let path = entry.path();
                if path.is_dir() && webhelp::is_webhelp_dir(&path) {
                    webhelp_dirs.push(path);
                }
            }
        }
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "Failed to read directory for webhelp detection");
        }
    }

    // Convert web help directories as single documents
    for wh_dir in &webhelp_dirs {
        match convert_webhelp(wh_dir, opts) {
            Ok(r) => results.push(r),
            Err(e) => tracing::warn!(
                path = %wh_dir.display(),
                error = %e,
                "Failed to convert web help directory"
            ),
        }
    }

    // P3 #108: env-overridable walkdir depth cap (CQS_CONVERT_MAX_WALK_DEPTH).
    let max_walk_depth = crate::limits::doc_max_walk_depth();
    let mut depth_cap_hit = false;
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(max_walk_depth)
        .into_iter()
        .filter_entry(|e| !e.path_is_symlink())
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(error = %err, "Skipping directory entry due to walkdir error");
                None
            }
        })
        .filter(|e| e.file_type().is_file())
        .filter(|e| detect_format(e.path()).is_some())
        .filter(|e| !webhelp_dirs.iter().any(|wh| e.path().starts_with(wh)))
    {
        // P3 #108: any entry exactly at the depth cap means deeper
        // descendants were silently dropped — surface it once.
        if entry.depth() >= max_walk_depth {
            depth_cap_hit = true;
        }
        match convert_file(entry.path(), opts) {
            Ok(r) => results.push(r),
            Err(e) => tracing::warn!(
                path = %entry.path().display(),
                error = %e,
                "Failed to convert document"
            ),
        }
    }
    if depth_cap_hit {
        tracing::warn!(
            dir = %dir.display(),
            cap = max_walk_depth,
            "Doc walk hit max-depth cap; bump CQS_CONVERT_MAX_WALK_DEPTH if your tree is legitimately deeper"
        );
    }

    tracing::info!(
        dir = %dir.display(),
        converted = results.len(),
        "Directory conversion complete"
    );
    Ok(results)
}

/// Convert a web help directory to a single cleaned Markdown document.
#[cfg(feature = "convert")]
fn convert_webhelp(dir: &Path, opts: &ConvertOptions) -> anyhow::Result<ConvertResult> {
    let _span = tracing::info_span!("convert_webhelp", dir = %dir.display()).entered();

    let raw_markdown = webhelp::webhelp_to_markdown(dir)?;

    // Clean
    let tag_refs: Vec<&str> = opts.clean_tags.iter().map(|s| s.as_str()).collect();
    let cleaned = cleaning::clean_markdown(&raw_markdown, &tag_refs);

    // Title + filename
    let title = naming::extract_title(&cleaned, dir);
    let filename = naming::title_to_filename(&title);
    let filename = naming::resolve_conflict(&filename, dir, &opts.output_dir);

    let sections = cleaned.lines().filter(|l| l.starts_with('#')).count();

    finalize_output(
        dir,
        &cleaned,
        &filename,
        &title,
        sections,
        DocFormat::WebHelp,
        opts,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "convert")]
    fn test_format_table_complete() {
        // Exhaustive list — adding a variant without updating this list causes
        // a compile warning (unused variant) AND this test fails.
        let all = [
            DocFormat::Pdf,
            DocFormat::Html,
            DocFormat::Chm,
            DocFormat::Markdown,
            DocFormat::WebHelp,
        ];
        for v in &all {
            let entry = FORMAT_TABLE.iter().find(|e| e.variant == *v);
            assert!(entry.is_some(), "FORMAT_TABLE missing entry for {:?}", v);
            let entry = entry.unwrap();
            // Display name must be non-empty
            assert!(
                !entry.display_name.is_empty(),
                "Empty display_name for {:?}",
                v
            );
            // File-based formats must have extensions
            if entry.converter.is_some() {
                assert!(
                    !entry.extensions.is_empty(),
                    "File-based format {:?} must have at least one extension",
                    v
                );
            }
        }
    }

    #[test]
    #[cfg(feature = "convert")]
    fn test_detect_format_roundtrips() {
        // Every file-based format's extensions should round-trip through detect_format
        for entry in FORMAT_TABLE.iter().filter(|e| e.converter.is_some()) {
            for ext in entry.extensions {
                let path = std::path::Path::new("test").with_extension(ext);
                assert_eq!(
                    detect_format(&path),
                    Some(entry.variant),
                    "detect_format failed for .{} (expected {:?})",
                    ext,
                    entry.variant
                );
            }
        }
        // Unsupported extensions return None
        assert_eq!(detect_format(std::path::Path::new("doc.rs")), None);
        assert_eq!(detect_format(std::path::Path::new("doc")), None);
    }

    #[test]
    #[cfg(feature = "convert")]
    fn test_detect_format_case_insensitive() {
        assert_eq!(
            detect_format(std::path::Path::new("doc.PDF")),
            Some(DocFormat::Pdf)
        );
        assert_eq!(
            detect_format(std::path::Path::new("doc.HTM")),
            Some(DocFormat::Html)
        );
    }

    // SEC-V1.25-10: is_safe_executable_path rejects /tmp and world-writable parents.
    #[test]
    #[cfg(all(feature = "convert", unix))]
    fn is_safe_executable_path_rejects_tmp() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let exe = dir.path().join("fake_python");
        std::fs::write(&exe, "#!/bin/sh\necho hi\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            !is_safe_executable_path(&exe),
            "binary in /tmp must be rejected: {}",
            exe.display()
        );
    }

    #[test]
    #[cfg(all(feature = "convert", unix))]
    fn is_safe_executable_path_accepts_safe_location() {
        // /bin/sh exists on every unix system we build for.
        let sh = std::path::Path::new("/bin/sh");
        if sh.exists() {
            assert!(
                is_safe_executable_path(sh),
                "/bin/sh should be accepted as a safe executable"
            );
        }
    }

    #[test]
    #[cfg(feature = "convert")]
    fn is_safe_executable_path_rejects_nonexistent() {
        let bogus = std::path::Path::new("/nonexistent/cqs_fake_binary_xyz");
        assert!(
            !is_safe_executable_path(bogus),
            "nonexistent path must fail canonicalize and be rejected"
        );
    }

    /// Audit P2 #35: a binary dropped into the OS temp dir must be rejected
    /// regardless of platform — the prior code only rejected hard-coded
    /// `/tmp/` and `/var/tmp/`. `std::env::temp_dir()` covers the platform
    /// convention (`%TEMP%` on Windows, `$TMPDIR` on macOS, `/tmp` on Linux),
    /// so the test runs on every host.
    #[test]
    fn is_safe_executable_path_rejects_runtime_temp_dir() {
        let dir = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let fake = dir.path().join("fake_binary");
        std::fs::write(&fake, b"placeholder").unwrap();
        assert!(
            !is_safe_executable_path(&fake),
            "binary in std::env::temp_dir() must be rejected: {}",
            fake.display()
        );
    }

    /// Audit P2 #35: same as above but specifically gated on Windows so the
    /// test name pins the threat model (`%TEMP%\python.exe` PATH injection).
    #[test]
    #[cfg(windows)]
    fn is_safe_executable_path_rejects_windows_temp_python() {
        let dir = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let exe = dir.path().join("python.exe");
        std::fs::write(&exe, b"MZ\x90\x00").unwrap();
        assert!(
            !is_safe_executable_path(&exe),
            "python.exe under %TEMP% must be rejected (audit P2 #35): {}",
            exe.display()
        );
    }

    /// Audit P2 #35: the python-basename helper recognizes the documented
    /// interpreter names so the Windows allowlist can be applied.
    #[test]
    #[cfg(not(unix))]
    fn is_python_basename_matches_documented_names() {
        use std::path::PathBuf;
        for name in &[
            "python.exe",
            "python3.exe",
            "python",
            "python3",
            "python3.12.exe",
            "py.exe",
        ] {
            let p = PathBuf::from(name);
            assert!(
                is_python_basename(&p),
                "{name} should be recognized as a python interpreter"
            );
        }
        for name in &["pwsh.exe", "powershell.exe", "ruby.exe", "node.exe"] {
            let p = PathBuf::from(name);
            assert!(
                !is_python_basename(&p),
                "{name} should NOT be classified as python"
            );
        }
    }

    /// Audit P2 #35: the Windows allowlist for python interpreters must
    /// reject anything that doesn't live under the documented install roots.
    #[test]
    #[cfg(not(unix))]
    fn is_safe_python_path_windows_allowlist() {
        use std::path::PathBuf;
        for safe in &[
            r"C:\Python311\python.exe",
            r"D:\Python\python.exe",
            r"C:\Program Files\Python311\python.exe",
            r"C:\Program Files (x86)\Python310\python.exe",
        ] {
            assert!(
                is_safe_python_path_windows(&PathBuf::from(safe)),
                "{safe} should be in the allowlist"
            );
        }
        for bad in &[
            r"C:\Users\u\AppData\Local\Temp\python.exe",
            r"C:\Users\u\Downloads\python.exe",
            r"C:\evil\python.exe",
        ] {
            assert!(
                !is_safe_python_path_windows(&PathBuf::from(bad)),
                "{bad} should NOT be in the allowlist"
            );
        }
    }
}
