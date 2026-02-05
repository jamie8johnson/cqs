//! Filesystem source for indexing local files

use super::{language_from_path, Source, SourceError, SourceItem};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// A source that reads files from the local filesystem
///
/// Uses the `ignore` crate to respect .gitignore rules.
pub struct FileSystemSource {
    /// Root directory to index
    root: PathBuf,
    /// Maximum file size to index (bytes)
    max_file_size: u64,
}

impl FileSystemSource {
    /// Create a new filesystem source
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_file_size: 1024 * 1024, // 1MB default
        }
    }

    /// Set maximum file size to index
    pub fn with_max_file_size(mut self, size: u64) -> Self {
        self.max_file_size = size;
        self
    }

    /// Get the root directory
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Enumerate files matching supported extensions
    fn enumerate_files(&self) -> Result<Vec<PathBuf>, SourceError> {
        let mut files = Vec::new();

        let walker = WalkBuilder::new(&self.root)
            .hidden(true) // Include hidden files (but .git is ignored by default)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build();

        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // Check if we support this file type
            if language_from_path(path).is_none() {
                continue;
            }

            // Check file size
            if let Ok(meta) = path.metadata() {
                if meta.len() > self.max_file_size {
                    tracing::debug!(
                        "Skipping large file: {} ({} bytes)",
                        path.display(),
                        meta.len()
                    );
                    continue;
                }
            }

            files.push(path.to_path_buf());
        }

        Ok(files)
    }
}

impl Source for FileSystemSource {
    fn source_type(&self) -> &'static str {
        "file"
    }

    fn enumerate(&self) -> Result<Vec<SourceItem>, SourceError> {
        let files = self.enumerate_files()?;
        let mut items = Vec::with_capacity(files.len());

        for path in files {
            // Get language
            let language = match language_from_path(&path) {
                Some(lang) => lang,
                None => continue, // Should not happen since we filter above
            };

            // Read content
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    tracing::debug!("Skipping non-UTF8 file: {}", path.display());
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            // Normalize line endings (CRLF -> LF) for consistent hashing across platforms
            let content = content.replace("\r\n", "\n");

            // Get mtime
            let mtime = path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);

            // Compute relative path for origin
            let rel_path = path.strip_prefix(&self.root).unwrap_or(&path).to_path_buf();

            items.push(SourceItem {
                origin: rel_path.to_string_lossy().to_string(),
                source_type: "file",
                content,
                language,
                mtime,
                display_path: rel_path,
            });
        }

        Ok(items)
    }

    fn get_mtime(&self, origin: &str) -> Result<Option<i64>, SourceError> {
        let path = self.root.join(origin);
        let mtime = path
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        Ok(mtime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_filesystem_source_enumerate() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Create test files
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("lib.py"), "def foo(): pass").unwrap();
        fs::write(root.join("ignored.txt"), "not code").unwrap();

        let source = FileSystemSource::new(root);
        let items = source.enumerate().unwrap();

        assert_eq!(items.len(), 2); // .rs and .py, not .txt
        assert!(items.iter().any(|i| i.origin.ends_with("main.rs")));
        assert!(items.iter().any(|i| i.origin.ends_with("lib.py")));
    }

    #[test]
    fn test_filesystem_source_mtime() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::write(root.join("test.rs"), "fn test() {}").unwrap();

        let source = FileSystemSource::new(root);
        let mtime = source.get_mtime("test.rs").unwrap();

        assert!(mtime.is_some());
        assert!(mtime.unwrap() > 0);
    }

    #[test]
    fn test_filesystem_source_skips_large_files() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Create a file larger than the limit
        let large_content = "x".repeat(100);
        fs::write(root.join("large.rs"), &large_content).unwrap();
        fs::write(root.join("small.rs"), "fn small() {}").unwrap();

        let source = FileSystemSource::new(root).with_max_file_size(50);
        let items = source.enumerate().unwrap();

        // Only small file should be included
        assert_eq!(items.len(), 1);
        assert!(items[0].origin.ends_with("small.rs"));
    }

    #[test]
    fn test_filesystem_source_mtime_nonexistent() {
        let dir = TempDir::new().unwrap();
        let source = FileSystemSource::new(dir.path());

        // Non-existent file returns None, not error
        let mtime = source.get_mtime("does_not_exist.rs").unwrap();
        assert!(mtime.is_none());
    }

    #[test]
    fn test_filesystem_source_empty_dir() {
        let dir = TempDir::new().unwrap();
        let source = FileSystemSource::new(dir.path());
        let items = source.enumerate().unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_filesystem_source_root() {
        let dir = TempDir::new().unwrap();
        let source = FileSystemSource::new(dir.path());
        assert_eq!(source.root(), dir.path());
    }

    #[test]
    fn test_filesystem_source_type() {
        let dir = TempDir::new().unwrap();
        let source = FileSystemSource::new(dir.path());
        assert_eq!(source.source_type(), "file");
    }
}
