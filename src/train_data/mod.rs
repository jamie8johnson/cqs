pub mod bm25;
pub mod checkpoint;
pub mod diff;
pub mod git;
pub mod query;

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::parser::{Chunk, Language, Parser};

use self::bm25::Bm25Index;
use self::checkpoint::{read_checkpoints, truncate_incomplete_line, write_checkpoint};
use self::diff::{find_changed_functions, parse_diff_output, FunctionSpan};
use self::git::{git_diff_tree, git_log, git_show, is_shallow};
use self::query::normalize_query;

// ─── Error ──────────────────────────────────────────────────────────────────

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

// ─── Types ──────────────────────────────────────────────────────────────────

/// A single training triplet: query + positive + hard negatives.
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

/// Configuration for training data generation.
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

/// Statistics from a training data generation run.
pub struct TrainDataStats {
    pub total_triplets: usize,
    pub repos_processed: usize,
    pub commits_processed: usize,
    pub commits_skipped: usize,
    pub parse_failures: usize,
    pub language_counts: HashMap<String, usize>,
}

// ─── Orchestration ──────────────────────────────────────────────────────────

/// Generate training data JSONL from git history across one or more repos.
///
/// For each repo: walks HEAD files to build a BM25 corpus, then iterates
/// commits to find changed functions. Each changed function produces one
/// triplet with the normalized commit message as query, the function content
/// as positive, and BM25-selected hard negatives.
pub fn generate_training_data(config: &TrainDataConfig) -> Result<TrainDataStats, TrainDataError> {
    let _span = tracing::info_span!("generate_training_data").entered();

    let parser = Parser::new().map_err(|e| TrainDataError::InvalidRepo(format!("{}", e)))?;

    // Checkpoint path is output path + ".checkpoint"
    let checkpoint_path = config.output.with_extension("jsonl.checkpoint");

    // Load checkpoints for resume
    let checkpoints = if config.resume {
        truncate_incomplete_line(&config.output)?;
        read_checkpoints(&checkpoint_path)?
    } else {
        HashMap::new()
    };

    // Open output file (append if resume, create/truncate otherwise)
    let output_file = if config.resume {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config.output)?
    } else {
        File::create(&config.output)?
    };
    let mut writer = BufWriter::new(output_file);

    let mut stats = TrainDataStats {
        total_triplets: 0,
        repos_processed: 0,
        commits_processed: 0,
        commits_skipped: 0,
        parse_failures: 0,
        language_counts: HashMap::new(),
    };

    for repo_path in &config.repos {
        let repo_str = repo_path.display().to_string();
        let _repo_span = tracing::info_span!("repo", repo = %repo_str).entered();

        // Validate repo
        if !repo_path.join(".git").exists() && !repo_path.join("HEAD").exists() {
            tracing::warn!(repo = %repo_str, "Not a git repository, skipping");
            continue;
        }

        // Shallow clone warning
        if is_shallow(repo_path) {
            tracing::warn!(
                repo = %repo_str,
                "Repository is a shallow clone — limited commit history"
            );
        }

        // Step 1: Build BM25 corpus from HEAD files on disk
        let bm25_docs = build_bm25_corpus(repo_path, &parser);
        let bm25 = Bm25Index::build(&bm25_docs);
        tracing::info!(
            repo = %repo_str,
            functions = bm25_docs.len(),
            "Built BM25 index from HEAD"
        );

        // Step 2: Walk git log
        let commits = git_log(repo_path, config.max_commits)?;
        let checkpoint_sha = checkpoints.get(&repo_str).cloned();

        // Track dedup per repo: content_hash -> count
        let mut dedup: HashMap<String, usize> = HashMap::new();

        let mut repo_triplets = 0usize;
        let mut past_checkpoint = checkpoint_sha.is_none();

        for (commit_idx, commit) in commits.iter().enumerate() {
            // Skip commits already processed (before checkpoint SHA)
            if !past_checkpoint {
                if commit.sha == *checkpoint_sha.as_ref().unwrap() {
                    past_checkpoint = true;
                    stats.commits_skipped += 1;
                    continue;
                }
                stats.commits_skipped += 1;
                continue;
            }

            // Skip short messages
            if commit.message.len() < config.min_msg_len {
                stats.commits_skipped += 1;
                continue;
            }

            // Get diff
            let diff_str = match git_diff_tree(repo_path, &commit.sha) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(sha = %commit.sha, error = %e, "Failed to get diff");
                    stats.commits_skipped += 1;
                    continue;
                }
            };

            let diff_files = parse_diff_output(&diff_str);

            // Skip if too many files or empty
            if diff_files.is_empty() || diff_files.len() > config.max_files {
                stats.commits_skipped += 1;
                // Still write checkpoint so we don't re-visit
                write_checkpoint(&checkpoint_path, &repo_str, &commit.sha)?;
                continue;
            }

            let files_changed = diff_files.len();
            let raw_query = commit.message.clone();
            let query = normalize_query(&raw_query);

            // Process each changed file
            for diff_file in &diff_files {
                // Check extension is supported
                let ext = match Path::new(&diff_file.path).extension() {
                    Some(e) => e.to_string_lossy().to_string(),
                    None => continue,
                };

                let language = match Language::from_extension(&ext) {
                    Some(l) => l,
                    None => continue,
                };

                // Get file content at this commit
                let content = match git_show(repo_path, &commit.sha, &diff_file.path) {
                    Ok(Some(c)) => c,
                    Ok(None) => continue, // oversized or binary
                    Err(_) => continue,   // file doesn't exist at this commit
                };

                // Parse to get function spans
                let chunks =
                    match parser.parse_source(&content, language, Path::new(&diff_file.path)) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::debug!(
                                file = %diff_file.path,
                                sha = %commit.sha,
                                error = %e,
                                "Parse failed"
                            );
                            stats.parse_failures += 1;
                            continue;
                        }
                    };

                let functions: Vec<FunctionSpan> = chunks_to_function_spans(&chunks);
                let changed = find_changed_functions(&diff_file.hunks, &functions);
                let diff_lines = diff_file.total_added_lines();

                for func in &changed {
                    // Dedup by content hash
                    let content_hash = blake3::hash(func.content.as_bytes()).to_hex().to_string();
                    let count = dedup.entry(content_hash.clone()).or_insert(0);
                    *count += 1;
                    if *count > config.dedup_cap {
                        continue;
                    }

                    // Select hard negatives
                    let negatives_raw =
                        bm25.select_negatives(&query, &content_hash, &func.content, 5);
                    let negatives: Vec<String> =
                        negatives_raw.into_iter().map(|(_, c)| c).collect();

                    let triplet = Triplet {
                        query: query.clone(),
                        raw_query: raw_query.clone(),
                        positive: func.content.clone(),
                        negatives,
                        repo: repo_str.clone(),
                        commit: commit.sha.clone(),
                        file: diff_file.path.clone(),
                        function_name: func.name.clone(),
                        language: language.to_string(),
                        files_changed,
                        msg_len: raw_query.len(),
                        diff_lines,
                        function_size: func.content.len(),
                        commit_date: commit.date.clone(),
                    };

                    serde_json::to_writer(&mut writer, &triplet)?;
                    writer.write_all(b"\n")?;

                    stats.total_triplets += 1;
                    repo_triplets += 1;
                    *stats
                        .language_counts
                        .entry(language.to_string())
                        .or_insert(0) += 1;
                }
            }

            stats.commits_processed += 1;
            write_checkpoint(&checkpoint_path, &repo_str, &commit.sha)?;

            if config.verbose {
                tracing::debug!(
                    sha = %commit.sha,
                    msg = %commit.message,
                    triplets = repo_triplets,
                    "Processed commit"
                );
            }

            // Progress every 100 commits
            if (commit_idx + 1) % 100 == 0 {
                tracing::info!(
                    repo = %repo_str,
                    commits = commit_idx + 1,
                    triplets = repo_triplets,
                    "Progress"
                );
            }
        }

        stats.repos_processed += 1;
        tracing::info!(
            repo = %repo_str,
            triplets = repo_triplets,
            commits = stats.commits_processed,
            skipped = stats.commits_skipped,
            "Repo complete"
        );
    }

    writer.flush()?;

    tracing::info!(
        total_triplets = stats.total_triplets,
        repos = stats.repos_processed,
        commits = stats.commits_processed,
        skipped = stats.commits_skipped,
        parse_failures = stats.parse_failures,
        "Training data generation complete"
    );

    Ok(stats)
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Convert parser Chunks into FunctionSpans for diff intersection.
fn chunks_to_function_spans(chunks: &[Chunk]) -> Vec<FunctionSpan> {
    chunks
        .iter()
        .map(|c| FunctionSpan {
            name: c.name.clone(),
            start_line: c.line_start as usize,
            end_line: c.line_end as usize,
            content: c.content.clone(),
        })
        .collect()
}

/// Walk a repo's files on disk, parse them, and build BM25 corpus.
///
/// Returns (content_hash, content) pairs for each function found.
/// Uses the `ignore` crate to respect .gitignore.
fn build_bm25_corpus(repo_path: &Path, parser: &Parser) -> Vec<(String, String)> {
    let _span = tracing::info_span!("build_bm25_corpus", repo = %repo_path.display()).entered();

    let mut docs: Vec<(String, String)> = Vec::new();

    let walker = ignore::WalkBuilder::new(repo_path)
        .hidden(true) // skip dotfiles
        .git_ignore(true)
        .build();

    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();

        // Check extension is supported
        let ext = match path.extension() {
            Some(e) => e.to_string_lossy().to_string(),
            None => continue,
        };
        if Language::from_extension(&ext).is_none() {
            continue;
        }

        // Parse file
        let chunks = match parser.parse_file(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for chunk in &chunks {
            // Only callable chunks from programming languages as negatives.
            // Config files (TOML, YAML, JSON, INI) and docs (Markdown) produce
            // chunks that are too easy to discriminate — the base model already
            // handles code-vs-prose distinction. Training budget should go toward
            // hard negatives: similar-looking code functions with different purposes.
            if !chunk.chunk_type.is_callable() {
                continue;
            }
            if matches!(
                chunk.language,
                Language::Toml
                    | Language::Yaml
                    | Language::Json
                    | Language::Ini
                    | Language::Markdown
                    | Language::Xml
                    | Language::Html
                    | Language::Css
                    | Language::Latex
            ) {
                continue;
            }
            let hash = blake3::hash(chunk.content.as_bytes()).to_hex().to_string();
            docs.push((hash, chunk.content.clone()));
        }
    }

    docs
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Create a test git repo with 3 commits and 2 files.
    ///
    /// Commit 1: initial — test.rs with `fn hello()`
    /// Commit 2: add greeting — test.rs modified, utils.rs added with `fn greet()`
    /// Commit 3: add farewell to utils — utils.rs modified with `fn farewell()`
    fn create_test_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();

        // git init + config
        run_git(repo, &["init"]);
        run_git(repo, &["config", "user.email", "test@test.com"]);
        run_git(repo, &["config", "user.name", "Test"]);

        // Commit 1: initial
        std::fs::write(
            repo.join("test.rs"),
            "fn hello() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        run_git(repo, &["add", "."]);
        run_git(
            repo,
            &["commit", "-m", "initial commit with hello function"],
        );

        // Commit 2: modify test.rs + add utils.rs
        std::fs::write(
            repo.join("test.rs"),
            "fn hello() {\n    println!(\"hello world\");\n}\n\nfn goodbye() {\n    println!(\"bye\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("utils.rs"),
            "fn greet(name: &str) {\n    println!(\"Hello, {}!\", name);\n}\n",
        )
        .unwrap();
        run_git(repo, &["add", "."]);
        run_git(
            repo,
            &["commit", "-m", "add greeting utilities and goodbye"],
        );

        // Commit 3: modify utils.rs
        std::fs::write(
            repo.join("utils.rs"),
            "fn greet(name: &str) {\n    println!(\"Hello, {}!\", name);\n}\n\nfn farewell(name: &str) {\n    println!(\"Goodbye, {}!\", name);\n}\n",
        )
        .unwrap();
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "-m", "add farewell function to utils"]);

        dir
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn integration_generate_training_data() {
        let repo_dir = create_test_repo();
        let out_dir = tempfile::TempDir::new().unwrap();
        let output_path = out_dir.path().join("train.jsonl");

        let config = TrainDataConfig {
            repos: vec![repo_dir.path().to_path_buf()],
            output: output_path.clone(),
            max_commits: 0,
            min_msg_len: 10,
            max_files: 20,
            dedup_cap: 5,
            resume: false,
            verbose: true,
        };

        let stats = generate_training_data(&config).unwrap();

        // We should get triplets from commits 2 and 3 (commit 1 is "initial commit"
        // which is 32 chars, so not skipped by min_msg_len=10)
        assert!(
            stats.total_triplets > 0,
            "Expected some triplets, got {}",
            stats.total_triplets
        );
        assert_eq!(stats.repos_processed, 1);
        assert!(stats.commits_processed > 0);

        // Verify JSONL output is valid
        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(!content.is_empty(), "Output file should not be empty");

        for line in content.lines() {
            let triplet: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("Invalid JSON line: {}\n{}", e, line));

            // Check required fields
            assert!(triplet.get("query").is_some(), "Missing query field");
            assert!(triplet.get("positive").is_some(), "Missing positive field");
            assert!(
                triplet.get("negatives").is_some(),
                "Missing negatives field"
            );
            assert!(triplet.get("repo").is_some(), "Missing repo field");
            assert!(triplet.get("commit").is_some(), "Missing commit field");
            assert!(triplet.get("file").is_some(), "Missing file field");
            assert!(
                triplet.get("function_name").is_some(),
                "Missing function_name"
            );
            assert!(triplet.get("language").is_some(), "Missing language");
            assert!(triplet.get("commit_date").is_some(), "Missing commit_date");
            assert!(triplet.get("diff_lines").is_some(), "Missing diff_lines");
            assert!(
                triplet.get("function_size").is_some(),
                "Missing function_size"
            );

            // Query should be normalized (no conventional prefix)
            let q = triplet["query"].as_str().unwrap();
            assert!(
                !q.starts_with("add ") && !q.starts_with("fix "),
                "Query not normalized: {}",
                q
            );

            // Language should be rust
            assert_eq!(triplet["language"].as_str().unwrap(), "rust");
        }

        // Checkpoint file should exist
        let checkpoint_path = output_path.with_extension("jsonl.checkpoint");
        assert!(
            checkpoint_path.exists(),
            "Checkpoint file should exist at {}",
            checkpoint_path.display()
        );
    }

    #[test]
    fn integration_resume_produces_no_duplicates() {
        let repo_dir = create_test_repo();
        let out_dir = tempfile::TempDir::new().unwrap();
        let output_path = out_dir.path().join("train.jsonl");

        let config = TrainDataConfig {
            repos: vec![repo_dir.path().to_path_buf()],
            output: output_path.clone(),
            max_commits: 0,
            min_msg_len: 10,
            max_files: 20,
            dedup_cap: 5,
            resume: false,
            verbose: false,
        };

        // First run
        let stats1 = generate_training_data(&config).unwrap();
        let first_count = std::fs::read_to_string(&output_path)
            .unwrap()
            .lines()
            .count();

        // Second run with resume
        let config_resume = TrainDataConfig {
            repos: vec![repo_dir.path().to_path_buf()],
            output: output_path.clone(),
            max_commits: 0,
            min_msg_len: 10,
            max_files: 20,
            dedup_cap: 5,
            resume: true,
            verbose: false,
        };

        let stats2 = generate_training_data(&config_resume).unwrap();
        let second_count = std::fs::read_to_string(&output_path)
            .unwrap()
            .lines()
            .count();

        // Resume should produce no new triplets
        assert_eq!(
            first_count, second_count,
            "Resume should not produce duplicates (first: {}, second: {})",
            first_count, second_count
        );
        assert_eq!(
            stats2.total_triplets, 0,
            "Resume run should emit 0 new triplets"
        );
        assert!(
            stats1.total_triplets > 0,
            "First run should have produced triplets"
        );
    }

    #[test]
    fn skips_non_git_repos() {
        let dir = tempfile::TempDir::new().unwrap();
        let out_dir = tempfile::TempDir::new().unwrap();
        let output_path = out_dir.path().join("train.jsonl");

        let config = TrainDataConfig {
            repos: vec![dir.path().to_path_buf()],
            output: output_path,
            max_commits: 0,
            min_msg_len: 10,
            max_files: 20,
            dedup_cap: 5,
            resume: false,
            verbose: false,
        };

        let stats = generate_training_data(&config).unwrap();
        assert_eq!(stats.repos_processed, 0);
        assert_eq!(stats.total_triplets, 0);
    }

    #[test]
    fn dedup_cap_limits_triplets() {
        let repo_dir = create_test_repo();
        let out_dir = tempfile::TempDir::new().unwrap();
        let output_path = out_dir.path().join("train.jsonl");

        // dedup_cap=1 means each unique function content only produces 1 triplet
        let config = TrainDataConfig {
            repos: vec![repo_dir.path().to_path_buf()],
            output: output_path.clone(),
            max_commits: 0,
            min_msg_len: 10,
            max_files: 20,
            dedup_cap: 1,
            resume: false,
            verbose: false,
        };

        let stats_capped = generate_training_data(&config).unwrap();

        // Run again with high cap for comparison
        let output_path2 = out_dir.path().join("train2.jsonl");
        let config2 = TrainDataConfig {
            repos: vec![repo_dir.path().to_path_buf()],
            output: output_path2,
            max_commits: 0,
            min_msg_len: 10,
            max_files: 20,
            dedup_cap: 100,
            resume: false,
            verbose: false,
        };

        let stats_uncapped = generate_training_data(&config2).unwrap();

        assert!(
            stats_capped.total_triplets <= stats_uncapped.total_triplets,
            "Capped ({}) should be <= uncapped ({})",
            stats_capped.total_triplets,
            stats_uncapped.total_triplets
        );
    }
}
