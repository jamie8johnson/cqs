# `cqs train-data` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `cqs train-data` command that generates JSONL training triplets from git history for LoRA fine-tuning of code search embeddings.

**Architecture:** 6 new library files (train_data module) + 1 parser modification + CLI wiring. The train_data module handles git operations, diff parsing, query normalization, BM25 scoring, and JSONL output. Parser gets a new `parse_source()` for in-memory content.

**Tech Stack:** Rust, tree-sitter, std::process::Command (git), blake3, serde_json, regex

**Spec:** `docs/superpowers/specs/2026-03-19-train-data-design.md`

---

## File Map

| File | Responsibility |
|------|---------------|
| `src/train_data/mod.rs` | Public API: `generate_training_data()`, types (`Triplet`, `TrainDataConfig`, `TrainDataStats`, `TrainDataError`), orchestration |
| `src/train_data/git.rs` | Git operations: log, diff-tree, show, shallow detection |
| `src/train_data/diff.rs` | Unified diff parsing: hunk extraction, function-span intersection |
| `src/train_data/query.rs` | Query normalization: conventional commit stripping, verb removal, trailing noise |
| `src/train_data/bm25.rs` | BM25 index: build from function corpus (with IDF), score queries, select negatives with content hash guard |
| `src/train_data/checkpoint.rs` | Checkpoint read/write, JSONL truncation, resume logic |
| `src/parser/mod.rs` | Add `parse_source()` method (factor file-read out of `parse_file`) |
| `src/cli/commands/train_data.rs` | CLI command: arg parsing, call orchestration |
| `src/cli/mod.rs` | Add TrainData variant to Commands enum |
| `src/lib.rs` | Add `pub mod train_data;` |

---

### Task 1: Parser::parse_source() API

**Files:**
- Modify: `src/parser/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn parse_source_extracts_functions() {
    let parser = Parser::new();
    let source = "fn hello() { println!(\"hi\"); }\nfn world() { }";
    let chunks = parser.parse_source(source, Language::Rust, Path::new("test.rs")).unwrap();
    assert!(chunks.len() >= 2);
    assert!(chunks.iter().any(|c| c.name == "hello"));
    assert!(chunks.iter().any(|c| c.name == "world"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features gpu-index -p cqs --lib -- parse_source_extracts 2>&1`

- [ ] **Step 3: Implement parse_source**

```rust
pub fn parse_source(
    &self,
    source: &str,
    language: Language,
    path: &Path,  // for chunk origin metadata (Chunk.file field)
) -> Result<Vec<Chunk>, ParserError>
```

Factor the file-read and language-detection out of `parse_file`. The shared body (grammar lookup, tree-sitter parse, chunk extraction) goes into `parse_source`. Then `parse_file` becomes: read file → detect language → call `parse_source`.

- [ ] **Step 4: Run tests — both new test and existing parser tests pass**

Run: `cargo test --features gpu-index -p cqs --lib -- parser 2>&1 | tail -5`

- [ ] **Step 5: Commit**

```
feat(parser): add parse_source for in-memory content parsing
```

---

### Task 2: Error type + diff parsing (train_data/diff.rs)

**Files:**
- Create: `src/train_data/mod.rs` (module structure + `TrainDataError` enum)
- Create: `src/train_data/diff.rs`
- Modify: `src/lib.rs` (add `pub mod train_data;`)

- [ ] **Step 1: Create module structure with error type**

`src/train_data/mod.rs`:
```rust
pub mod diff;

#[derive(Debug, thiserror::Error)]
pub enum TrainDataError {
    #[error("Git error: {0}")]
    Git(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parser error: {0}")]
    Parser(#[from] crate::parser::ParserError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Invalid repo: {0}")]
    InvalidRepo(String),
}
```

Add `pub mod train_data;` to `src/lib.rs`.

- [ ] **Step 2: Write failing tests for diff parsing**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hunk_header() {
        let line = "@@ -10,5 +12,8 @@ fn some_context";
        let hunk = parse_hunk_header(line).unwrap();
        assert_eq!(hunk.new_start, 12);
        assert_eq!(hunk.new_count, 8);
    }

    #[test]
    fn parse_diff_extracts_files_and_hunks() {
        let diff = "diff --git a/src/foo.rs b/src/foo.rs\n--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1,3 +1,5 @@\n+new line\n context\n";
        let files = parse_diff_output(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/foo.rs");
        assert_eq!(files[0].hunks.len(), 1);
    }

    #[test]
    fn intersect_hunks_with_functions() {
        let hunks = vec![HunkRange { new_start: 5, new_count: 3 }];
        let functions = vec![
            FunctionSpan { name: "a".into(), start_line: 1, end_line: 4, content: "fn a()".into() },
            FunctionSpan { name: "b".into(), start_line: 5, end_line: 10, content: "fn b()".into() },
        ];
        let changed = find_changed_functions(&hunks, &functions);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].name, "b");
    }

    #[test]
    fn hunk_spanning_two_functions() {
        let hunks = vec![HunkRange { new_start: 4, new_count: 4 }];
        let functions = vec![
            FunctionSpan { name: "a".into(), start_line: 1, end_line: 5, content: "fn a()".into() },
            FunctionSpan { name: "b".into(), start_line: 6, end_line: 10, content: "fn b()".into() },
        ];
        let changed = find_changed_functions(&hunks, &functions);
        assert_eq!(changed.len(), 2);
    }

    #[test]
    fn change_outside_functions_skipped() {
        let hunks = vec![HunkRange { new_start: 1, new_count: 2 }];
        let functions = vec![
            FunctionSpan { name: "a".into(), start_line: 5, end_line: 10, content: "fn a()".into() },
        ];
        let changed = find_changed_functions(&hunks, &functions);
        assert!(changed.is_empty());
    }

    #[test]
    fn skips_submodule_entries() {
        let diff = "diff --git a/submod b/submod\n--- a/submod\n+++ b/submod\n@@ -1 +1 @@\n-Subproject commit abc\n+Subproject commit def\n";
        let files = parse_diff_output(diff);
        assert!(files.is_empty()); // submodule detected by "Subproject commit" line
    }

    #[test]
    fn empty_diff_returns_empty() {
        let files = parse_diff_output("");
        assert!(files.is_empty());
    }

    #[test]
    fn nested_closure_attributed_to_outer() {
        let hunks = vec![HunkRange { new_start: 3, new_count: 1 }]; // line 3, inside closure inside fn
        let functions = vec![
            FunctionSpan { name: "outer".into(), start_line: 1, end_line: 10, content: "fn outer()".into() },
            FunctionSpan { name: "closure".into(), start_line: 2, end_line: 5, content: "|| {}".into() },
        ];
        // Both match the hunk, but find_changed_functions deduplicates to outermost
        let changed = find_changed_functions(&hunks, &functions);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].name, "outer");
    }

    #[test]
    fn total_diff_lines_counted() {
        let diff = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,5 @@\n+a\n+b\n c\n@@ -10,2 +12,4 @@\n+d\n+e\n";
        let files = parse_diff_output(diff);
        assert_eq!(files[0].total_added_lines(), 4); // 2 + 2
    }
}
```

- [ ] **Step 3: Implement diff parsing**

```rust
pub struct HunkRange { pub new_start: usize, pub new_count: usize }

pub struct DiffFile {
    pub path: String,
    pub hunks: Vec<HunkRange>,
}

impl DiffFile {
    pub fn total_added_lines(&self) -> usize {
        self.hunks.iter().map(|h| h.new_count).sum()
    }
}

pub struct FunctionSpan {
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

pub fn parse_hunk_header(line: &str) -> Option<HunkRange>
pub fn parse_diff_output(diff: &str) -> Vec<DiffFile>  // skips submodules ("Subproject commit"), empty diffs
pub fn find_changed_functions(hunks: &[HunkRange], functions: &[FunctionSpan]) -> Vec<FunctionSpan>
// Deduplicates nested functions — if a nested span (closure) and its parent both match, keep only the parent
```

- [ ] **Step 4: Tests pass, commit**

```
feat(train-data): diff hunk parsing, function-span intersection, TrainDataError
```

---

### Task 3: Query normalization (train_data/query.rs)

**Files:**
- Create: `src/train_data/query.rs`
- Modify: `src/train_data/mod.rs` (add `pub mod query;`)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn strip_conventional_commit_prefix() {
    assert_eq!(normalize_query("fix(parser): timeout handling"), "timeout handling");
    assert_eq!(normalize_query("feat!: breaking change"), "breaking change");
    assert_eq!(normalize_query("FIX: case insensitive"), "case insensitive");
}

#[test]
fn strip_leading_verb() {
    assert_eq!(normalize_query("add retry logic to HTTP client"), "retry logic to HTTP client");
    assert_eq!(normalize_query("implement batch processing"), "batch processing");
    assert_eq!(normalize_query("wip config changes"), "config changes");
}

#[test]
fn strip_trailing_pr_reference() {
    assert_eq!(normalize_query("fix timeout (#234)"), "timeout");  // also strips "fix"
    assert_eq!(normalize_query("update config #123"), "config");
}

#[test]
fn combined_stripping() {
    assert_eq!(normalize_query("fix(parser): add timeout handling (#456)"), "timeout handling");
}

#[test]
fn empty_after_strip_returns_original() {
    assert_eq!(normalize_query("fix:"), "fix:");
    assert_eq!(normalize_query("wip"), "wip");
}

#[test]
fn no_stripping_needed() {
    assert_eq!(normalize_query("config parser timeout"), "config parser timeout");
}
```

- [ ] **Step 2: Implement**

```rust
pub fn normalize_query(msg: &str) -> String
```

Use `std::sync::OnceLock<regex::Regex>` for compiled patterns (3 regexes: conventional prefix, leading verb, trailing noise). Apply in order: prefix → verb → trailing. If result is empty after stripping, return original trimmed.

- [ ] **Step 3: Tests pass, commit**

```
feat(train-data): query normalization — strip prefixes, verbs, PR refs
```

---

### Task 4: BM25 index (train_data/bm25.rs)

**Files:**
- Create: `src/train_data/bm25.rs`
- Modify: `src/train_data/mod.rs` (add `pub mod bm25;`)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn bm25_build_and_score() {
    let docs = vec![
        ("hash1".into(), "fn parse config file timeout".into()),
        ("hash2".into(), "fn validate schema input data".into()),
        ("hash3".into(), "fn parse json data format".into()),
    ];
    let index = Bm25Index::build(&docs);
    let results = index.score("parse config");
    assert_eq!(results[0].0, "hash1"); // both terms match
}

#[test]
fn idf_downweights_common_terms() {
    let docs = vec![
        ("h1".into(), "fn common rare_term".into()),
        ("h2".into(), "fn common other_stuff".into()),
        ("h3".into(), "fn common more_things".into()),
    ];
    let index = Bm25Index::build(&docs);
    let results = index.score("rare_term");
    assert_eq!(results[0].0, "h1"); // "fn" and "common" downweighted, "rare_term" discriminates
}

#[test]
fn select_negatives_excludes_positive_by_hash() {
    let docs = vec![
        ("h1".into(), "fn foo bar".into()),
        ("h2".into(), "fn foo baz".into()),
        ("h3".into(), "fn qux quux".into()),
    ];
    let index = Bm25Index::build(&docs);
    let negs = index.select_negatives("foo bar", "h1", "fn foo bar", 3);
    assert!(negs.iter().all(|(hash, _)| hash != "h1"));
}

#[test]
fn content_hash_guard_excludes_identical_content() {
    // h1 and h2 have identical content but different hashes (simulating rename)
    let docs = vec![
        ("h1".into(), "fn identical code here".into()),
        ("h2".into(), "fn identical code here".into()),
        ("h3".into(), "fn different code entirely".into()),
    ];
    let index = Bm25Index::build(&docs);
    // positive is h1 with content "fn identical code here"
    // h2 has same content — content hash guard should exclude it
    let negs = index.select_negatives("identical code", "h1", "fn identical code here", 3);
    assert!(negs.iter().all(|(_, content)| content != "fn identical code here"));
}

#[test]
fn fallback_to_random_when_few_candidates() {
    let docs = vec![
        ("h1".into(), "fn only function".into()),
    ];
    let index = Bm25Index::build(&docs);
    let negs = index.select_negatives("only function", "h1", "fn only function", 3);
    assert!(negs.is_empty());
}
```

- [ ] **Step 2: Implement BM25**

```rust
pub struct Bm25Index {
    docs: Vec<(String, String)>,          // (content_hash, content)
    doc_terms: Vec<HashMap<String, f32>>, // TF per doc
    idf: HashMap<String, f32>,            // IDF per term
    avg_dl: f32,
}

impl Bm25Index {
    pub fn build(docs: &[(String, String)]) -> Self
    pub fn score(&self, query: &str) -> Vec<(String, f32)>  // sorted desc by score
    /// Select top-k negatives, excluding positive by hash AND content hash guard
    pub fn select_negatives(&self, query: &str, positive_hash: &str, positive_content: &str, k: usize) -> Vec<(String, String)>
}
```

BM25: `score(t,d) = idf(t) * (tf * (k1+1)) / (tf + k1 * (1 - b + b * dl/avgdl))`, k1=1.2, b=0.75. Terms = whitespace-split lowercase. Content hash guard: compute BLAKE3 of each candidate's content, skip if matches BLAKE3 of positive_content.

- [ ] **Step 3: Tests pass, commit**

```
feat(train-data): BM25 index with IDF for hard negative selection
```

---

### Task 5: Git operations (train_data/git.rs)

**Files:**
- Create: `src/train_data/git.rs`
- Modify: `src/train_data/mod.rs` (add `pub mod git;`)

- [ ] **Step 1: Implement git wrapper functions**

```rust
pub struct CommitInfo {
    pub sha: String,
    pub message: String,
    pub date: String,
}

/// List commits (non-merge). Uses --format with null separators for reliable parsing.
pub fn git_log(repo: &Path, max_commits: usize) -> Result<Vec<CommitInfo>, TrainDataError>

/// Get unified diff with hunks for a commit. Uses --root to handle initial commit.
pub fn git_diff_tree(repo: &Path, sha: &str) -> Result<String, TrainDataError>

/// Get file content at a specific commit. Returns None if > 50MB or non-UTF-8.
pub fn git_show(repo: &Path, sha: &str, path: &str) -> Result<Option<String>, TrainDataError>

/// Check if repo is a shallow clone.
pub fn is_shallow(repo: &Path) -> bool
```

`git_log` uses `--format="%H%x00%s%x00%aI" --no-merges`. `git_diff_tree` uses `--root --no-commit-id -r -p`. `git_show` checks output length before converting to String (50MB guard), returns `Ok(None)` for non-UTF-8 or oversized. All functions set `-C {repo}` for working directory.

- [ ] **Step 2: Write tests with real git repos**

```rust
#[test]
fn git_log_on_test_repo() {
    let dir = create_test_repo();
    let commits = git_log(dir.path(), 0).unwrap();
    assert!(!commits.is_empty());
    assert!(!commits[0].sha.is_empty());
    assert!(!commits[0].message.is_empty());
}

#[test]
fn git_diff_tree_on_test_repo() {
    let dir = create_test_repo_with_change();
    let commits = git_log(dir.path(), 0).unwrap();
    let diff = git_diff_tree(dir.path(), &commits[0].sha).unwrap();
    assert!(diff.contains("test.rs"));
}

#[test]
fn git_show_returns_content() {
    let dir = create_test_repo();
    let commits = git_log(dir.path(), 0).unwrap();
    let content = git_show(dir.path(), &commits[0].sha, "test.rs").unwrap();
    assert!(content.is_some());
    assert!(content.unwrap().contains("fn hello"));
}

#[test]
fn git_show_returns_none_for_nonexistent() {
    let dir = create_test_repo();
    let commits = git_log(dir.path(), 0).unwrap();
    let content = git_show(dir.path(), &commits[0].sha, "nonexistent.rs").unwrap_err();
    // Should be a Git error
}

#[test]
fn is_shallow_on_normal_repo() {
    let dir = create_test_repo();
    assert!(!is_shallow(dir.path()));
}
```

Test helper `create_test_repo()`: `git init`, write `test.rs` with `fn hello() {}`, `git add .`, `git commit -m "initial"`. Return TempDir. `create_test_repo_with_change()`: same + second commit modifying the function.

- [ ] **Step 3: Tests pass, commit**

```
feat(train-data): git operations — log, diff-tree, show, shallow detection
```

---

### Task 6: Checkpoint (train_data/checkpoint.rs)

**Files:**
- Create: `src/train_data/checkpoint.rs`
- Modify: `src/train_data/mod.rs` (add `pub mod checkpoint;`)

- [ ] **Step 1: Write failing tests**

```rust
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
```

- [ ] **Step 2: Implement**

```rust
pub fn read_checkpoints(path: &Path) -> Result<HashMap<String, String>, TrainDataError>
// Reads tab-separated lines: repo_path\tsha. Returns empty map if file doesn't exist.

pub fn write_checkpoint(path: &Path, repo: &str, sha: &str) -> Result<(), TrainDataError>
// Reads existing, updates entry for repo, writes back atomically.

pub fn truncate_incomplete_line(path: &Path) -> Result<(), TrainDataError>
// If file doesn't end with \n, truncate to last \n. For crash recovery.
```

- [ ] **Step 3: Tests pass, commit**

```
feat(train-data): checkpoint read/write and JSONL truncation for resume
```

---

### Task 7: Orchestration (train_data/mod.rs)

**Files:**
- Modify: `src/train_data/mod.rs`

**Depends on:** Tasks 1-6 (all library components)

- [ ] **Step 1: Define public types**

```rust
#[derive(Debug, serde::Serialize)]
pub struct Triplet {
    pub query: String,
    pub raw_query: String,
    pub positive: String,
    pub negatives: Vec<String>,
    pub repo: String,
    pub commit: String,
    pub file: String,
    pub function_name: String,
    pub language: String,
    pub files_changed: usize,
    pub msg_len: usize,
    pub diff_lines: usize,
    pub function_size: usize,
    pub commit_date: String,
}

pub struct TrainDataConfig {
    pub repos: Vec<PathBuf>,
    pub output: PathBuf,
    pub max_commits: usize,
    pub min_msg_len: usize,
    pub max_files: usize,
    pub dedup_cap: usize,
    pub resume: bool,
    pub verbose: bool,
}

pub struct TrainDataStats {
    pub total_triplets: usize,
    pub repos_processed: usize,
    pub commits_processed: usize,
    pub commits_skipped: usize,
    pub parse_failures: usize,
    pub language_counts: HashMap<String, usize>,
}
```

- [ ] **Step 2: Implement generate_training_data**

```rust
pub fn generate_training_data(config: &TrainDataConfig) -> Result<TrainDataStats, TrainDataError>
```

Orchestration per repo:
1. Check `is_shallow()` — warn if true
2. Parse HEAD files with `Parser` (via `parse_file`, files exist on disk) → build BM25 index from function name+content+BLAKE3 hash
3. Load checkpoints if `--resume` — truncate incomplete JSONL line
4. Walk `git_log()` — for each commit:
   a. Skip if before checkpoint SHA
   b. Skip if message < `min_msg_len`
   c. `git_diff_tree()` → `parse_diff_output()` — skip if `files.len() > max_files` or empty
   d. For each file with supported extension:
      - `git_show()` → skip if None (oversized/non-UTF-8)
      - Detect language from extension
      - `parser.parse_source()` → extract `FunctionSpan`s
      - `find_changed_functions()` with hunks
   e. For each changed function:
      - Compute BLAKE3 content hash, check dedup cap
      - `normalize_query()` for query
      - `bm25.select_negatives()` with content hash guard
      - Build `Triplet`, serialize to JSONL, write to output
      - Track `diff_lines` from `DiffFile::total_added_lines()`
   f. Write checkpoint after commit completes
   g. If `verbose`: `tracing::debug!` per commit
   h. Progress: `tracing::info!` every 100 commits
5. Emit per-repo summary via `tracing::info!`
6. After all repos: emit final summary

- [ ] **Step 3: Write integration test with fixture repo**

Create a test repo with 3 commits (initial + 2 changes), 2 files, known function content. Run `generate_training_data`, parse output JSONL, verify:
- Correct number of triplets
- Queries are normalized
- Positives match commit-time content
- Negatives don't match positive content hash
- Checkpoint file exists

- [ ] **Step 4: Tests pass, commit**

```
feat(train-data): orchestration — generate_training_data with streaming JSONL
```

---

### Task 8: CLI wiring

**Files:**
- Create: `src/cli/commands/train_data.rs`
- Modify: `src/cli/mod.rs`

**Depends on:** Task 7

- [ ] **Step 1: Add TrainData to Commands enum in cli/mod.rs**

```rust
/// Generate training data for LoRA fine-tuning from git history
TrainData {
    /// Repository paths to process
    #[arg(long, required = true, num_args = 1..)]
    repos: Vec<PathBuf>,
    /// Output JSONL file path
    #[arg(long)]
    output: PathBuf,
    /// Maximum commits per repo (0 = all)
    #[arg(long, default_value = "0")]
    max_commits: usize,
    /// Skip commits with messages shorter than this
    #[arg(long, default_value = "15")]
    min_msg_len: usize,
    /// Skip commits touching more than this many files
    #[arg(long, default_value = "20")]
    max_files: usize,
    /// Max triplets per unique function content
    #[arg(long, default_value = "5")]
    dedup_cap: usize,
    /// Resume from checkpoint
    #[arg(long)]
    resume: bool,
    /// Per-commit debug logging
    #[arg(long)]
    verbose: bool,
},
```

- [ ] **Step 2: Create command handler**

`src/cli/commands/train_data.rs`:
```rust
pub fn cmd_train_data(...) -> Result<()> {
    let config = TrainDataConfig { ... };
    let stats = cqs::train_data::generate_training_data(&config)?;
    println!("Generated {} triplets from {} repos ({} commits)",
        stats.total_triplets, stats.repos_processed, stats.commits_processed);
    for (lang, count) in &stats.language_counts {
        println!("  {}: {} triplets", lang, count);
    }
    Ok(())
}
```

- [ ] **Step 3: Wire dispatch in run_with()**

Add `Commands::TrainData { ... }` match arm.

- [ ] **Step 4: Verify help works**

Run: `cargo run --features gpu-index -- train-data --help 2>&1`

- [ ] **Step 5: Commit**

```
feat(cli): add train-data command for LoRA training data generation
```

---

### Task 9: End-to-end test on cqs repo

**Depends on:** Task 8

- [ ] **Step 1: Run on cqs with limited commits**

```bash
cargo run --features gpu-index -- train-data \
  --repos /mnt/c/Projects/cqs \
  --output /tmp/cqs_train.jsonl \
  --max-commits 50
```

- [ ] **Step 2: Verify output**

```bash
wc -l /tmp/cqs_train.jsonl
head -1 /tmp/cqs_train.jsonl | python3 -m json.tool
```

- [ ] **Step 3: Spot-check 5 triplets**

Verify: query makes sense, positive is function content, negatives are keyword-similar but different. Check that `diff_lines`, `function_size`, `commit_date` are populated.

- [ ] **Step 4: Test resume**

```bash
# Run again with --resume — should produce no new output (all commits checkpointed)
cargo run --features gpu-index -- train-data \
  --repos /mnt/c/Projects/cqs \
  --output /tmp/cqs_train.jsonl \
  --max-commits 50 --resume
```

- [ ] **Step 5: Commit any fixes, then final commit**

```
test: verify train-data end-to-end on cqs repo
```

---

## Task Parallelism

| Phase | Tasks | Notes |
|-------|-------|-------|
| 1 (parallel) | 1, 2, 3, 4, 5, 6 | All independent files, zero overlap |
| 2 (sequential) | 7 | Depends on all of 1-6 |
| 3 (sequential) | 8 | Depends on 7 |
| 4 (sequential) | 9 | Depends on 8 |
