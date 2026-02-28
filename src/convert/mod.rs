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
///
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
#[cfg(feature = "convert")]
fn markdown_passthrough(path: &Path) -> anyhow::Result<String> {
    let _span = tracing::info_span!("markdown_passthrough", path = %path.display()).entered();
    std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))
}

#[cfg(feature = "convert")]
impl std::fmt::Display for DocFormat {
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
///
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
///
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
        .unwrap_or_else(|| panic!("FORMAT_TABLE missing entry for {:?}", format));

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
///
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
///
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
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() && webhelp::is_webhelp_dir(&path) {
                webhelp_dirs.push(path);
            }
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

    // Walk individual files, skipping symlinks and those under web help directories
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| !e.path_is_symlink())
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| detect_format(e.path()).is_some())
        .filter(|e| !webhelp_dirs.iter().any(|wh| e.path().starts_with(wh)))
    {
        match convert_file(entry.path(), opts) {
            Ok(r) => results.push(r),
            Err(e) => tracing::warn!(
                path = %entry.path().display(),
                error = %e,
                "Failed to convert document"
            ),
        }
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
}
