use super::TrainDataError;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// Read checkpoint file: tab-separated `repo_path\tsha` lines.
/// Returns empty map if file doesn't exist.
pub fn read_checkpoints(path: &Path) -> Result<HashMap<String, String>, TrainDataError> {
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
    fs::rename(&tmp, path)?;
    Ok(())
}

/// If a file doesn't end with `\n`, truncate to the last `\n`.
/// Used for crash recovery: partial JSONL lines from interrupted writes.
pub fn truncate_incomplete_line(path: &Path) -> Result<(), TrainDataError> {
    let content = match fs::read(path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    if content.is_empty() || content.last() == Some(&b'\n') {
        return Ok(());
    }

    // Find last newline and truncate after it
    if let Some(pos) = content.iter().rposition(|&b| b == b'\n') {
        fs::write(path, &content[..=pos])?;
    } else {
        // No newline at all — entire file is one incomplete line, truncate to empty
        fs::write(path, b"")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl.checkpoint");
        write_checkpoint(&path, "/repo/cqs", "abc123").unwrap();
        let map = read_checkpoints(&path).unwrap();
        assert_eq!(map.get("/repo/cqs"), Some(&"abc123".to_string()));
    }

    #[test]
    fn checkpoint_updates_existing_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl.checkpoint");
        write_checkpoint(&path, "/repo/a", "sha1").unwrap();
        write_checkpoint(&path, "/repo/a", "sha2").unwrap();
        let map = read_checkpoints(&path).unwrap();
        assert_eq!(map.get("/repo/a"), Some(&"sha2".to_string()));
    }

    #[test]
    fn checkpoint_multiple_repos() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl.checkpoint");
        write_checkpoint(&path, "/repo/a", "sha1").unwrap();
        write_checkpoint(&path, "/repo/b", "sha2").unwrap();
        let map = read_checkpoints(&path).unwrap();
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn truncate_incomplete_jsonl() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl");
        std::fs::write(&path, "{\"complete\":true}\n{\"incomplete\":tr").unwrap();
        truncate_incomplete_line(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "{\"complete\":true}\n");
    }

    #[test]
    fn truncate_complete_file_unchanged() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        truncate_incomplete_line(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "{\"a\":1}\n{\"b\":2}\n");
    }

    #[test]
    fn read_nonexistent_checkpoint_returns_empty() {
        let map = read_checkpoints(Path::new("/nonexistent/path")).unwrap();
        assert!(map.is_empty());
    }
}
