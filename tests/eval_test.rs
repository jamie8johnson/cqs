//! Eval suite for measuring search quality
//!
//! Run with: cargo test eval -- --ignored --nocapture
//! (Ignored by default because embedding generation is slow)

use cqs::embedder::Embedder;
use cqs::parser::{Language, Parser};
use cqs::store::{ModelInfo, SearchFilter, Store};
use std::collections::HashMap;
use std::path::PathBuf;
use tempfile::TempDir;

/// An eval test case: query -> expected function name
struct EvalCase {
    query: &'static str,
    expected_name: &'static str,
    language: Language,
}

/// Eval cases: 10 per language = 50 total
/// Queries are semantic descriptions, expected_name is the function that should match
const EVAL_CASES: &[EvalCase] = &[
    // Rust (10)
    EvalCase {
        query: "retry with exponential backoff",
        expected_name: "retry_with_backoff",
        language: Language::Rust,
    },
    EvalCase {
        query: "validate email address format",
        expected_name: "validate_email",
        language: Language::Rust,
    },
    EvalCase {
        query: "parse JSON configuration file",
        expected_name: "parse_json_config",
        language: Language::Rust,
    },
    EvalCase {
        query: "compute SHA256 hash",
        expected_name: "hash_sha256",
        language: Language::Rust,
    },
    EvalCase {
        query: "format number as currency with commas",
        expected_name: "format_currency",
        language: Language::Rust,
    },
    EvalCase {
        query: "convert camelCase to snake_case",
        expected_name: "camel_to_snake",
        language: Language::Rust,
    },
    EvalCase {
        query: "truncate string with ellipsis",
        expected_name: "truncate_string",
        language: Language::Rust,
    },
    EvalCase {
        query: "check if string is valid UUID",
        expected_name: "is_valid_uuid",
        language: Language::Rust,
    },
    EvalCase {
        query: "sort array with quicksort algorithm",
        expected_name: "quicksort",
        language: Language::Rust,
    },
    EvalCase {
        query: "memoize function results",
        expected_name: "get_or_compute",
        language: Language::Rust,
    },
    // Python (10)
    EvalCase {
        query: "retry with exponential backoff",
        expected_name: "retry_with_backoff",
        language: Language::Python,
    },
    EvalCase {
        query: "validate email address format",
        expected_name: "validate_email",
        language: Language::Python,
    },
    EvalCase {
        query: "parse JSON config from file",
        expected_name: "parse_json_config",
        language: Language::Python,
    },
    EvalCase {
        query: "compute SHA256 hash of bytes",
        expected_name: "hash_sha256",
        language: Language::Python,
    },
    EvalCase {
        query: "format currency with dollar sign",
        expected_name: "format_currency",
        language: Language::Python,
    },
    EvalCase {
        query: "convert camelCase to snake_case",
        expected_name: "camel_to_snake",
        language: Language::Python,
    },
    EvalCase {
        query: "truncate string with ellipsis",
        expected_name: "truncate_string",
        language: Language::Python,
    },
    EvalCase {
        query: "check UUID format validity",
        expected_name: "is_valid_uuid",
        language: Language::Python,
    },
    EvalCase {
        query: "quicksort sorting algorithm",
        expected_name: "quicksort",
        language: Language::Python,
    },
    EvalCase {
        query: "cache function results decorator",
        expected_name: "memoize",
        language: Language::Python,
    },
    // TypeScript (10)
    EvalCase {
        query: "retry operation with exponential backoff",
        expected_name: "retryWithBackoff",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "validate email address",
        expected_name: "validateEmail",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "parse JSON config string",
        expected_name: "parseJsonConfig",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "SHA256 hash computation",
        expected_name: "hashSha256",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "format money with commas",
        expected_name: "formatCurrency",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "camelCase to snake_case conversion",
        expected_name: "camelToSnake",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "truncate long string with dots",
        expected_name: "truncateString",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "UUID format validation",
        expected_name: "isValidUuid",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "quicksort implementation",
        expected_name: "quicksort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "memoization cache wrapper",
        expected_name: "memoize",
        language: Language::TypeScript,
    },
    // JavaScript (10)
    EvalCase {
        query: "retry with exponential backoff delay",
        expected_name: "retryWithBackoff",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "email validation regex",
        expected_name: "validateEmail",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "JSON configuration parser",
        expected_name: "parseJsonConfig",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "SHA256 cryptographic hash",
        expected_name: "hashSha256",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "currency formatter",
        expected_name: "formatCurrency",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "convert camel case to snake case",
        expected_name: "camelToSnake",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "string truncation with ellipsis",
        expected_name: "truncateString",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "UUID validation check",
        expected_name: "isValidUuid",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "quicksort divide and conquer",
        expected_name: "quicksort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "function result memoization",
        expected_name: "memoize",
        language: Language::JavaScript,
    },
    // Go (10)
    EvalCase {
        query: "retry with exponential backoff",
        expected_name: "RetryWithBackoff",
        language: Language::Go,
    },
    EvalCase {
        query: "email address validation",
        expected_name: "ValidateEmail",
        language: Language::Go,
    },
    EvalCase {
        query: "parse JSON config file",
        expected_name: "ParseJsonConfig",
        language: Language::Go,
    },
    EvalCase {
        query: "compute SHA256 hash",
        expected_name: "HashSha256",
        language: Language::Go,
    },
    EvalCase {
        query: "format currency with commas",
        expected_name: "FormatCurrency",
        language: Language::Go,
    },
    EvalCase {
        query: "camelCase to snake_case",
        expected_name: "CamelToSnake",
        language: Language::Go,
    },
    EvalCase {
        query: "truncate string ellipsis",
        expected_name: "TruncateString",
        language: Language::Go,
    },
    EvalCase {
        query: "validate UUID format",
        expected_name: "IsValidUuid",
        language: Language::Go,
    },
    EvalCase {
        query: "quicksort algorithm",
        expected_name: "Quicksort",
        language: Language::Go,
    },
    EvalCase {
        query: "memoization get or compute",
        expected_name: "GetOrCompute",
        language: Language::Go,
    },
];

/// Get fixture path for a language
fn fixture_path(lang: Language) -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let ext = match lang {
        Language::Rust => "rs",
        Language::Python => "py",
        Language::TypeScript => "ts",
        Language::JavaScript => "js",
        Language::Go => "go",
    };
    PathBuf::from(manifest_dir)
        .join("tests")
        .join("fixtures")
        .join(format!("eval_{}.{}", lang.to_string().to_lowercase(), ext))
}

#[test]
#[ignore] // Slow test - run with: cargo test eval -- --ignored --nocapture
fn test_recall_at_5() {
    // Initialize embedder
    eprintln!("Initializing embedder...");
    let mut embedder = Embedder::new().expect("Failed to initialize embedder");

    // Initialize parser
    let parser = Parser::new().expect("Failed to initialize parser");

    // Create temporary store
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("eval.db");
    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Parse and index all fixtures
    eprintln!("Parsing and indexing fixtures...");
    let mut chunk_count = 0;
    for lang in [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ] {
        let path = fixture_path(lang);
        eprintln!("  Parsing {:?}...", path);

        let chunks = parser.parse_file(&path).expect("Failed to parse fixture");
        eprintln!("    Found {} chunks", chunks.len());

        for chunk in &chunks {
            // Generate embedding for chunk (combine signature + doc + content)
            let text = format!(
                "{}\n{}\n{}",
                chunk.signature,
                chunk.doc.as_deref().unwrap_or(""),
                chunk.content
            );
            let embeddings = embedder
                .embed_documents(&[&text])
                .expect("Failed to embed chunk");
            let embedding = &embeddings[0];

            // Store chunk (mtime 0 since these are test fixtures)
            store
                .upsert_chunk(chunk, embedding, 0)
                .expect("Failed to store chunk");
            chunk_count += 1;
        }
    }
    eprintln!("Indexed {} total chunks", chunk_count);

    // Run eval cases
    eprintln!("\nRunning {} eval cases...\n", EVAL_CASES.len());

    let mut results_by_lang: HashMap<Language, (usize, usize)> = HashMap::new();
    let mut total_hits = 0;
    let mut total_cases = 0;

    for case in EVAL_CASES {
        // Generate query embedding
        let query_embedding = embedder
            .embed_query(case.query)
            .expect("Failed to embed query");

        // Search with language filter
        let filter = SearchFilter {
            languages: Some(vec![case.language]),
            path_pattern: None,
            ..Default::default()
        };
        let results = store
            .search_filtered(&query_embedding, &filter, 5, 0.0)
            .expect("Failed to search");

        // Check if expected name is in top-5
        let found = results.iter().any(|r| r.chunk.name == case.expected_name);

        // Track results
        let (hits, total) = results_by_lang.entry(case.language).or_insert((0, 0));
        *total += 1;
        if found {
            *hits += 1;
            total_hits += 1;
        }
        total_cases += 1;

        // Print result
        let status = if found { "✓" } else { "✗" };
        let top_names: Vec<_> = results
            .iter()
            .take(3)
            .map(|r| r.chunk.name.as_str())
            .collect();
        eprintln!(
            "{} [{:?}] \"{}\" -> expected: {}, got: {:?}",
            status, case.language, case.query, case.expected_name, top_names
        );
    }

    // Print summary
    eprintln!("\n=== Results ===");
    for lang in [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ] {
        if let Some((hits, total)) = results_by_lang.get(&lang) {
            let pct = (*hits as f64 / *total as f64) * 100.0;
            eprintln!("{:?}: {}/{} ({:.0}%)", lang, hits, total, pct);
        }
    }
    let overall_pct = (total_hits as f64 / total_cases as f64) * 100.0;
    eprintln!(
        "\nOverall Recall@5: {}/{} ({:.0}%)",
        total_hits, total_cases, overall_pct
    );

    // Assert minimum quality threshold (8/10 = 80% per language is the goal)
    assert!(
        overall_pct >= 60.0,
        "Recall@5 below 60% threshold: {:.0}%",
        overall_pct
    );
}

#[test]
fn test_fixtures_exist() {
    // Quick sanity check that all fixtures exist
    for lang in [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ] {
        let path = fixture_path(lang);
        assert!(path.exists(), "Fixture missing: {:?}", path);
    }
}
