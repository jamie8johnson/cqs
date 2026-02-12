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
use std::path::{Path, PathBuf};

/// Document format detected from file extension.
#[cfg(feature = "convert")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFormat {
    Pdf,
    Html,
    Chm,
    /// Markdown passthrough — no conversion, just cleaning + renaming.
    Markdown,
}

#[cfg(feature = "convert")]
impl std::fmt::Display for DocFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocFormat::Pdf => write!(f, "PDF"),
            DocFormat::Html => write!(f, "HTML"),
            DocFormat::Chm => write!(f, "CHM"),
            DocFormat::Markdown => write!(f, "Markdown"),
        }
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
#[cfg(feature = "convert")]
pub fn detect_format(path: &Path) -> Option<DocFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => Some(DocFormat::Pdf),
        "html" | "htm" => Some(DocFormat::Html),
        "chm" => Some(DocFormat::Chm),
        "md" | "markdown" => Some(DocFormat::Markdown),
        _ => None,
    }
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

    // Step 1: Convert to raw markdown (passthrough for .md files)
    let raw_markdown = match format {
        DocFormat::Pdf => pdf::pdf_to_markdown(path)?,
        DocFormat::Html => {
            let source = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;
            html::html_to_markdown(&source)?
        }
        DocFormat::Chm => chm::chm_to_markdown(path)?,
        DocFormat::Markdown => std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?,
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

    let output_path = opts.output_dir.join(&filename);

    if !opts.dry_run {
        std::fs::create_dir_all(&opts.output_dir)?;

        // Guard: don't overwrite the source file
        if let (Ok(src), Ok(dst)) = (
            dunce::canonicalize(path),
            dunce::canonicalize(&output_path).or_else(|_| {
                // Output doesn't exist yet — canonicalize the parent + filename
                opts.output_dir.canonicalize().map(|d| d.join(&filename))
            }),
        ) {
            if src == dst {
                tracing::warn!(path = %path.display(), "Skipping: output would overwrite source");
                anyhow::bail!(
                    "Output would overwrite source file: {} (use a different --output directory)",
                    path.display()
                );
            }
        }

        if output_path.exists() && !opts.overwrite {
            anyhow::bail!(
                "Output file already exists: {} (use --overwrite to replace)",
                output_path.display()
            );
        }

        std::fs::write(&output_path, &cleaned)?;
        tracing::info!(
            source = %path.display(),
            output = %output_path.display(),
            title = %title,
            sections = sections,
            "Converted document"
        );
    }

    Ok(ConvertResult {
        source: path.to_path_buf(),
        output: output_path,
        format,
        title,
        sections,
    })
}

/// Convert all supported documents in a directory (recursive).
#[cfg(feature = "convert")]
fn convert_directory(dir: &Path, opts: &ConvertOptions) -> anyhow::Result<Vec<ConvertResult>> {
    let _span = tracing::info_span!("convert_directory", dir = %dir.display()).entered();

    let mut results = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| detect_format(e.path()).is_some())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "convert")]
    fn test_detect_format() {
        use std::path::Path;
        assert_eq!(detect_format(Path::new("doc.pdf")), Some(DocFormat::Pdf));
        assert_eq!(detect_format(Path::new("doc.PDF")), Some(DocFormat::Pdf));
        assert_eq!(detect_format(Path::new("doc.html")), Some(DocFormat::Html));
        assert_eq!(detect_format(Path::new("doc.htm")), Some(DocFormat::Html));
        assert_eq!(detect_format(Path::new("doc.HTM")), Some(DocFormat::Html));
        assert_eq!(detect_format(Path::new("doc.chm")), Some(DocFormat::Chm));
        assert_eq!(
            detect_format(Path::new("doc.md")),
            Some(DocFormat::Markdown)
        );
        assert_eq!(
            detect_format(Path::new("doc.markdown")),
            Some(DocFormat::Markdown)
        );
        assert_eq!(detect_format(Path::new("doc.rs")), None);
        assert_eq!(detect_format(Path::new("doc")), None);
    }
}
