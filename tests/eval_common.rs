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
    /// Alternative acceptable names (e.g., class methods when expected is the class).
    /// Empty slice means only `expected_name` is accepted.
    pub also_accept: &'static [&'static str],
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
        #[cfg(feature = "lang-kotlin")]
        Language::Kotlin => "kt",
        #[cfg(feature = "lang-swift")]
        Language::Swift => "swift",
        #[cfg(feature = "lang-objc")]
        Language::ObjC => "m",
        Language::Sql => "sql",
        #[cfg(feature = "lang-protobuf")]
        Language::Protobuf => "proto",
        #[cfg(feature = "lang-graphql")]
        Language::GraphQL => "graphql",
        #[cfg(feature = "lang-php")]
        Language::Php => "php",
        #[cfg(feature = "lang-lua")]
        Language::Lua => "lua",
        #[cfg(feature = "lang-zig")]
        Language::Zig => "zig",
        #[cfg(feature = "lang-r")]
        Language::R => "r",
        #[cfg(feature = "lang-yaml")]
        Language::Yaml => "yaml",
        #[cfg(feature = "lang-toml")]
        Language::Toml => "toml",
        #[cfg(feature = "lang-elixir")]
        Language::Elixir => "ex",
        #[cfg(feature = "lang-erlang")]
        Language::Erlang => "erl",
        #[cfg(feature = "lang-haskell")]
        Language::Haskell => "hs",
        #[cfg(feature = "lang-ocaml")]
        Language::OCaml => "ml",
        #[cfg(feature = "lang-julia")]
        Language::Julia => "jl",
        #[cfg(feature = "lang-gleam")]
        Language::Gleam => "gleam",
        #[cfg(feature = "lang-css")]
        Language::Css => "css",
        #[cfg(feature = "lang-perl")]
        Language::Perl => "pl",
        #[cfg(feature = "lang-html")]
        Language::Html => "html",
        #[cfg(feature = "lang-json")]
        Language::Json => "json",
        #[cfg(feature = "lang-xml")]
        Language::Xml => "xml",
        #[cfg(feature = "lang-ini")]
        Language::Ini => "ini",
        #[cfg(feature = "lang-nix")]
        Language::Nix => "nix",
        #[cfg(feature = "lang-make")]
        Language::Make => "mk",
        #[cfg(feature = "lang-latex")]
        Language::Latex => "tex",
        #[cfg(feature = "lang-solidity")]
        Language::Solidity => "sol",
        #[cfg(feature = "lang-cuda")]
        Language::Cuda => "cu",
        #[cfg(feature = "lang-glsl")]
        Language::Glsl => "vert",
        Language::Markdown => "md",
        #[cfg(feature = "lang-svelte")]
        Language::Svelte => "svelte",
        #[cfg(feature = "lang-razor")]
        Language::Razor => "cshtml",
        #[cfg(feature = "lang-vbnet")]
        Language::VbNet => "vb",
        #[cfg(feature = "lang-vue")]
        Language::Vue => "vue",
        #[cfg(feature = "lang-aspx")]
        Language::Aspx => "aspx",
        #[cfg(feature = "lang-st")]
        Language::StructuredText => "st",
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
        #[cfg(feature = "lang-kotlin")]
        Language::Kotlin => "kt",
        #[cfg(feature = "lang-swift")]
        Language::Swift => "swift",
        #[cfg(feature = "lang-objc")]
        Language::ObjC => "m",
        Language::Sql => "sql",
        #[cfg(feature = "lang-protobuf")]
        Language::Protobuf => "proto",
        #[cfg(feature = "lang-graphql")]
        Language::GraphQL => "graphql",
        #[cfg(feature = "lang-php")]
        Language::Php => "php",
        #[cfg(feature = "lang-lua")]
        Language::Lua => "lua",
        #[cfg(feature = "lang-zig")]
        Language::Zig => "zig",
        #[cfg(feature = "lang-r")]
        Language::R => "r",
        #[cfg(feature = "lang-yaml")]
        Language::Yaml => "yaml",
        #[cfg(feature = "lang-toml")]
        Language::Toml => "toml",
        #[cfg(feature = "lang-elixir")]
        Language::Elixir => "ex",
        #[cfg(feature = "lang-erlang")]
        Language::Erlang => "erl",
        #[cfg(feature = "lang-haskell")]
        Language::Haskell => "hs",
        #[cfg(feature = "lang-ocaml")]
        Language::OCaml => "ml",
        #[cfg(feature = "lang-julia")]
        Language::Julia => "jl",
        #[cfg(feature = "lang-gleam")]
        Language::Gleam => "gleam",
        #[cfg(feature = "lang-css")]
        Language::Css => "css",
        #[cfg(feature = "lang-perl")]
        Language::Perl => "pl",
        #[cfg(feature = "lang-html")]
        Language::Html => "html",
        #[cfg(feature = "lang-json")]
        Language::Json => "json",
        #[cfg(feature = "lang-xml")]
        Language::Xml => "xml",
        #[cfg(feature = "lang-ini")]
        Language::Ini => "ini",
        #[cfg(feature = "lang-nix")]
        Language::Nix => "nix",
        #[cfg(feature = "lang-make")]
        Language::Make => "mk",
        #[cfg(feature = "lang-latex")]
        Language::Latex => "tex",
        #[cfg(feature = "lang-solidity")]
        Language::Solidity => "sol",
        #[cfg(feature = "lang-cuda")]
        Language::Cuda => "cu",
        #[cfg(feature = "lang-glsl")]
        Language::Glsl => "vert",
        Language::Markdown => "md",
        #[cfg(feature = "lang-svelte")]
        Language::Svelte => "svelte",
        #[cfg(feature = "lang-razor")]
        Language::Razor => "cshtml",
        #[cfg(feature = "lang-vbnet")]
        Language::VbNet => "vb",
        #[cfg(feature = "lang-vue")]
        Language::Vue => "vue",
        #[cfg(feature = "lang-aspx")]
        Language::Aspx => "aspx",
        #[cfg(feature = "lang-st")]
        Language::StructuredText => "st",
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
        also_accept: &[],
    },
    EvalCase {
        query: "validate email address format",
        expected_name: "validate_email",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "parse JSON configuration file",
        expected_name: "parse_json_config",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "compute SHA256 hash",
        expected_name: "hash_sha256",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "format number as currency with commas",
        expected_name: "format_currency",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "convert camelCase to snake_case",
        expected_name: "camel_to_snake",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "truncate string with ellipsis",
        expected_name: "truncate_string",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "check if string is valid UUID",
        expected_name: "is_valid_uuid",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "sort array with quicksort algorithm",
        expected_name: "quicksort",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "memoize function results",
        expected_name: "get_or_compute",
        language: Language::Rust,
        also_accept: &[],
    },
    // Python (10)
    EvalCase {
        query: "retry with exponential backoff",
        expected_name: "retry_with_backoff",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "validate email address format",
        expected_name: "validate_email",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "parse JSON config from file",
        expected_name: "parse_json_config",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "compute SHA256 hash of bytes",
        expected_name: "hash_sha256",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "format currency with dollar sign",
        expected_name: "format_currency",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "convert camelCase to snake_case",
        expected_name: "camel_to_snake",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "truncate string with ellipsis",
        expected_name: "truncate_string",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "check UUID format validity",
        expected_name: "is_valid_uuid",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "quicksort sorting algorithm",
        expected_name: "quicksort",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "cache function results decorator",
        expected_name: "memoize",
        language: Language::Python,
        also_accept: &[],
    },
    // TypeScript (10)
    EvalCase {
        query: "retry operation with exponential backoff",
        expected_name: "retryWithBackoff",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "validate email address",
        expected_name: "validateEmail",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "parse JSON config string",
        expected_name: "parseJsonConfig",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "SHA256 hash computation",
        expected_name: "hashSha256",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "format money with commas",
        expected_name: "formatCurrency",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "camelCase to snake_case conversion",
        expected_name: "camelToSnake",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "truncate long string with dots",
        expected_name: "truncateString",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "UUID format validation",
        expected_name: "isValidUuid",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "quicksort implementation",
        expected_name: "quicksort",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "memoization cache wrapper",
        expected_name: "memoize",
        language: Language::TypeScript,
        also_accept: &[],
    },
    // JavaScript (10)
    EvalCase {
        query: "retry with exponential backoff delay",
        expected_name: "retryWithBackoff",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "email validation regex",
        expected_name: "validateEmail",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "JSON configuration parser",
        expected_name: "parseJsonConfig",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "SHA256 cryptographic hash",
        expected_name: "hashSha256",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "currency formatter",
        expected_name: "formatCurrency",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "convert camel case to snake case",
        expected_name: "camelToSnake",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "string truncation with ellipsis",
        expected_name: "truncateString",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "UUID validation check",
        expected_name: "isValidUuid",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "quicksort divide and conquer",
        expected_name: "quicksort",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "function result memoization",
        expected_name: "memoize",
        language: Language::JavaScript,
        also_accept: &[],
    },
    // Go (10)
    EvalCase {
        query: "retry with exponential backoff",
        expected_name: "RetryWithBackoff",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "email address validation",
        expected_name: "ValidateEmail",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "parse JSON config file",
        expected_name: "ParseJsonConfig",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "compute SHA256 hash",
        expected_name: "HashSha256",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "format currency with commas",
        expected_name: "FormatCurrency",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "camelCase to snake_case",
        expected_name: "CamelToSnake",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "truncate string ellipsis",
        expected_name: "TruncateString",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "validate UUID format",
        expected_name: "IsValidUuid",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "quicksort algorithm",
        expected_name: "Quicksort",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "memoization get or compute",
        expected_name: "GetOrCompute",
        language: Language::Go,
        also_accept: &[],
    },
];

/// Hard eval cases - confusable queries where multiple similar functions exist
/// 11 per language x 7 languages = 77 total (PHP behind lang-php feature)
pub const HARD_EVAL_CASES: &[EvalCase] = &[
    // Rust (11) - must distinguish between 6 sort variants, 4 validators, etc.
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "merge_sort",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heap_sort",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertion_sort",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radix_sort",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validate_phone",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validate_url",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "pad_string",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "count_words",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extract_numbers",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::Rust,
        also_accept: &["should_allow", "record_failure"],
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "should_allow",
        language: Language::Rust,
        also_accept: &["CircuitBreaker"],
    },
    // Python (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "merge_sort",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heap_sort",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertion_sort",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radix_sort",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validate_phone",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validate_url",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "pad_string",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "count_words",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extract_numbers",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::Python,
        also_accept: &["should_allow", "record_failure"],
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "should_allow",
        language: Language::Python,
        also_accept: &["CircuitBreaker"],
    },
    // TypeScript (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "mergeSort",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heapSort",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertionSort",
        language: Language::TypeScript,
        also_accept: &["_insertionSortSmall"],
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radixSort",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validatePhone",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validateUrl",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "padString",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "countWords",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extractNumbers",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::TypeScript,
        also_accept: &["shouldAllow", "recordFailure"],
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "shouldAllow",
        language: Language::TypeScript,
        also_accept: &["CircuitBreaker"],
    },
    // JavaScript (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "mergeSort",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heapSort",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertionSort",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radixSort",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validatePhone",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validateUrl",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "padString",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "countWords",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extractNumbers",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::JavaScript,
        also_accept: &["shouldAllow", "recordFailure"],
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "shouldAllow",
        language: Language::JavaScript,
        also_accept: &["CircuitBreaker"],
    },
    // Go (11)
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "MergeSort",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "HeapSort",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "InsertionSort",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "RadixSort",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "ValidatePhone",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "ValidateUrl",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "PadString",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "CountWords",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "ExtractNumbers",
        language: Language::Go,
        also_accept: &[],
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreakerGo",
        language: Language::Go,
        also_accept: &["ShouldAllow", "RecordFailure"],
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "ShouldAllow",
        language: Language::Go,
        also_accept: &["CircuitBreakerGo"],
    },
    // Java (11) - same confusable categories
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "mergeSort",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heapSort",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertionSort",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radixSort",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validatePhone",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validateUrl",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "padString",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "count number of words in text",
        expected_name: "countWords",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extractNumbers",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::Java,
        also_accept: &["shouldAllow", "recordFailure"],
    },
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "shouldAllow",
        language: Language::Java,
        also_accept: &["CircuitBreaker"],
    },
    // PHP (11) - same confusable categories
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "stable sort preserving relative order of equal elements",
        expected_name: "mergeSort",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "sort using binary max-heap data structure",
        expected_name: "heapSort",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "simple sort efficient for small nearly sorted arrays",
        expected_name: "insertionSort",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "non-comparison integer sort processing digits",
        expected_name: "radixSort",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "validate phone number with international country code",
        expected_name: "validatePhone",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "check if URL has valid protocol and hostname",
        expected_name: "validateUrl",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "pad string to fixed width with fill character",
        expected_name: "padString",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "count number of words in text",
        expected_name: "countWords",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "extract numeric values from mixed text string",
        expected_name: "extractNumbers",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "stop calling service after consecutive failures",
        expected_name: "CircuitBreaker",
        language: Language::Php,
        also_accept: &["shouldAllow", "recordFailure"],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "check whether circuit allows request through",
        expected_name: "shouldAllow",
        language: Language::Php,
        also_accept: &["CircuitBreaker"],
    },
];

/// Held-out evaluation cases — never tuned against, used for unbiased measurement.
///
/// Three categories:
/// 1. Uncovered functions (exist in fixtures but had no eval query)
/// 2. Paraphrase queries (different phrasing for already-covered functions)
/// 3. Behavioral queries (describe need/use-case, not the algorithm)
pub const HOLDOUT_EVAL_CASES: &[EvalCase] = &[
    // ================================================================
    // Category 1: Uncovered basic eval functions
    // ================================================================

    // --- http_post_json ---
    EvalCase {
        query: "send data to remote API endpoint as JSON",
        expected_name: "http_post_json",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "send data to remote API endpoint as JSON",
        expected_name: "http_post_json",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "send data to remote API endpoint as JSON",
        expected_name: "httpPostJson",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "send data to remote API endpoint as JSON",
        expected_name: "httpPostJson",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "send data to remote API endpoint as JSON",
        expected_name: "HttpPostJson",
        language: Language::Go,
        also_accept: &[],
    },
    // --- read_file_utf8 ---
    EvalCase {
        query: "load text content from file on disk",
        expected_name: "read_file_utf8",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "load text content from file on disk",
        expected_name: "read_file_utf8",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "load text content from file on disk",
        expected_name: "readFileUtf8",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "load text content from file on disk",
        expected_name: "readFileUtf8",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "load text content from file on disk",
        expected_name: "ReadFileUtf8",
        language: Language::Go,
        also_accept: &[],
    },
    // --- write_file_atomic ---
    EvalCase {
        query: "safely write data to file without corruption on crash",
        expected_name: "write_file_atomic",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "safely write data to file without corruption on crash",
        expected_name: "write_file_atomic",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "safely write data to file without corruption on crash",
        expected_name: "writeFileAtomic",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "safely write data to file without corruption on crash",
        expected_name: "writeFileAtomic",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "safely write data to file without corruption on crash",
        expected_name: "WriteFileAtomic",
        language: Language::Go,
        also_accept: &[],
    },
    // --- calculate_mean ---
    EvalCase {
        query: "compute arithmetic average of a list of numbers",
        expected_name: "calculate_mean",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "compute arithmetic average of a list of numbers",
        expected_name: "calculate_mean",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "compute arithmetic average of a list of numbers",
        expected_name: "calculateMean",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "compute arithmetic average of a list of numbers",
        expected_name: "calculateMean",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "compute arithmetic average of a list of numbers",
        expected_name: "CalculateMean",
        language: Language::Go,
        also_accept: &[],
    },
    // --- find_maximum ---
    EvalCase {
        query: "find the largest element in an array",
        expected_name: "find_maximum",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "find the largest element in an array",
        expected_name: "find_maximum",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "find the largest element in an array",
        expected_name: "findMaximum",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "find the largest element in an array",
        expected_name: "findMaximum",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "find the largest element in an array",
        expected_name: "FindMaximum",
        language: Language::Go,
        also_accept: &[],
    },
    // --- generate_random_id ---
    EvalCase {
        query: "create a unique random identifier string",
        expected_name: "generate_random_id",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "create a unique random identifier string",
        expected_name: "generate_random_id",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "create a unique random identifier string",
        expected_name: "generateRandomId",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "create a unique random identifier string",
        expected_name: "generateRandomId",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "create a unique random identifier string",
        expected_name: "GenerateRandomId",
        language: Language::Go,
        also_accept: &[],
    },
    // --- compress_rle ---
    EvalCase {
        query: "compress data using run-length encoding",
        expected_name: "compress_rle",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "compress data using run-length encoding",
        expected_name: "compress_rle",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "compress data using run-length encoding",
        expected_name: "compressRle",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "compress data using run-length encoding",
        expected_name: "compressRle",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "compress data using run-length encoding",
        expected_name: "CompressRle",
        language: Language::Go,
        also_accept: &[],
    },
    // --- parse_cli_args ---
    EvalCase {
        query: "parse command-line flags and arguments into a map",
        expected_name: "parse_cli_args",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "parse command-line flags and arguments into a map",
        expected_name: "parse_cli_args",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "parse command-line flags and arguments into a map",
        expected_name: "parseCliArgs",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "parse command-line flags and arguments into a map",
        expected_name: "parseCliArgs",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "parse command-line flags and arguments into a map",
        expected_name: "ParseCliArgs",
        language: Language::Go,
        also_accept: &[],
    },
    // --- debounce ---
    EvalCase {
        query: "delay function execution until input stops changing",
        expected_name: "Debouncer",
        language: Language::Rust,
        also_accept: &["should_execute"],
    },
    EvalCase {
        query: "delay function execution until input stops changing",
        expected_name: "debounce",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "delay function execution until input stops changing",
        expected_name: "debounce",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "delay function execution until input stops changing",
        expected_name: "debounce",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "delay function execution until input stops changing",
        expected_name: "NewDebouncer",
        language: Language::Go,
        also_accept: &["Debouncer", "ShouldExecute"],
    },
    // --- flatten_nested ---
    // (Rust has no flatten in basic eval fixture — skip Rust)
    EvalCase {
        query: "recursively flatten nested lists into a single flat list",
        expected_name: "flatten_nested_list",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "recursively flatten nested lists into a single flat list",
        expected_name: "flattenNestedArray",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "recursively flatten nested lists into a single flat list",
        expected_name: "flattenNestedArray",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "recursively flatten nested lists into a single flat list",
        expected_name: "FlattenNestedSlice",
        language: Language::Go,
        also_accept: &[],
    },
    // --- deep_merge ---
    // (Rust has no deep_merge in basic eval fixture — skip Rust)
    EvalCase {
        query: "recursively merge two nested dictionaries or config objects",
        expected_name: "deep_merge_dicts",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "recursively merge two nested dictionaries or config objects",
        expected_name: "deepMergeObjects",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "recursively merge two nested dictionaries or config objects",
        expected_name: "deepMergeObjects",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "recursively merge two nested dictionaries or config objects",
        expected_name: "DeepMergeMaps",
        language: Language::Go,
        also_accept: &[],
    },
    // ================================================================
    // Category 1b: Uncovered hard eval functions
    // ================================================================

    // --- bubble_sort ---
    EvalCase {
        query: "sort by repeatedly swapping adjacent out-of-order elements",
        expected_name: "bubble_sort",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "sort by repeatedly swapping adjacent out-of-order elements",
        expected_name: "bubble_sort",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "sort by repeatedly swapping adjacent out-of-order elements",
        expected_name: "bubbleSort",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "sort by repeatedly swapping adjacent out-of-order elements",
        expected_name: "bubbleSort",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "sort by repeatedly swapping adjacent out-of-order elements",
        expected_name: "BubbleSort",
        language: Language::Go,
        also_accept: &[],
    },
    // --- reverse_string ---
    EvalCase {
        query: "reverse the order of characters in a string",
        expected_name: "reverse_string",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "reverse the order of characters in a string",
        expected_name: "reverse_string",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "reverse the order of characters in a string",
        expected_name: "reverseString",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "reverse the order of characters in a string",
        expected_name: "reverseString",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "reverse the order of characters in a string",
        expected_name: "ReverseString",
        language: Language::Go,
        also_accept: &[],
    },
    // --- validate_ip_address ---
    EvalCase {
        query: "check if string is a valid IPv4 address with four octets",
        expected_name: "validate_ip_address",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "check if string is a valid IPv4 address with four octets",
        expected_name: "validate_ip_address",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "check if string is a valid IPv4 address with four octets",
        expected_name: "validateIpAddress",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "check if string is a valid IPv4 address with four octets",
        expected_name: "validateIpAddress",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "check if string is a valid IPv4 address with four octets",
        expected_name: "ValidateIpAddress",
        language: Language::Go,
        also_accept: &[],
    },
    // --- hash_crc32 ---
    EvalCase {
        query: "compute CRC32 checksum of byte data",
        expected_name: "hash_crc32",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "compute CRC32 checksum of byte data",
        expected_name: "hash_crc32",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "compute CRC32 checksum of byte data",
        expected_name: "hashCrc32",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "compute CRC32 checksum of byte data",
        expected_name: "hashCrc32",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "compute CRC32 checksum of byte data",
        expected_name: "HashCrc32",
        language: Language::Go,
        also_accept: &[],
    },
    // --- RateLimiter ---
    EvalCase {
        query: "throttle request rate using token bucket algorithm",
        expected_name: "RateLimiter",
        language: Language::Rust,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "throttle request rate using token bucket algorithm",
        expected_name: "RateLimiter",
        language: Language::Python,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "throttle request rate using token bucket algorithm",
        expected_name: "RateLimiter",
        language: Language::TypeScript,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "throttle request rate using token bucket algorithm",
        expected_name: "RateLimiter",
        language: Language::JavaScript,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "throttle request rate using token bucket algorithm",
        expected_name: "RateLimiterGo",
        language: Language::Go,
        also_accept: &["NewRateLimiter", "Allow"],
    },
    // --- record_success ---
    EvalCase {
        query: "mark a successful call to reset circuit breaker failure count",
        expected_name: "record_success",
        language: Language::Rust,
        also_accept: &["CircuitBreaker"],
    },
    EvalCase {
        query: "mark a successful call to reset circuit breaker failure count",
        expected_name: "record_success",
        language: Language::Python,
        also_accept: &["CircuitBreaker"],
    },
    EvalCase {
        query: "mark a successful call to reset circuit breaker failure count",
        expected_name: "recordSuccess",
        language: Language::TypeScript,
        also_accept: &["CircuitBreaker"],
    },
    EvalCase {
        query: "mark a successful call to reset circuit breaker failure count",
        expected_name: "recordSuccess",
        language: Language::JavaScript,
        also_accept: &["CircuitBreaker"],
    },
    EvalCase {
        query: "mark a successful call to reset circuit breaker failure count",
        expected_name: "RecordSuccess",
        language: Language::Go,
        also_accept: &["CircuitBreakerGo"],
    },
    // ================================================================
    // Category 2: Paraphrase queries (alt phrasing for covered functions)
    // ================================================================

    // --- retry_with_backoff (original: "retry HTTP request with exponential backoff") ---
    EvalCase {
        query: "automatically retry failed operations with increasing delay between attempts",
        expected_name: "retry_with_backoff",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "automatically retry failed operations with increasing delay between attempts",
        expected_name: "retry_with_backoff",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "automatically retry failed operations with increasing delay between attempts",
        expected_name: "retryWithBackoff",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "automatically retry failed operations with increasing delay between attempts",
        expected_name: "retryWithBackoff",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "automatically retry failed operations with increasing delay between attempts",
        expected_name: "RetryWithBackoff",
        language: Language::Go,
        also_accept: &[],
    },
    // --- validate_email (original: "validate email address format with regex") ---
    EvalCase {
        query: "check if a string looks like a properly formatted email with @ and domain",
        expected_name: "validate_email",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "check if a string looks like a properly formatted email with @ and domain",
        expected_name: "validate_email",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "check if a string looks like a properly formatted email with @ and domain",
        expected_name: "validateEmail",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "check if a string looks like a properly formatted email with @ and domain",
        expected_name: "validateEmail",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "check if a string looks like a properly formatted email with @ and domain",
        expected_name: "ValidateEmail",
        language: Language::Go,
        also_accept: &[],
    },
    // --- quicksort (original: "partition-based in-place sorting algorithm") ---
    EvalCase {
        query: "divide and conquer sort that picks a pivot and partitions around it",
        expected_name: "quicksort",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "divide and conquer sort that picks a pivot and partitions around it",
        expected_name: "quicksort",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "divide and conquer sort that picks a pivot and partitions around it",
        expected_name: "quicksort",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "divide and conquer sort that picks a pivot and partitions around it",
        expected_name: "quicksort",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "divide and conquer sort that picks a pivot and partitions around it",
        expected_name: "Quicksort",
        language: Language::Go,
        also_accept: &[],
    },
    // --- format_currency (original: "format number as US currency string") ---
    EvalCase {
        query: "display a decimal number as money with dollar sign and commas",
        expected_name: "format_currency",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "display a decimal number as money with dollar sign and commas",
        expected_name: "format_currency",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "display a decimal number as money with dollar sign and commas",
        expected_name: "formatCurrency",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "display a decimal number as money with dollar sign and commas",
        expected_name: "formatCurrency",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "display a decimal number as money with dollar sign and commas",
        expected_name: "FormatCurrency",
        language: Language::Go,
        also_accept: &[],
    },
    // --- camel_to_snake (original: "convert camelCase string to snake_case") ---
    EvalCase {
        query: "transform PascalCase or camelCase identifiers to underscore_separated lowercase",
        expected_name: "camel_to_snake",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "transform PascalCase or camelCase identifiers to underscore_separated lowercase",
        expected_name: "camel_to_snake",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "transform PascalCase or camelCase identifiers to underscore_separated lowercase",
        expected_name: "camelToSnake",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "transform PascalCase or camelCase identifiers to underscore_separated lowercase",
        expected_name: "camelToSnake",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "transform PascalCase or camelCase identifiers to underscore_separated lowercase",
        expected_name: "CamelToSnake",
        language: Language::Go,
        also_accept: &[],
    },
    // --- is_valid_uuid (original: "check if string is valid UUID format") ---
    EvalCase {
        query: "validate that a string matches the 8-4-4-4-12 hexadecimal UUID pattern",
        expected_name: "is_valid_uuid",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "validate that a string matches the 8-4-4-4-12 hexadecimal UUID pattern",
        expected_name: "is_valid_uuid",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "validate that a string matches the 8-4-4-4-12 hexadecimal UUID pattern",
        expected_name: "isValidUuid",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "validate that a string matches the 8-4-4-4-12 hexadecimal UUID pattern",
        expected_name: "isValidUuid",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "validate that a string matches the 8-4-4-4-12 hexadecimal UUID pattern",
        expected_name: "IsValidUuid",
        language: Language::Go,
        also_accept: &[],
    },
    // --- hash_sha256 (original: "compute SHA-256 hash of input data") ---
    EvalCase {
        query: "generate a cryptographic digest of data using the SHA-256 algorithm",
        expected_name: "hash_sha256",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "generate a cryptographic digest of data using the SHA-256 algorithm",
        expected_name: "hash_sha256",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "generate a cryptographic digest of data using the SHA-256 algorithm",
        expected_name: "hashSha256",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "generate a cryptographic digest of data using the SHA-256 algorithm",
        expected_name: "hashSha256",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "generate a cryptographic digest of data using the SHA-256 algorithm",
        expected_name: "HashSha256",
        language: Language::Go,
        also_accept: &[],
    },
    // --- truncate_string (original: "truncate string to maximum length with ellipsis") ---
    EvalCase {
        query: "shorten text to a character limit and append ellipsis if trimmed",
        expected_name: "truncate_string",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "shorten text to a character limit and append ellipsis if trimmed",
        expected_name: "truncate_string",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "shorten text to a character limit and append ellipsis if trimmed",
        expected_name: "truncateString",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "shorten text to a character limit and append ellipsis if trimmed",
        expected_name: "truncateString",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "shorten text to a character limit and append ellipsis if trimmed",
        expected_name: "TruncateString",
        language: Language::Go,
        also_accept: &[],
    },
    // --- parse_json_config (original: "parse JSON configuration file into structured data") ---
    EvalCase {
        query: "deserialize a JSON string into a typed configuration object",
        expected_name: "parse_json_config",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "deserialize a JSON string into a typed configuration object",
        expected_name: "parse_json_config",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "deserialize a JSON string into a typed configuration object",
        expected_name: "parseJsonConfig",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "deserialize a JSON string into a typed configuration object",
        expected_name: "parseJsonConfig",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "deserialize a JSON string into a typed configuration object",
        expected_name: "ParseJsonConfig",
        language: Language::Go,
        also_accept: &[],
    },
    // ================================================================
    // Category 3: Behavioral / use-case queries
    // ================================================================

    // "I need to prevent API abuse" → RateLimiter
    EvalCase {
        query: "prevent API abuse by limiting how many requests a client can make per second",
        expected_name: "RateLimiter",
        language: Language::Rust,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "prevent API abuse by limiting how many requests a client can make per second",
        expected_name: "RateLimiter",
        language: Language::Python,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "prevent API abuse by limiting how many requests a client can make per second",
        expected_name: "RateLimiter",
        language: Language::TypeScript,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "prevent API abuse by limiting how many requests a client can make per second",
        expected_name: "RateLimiter",
        language: Language::JavaScript,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "prevent API abuse by limiting how many requests a client can make per second",
        expected_name: "RateLimiterGo",
        language: Language::Go,
        also_accept: &["NewRateLimiter", "Allow"],
    },
    // "I need to cache expensive computation results" → memoize/get_or_compute
    EvalCase {
        query: "cache the results of expensive function calls to avoid redundant computation",
        expected_name: "get_or_compute",
        language: Language::Rust,
        also_accept: &["Memoizer"],
    },
    EvalCase {
        query: "cache the results of expensive function calls to avoid redundant computation",
        expected_name: "memoize",
        language: Language::Python,
        also_accept: &["Memoizer"],
    },
    EvalCase {
        query: "cache the results of expensive function calls to avoid redundant computation",
        expected_name: "memoize",
        language: Language::TypeScript,
        also_accept: &["Memoizer"],
    },
    EvalCase {
        query: "cache the results of expensive function calls to avoid redundant computation",
        expected_name: "memoize",
        language: Language::JavaScript,
        also_accept: &["Memoizer"],
    },
    EvalCase {
        query: "cache the results of expensive function calls to avoid redundant computation",
        expected_name: "GetOrCompute",
        language: Language::Go,
        also_accept: &["Memoizer", "NewMemoizer"],
    },
    // "I need to make sure a file path is a real file" → read_file_utf8
    EvalCase {
        query: "read the entire contents of a text file as a UTF-8 encoded string",
        expected_name: "read_file_utf8",
        language: Language::Rust,
        also_accept: &[],
    },
    EvalCase {
        query: "read the entire contents of a text file as a UTF-8 encoded string",
        expected_name: "read_file_utf8",
        language: Language::Python,
        also_accept: &[],
    },
    EvalCase {
        query: "read the entire contents of a text file as a UTF-8 encoded string",
        expected_name: "readFileUtf8",
        language: Language::TypeScript,
        also_accept: &[],
    },
    EvalCase {
        query: "read the entire contents of a text file as a UTF-8 encoded string",
        expected_name: "readFileUtf8",
        language: Language::JavaScript,
        also_accept: &[],
    },
    EvalCase {
        query: "read the entire contents of a text file as a UTF-8 encoded string",
        expected_name: "ReadFileUtf8",
        language: Language::Go,
        also_accept: &[],
    },
    // ================================================================
    // Java holdout cases
    // ================================================================

    // --- Category 1: Uncovered basic eval functions (Java) ---
    EvalCase {
        query: "send data to remote API endpoint as JSON",
        expected_name: "httpPostJson",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "load text content from file on disk",
        expected_name: "readFileUtf8",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "safely write data to file without corruption on crash",
        expected_name: "writeFileAtomic",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "compute arithmetic average of a list of numbers",
        expected_name: "calculateMean",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "find the largest element in an array",
        expected_name: "findMaximum",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "create a unique random identifier string",
        expected_name: "generateRandomId",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "compress data using run-length encoding",
        expected_name: "compressRle",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "parse command-line flags and arguments into a map",
        expected_name: "parseCliArgs",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "delay function execution until input stops changing",
        expected_name: "Debouncer",
        language: Language::Java,
        also_accept: &["debounce"],
    },
    // --- Category 1b: Uncovered hard eval functions (Java) ---
    EvalCase {
        query: "sort by repeatedly swapping adjacent out-of-order elements",
        expected_name: "bubbleSort",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "reverse the order of characters in a string",
        expected_name: "reverseString",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "check if string is a valid IPv4 address with four octets",
        expected_name: "validateIpAddress",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "compute CRC32 checksum of byte data",
        expected_name: "hashCrc32",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "throttle request rate using token bucket algorithm",
        expected_name: "RateLimiter",
        language: Language::Java,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "mark a successful call to reset circuit breaker failure count",
        expected_name: "recordSuccess",
        language: Language::Java,
        also_accept: &["CircuitBreaker"],
    },
    // --- Category 2: Paraphrase queries (Java) ---
    EvalCase {
        query: "automatically retry failed operations with increasing delay between attempts",
        expected_name: "retryWithBackoff",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "check if a string looks like a properly formatted email with @ and domain",
        expected_name: "validateEmail",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "divide and conquer sort that picks a pivot and partitions around it",
        expected_name: "quicksort",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "display a decimal number as money with dollar sign and commas",
        expected_name: "formatCurrency",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "transform PascalCase or camelCase identifiers to underscore_separated lowercase",
        expected_name: "camelToSnake",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "validate that a string matches the 8-4-4-4-12 hexadecimal UUID pattern",
        expected_name: "isValidUuid",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "generate a cryptographic digest of data using the SHA-256 algorithm",
        expected_name: "hashSha256",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "shorten text to a character limit and append ellipsis if trimmed",
        expected_name: "truncateString",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "deserialize a JSON string into a typed configuration object",
        expected_name: "parseJsonConfig",
        language: Language::Java,
        also_accept: &[],
    },
    // --- Category 3: Behavioral queries (Java) ---
    EvalCase {
        query: "prevent API abuse by limiting how many requests a client can make per second",
        expected_name: "RateLimiter",
        language: Language::Java,
        also_accept: &["allow"],
    },
    EvalCase {
        query: "cache the results of expensive function calls to avoid redundant computation",
        expected_name: "getOrCompute",
        language: Language::Java,
        also_accept: &["Memoizer"],
    },
    EvalCase {
        query: "read the entire contents of a text file as a UTF-8 encoded string",
        expected_name: "readFileUtf8",
        language: Language::Java,
        also_accept: &[],
    },
    // --- Category 4: New query categories (Java) ---
    // Tree/graph traversal
    EvalCase {
        query: "traverse graph level by level visiting nearest nodes first using a queue",
        expected_name: "bfsTraversal",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "traverse graph exploring as deep as possible before backtracking using a stack",
        expected_name: "dfsTraversal",
        language: Language::Java,
        also_accept: &[],
    },
    // Caching strategies
    EvalCase {
        query: "cache that evicts the least recently accessed entry when full",
        expected_name: "LruCache",
        language: Language::Java,
        also_accept: &["get", "put"],
    },
    EvalCase {
        query: "cache that automatically expires entries after a time-to-live duration",
        expected_name: "TtlCache",
        language: Language::Java,
        also_accept: &["get", "put", "evictExpired"],
    },
    // Serialization
    EvalCase {
        query: "convert list of records into comma-separated values format with header",
        expected_name: "serializeToCsv",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "convert key-value data into XML document with named elements",
        expected_name: "serializeToXml",
        language: Language::Java,
        also_accept: &[],
    },
    // String matching
    EvalCase {
        query: "match filename against wildcard pattern with asterisk and question mark",
        expected_name: "globMatch",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "match text against regular expression and extract captured groups",
        expected_name: "regexMatchGroups",
        language: Language::Java,
        also_accept: &[],
    },
    // Error handling
    EvalCase {
        query: "try primary operation and fall back to alternative on repeated failure",
        expected_name: "retryWithFallback",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "recursively flatten nested lists into a single flat list",
        expected_name: "flattenNestedList",
        language: Language::Java,
        also_accept: &[],
    },
    EvalCase {
        query: "recursively merge two nested dictionaries or config objects",
        expected_name: "deepMergeMaps",
        language: Language::Java,
        also_accept: &[],
    },
    // ================================================================
    // PHP holdout cases
    // ================================================================

    // --- Category 1: Uncovered basic eval functions (PHP) ---
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "send data to remote API endpoint as JSON",
        expected_name: "httpPostJson",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "load text content from file on disk",
        expected_name: "readFileUtf8",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "safely write data to file without corruption on crash",
        expected_name: "writeFileAtomic",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "compute arithmetic average of a list of numbers",
        expected_name: "calculateMean",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "find the largest element in an array",
        expected_name: "findMaximum",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "create a unique random identifier string",
        expected_name: "generateRandomId",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "compress data using run-length encoding",
        expected_name: "compressRle",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "parse command-line flags and arguments into a map",
        expected_name: "parseCliArgs",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "delay function execution until input stops changing",
        expected_name: "debounce",
        language: Language::Php,
        also_accept: &[],
    },
    // --- Category 1b: Uncovered hard eval functions (PHP) ---
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "sort by repeatedly swapping adjacent out-of-order elements",
        expected_name: "bubbleSort",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "reverse the order of characters in a string",
        expected_name: "reverseString",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "check if string is a valid IPv4 address with four octets",
        expected_name: "validateIpAddress",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "compute CRC32 checksum of string data",
        expected_name: "hashCrc32",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "throttle request rate using token bucket algorithm",
        expected_name: "RateLimiter",
        language: Language::Php,
        also_accept: &["allow"],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "mark a successful call to reset circuit breaker failure count",
        expected_name: "recordSuccess",
        language: Language::Php,
        also_accept: &["CircuitBreaker"],
    },
    // --- Category 2: Paraphrase queries (PHP) ---
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "automatically retry failed operations with increasing delay between attempts",
        expected_name: "retryWithBackoff",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "check if a string looks like a properly formatted email with @ and domain",
        expected_name: "validateEmail",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "divide and conquer sort that picks a pivot and partitions around it",
        expected_name: "quicksort",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "display a decimal number as money with dollar sign and commas",
        expected_name: "formatCurrency",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "transform PascalCase or camelCase identifiers to underscore_separated lowercase",
        expected_name: "camelToSnake",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "validate that a string matches the 8-4-4-4-12 hexadecimal UUID pattern",
        expected_name: "isValidUuid",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "generate a cryptographic digest of data using the SHA-256 algorithm",
        expected_name: "hashSha256",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "shorten text to a character limit and append ellipsis if trimmed",
        expected_name: "truncateString",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "deserialize a JSON string into a typed configuration object",
        expected_name: "parseJsonConfig",
        language: Language::Php,
        also_accept: &[],
    },
    // --- Category 3: Behavioral queries (PHP) ---
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "prevent API abuse by limiting how many requests a client can make per second",
        expected_name: "RateLimiter",
        language: Language::Php,
        also_accept: &["allow"],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "cache the results of expensive function calls to avoid redundant computation",
        expected_name: "memoize",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "read the entire contents of a text file as a UTF-8 encoded string",
        expected_name: "readFileUtf8",
        language: Language::Php,
        also_accept: &[],
    },
    // --- Category 4: New query categories (PHP) ---
    // Tree/graph traversal
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "traverse graph level by level visiting nearest nodes first using a queue",
        expected_name: "bfsTraversal",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "traverse graph exploring as deep as possible before backtracking using a stack",
        expected_name: "dfsTraversal",
        language: Language::Php,
        also_accept: &[],
    },
    // Caching strategies
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "cache that evicts the least recently accessed entry when full",
        expected_name: "LruCache",
        language: Language::Php,
        also_accept: &["get", "put"],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "cache that automatically expires entries after a time-to-live duration",
        expected_name: "TtlCache",
        language: Language::Php,
        also_accept: &["get", "put", "evictExpired"],
    },
    // Serialization
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "convert list of records into comma-separated values format with header",
        expected_name: "serializeToCsv",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "convert key-value data into XML document with named elements",
        expected_name: "serializeToXml",
        language: Language::Php,
        also_accept: &[],
    },
    // String matching
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "match filename against wildcard pattern with asterisk and question mark",
        expected_name: "globMatch",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "match text against regular expression and extract captured groups",
        expected_name: "regexMatchGroups",
        language: Language::Php,
        also_accept: &[],
    },
    // Error handling
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "try primary operation and fall back to alternative on repeated failure",
        expected_name: "retryWithFallback",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "recursively flatten nested arrays into a single flat array",
        expected_name: "flattenNestedArray",
        language: Language::Php,
        also_accept: &[],
    },
    #[cfg(feature = "lang-php")]
    EvalCase {
        query: "recursively merge two nested arrays with deep merge strategy",
        expected_name: "deepMergeArrays",
        language: Language::Php,
        also_accept: &[],
    },
];
