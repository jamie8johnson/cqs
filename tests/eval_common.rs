//! Shared evaluation infrastructure — types, test cases, and fixture paths.
//!
//! Used by eval_test.rs, model_eval.rs, and pipeline_eval.rs.

#![allow(dead_code)]

use cqs::parser::Language;
use std::path::PathBuf;

/// A single evaluation case: semantic query -> expected function name
pub struct EvalCase {
    pub query: &'static str,
    pub expected_name: &'static str,
    pub language: Language,
}

/// Get fixture path for a language (original fixtures for basic eval)
pub fn fixture_path(lang: Language) -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let ext = match lang {
        Language::Rust => "rs",
        Language::Python => "py",
        Language::TypeScript => "ts",
        Language::JavaScript => "js",
        Language::Go => "go",
        Language::C => "c",
        #[cfg(feature = "lang-cpp")]
        Language::Cpp => "cpp",
        Language::Java => "java",
        #[cfg(feature = "lang-csharp")]
        Language::CSharp => "cs",
        #[cfg(feature = "lang-fsharp")]
        Language::FSharp => "fs",
        #[cfg(feature = "lang-powershell")]
        Language::PowerShell => "ps1",
        #[cfg(feature = "lang-scala")]
        Language::Scala => "scala",
        #[cfg(feature = "lang-ruby")]
        Language::Ruby => "rb",
        #[cfg(feature = "lang-bash")]
        Language::Bash => "sh",
        #[cfg(feature = "lang-hcl")]
        Language::Hcl => "tf",
        Language::Sql => "sql",
        Language::Markdown => "md",
    };
    PathBuf::from(manifest_dir)
        .join("tests")
        .join("fixtures")
        .join(format!("eval_{}.{}", lang.to_string().to_lowercase(), ext))
}

/// Get fixture path for hard eval fixtures (confusable functions)
pub fn hard_fixture_path(lang: Language) -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let ext = match lang {
        Language::Rust => "rs",
        Language::Python => "py",
        Language::TypeScript => "ts",
        Language::JavaScript => "js",
        Language::Go => "go",
        Language::C => "c",
        #[cfg(feature = "lang-cpp")]
        Language::Cpp => "cpp",
        Language::Java => "java",
        #[cfg(feature = "lang-csharp")]
        Language::CSharp => "cs",
        #[cfg(feature = "lang-fsharp")]
        Language::FSharp => "fs",
        #[cfg(feature = "lang-powershell")]
        Language::PowerShell => "ps1",
        #[cfg(feature = "lang-scala")]
        Language::Scala => "scala",
        #[cfg(feature = "lang-ruby")]
        Language::Ruby => "rb",
        #[cfg(feature = "lang-bash")]
        Language::Bash => "sh",
        #[cfg(feature = "lang-hcl")]
        Language::Hcl => "tf",
        Language::Sql => "sql",
        Language::Markdown => "md",
    };
    PathBuf::from(manifest_dir)
        .join("tests")
        .join("fixtures")
        .join(format!(
            "eval_hard_{}.{}",
            lang.to_string().to_lowercase(),
            ext
        ))
}

/// Eval cases: 10 per language = 50 total
/// Queries are semantic descriptions, expected_name is the function that should match
pub const EVAL_CASES: &[EvalCase] = &[
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

/// Hard eval cases - confusable queries where multiple similar functions exist
/// 11 per language = 55 total
pub const HARD_EVAL_CASES: &[EvalCase] = &[
    // Rust (11) - must distinguish between 6 sort variants, 4 validators, etc.
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "merge_sort",
        language: Language::Rust,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heap_sort",
        language: Language::Rust,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertion_sort",
        language: Language::Rust,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radix_sort",
        language: Language::Rust,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validate_phone",
        language: Language::Rust,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validate_url",
        language: Language::Rust,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "pad_string",
        language: Language::Rust,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "count_words",
        language: Language::Rust,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extract_numbers",
        language: Language::Rust,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::Rust,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "should_allow",
        language: Language::Rust,
    },
    // Python (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "merge_sort",
        language: Language::Python,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heap_sort",
        language: Language::Python,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertion_sort",
        language: Language::Python,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radix_sort",
        language: Language::Python,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validate_phone",
        language: Language::Python,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validate_url",
        language: Language::Python,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "pad_string",
        language: Language::Python,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "count_words",
        language: Language::Python,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extract_numbers",
        language: Language::Python,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::Python,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "should_allow",
        language: Language::Python,
    },
    // TypeScript (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "mergeSort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heapSort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertionSort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radixSort",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validatePhone",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validateUrl",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "padString",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "countWords",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extractNumbers",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::TypeScript,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "shouldAllow",
        language: Language::TypeScript,
    },
    // JavaScript (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "mergeSort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heapSort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertionSort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radixSort",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validatePhone",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validateUrl",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "padString",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "countWords",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extractNumbers",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::JavaScript,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "shouldAllow",
        language: Language::JavaScript,
    },
    // Go (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "MergeSort",
        language: Language::Go,
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "HeapSort",
        language: Language::Go,
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "InsertionSort",
        language: Language::Go,
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "RadixSort",
        language: Language::Go,
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "ValidatePhone",
        language: Language::Go,
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "ValidateUrl",
        language: Language::Go,
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "PadString",
        language: Language::Go,
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "CountWords",
        language: Language::Go,
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "ExtractNumbers",
        language: Language::Go,
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreakerGo",
        language: Language::Go,
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "ShouldAllow",
        language: Language::Go,
    },
];
