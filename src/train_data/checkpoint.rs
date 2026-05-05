use super::TrainDataError;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// Read checkpoint file: tab-separated `repo_path\tsha` lines.
/// Returns empty map if file doesn't exist.
pub fn read_checkpoints(path: &Path) -> Result<HashMap<String, String>, TrainDataError> {
    let _span = tracing::info_span!("read_checkpoints", path = %path.display()).entered();
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e.into()),
    };

    let mut map = HashMap::new();
    for line in content.lines() {
        if let Some((repo, sha)) = line.split_once('\t') {
            map.insert(repo.to_string(), sha.to_string());
        }
    }
    Ok(map)
}

/// Write or update a checkpoint entry for a repo.
/// Reads existing entries, updates the entry for `repo`, writes back atomically.
pub fn write_checkpoint(path: &Path, repo: &str, sha: &str) -> Result<(), TrainDataError> {
    let _span = tracing::info_span!("write_checkpoint", path = %path.display(), repo).entered();
    let mut map = read_checkpoints(path)?;
    map.insert(repo.to_string(), sha.to_string());

    // Write to temp file then rename for atomicity
    let tmp = path.with_extension("checkpoint.tmp");
    let mut content = String::new();
    for (r, s) in &map {
        content.push_str(r);
        content.push('\t');
        content.push_str(s);
        content.push('\n');
    }
    fs::write(&tmp, &content)?;
    // atomic_replace: picks up fsync-tmp + EXDEV fallback + fsync-parent
    // that the previous bare `fs::rename` skipped. Checkpoints drive
    // training-data resumption, so lost checkpoint writes = re-traversing
    // thousands of commits on the next run.
    crate::fs::atomic_replace(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        TrainDataError::Io(e)
    })?;
    Ok(())
}

/// If a file doesn't end with `\n`, truncate to the last `\n`.
/// Used for crash recovery: partial JSONL lines from interrupted writes.
///
/// RM-V1.36-1: previously read the whole file into memory, then scanned for
/// the last newline. Training-data JSONL files routinely run multi-GB; the
/// in-memory scan allocated 2× file size at peak. Replaced with a tail-seek
/// scan of the last 64 KiB (chosen to comfortably cover any realistic JSONL
/// line; we widen iteratively for the long-line edge case).
pub fn truncate_incomplete_line(path: &Path) -> Result<(), TrainDataError> {
    use io::{Read, Seek, SeekFrom};
    let _span = tracing::info_span!("truncate_incomplete_line", path = %path.display()).entered();
    let mut f = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    // Read the trailing byte first to short-circuit the common (clean) case.
    f.seek(SeekFrom::End(-1))?;
    let mut tail = [0u8; 1];
    f.read_exact(&mut tail)?;
    if tail[0] == b'\n' {
        return Ok(());
    }
    // Walk back in 64 KiB chunks until we find a newline or hit the start.
    const CHUNK: u64 = 64 * 1024;
    let mut window_end = len;
    while window_end > 0 {
        let read_len = CHUNK.min(window_end);
        let read_start = window_end - read_len;
        f.seek(SeekFrom::Start(read_start))?;
        let mut buf = vec![0u8; read_len as usize];
        f.read_exact(&mut buf)?;
        if let Some(rel) = buf.iter().rposition(|&b| b == b'\n') {
            // Truncate to the byte after the newline.
            let truncate_to = read_start + rel as u64 + 1;
            f.set_len(truncate_to)?;
            return Ok(());
        }
        window_end = read_start;
    }
    // No newline anywhere in the file — entire file is one incomplete line.
    f.set_len(0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Tests the checkpoint serialization and deserialization roundtrip.
    /// Creates a temporary directory, writes a checkpoint with a repository path and commit hash, reads it back, and verifies the data matches what was written.
    /// # Panics
    /// Panics if temporary directory creation fails, checkpoint writing fails, or checkpoint reading fails.

    #[test]
    fn checkpoint_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl.checkpoint");
        write_checkpoint(&path, "/repo/cqs", "abc123").unwrap();
        let map = read_checkpoints(&path).unwrap();
        assert_eq!(map.get("/repo/cqs"), Some(&"abc123".to_string()));
    }
    /// Tests that writing checkpoints for the same repository path overwrites the existing checkpoint with the new value. Verifies that when multiple checkpoints are written for the same repository path, only the latest checkpoint is retained when read back.
    /// # Arguments
    /// None - this is a test function that creates its own test data.
    /// # Returns
    /// None - this is a test function that asserts expected behavior.
    /// # Panics
    /// Panics if any assertion fails or if temporary file operations fail.

    #[test]
    fn checkpoint_updates_existing_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl.checkpoint");
        write_checkpoint(&path, "/repo/a", "sha1").unwrap();
        write_checkpoint(&path, "/repo/a", "sha2").unwrap();
        let map = read_checkpoints(&path).unwrap();
        assert_eq!(map.get("/repo/a"), Some(&"sha2".to_string()));
    }
    /// Tests that multiple repository checkpoints can be written to and read from a single checkpoint file. Writes checkpoints for two different repositories with different SHAs to a file, then verifies that reading the file returns both checkpoints in a map with the correct count.
    /// # Arguments
    /// None (this is a test function)
    /// # Returns
    /// None (assertions validate correctness)
    /// # Panics
    /// Panics if any checkpoint operations fail or if the number of checkpoints read does not equal 2.

    #[test]
    fn checkpoint_multiple_repos() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl.checkpoint");
        write_checkpoint(&path, "/repo/a", "sha1").unwrap();
        write_checkpoint(&path, "/repo/b", "sha2").unwrap();
        let map = read_checkpoints(&path).unwrap();
        assert_eq!(map.len(), 2);
    }
    /// Tests the `truncate_incomplete_line` function by creating a temporary JSONL file with a complete JSON object followed by an incomplete one, verifying that the incomplete line is removed.
    /// # Arguments
    /// None. This is a test function that creates its own test data.
    /// # Returns
    /// None. This function asserts expected behavior but does not return a value.
    /// # Panics
    /// Panics if any of the following operations fail: temporary directory creation, file writing, the `truncate_incomplete_line` function, file reading, or the assertion that the file contains only the complete JSON line.

    #[test]
    fn truncate_incomplete_jsonl() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl");
        std::fs::write(&path, "{\"complete\":true}\n{\"incomplete\":tr").unwrap();
        truncate_incomplete_line(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "{\"complete\":true}\n");
    }
    /// Tests that truncate_incomplete_line leaves a complete JSONL file unchanged.
    /// Creates a temporary file with two complete JSON lines, calls truncate_incomplete_line on it, and verifies the file content remains unmodified since all lines are properly terminated with newlines.
    /// # Arguments
    /// None.
    /// # Returns
    /// None (unit test).
    /// # Panics
    /// Panics if temporary directory creation, file operations, or assertions fail.

    #[test]
    fn truncate_complete_file_unchanged() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        truncate_incomplete_line(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "{\"a\":1}\n{\"b\":2}\n");
    }
    /// Tests that reading checkpoints from a non-existent path returns an empty map instead of failing.
    /// # Arguments
    /// None
    /// # Returns
    /// Unit type. This is a test function that asserts behavior rather than returning a meaningful value.
    /// # Panics
    /// Panics if the assertion fails, indicating that reading from a non-existent checkpoint path did not return an empty map as expected.

    #[test]
    fn read_nonexistent_checkpoint_returns_empty() {
        let map = read_checkpoints(Path::new("/nonexistent/path")).unwrap();
        assert!(map.is_empty());
    }
}
