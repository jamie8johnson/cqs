//! Language registry for code parsing
//!
//! This module provides a registry of supported programming languages,
//! each with its own tree-sitter grammar, query patterns, and extraction rules.
//!
//! Languages are registered at compile time based on feature flags.
//! To add a new language, add one line to the `define_languages!` invocation
//! and create a language module file (see existing language modules for examples).
//!
//! # Feature Flags
//!
//! - `lang-rust` - Rust support (enabled by default)
//! - `lang-python` - Python support (enabled by default)
//! - `lang-typescript` - TypeScript support (enabled by default)
//! - `lang-javascript` - JavaScript support (enabled by default)
//! - `lang-go` - Go support (enabled by default)
//! - `lang-c` - C support (enabled by default)
//! - `lang-cpp` - C++ support (enabled by default)
//! - `lang-java` - Java support (enabled by default)
//! - `lang-csharp` - C# support (enabled by default)
//! - `lang-fsharp` - F# support (enabled by default)
//! - `lang-powershell` - PowerShell support (enabled by default)
//! - `lang-scala` - Scala support (enabled by default)
//! - `lang-ruby` - Ruby support (enabled by default)
//! - `lang-bash` - Bash support (enabled by default)
//! - `lang-hcl` - HCL/Terraform support (enabled by default)
//! - `lang-kotlin` - Kotlin support (enabled by default)
//! - `lang-swift` - Swift support (enabled by default)
//! - `lang-objc` - Objective-C support (enabled by default)
//! - `lang-sql` - SQL support (enabled by default)
//! - `lang-protobuf` - Protobuf support (enabled by default)
//! - `lang-graphql` - GraphQL support (enabled by default)
//! - `lang-php` - PHP support (enabled by default)
//! - `lang-lua` - Lua support (enabled by default)
//! - `lang-zig` - Zig support (enabled by default)
//! - `lang-r` - R support (enabled by default)
//! - `lang-yaml` - YAML support (enabled by default)
//! - `lang-toml` - TOML support (enabled by default)
//! - `lang-elixir` - Elixir support (enabled by default)
//! - `lang-erlang` - Erlang support (enabled by default)
//! - `lang-haskell` - Haskell support (enabled by default)
//! - `lang-ocaml` - OCaml support (enabled by default)
//! - `lang-julia` - Julia support (enabled by default)
//! - `lang-gleam` - Gleam support (enabled by default)
//! - `lang-css` - CSS support (enabled by default)
//! - `lang-perl` - Perl support (enabled by default)
//! - `lang-html` - HTML support (enabled by default)
//! - `lang-json` - JSON support (enabled by default)
//! - `lang-xml` - XML support (enabled by default)
//! - `lang-ini` - INI support (enabled by default)
//! - `lang-nix` - Nix support (enabled by default)
//! - `lang-make` - Makefile support (enabled by default)
//! - `lang-latex` - LaTeX support (enabled by default)
//! - `lang-solidity` - Solidity support (enabled by default)
//! - `lang-cuda` - CUDA support (enabled by default)
//! - `lang-glsl` - GLSL support (enabled by default)
//! - `lang-svelte` - Svelte support (enabled by default)
//! - `lang-razor` - Razor/CSHTML support (enabled by default)
//! - `lang-vbnet` - VB.NET support (enabled by default)
//! - `lang-vue` - Vue support (enabled by default)
//! - `lang-markdown` - Markdown support (enabled by default, no external deps)
//! - `lang-aspx` - ASP.NET Web Forms support (enabled by default, no external deps)
//! - `lang-st` - IEC 61131-3 Structured Text support (enabled by default)
//! - `lang-all` - All languages

use std::collections::HashMap;
use std::sync::LazyLock;

mod languages;

// ---------------------------------------------------------------------------
// Macro: define_languages!
//
// Generates from a single declaration table:
//   - Feature-gated `mod` declarations
//   - `Language` enum with variants and doc comments
//   - `Display` impl (variant → name string)
//   - `FromStr` impl (name string → variant, case-insensitive)
//   - `Language::all_variants()`, `valid_names()`, `valid_names_display()`
//   - `LanguageRegistry::new()` with feature-gated registrations
//
// Adding a language = one new line here + a language module file + Cargo.toml.
// ---------------------------------------------------------------------------
/// Defines a set of supported programming languages with feature-gating and serialization support.
///
/// # Arguments
///
/// - `$variant`: The enum variant name for each language
/// - `$doc`: Optional documentation comments for each variant
/// - `$name`: The string representation of each language (used for display and parsing)
/// - `$feature`: The cargo feature flag that gates each language module
/// - `$module`: The module name containing language-specific implementation
///
/// # Returns
///
/// Expands to:
/// - A `Language` enum with all variants, deriving `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `Hash`, and serde serialization
/// - `Display` implementation converting enum variants to their string names
/// - `FromStr` implementation parsing case-insensitive strings to enum variants
/// - `Language::all_variants()` method returning a static slice of all language variants
/// - Feature-gated module imports for each language
///
/// # Panics
///
/// Calling methods on a language variant whose feature flag is disabled may panic; use `is_enabled()` to check first.
macro_rules! define_languages {
    (
        $(
            $(#[doc = $doc:expr])*
            $variant:ident => $name:literal, feature = $feature:literal, def = $def_fn:path;
        )+
    ) => {
        /// Supported programming languages
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
        #[serde(rename_all = "lowercase")]
        pub enum Language {
            $(
                $(#[doc = $doc])*
                $variant,
            )+
        }

        impl std::fmt::Display for Language {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $(Language::$variant => write!(f, $name),)+
                }
            }
        }

        impl std::str::FromStr for Language {
            type Err = ParseLanguageError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s.to_lowercase().as_str() {
                    $($name => Ok(Language::$variant),)+
                    _ => Err(ParseLanguageError { input: s.to_string() }),
                }
            }
        }

        impl Language {
            /// Returns a slice of all Language variants (regardless of feature flags).
            ///
            /// **Note:** Calling `.def()` on a variant whose feature is disabled will panic.
            /// Use `is_enabled()` to check first, or use `REGISTRY.all()` for enabled-only iteration.
            pub fn all_variants() -> &'static [Language] {
                &[$(Language::$variant),+]
            }

            /// Returns all valid language name strings
            pub fn valid_names() -> &'static [&'static str] {
                &[$($name),+]
            }

            /// Formatted string of valid language names for error messages
            pub fn valid_names_display() -> String {
                [$($name),+].join(", ")
            }
        }

        impl LanguageRegistry {
            /// Create a new registry with all enabled languages
            fn new() -> Self {
                let mut reg = Self {
                    by_name: HashMap::new(),
                    by_extension: HashMap::new(),
                };
                $(
                    #[cfg(feature = $feature)]
                    reg.register($def_fn());
                )+
                reg
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Type definitions (prerequisites for language modules and macro expansion)
// ---------------------------------------------------------------------------

/// Function signature for post-processing extracted chunks.
/// Takes `(&mut name, &mut chunk_type, definition_node, source)`.
/// Returns `false` to discard the chunk.
#[allow(clippy::ptr_arg)] // &mut String required: 14 implementations mutate the name (push_str, replace, etc.)
pub type PostProcessChunkFn = fn(&mut String, &mut ChunkType, tree_sitter::Node, &str) -> bool;

/// Function signature for language-specific structural pattern matchers.
/// Takes `(content, name)` and returns true if the pattern matches.
pub type StructuralMatcherFn = fn(&str, &str) -> bool;

/// An injection rule for multi-grammar parsing.
///
/// Defines how embedded language regions within a host grammar are identified
/// and parsed. For example, `<script>` within HTML → JavaScript.
#[derive(Debug)]
pub struct InjectionRule {
    /// Node kind of the container element (e.g., "script_element", "style_element")
    pub container_kind: &'static str,
    /// Node kind of the content node within the container (e.g., "raw_text")
    pub content_kind: &'static str,
    /// Default target language for the embedded content
    pub target_language: &'static str,
    /// Optional: detect language from container attributes (e.g., `<script lang="ts">`)
    pub detect_language: Option<fn(tree_sitter::Node, &str) -> Option<&'static str>>,
    /// When true, `container_lines` derives from each content child's line range
    /// instead of the container's line range. Required for languages like PHP where
    /// the container is `program` (entire file) but content is individual `text`
    /// nodes between `<?php ... ?>` blocks.
    pub content_scoped_lines: bool,
}

/// How to extract field names from struct/class/record bodies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldStyle {
    /// No field extraction (markup, config, shell languages).
    None,
    /// Name appears before separator: `name: Type`, `name = value`.
    NameFirst {
        /// Characters to split on (e.g., ":=")
        separators: &'static str,
        /// Space-separated prefixes to strip before extraction.
        /// Includes both visibility and value keywords.
        strip_prefixes: &'static str,
    },
    /// Type appears before name: `Type name;` (C, C++, Java, C#).
    /// Takes last whitespace-delimited token before `;`, `=`, or `,`.
    TypeFirst {
        /// Space-separated prefixes to strip.
        strip_prefixes: &'static str,
    },
}

/// A language definition with all parsing configuration
#[non_exhaustive]
pub struct LanguageDef {
    /// Language name (e.g., "rust", "python")
    pub name: &'static str,
    /// Function to get the tree-sitter grammar (None for non-tree-sitter languages like Markdown)
    pub grammar: Option<fn() -> tree_sitter::Language>,
    /// File extensions for this language
    pub extensions: &'static [&'static str],
    /// Tree-sitter query for extracting code chunks
    pub chunk_query: &'static str,
    /// Tree-sitter query for extracting function calls (optional)
    pub call_query: Option<&'static str>,
    /// How to extract signatures
    pub signature_style: SignatureStyle,
    /// Node types that contain doc comments
    pub doc_nodes: &'static [&'static str],
    /// Node kinds that are themselves methods (e.g., Go's "method_declaration")
    pub method_node_kinds: &'static [&'static str],
    /// Parent node kinds that make a child function a method (e.g., Rust's "impl_item")
    pub method_containers: &'static [&'static str],
    /// Per-language stopwords for keyword extraction (used by `extract_body_keywords`)
    pub stopwords: &'static [&'static str],
    /// Per-language return type extractor (used by NL description generation).
    /// Returns `None` if the language has no type annotations or the signature has no return type.
    pub extract_return_nl: fn(&str) -> Option<String>,
    /// Suggest a test file path for a given source file.
    /// Receives `(stem, parent_dir)` and returns a suggested test path.
    /// `None` uses the fallback pattern `{parent}/tests/{stem}_test.{ext}`.
    pub test_file_suggestion: Option<fn(&str, &str) -> String>,
    /// Suggest a test function name for a given function name (EX-18).
    /// Receives `base_name` (stripped of `self.` prefix) and returns suggested test name.
    /// `None` uses the fallback `test_{base_name}` (snake_case).
    pub test_name_suggestion: Option<fn(&str) -> String>,
    /// Tree-sitter query for extracting type references (optional).
    /// Uses classified capture names: `@param_type`, `@return_type`, `@field_type`,
    /// `@impl_type`, `@bound_type`, `@alias_type`, `@type_ref` (catch-all).
    pub type_query: Option<&'static str>,
    /// Standard library / builtin types to exclude from type-edge analysis.
    /// Each language defines its own set. At runtime, these are unioned into
    /// the global `COMMON_TYPES` set in `focused_read.rs`.
    pub common_types: &'static [&'static str],
    /// Node kinds that are intermediate body containers (walk up to parent for name).
    /// e.g., `"class_body"` (JS/TS/Java), `"declaration_list"` (C#/Rust).
    /// Used by the generic container type extraction algorithm.
    pub container_body_kinds: &'static [&'static str],
    /// Override for extracting parent type name from a method container node.
    /// `None` = use default algorithm (walk up from body kinds, read `"name"` field).
    /// Only Rust needs an override (`impl_item` uses `"type"` field, not `"name"`).
    pub extract_container_name: Option<fn(tree_sitter::Node, &str) -> Option<String>>,
    /// Override for extracting parent type from a function's own declarator.
    /// For C++ out-of-class methods: `void MyClass::method()` → Some("MyClass").
    /// Called in `infer_chunk_type` before parent-walking.
    pub extract_qualified_method: Option<fn(tree_sitter::Node, &str) -> Option<String>>,
    /// Optional post-processing of extracted chunks.
    /// Called after basic extraction. Can override name, chunk_type, etc.
    /// Return `false` to discard the chunk entirely.
    /// Takes `(&mut name, &mut chunk_type, definition_node, source)`.
    pub post_process_chunk: Option<PostProcessChunkFn>,
    /// Test content markers — language-specific annotations/decorators.
    /// Used by `find_test_chunks` for SQL `content LIKE '%marker%'` filtering.
    /// E.g., Rust: `&["#[test]", "#[cfg(test)]"]`, Java: `&["@Test"]`, Python: `&["def test_"]`.
    pub test_markers: &'static [&'static str],
    /// Test path patterns — file path suffixes/directories (SQL LIKE syntax).
    /// E.g., `&["%_test.rs", "%/tests/%"]`. Empty = use global defaults.
    pub test_path_patterns: &'static [&'static str],
    /// Language-specific structural pattern matchers.
    /// Keyed by pattern name (e.g., "error_swallow", "async", "mutex", "unsafe").
    /// When present, `Pattern::matches` uses these instead of generic heuristics.
    /// `None` = fall through to generic pattern matching in `structural.rs`.
    pub structural_matchers: Option<&'static [(&'static str, StructuralMatcherFn)]>,
    /// Entry point names excluded from dead code detection.
    /// Functions called by the runtime, framework, or build system rather than
    /// by other indexed code. E.g., Rust: `&["main"]`, Python: `&["__init__"]`,
    /// Go: `&["init"]`. Cross-language names like `"main"` and `"new"` are in
    /// the global fallback constant.
    pub entry_point_names: &'static [&'static str],
    /// Well-known trait/interface method names excluded from dead code detection.
    /// Methods with these names are almost always called via dynamic dispatch
    /// and won't appear in the static call graph. E.g., Rust: `&["fmt", "from",
    /// "clone", "default"]`, Java: `&["equals", "hashCode", "toString"]`.
    /// Cross-language names are in the global fallback constant.
    pub trait_method_names: &'static [&'static str],
    /// Injection rules for multi-grammar parsing.
    /// Empty by default. Only languages with embedded content (e.g., HTML with
    /// `<script>` and `<style>`) define injection rules.
    pub injections: &'static [InjectionRule],
    /// Doc comment format identifier for this language.
    /// Used by `doc_format_for()` in `src/doc_writer/formats.rs` to select the
    /// correct comment syntax. Valid values: "triple_slash", "python_docstring",
    /// "go_comment", "javadoc", "hash_comment", "elixir_doc", "lua_ldoc",
    /// "haskell_haddock", "ocaml_doc", "erlang_edoc", "r_roxygen", "default".
    pub doc_format: &'static str,
    /// Language-specific doc comment convention instructions for LLM prompt appendix.
    /// Used by `build_doc_prompt` in `src/llm/prompts.rs` to generate
    /// language-appropriate documentation. Empty string means no convention.
    pub doc_convention: &'static str,
    /// Field extraction style for struct/class/record body parsing.
    /// Used by `extract_field_names` in `src/nl/fields.rs`.
    pub field_style: FieldStyle,
    /// Line prefixes that indicate non-field declaration lines (headers, decorators).
    /// Used by `should_skip_line` in `src/nl/fields.rs` to skip struct/class/enum
    /// headers during field extraction. Universal prefixes (empty, `//`, `/*`, `*`,
    /// braces) are always skipped regardless of this list.
    pub skip_line_prefixes: &'static [&'static str],
}

/// Helper: PascalCase test name from a base function name with a given prefix.
/// Used by language-specific `test_name_suggestion` closures.
fn pascal_test_name(prefix: &str, base_name: &str) -> String {
    match base_name.chars().next() {
        Some(c) => {
            let first = c.to_uppercase().to_string();
            let rest = &base_name[c.len_utf8()..];
            format!("{prefix}{first}{rest}")
        }
        None => format!("{prefix}_{base_name}"),
    }
}

/// How to extract function signatures
#[derive(Debug, Clone, Copy, Default)]
pub enum SignatureStyle {
    /// Extract until opening brace `{` (Rust, Go, JS, TS)
    #[default]
    UntilBrace,
    /// Extract until colon `:` (Python)
    UntilColon,
    /// Extract until standalone `AS` keyword (SQL)
    UntilAs,
    /// Extract first line only (Ruby — no `{` or `:` delimiter)
    FirstLine,
    /// Signature is built by the parser as a breadcrumb path (Markdown)
    Breadcrumb,
}

// ---------------------------------------------------------------------------
// Macro: define_chunk_types!
//
// Generates from a single declaration table:
//   - `ChunkType` enum with variants, doc comments, and serde
//   - `ChunkType::ALL` const array
//   - `Display` impl (variant → name string)
//   - `FromStr` impl (name string → variant, case-insensitive)
//   - `ParseChunkTypeError` error type
//   - `capture_name_to_chunk_type()` — maps tree-sitter capture names to ChunkType
//
// Each variant has an optional `capture = "name"` field. When omitted, the
// display name is used as the capture name. When present, the capture name
// differs from the display name (e.g., `Constant => "constant", capture = "const"`).
//
// Adding a chunk type = one new line here. Display, FromStr, ALL, capture
// mapping, and error messages stay in sync automatically.
// ---------------------------------------------------------------------------
/// Defines a ChunkType enum and associated utilities for parsing and working with code element types.
///
/// # Arguments
///
/// - `$variant`: The name of each enum variant representing a chunk type.
/// - `$doc`: Optional doc comment strings for each variant.
/// - `$name`: The string literal name corresponding to each variant.
/// - `$capture`: Optional capture group identifier for each chunk type (unused in macro expansion).
///
/// # Returns
///
/// Generates:
/// - A `ChunkType` enum with all specified variants, deriving Debug, Clone, Copy, PartialEq, Eq, Hash, and Serialize.
/// - An `impl ChunkType` block providing:
///   - `all`: A constant array of all ChunkType variants.
///   - `valid_names()`: Returns a static slice of all valid chunk type name strings.
/// - A `Display` implementation that formats ChunkType variants as their string names.
/// - A `ParseChunkTypeError` struct for representing invalid chunk type parse attempts.
/// - A `Display` implementation for `ParseChunkTypeError` showing the invalid input and listing valid options.
macro_rules! define_chunk_types {
    (
        $(
            $(#[doc = $doc:expr])*
            $variant:ident => $name:literal $(, capture = $capture:literal)? ;
        )+
    ) => {
        /// Type of code element extracted by the parser
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
        #[serde(rename_all = "lowercase")]
        pub enum ChunkType {
            $(
                $(#[doc = $doc])*
                $variant,
            )+
        }

        impl ChunkType {
            /// All ChunkType variants.
            pub const ALL: &'static [ChunkType] = &[
                $(ChunkType::$variant,)+
            ];

            /// All valid chunk type name strings
            pub fn valid_names() -> &'static [&'static str] {
                &[$($name),+]
            }
        }

        impl std::fmt::Display for ChunkType {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $(ChunkType::$variant => write!(f, $name),)+
                }
            }
        }

        /// Error returned when parsing an invalid ChunkType string
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct ParseChunkTypeError {
            /// The invalid input string
            pub input: String,
        }

        impl std::fmt::Display for ParseChunkTypeError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let names: Vec<&str> = ChunkType::valid_names().to_vec();
                write!(
                    f,
                    "Unknown chunk type: '{}'. Valid options: {}",
                    self.input,
                    names.join(", ")
                )
            }
        }

        impl std::error::Error for ParseChunkTypeError {}

        impl std::str::FromStr for ChunkType {
            type Err = ParseChunkTypeError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s.to_lowercase().as_str() {
                    $($name => Ok(ChunkType::$variant),)+
                    _ => Err(ParseChunkTypeError {
                        input: s.to_string(),
                    }),
                }
            }
        }

        /// Map a tree-sitter capture name to a `ChunkType`.
        ///
        /// Single source of truth — used by chunk extraction, call graph, and injection.
        /// Returns `None` for unknown capture names (including non-chunk captures like `"name"`).
        ///
        /// Generated by `define_chunk_types!`. Each variant uses its display name as capture
        /// name unless overridden with `capture = "..."`.
        pub fn capture_name_to_chunk_type(name: &str) -> Option<ChunkType> {
            match name {
                $(
                    define_chunk_types!(@capture $name $(, $capture)?) => Some(ChunkType::$variant),
                )+
                _ => None,
            }
        }
    };

    // Internal rule: resolve capture name. If explicit capture given, use it; otherwise use display name.
    (@capture $name:literal, $capture:literal) => { $capture };
    (@capture $name:literal) => { $name };
}

define_chunk_types! {
    /// Standalone function
    Function => "function";
    /// Method (function inside a class/struct/impl)
    Method => "method";
    /// Class definition (Python, TypeScript, JavaScript)
    Class => "class";
    /// Struct definition (Rust, Go)
    Struct => "struct";
    /// Enum definition
    Enum => "enum";
    /// Trait definition (Rust)
    Trait => "trait";
    /// Interface definition (TypeScript, Go)
    Interface => "interface";
    /// Constant or static variable
    Constant => "constant", capture = "const";
    /// Documentation section (Markdown)
    Section => "section";
    /// Property (C# get/set properties)
    Property => "property";
    /// Delegate type declaration (C#)
    Delegate => "delegate";
    /// Event declaration (C#)
    Event => "event";
    /// Module definition (F#, future: Ruby, Elixir)
    Module => "module";
    /// Macro definition (Rust `macro_rules!`, future: Elixir `defmacro`)
    Macro => "macro";
    /// Object/singleton definition (Scala)
    Object => "object";
    /// Type alias definition (Scala, future: Haskell, Kotlin)
    TypeAlias => "typealias";
    /// Extension (Swift `extension Type { ... }`)
    Extension => "extension";
    /// Constructor (initializer method — `__init__`, `new`, `init`, etc.)
    Constructor => "constructor";
    /// Configuration key (JSON, TOML, YAML, INI — data, not code)
    ConfigKey => "configkey";
}

impl ChunkType {
    /// Human-readable display name for use in NL text generation.
    ///
    /// Most variants return their canonical `Display` string (always single words), but
    /// multi-word concepts need a spaced form. Currently `TypeAlias` → `"type alias"`.
    /// This is the single authoritative place for that mapping — callers (e.g., `nl.rs`)
    /// must use this method rather than hardcoding `"typealias"` string comparisons.
    pub fn human_name(self) -> String {
        match self {
            ChunkType::TypeAlias => "type alias".to_string(),
            other => other.to_string(),
        }
    }

    /// Returns true for types that have call graph connections (Function, Method, Constructor, Property, Macro, Extension).
    pub fn is_callable(self) -> bool {
        matches!(
            self,
            ChunkType::Function
                | ChunkType::Method
                | ChunkType::Constructor
                | ChunkType::Property
                | ChunkType::Macro
                | ChunkType::Extension
        )
    }

    /// Returns true if this is a code chunk type (callable + type definitions).
    /// Excludes Section (markdown), Module (file-level), Object (misc).
    /// Matches the CLI default search filter.
    pub fn is_code(self) -> bool {
        self.is_callable()
            || matches!(
                self,
                ChunkType::Struct
                    | ChunkType::Enum
                    | ChunkType::Interface
                    | ChunkType::Trait
                    | ChunkType::TypeAlias
                    | ChunkType::Class
                    | ChunkType::Constant
            )
    }

    /// Returns all code chunk types (for use in SearchFilter::chunk_types).
    pub fn code_types() -> Vec<ChunkType> {
        Self::ALL.iter().copied().filter(|t| t.is_code()).collect()
    }

    /// SQL IN clause string for all callable chunk types.
    /// Derived from `ALL` filtered by `is_callable()` — stays in sync automatically.
    pub fn callable_sql_list() -> String {
        Self::ALL
            .iter()
            .filter(|ct| ct.is_callable())
            .map(|ct| {
                let s = ct.to_string();
                // SEC-13: Guard against SQL injection if a future variant name contains quotes
                debug_assert!(!s.contains('\''), "ChunkType display contains quote: {s}");
                format!("'{}'", s)
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Error returned when parsing an invalid Language string
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLanguageError {
    /// The invalid input string
    pub input: String,
}

impl std::fmt::Display for ParseLanguageError {
    /// Formats the error message for an unknown language variant.
    ///
    /// This method implements the Display trait to produce a human-readable error message that shows the invalid language input and lists all valid language options.
    ///
    /// # Arguments
    ///
    /// * `f` - The formatter to write the error message to
    ///
    /// # Returns
    ///
    /// A `std::fmt::Result` indicating whether the formatting succeeded
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Unknown language: '{}'. Valid options: {}",
            self.input,
            Language::valid_names_display()
        )
    }
}

impl std::error::Error for ParseLanguageError {}

/// Registry of all supported languages
pub struct LanguageRegistry {
    /// Languages indexed by name
    by_name: HashMap<&'static str, &'static LanguageDef>,
    /// Languages indexed by extension
    by_extension: HashMap<&'static str, &'static LanguageDef>,
}

impl LanguageRegistry {
    /// Registers a language definition in the registry.
    ///
    /// This method stores the language definition by its name and associates all of its file extensions with it for later lookup.
    ///
    /// # Arguments
    ///
    /// * `def` - A static reference to a `LanguageDef` containing the language metadata and file extensions to register.
    fn register(&mut self, def: &'static LanguageDef) {
        self.by_name.insert(def.name, def);
        for ext in def.extensions {
            self.by_extension.insert(*ext, def);
        }
    }

    /// Get a language definition by name
    pub fn get(&self, name: &str) -> Option<&'static LanguageDef> {
        self.by_name.get(name).copied()
    }

    /// Get a language definition by file extension
    pub fn from_extension(&self, ext: &str) -> Option<&'static LanguageDef> {
        self.by_extension.get(ext).copied()
    }

    /// Iterate over all registered languages
    pub fn all(&self) -> impl Iterator<Item = &'static LanguageDef> + '_ {
        self.by_name.values().copied()
    }

    /// Get all supported extensions
    pub fn supported_extensions(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.by_extension.keys().copied()
    }

    /// Collect all unique test content markers from all enabled languages.
    pub fn all_test_markers(&self) -> Vec<&'static str> {
        let mut markers = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for def in self.all() {
            for marker in def.test_markers {
                if seen.insert(*marker) {
                    markers.push(*marker);
                }
            }
        }
        markers
    }

    /// Collect all unique entry point names from all enabled languages.
    pub fn all_entry_point_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for def in self.all() {
            for name in def.entry_point_names {
                if seen.insert(*name) {
                    names.push(*name);
                }
            }
        }
        names
    }

    /// Collect all unique trait method names from all enabled languages.
    pub fn all_trait_method_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for def in self.all() {
            for name in def.trait_method_names {
                if seen.insert(*name) {
                    names.push(*name);
                }
            }
        }
        names
    }

    /// Collect all unique test path patterns from all enabled languages.
    pub fn all_test_path_patterns(&self) -> Vec<&'static str> {
        let mut patterns = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for def in self.all() {
            for pat in def.test_path_patterns {
                if seen.insert(*pat) {
                    patterns.push(*pat);
                }
            }
        }
        patterns
    }
}

// ---------------------------------------------------------------------------
// Language registration — one line per language
// ---------------------------------------------------------------------------

define_languages! {
    /// Rust (.rs files)
    Rust => "rust", feature = "lang-rust", def = languages::definition_rust;
    /// Python (.py, .pyi files)
    Python => "python", feature = "lang-python", def = languages::definition_python;
    /// TypeScript (.ts, .tsx files)
    TypeScript => "typescript", feature = "lang-typescript", def = languages::definition_typescript;
    /// JavaScript (.js, .jsx, .mjs, .cjs files)
    JavaScript => "javascript", feature = "lang-javascript", def = languages::definition_javascript;
    /// Go (.go files)
    Go => "go", feature = "lang-go", def = languages::definition_go;
    /// C (.c, .h files)
    C => "c", feature = "lang-c", def = languages::definition_c;
    /// C++ (.cpp, .cxx, .cc, .hpp, .hxx, .hh, .ipp files)
    Cpp => "cpp", feature = "lang-cpp", def = languages::definition_cpp;
    /// Java (.java files)
    Java => "java", feature = "lang-java", def = languages::definition_java;
    /// C# (.cs files)
    CSharp => "csharp", feature = "lang-csharp", def = languages::definition_csharp;
    /// F# (.fs, .fsi files)
    FSharp => "fsharp", feature = "lang-fsharp", def = languages::definition_fsharp;
    /// PowerShell (.ps1, .psm1 files)
    PowerShell => "powershell", feature = "lang-powershell", def = languages::definition_powershell;
    /// Scala (.scala, .sc files)
    Scala => "scala", feature = "lang-scala", def = languages::definition_scala;
    /// Ruby (.rb, .rake, .gemspec files)
    Ruby => "ruby", feature = "lang-ruby", def = languages::definition_ruby;
    /// Bash (.sh, .bash files)
    Bash => "bash", feature = "lang-bash", def = languages::definition_bash;
    /// HCL/Terraform (.tf, .tfvars, .hcl files)
    Hcl => "hcl", feature = "lang-hcl", def = languages::definition_hcl;
    /// Kotlin (.kt, .kts files)
    Kotlin => "kotlin", feature = "lang-kotlin", def = languages::definition_kotlin;
    /// Swift (.swift files)
    Swift => "swift", feature = "lang-swift", def = languages::definition_swift;
    /// Objective-C (.m, .mm files)
    ObjC => "objc", feature = "lang-objc", def = languages::definition_objc;
    /// SQL (.sql files)
    Sql => "sql", feature = "lang-sql", def = languages::definition_sql;
    /// Protobuf (.proto files)
    Protobuf => "protobuf", feature = "lang-protobuf", def = languages::definition_protobuf;
    /// GraphQL (.graphql, .gql files)
    GraphQL => "graphql", feature = "lang-graphql", def = languages::definition_graphql;
    /// PHP (.php files)
    Php => "php", feature = "lang-php", def = languages::definition_php;
    /// Lua (.lua files)
    Lua => "lua", feature = "lang-lua", def = languages::definition_lua;
    /// Zig (.zig files)
    Zig => "zig", feature = "lang-zig", def = languages::definition_zig;
    /// R (.r, .R files)
    R => "r", feature = "lang-r", def = languages::definition_r;
    /// YAML (.yaml, .yml files)
    Yaml => "yaml", feature = "lang-yaml", def = languages::definition_yaml;
    /// TOML (.toml files)
    Toml => "toml", feature = "lang-toml", def = languages::definition_toml;
    /// Elixir (.ex, .exs files)
    Elixir => "elixir", feature = "lang-elixir", def = languages::definition_elixir;
    /// Erlang (.erl, .hrl files)
    Erlang => "erlang", feature = "lang-erlang", def = languages::definition_erlang;
    /// Haskell (.hs files)
    Haskell => "haskell", feature = "lang-haskell", def = languages::definition_haskell;
    /// OCaml (.ml, .mli files)
    OCaml => "ocaml", feature = "lang-ocaml", def = languages::definition_ocaml;
    /// Julia (.jl files)
    Julia => "julia", feature = "lang-julia", def = languages::definition_julia;
    /// Gleam (.gleam files)
    Gleam => "gleam", feature = "lang-gleam", def = languages::definition_gleam;
    /// CSS (.css files)
    Css => "css", feature = "lang-css", def = languages::definition_css;
    /// Perl (.pl, .pm files)
    Perl => "perl", feature = "lang-perl", def = languages::definition_perl;
    /// HTML (.html, .htm, .xhtml files)
    Html => "html", feature = "lang-html", def = languages::definition_html;
    /// JSON (.json, .jsonc files)
    Json => "json", feature = "lang-json", def = languages::definition_json;
    /// XML (.xml, .xsl, .xsd, .svg files)
    Xml => "xml", feature = "lang-xml", def = languages::definition_xml;
    /// INI (.ini, .cfg files)
    Ini => "ini", feature = "lang-ini", def = languages::definition_ini;
    /// Nix (.nix files)
    Nix => "nix", feature = "lang-nix", def = languages::definition_nix;
    /// Makefile (.mk, .mak files)
    Make => "make", feature = "lang-make", def = languages::definition_make;
    /// LaTeX (.tex, .sty, .cls files)
    Latex => "latex", feature = "lang-latex", def = languages::definition_latex;
    /// Solidity (.sol files)
    Solidity => "solidity", feature = "lang-solidity", def = languages::definition_solidity;
    /// CUDA (.cu, .cuh files)
    Cuda => "cuda", feature = "lang-cuda", def = languages::definition_cuda;
    /// GLSL (.glsl, .vert, .frag, .geom, .comp, .tesc, .tese files)
    Glsl => "glsl", feature = "lang-glsl", def = languages::definition_glsl;
    /// Svelte (.svelte files)
    Svelte => "svelte", feature = "lang-svelte", def = languages::definition_svelte;
    /// Razor/CSHTML (.cshtml, .razor files)
    Razor => "razor", feature = "lang-razor", def = languages::definition_razor;
    /// VB.NET (.vb files)
    VbNet => "vbnet", feature = "lang-vbnet", def = languages::definition_vbnet;
    /// Vue (.vue files)
    Vue => "vue", feature = "lang-vue", def = languages::definition_vue;
    /// Markdown (.md, .mdx files)
    Markdown => "markdown", feature = "lang-markdown", def = languages::definition_markdown;
    /// ASP.NET Web Forms (.aspx, .ascx, .asmx, .master files)
    Aspx => "aspx", feature = "lang-aspx", def = languages::definition_aspx;
    /// IEC 61131-3 Structured Text (.st, .stl files)
    StructuredText => "structured_text", feature = "lang-st", def = languages::definition_structured_text;
    /// Dart (.dart files)
    Dart => "dart", feature = "lang-dart", def = languages::definition_dart;
}

// ---------------------------------------------------------------------------
// Language methods (delegate to LanguageDef — no per-variant match arms)
// ---------------------------------------------------------------------------

impl Language {
    /// Get the language definition, or `None` if its feature flag is disabled.
    pub fn try_def(&self) -> Option<&'static LanguageDef> {
        REGISTRY.get(&self.to_string())
    }

    /// Get the language definition from the registry.
    ///
    /// # Panics
    /// Panics if the language's feature flag is disabled.
    pub fn def(&self) -> &'static LanguageDef {
        self.try_def()
            .unwrap_or_else(|| panic!("Language '{}' not in registry — check feature flags", self))
    }

    /// Look up a language by file extension
    pub fn from_extension(ext: &str) -> Option<Self> {
        REGISTRY
            .from_extension(ext)
            .and_then(|def| def.name.parse().ok())
    }

    /// Check if this language's feature flag is enabled
    pub fn is_enabled(&self) -> bool {
        REGISTRY.get(&self.to_string()).is_some()
    }

    /// Get the tree-sitter grammar, returning `None` if the language feature
    /// is disabled or the language has no tree-sitter grammar (RB-16).
    pub fn try_grammar(&self) -> Option<tree_sitter::Language> {
        self.try_def()
            .and_then(|def| def.grammar)
            .map(|grammar_fn| grammar_fn())
    }

    /// Get the chunk extraction query pattern
    pub fn query_pattern(&self) -> &'static str {
        self.def().chunk_query
    }

    /// Get the primary file extension for this language (e.g., "rs" for Rust)
    pub fn primary_extension(&self) -> &'static str {
        self.def().extensions[0]
    }

    /// Get the call extraction query pattern
    pub fn call_query_pattern(&self) -> &'static str {
        self.def().call_query.unwrap_or("")
    }

    /// Get the type extraction query pattern
    pub fn type_query_pattern(&self) -> &'static str {
        self.def().type_query.unwrap_or("")
    }
}

/// Global language registry
pub static REGISTRY: LazyLock<LanguageRegistry> = LazyLock::new(LanguageRegistry::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "lang-rust")]
    fn test_registry_by_name() {
        let rust = REGISTRY.get("rust");
        assert!(rust.is_some());
        assert_eq!(rust.unwrap().name, "rust");
        assert_eq!(rust.unwrap().extensions, &["rs"]);
    }

    #[test]
    fn test_registry_by_extension() {
        #[cfg(feature = "lang-rust")]
        assert!(REGISTRY.from_extension("rs").is_some());
        #[cfg(feature = "lang-python")]
        assert!(REGISTRY.from_extension("py").is_some());
        #[cfg(feature = "lang-typescript")]
        {
            assert!(REGISTRY.from_extension("ts").is_some());
            assert!(REGISTRY.from_extension("tsx").is_some());
        }
        #[cfg(feature = "lang-javascript")]
        assert!(REGISTRY.from_extension("js").is_some());
        #[cfg(feature = "lang-go")]
        assert!(REGISTRY.from_extension("go").is_some());
        #[cfg(feature = "lang-c")]
        {
            assert!(REGISTRY.from_extension("c").is_some());
            assert!(REGISTRY.from_extension("h").is_some());
        }
        #[cfg(feature = "lang-java")]
        assert!(REGISTRY.from_extension("java").is_some());
        #[cfg(feature = "lang-csharp")]
        assert!(REGISTRY.from_extension("cs").is_some());
        #[cfg(feature = "lang-scala")]
        {
            assert!(REGISTRY.from_extension("scala").is_some());
            assert!(REGISTRY.from_extension("sc").is_some());
        }
        #[cfg(feature = "lang-ruby")]
        {
            assert!(REGISTRY.from_extension("rb").is_some());
            assert!(REGISTRY.from_extension("rake").is_some());
            assert!(REGISTRY.from_extension("gemspec").is_some());
        }
        #[cfg(feature = "lang-cpp")]
        {
            assert!(REGISTRY.from_extension("cpp").is_some());
            assert!(REGISTRY.from_extension("hpp").is_some());
        }
        #[cfg(feature = "lang-bash")]
        {
            assert!(REGISTRY.from_extension("sh").is_some());
            assert!(REGISTRY.from_extension("bash").is_some());
        }
        #[cfg(feature = "lang-hcl")]
        {
            assert!(REGISTRY.from_extension("tf").is_some());
            assert!(REGISTRY.from_extension("tfvars").is_some());
            assert!(REGISTRY.from_extension("hcl").is_some());
        }
        #[cfg(feature = "lang-kotlin")]
        {
            assert!(REGISTRY.from_extension("kt").is_some());
            assert!(REGISTRY.from_extension("kts").is_some());
        }
        #[cfg(feature = "lang-swift")]
        assert!(REGISTRY.from_extension("swift").is_some());
        #[cfg(feature = "lang-objc")]
        {
            assert!(REGISTRY.from_extension("m").is_some());
            assert!(REGISTRY.from_extension("mm").is_some());
        }
        #[cfg(feature = "lang-sql")]
        assert!(REGISTRY.from_extension("sql").is_some());
        #[cfg(feature = "lang-protobuf")]
        assert!(REGISTRY.from_extension("proto").is_some());
        #[cfg(feature = "lang-graphql")]
        {
            assert!(REGISTRY.from_extension("graphql").is_some());
            assert!(REGISTRY.from_extension("gql").is_some());
        }
        #[cfg(feature = "lang-php")]
        assert!(REGISTRY.from_extension("php").is_some());
        #[cfg(feature = "lang-lua")]
        assert!(REGISTRY.from_extension("lua").is_some());
        #[cfg(feature = "lang-zig")]
        assert!(REGISTRY.from_extension("zig").is_some());
        #[cfg(feature = "lang-r")]
        {
            assert!(REGISTRY.from_extension("r").is_some());
            assert!(REGISTRY.from_extension("R").is_some());
        }
        #[cfg(feature = "lang-yaml")]
        {
            assert!(REGISTRY.from_extension("yaml").is_some());
            assert!(REGISTRY.from_extension("yml").is_some());
        }
        #[cfg(feature = "lang-toml")]
        assert!(REGISTRY.from_extension("toml").is_some());
        #[cfg(feature = "lang-elixir")]
        {
            assert!(REGISTRY.from_extension("ex").is_some());
            assert!(REGISTRY.from_extension("exs").is_some());
        }
        #[cfg(feature = "lang-erlang")]
        {
            assert!(REGISTRY.from_extension("erl").is_some());
            assert!(REGISTRY.from_extension("hrl").is_some());
        }
        #[cfg(feature = "lang-haskell")]
        assert!(REGISTRY.from_extension("hs").is_some());
        #[cfg(feature = "lang-ocaml")]
        {
            assert!(REGISTRY.from_extension("ml").is_some());
            assert!(REGISTRY.from_extension("mli").is_some());
        }
        #[cfg(feature = "lang-julia")]
        assert!(REGISTRY.from_extension("jl").is_some());
        #[cfg(feature = "lang-gleam")]
        assert!(REGISTRY.from_extension("gleam").is_some());
        #[cfg(feature = "lang-css")]
        assert!(REGISTRY.from_extension("css").is_some());
        #[cfg(feature = "lang-perl")]
        {
            assert!(REGISTRY.from_extension("pl").is_some());
            assert!(REGISTRY.from_extension("pm").is_some());
        }
        #[cfg(feature = "lang-html")]
        {
            assert!(REGISTRY.from_extension("html").is_some());
            assert!(REGISTRY.from_extension("htm").is_some());
            assert!(REGISTRY.from_extension("xhtml").is_some());
        }
        #[cfg(feature = "lang-json")]
        {
            assert!(REGISTRY.from_extension("json").is_some());
            assert!(REGISTRY.from_extension("jsonc").is_some());
        }
        #[cfg(feature = "lang-xml")]
        {
            assert!(REGISTRY.from_extension("xml").is_some());
            assert!(REGISTRY.from_extension("xsl").is_some());
            assert!(REGISTRY.from_extension("svg").is_some());
        }
        #[cfg(feature = "lang-ini")]
        {
            assert!(REGISTRY.from_extension("ini").is_some());
            assert!(REGISTRY.from_extension("cfg").is_some());
        }
        #[cfg(feature = "lang-nix")]
        assert!(REGISTRY.from_extension("nix").is_some());
        #[cfg(feature = "lang-make")]
        {
            assert!(REGISTRY.from_extension("mk").is_some());
            assert!(REGISTRY.from_extension("mak").is_some());
        }
        #[cfg(feature = "lang-latex")]
        {
            assert!(REGISTRY.from_extension("tex").is_some());
            assert!(REGISTRY.from_extension("sty").is_some());
            assert!(REGISTRY.from_extension("cls").is_some());
        }
        #[cfg(feature = "lang-solidity")]
        assert!(REGISTRY.from_extension("sol").is_some());
        #[cfg(feature = "lang-cuda")]
        {
            assert!(REGISTRY.from_extension("cu").is_some());
            assert!(REGISTRY.from_extension("cuh").is_some());
        }
        #[cfg(feature = "lang-glsl")]
        {
            assert!(REGISTRY.from_extension("glsl").is_some());
            assert!(REGISTRY.from_extension("vert").is_some());
            assert!(REGISTRY.from_extension("frag").is_some());
            assert!(REGISTRY.from_extension("comp").is_some());
        }
        #[cfg(feature = "lang-markdown")]
        {
            assert!(REGISTRY.from_extension("md").is_some());
            assert!(REGISTRY.from_extension("mdx").is_some());
        }
        assert!(REGISTRY.from_extension("xyz").is_none());
    }

    #[test]
    fn test_registry_all_languages() {
        let all: Vec<_> = REGISTRY.all().collect();
        // Count depends on which features are enabled
        let mut expected = 0;
        #[cfg(feature = "lang-rust")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-python")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-typescript")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-javascript")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-go")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-c")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-java")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-csharp")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-fsharp")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-powershell")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-scala")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-ruby")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-cpp")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-bash")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-hcl")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-kotlin")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-swift")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-objc")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-sql")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-protobuf")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-graphql")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-php")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-lua")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-zig")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-r")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-yaml")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-toml")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-elixir")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-erlang")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-haskell")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-ocaml")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-julia")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-gleam")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-css")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-perl")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-html")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-json")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-xml")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-ini")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-nix")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-make")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-latex")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-solidity")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-cuda")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-glsl")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-markdown")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-svelte")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-razor")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-vbnet")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-vue")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-aspx")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-st")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-dart")]
        {
            expected += 1;
        }
        assert_eq!(all.len(), expected);
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn test_language_grammar() {
        // Verify we can get grammars for tree-sitter languages
        let rust = REGISTRY.get("rust").unwrap();
        let grammar = (rust.grammar.unwrap())();
        // Just verify grammar is valid by checking ABI version
        assert!(grammar.abi_version() > 0);
    }

    #[test]
    #[cfg(feature = "lang-markdown")]
    fn test_markdown_no_grammar() {
        let md = REGISTRY.get("markdown").unwrap();
        assert!(md.grammar.is_none());
    }

    // ===== Language tests =====

    #[test]
    fn test_from_extension() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("pyi"), Some(Language::Python));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("jsx"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("mjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("cjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
        assert_eq!(Language::from_extension("c"), Some(Language::C));
        assert_eq!(Language::from_extension("h"), Some(Language::C));
        assert_eq!(Language::from_extension("java"), Some(Language::Java));
        assert_eq!(Language::from_extension("cs"), Some(Language::CSharp));
        assert_eq!(Language::from_extension("fs"), Some(Language::FSharp));
        assert_eq!(Language::from_extension("fsi"), Some(Language::FSharp));
        assert_eq!(Language::from_extension("ps1"), Some(Language::PowerShell));
        assert_eq!(Language::from_extension("psm1"), Some(Language::PowerShell));
        assert_eq!(Language::from_extension("scala"), Some(Language::Scala));
        assert_eq!(Language::from_extension("sc"), Some(Language::Scala));
        assert_eq!(Language::from_extension("rb"), Some(Language::Ruby));
        assert_eq!(Language::from_extension("rake"), Some(Language::Ruby));
        assert_eq!(Language::from_extension("gemspec"), Some(Language::Ruby));
        assert_eq!(Language::from_extension("cpp"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("cxx"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("cc"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("hpp"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("hxx"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("hh"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("ipp"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("sh"), Some(Language::Bash));
        assert_eq!(Language::from_extension("bash"), Some(Language::Bash));
        assert_eq!(Language::from_extension("tf"), Some(Language::Hcl));
        assert_eq!(Language::from_extension("tfvars"), Some(Language::Hcl));
        assert_eq!(Language::from_extension("hcl"), Some(Language::Hcl));
        assert_eq!(Language::from_extension("kt"), Some(Language::Kotlin));
        assert_eq!(Language::from_extension("kts"), Some(Language::Kotlin));
        assert_eq!(Language::from_extension("swift"), Some(Language::Swift));
        assert_eq!(Language::from_extension("m"), Some(Language::ObjC));
        assert_eq!(Language::from_extension("mm"), Some(Language::ObjC));
        assert_eq!(Language::from_extension("sql"), Some(Language::Sql));
        assert_eq!(Language::from_extension("proto"), Some(Language::Protobuf));
        assert_eq!(Language::from_extension("graphql"), Some(Language::GraphQL));
        assert_eq!(Language::from_extension("gql"), Some(Language::GraphQL));
        assert_eq!(Language::from_extension("php"), Some(Language::Php));
        assert_eq!(Language::from_extension("lua"), Some(Language::Lua));
        assert_eq!(Language::from_extension("zig"), Some(Language::Zig));
        assert_eq!(Language::from_extension("r"), Some(Language::R));
        assert_eq!(Language::from_extension("R"), Some(Language::R));
        assert_eq!(Language::from_extension("yaml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("yml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("toml"), Some(Language::Toml));
        assert_eq!(Language::from_extension("ex"), Some(Language::Elixir));
        assert_eq!(Language::from_extension("exs"), Some(Language::Elixir));
        assert_eq!(Language::from_extension("erl"), Some(Language::Erlang));
        assert_eq!(Language::from_extension("hrl"), Some(Language::Erlang));
        assert_eq!(Language::from_extension("hs"), Some(Language::Haskell));
        assert_eq!(Language::from_extension("ml"), Some(Language::OCaml));
        assert_eq!(Language::from_extension("mli"), Some(Language::OCaml));
        assert_eq!(Language::from_extension("jl"), Some(Language::Julia));
        assert_eq!(Language::from_extension("gleam"), Some(Language::Gleam));
        assert_eq!(Language::from_extension("css"), Some(Language::Css));
        assert_eq!(Language::from_extension("pl"), Some(Language::Perl));
        assert_eq!(Language::from_extension("pm"), Some(Language::Perl));
        assert_eq!(Language::from_extension("html"), Some(Language::Html));
        assert_eq!(Language::from_extension("htm"), Some(Language::Html));
        assert_eq!(Language::from_extension("xhtml"), Some(Language::Html));
        assert_eq!(Language::from_extension("json"), Some(Language::Json));
        assert_eq!(Language::from_extension("jsonc"), Some(Language::Json));
        assert_eq!(Language::from_extension("xml"), Some(Language::Xml));
        assert_eq!(Language::from_extension("xsl"), Some(Language::Xml));
        assert_eq!(Language::from_extension("xsd"), Some(Language::Xml));
        assert_eq!(Language::from_extension("svg"), Some(Language::Xml));
        assert_eq!(Language::from_extension("ini"), Some(Language::Ini));
        assert_eq!(Language::from_extension("cfg"), Some(Language::Ini));
        assert_eq!(Language::from_extension("nix"), Some(Language::Nix));
        assert_eq!(Language::from_extension("mk"), Some(Language::Make));
        assert_eq!(Language::from_extension("mak"), Some(Language::Make));
        assert_eq!(Language::from_extension("tex"), Some(Language::Latex));
        assert_eq!(Language::from_extension("sty"), Some(Language::Latex));
        assert_eq!(Language::from_extension("cls"), Some(Language::Latex));
        assert_eq!(Language::from_extension("sol"), Some(Language::Solidity));
        assert_eq!(Language::from_extension("cu"), Some(Language::Cuda));
        assert_eq!(Language::from_extension("cuh"), Some(Language::Cuda));
        assert_eq!(Language::from_extension("glsl"), Some(Language::Glsl));
        assert_eq!(Language::from_extension("vert"), Some(Language::Glsl));
        assert_eq!(Language::from_extension("frag"), Some(Language::Glsl));
        assert_eq!(Language::from_extension("geom"), Some(Language::Glsl));
        assert_eq!(Language::from_extension("comp"), Some(Language::Glsl));
        assert_eq!(Language::from_extension("tesc"), Some(Language::Glsl));
        assert_eq!(Language::from_extension("tese"), Some(Language::Glsl));
        assert_eq!(Language::from_extension("md"), Some(Language::Markdown));
        assert_eq!(Language::from_extension("mdx"), Some(Language::Markdown));
        assert_eq!(Language::from_extension("unknown"), None);
    }

    #[test]
    fn test_language_from_str() {
        assert_eq!("rust".parse::<Language>().unwrap(), Language::Rust);
        assert_eq!("PYTHON".parse::<Language>().unwrap(), Language::Python);
        assert_eq!(
            "TypeScript".parse::<Language>().unwrap(),
            Language::TypeScript
        );
        assert_eq!("c".parse::<Language>().unwrap(), Language::C);
        assert_eq!("java".parse::<Language>().unwrap(), Language::Java);
        assert_eq!("csharp".parse::<Language>().unwrap(), Language::CSharp);
        assert_eq!("fsharp".parse::<Language>().unwrap(), Language::FSharp);
        assert_eq!(
            "powershell".parse::<Language>().unwrap(),
            Language::PowerShell
        );
        assert_eq!("scala".parse::<Language>().unwrap(), Language::Scala);
        assert_eq!("ruby".parse::<Language>().unwrap(), Language::Ruby);
        assert_eq!("cpp".parse::<Language>().unwrap(), Language::Cpp);
        assert_eq!("bash".parse::<Language>().unwrap(), Language::Bash);
        assert_eq!("hcl".parse::<Language>().unwrap(), Language::Hcl);
        assert_eq!("kotlin".parse::<Language>().unwrap(), Language::Kotlin);
        assert_eq!("swift".parse::<Language>().unwrap(), Language::Swift);
        assert_eq!("objc".parse::<Language>().unwrap(), Language::ObjC);
        assert_eq!("sql".parse::<Language>().unwrap(), Language::Sql);
        assert_eq!("protobuf".parse::<Language>().unwrap(), Language::Protobuf);
        assert_eq!("graphql".parse::<Language>().unwrap(), Language::GraphQL);
        assert_eq!("php".parse::<Language>().unwrap(), Language::Php);
        assert_eq!("lua".parse::<Language>().unwrap(), Language::Lua);
        assert_eq!("zig".parse::<Language>().unwrap(), Language::Zig);
        assert_eq!("r".parse::<Language>().unwrap(), Language::R);
        assert_eq!("yaml".parse::<Language>().unwrap(), Language::Yaml);
        assert_eq!("toml".parse::<Language>().unwrap(), Language::Toml);
        assert_eq!("elixir".parse::<Language>().unwrap(), Language::Elixir);
        assert_eq!("erlang".parse::<Language>().unwrap(), Language::Erlang);
        assert_eq!("haskell".parse::<Language>().unwrap(), Language::Haskell);
        assert_eq!("ocaml".parse::<Language>().unwrap(), Language::OCaml);
        assert_eq!("julia".parse::<Language>().unwrap(), Language::Julia);
        assert_eq!("gleam".parse::<Language>().unwrap(), Language::Gleam);
        assert_eq!("css".parse::<Language>().unwrap(), Language::Css);
        assert_eq!("perl".parse::<Language>().unwrap(), Language::Perl);
        assert_eq!("html".parse::<Language>().unwrap(), Language::Html);
        assert_eq!("json".parse::<Language>().unwrap(), Language::Json);
        assert_eq!("xml".parse::<Language>().unwrap(), Language::Xml);
        assert_eq!("ini".parse::<Language>().unwrap(), Language::Ini);
        assert_eq!("nix".parse::<Language>().unwrap(), Language::Nix);
        assert_eq!("make".parse::<Language>().unwrap(), Language::Make);
        assert_eq!("latex".parse::<Language>().unwrap(), Language::Latex);
        assert_eq!("solidity".parse::<Language>().unwrap(), Language::Solidity);
        assert_eq!("cuda".parse::<Language>().unwrap(), Language::Cuda);
        assert_eq!("glsl".parse::<Language>().unwrap(), Language::Glsl);
        assert_eq!("markdown".parse::<Language>().unwrap(), Language::Markdown);
        assert!("invalid".parse::<Language>().is_err());
    }

    #[test]
    fn test_language_display() {
        assert_eq!(Language::Rust.to_string(), "rust");
        assert_eq!(Language::Python.to_string(), "python");
        assert_eq!(Language::TypeScript.to_string(), "typescript");
        assert_eq!(Language::JavaScript.to_string(), "javascript");
        assert_eq!(Language::Go.to_string(), "go");
        assert_eq!(Language::C.to_string(), "c");
        assert_eq!(Language::Java.to_string(), "java");
        assert_eq!(Language::CSharp.to_string(), "csharp");
        assert_eq!(Language::FSharp.to_string(), "fsharp");
        assert_eq!(Language::PowerShell.to_string(), "powershell");
        assert_eq!(Language::Scala.to_string(), "scala");
        assert_eq!(Language::Ruby.to_string(), "ruby");
        assert_eq!(Language::Cpp.to_string(), "cpp");
        assert_eq!(Language::Bash.to_string(), "bash");
        assert_eq!(Language::Hcl.to_string(), "hcl");
        assert_eq!(Language::Kotlin.to_string(), "kotlin");
        assert_eq!(Language::Swift.to_string(), "swift");
        assert_eq!(Language::ObjC.to_string(), "objc");
        assert_eq!(Language::Sql.to_string(), "sql");
        assert_eq!(Language::Protobuf.to_string(), "protobuf");
        assert_eq!(Language::GraphQL.to_string(), "graphql");
        assert_eq!(Language::Php.to_string(), "php");
        assert_eq!(Language::Lua.to_string(), "lua");
        assert_eq!(Language::Zig.to_string(), "zig");
        assert_eq!(Language::R.to_string(), "r");
        assert_eq!(Language::Yaml.to_string(), "yaml");
        assert_eq!(Language::Toml.to_string(), "toml");
        assert_eq!(Language::Elixir.to_string(), "elixir");
        assert_eq!(Language::Erlang.to_string(), "erlang");
        assert_eq!(Language::Haskell.to_string(), "haskell");
        assert_eq!(Language::OCaml.to_string(), "ocaml");
        assert_eq!(Language::Julia.to_string(), "julia");
        assert_eq!(Language::Gleam.to_string(), "gleam");
        assert_eq!(Language::Css.to_string(), "css");
        assert_eq!(Language::Perl.to_string(), "perl");
        assert_eq!(Language::Html.to_string(), "html");
        assert_eq!(Language::Json.to_string(), "json");
        assert_eq!(Language::Xml.to_string(), "xml");
        assert_eq!(Language::Ini.to_string(), "ini");
        assert_eq!(Language::Nix.to_string(), "nix");
        assert_eq!(Language::Make.to_string(), "make");
        assert_eq!(Language::Latex.to_string(), "latex");
        assert_eq!(Language::Solidity.to_string(), "solidity");
        assert_eq!(Language::Cuda.to_string(), "cuda");
        assert_eq!(Language::Glsl.to_string(), "glsl");
        assert_eq!(Language::Markdown.to_string(), "markdown");
    }

    #[test]
    fn test_language_def_bridge() {
        // Verify def() returns the correct LanguageDef for each language
        assert_eq!(Language::Rust.def().name, "rust");
        assert_eq!(Language::Python.def().name, "python");
        assert_eq!(Language::Go.def().name, "go");
    }

    // ===== Macro / extensibility tests =====

    #[test]
    fn test_all_variants_count() {
        // Macro-generated all_variants() should agree with registry count (all features enabled)
        let variant_count = Language::all_variants().len();
        let registry_count = REGISTRY.all().count();
        assert_eq!(
            variant_count, registry_count,
            "all_variants() has {} but registry has {} (feature mismatch?)",
            variant_count, registry_count
        );
    }

    #[test]
    fn test_valid_names_roundtrip() {
        // Every entry in valid_names() should parse via FromStr and round-trip through Display
        for name in Language::valid_names() {
            let lang: Language = name.parse().unwrap_or_else(|_| {
                panic!("valid_names() entry '{}' should parse as Language", name)
            });
            assert_eq!(
                &lang.to_string(),
                name,
                "Display for '{}' should round-trip",
                name
            );
        }
    }

    #[test]
    fn test_valid_names_display_format() {
        let display = Language::valid_names_display();
        // Should contain commas (at least 2 languages)
        assert!(
            display.contains(", "),
            "valid_names_display() should contain commas: {}",
            display
        );
        // Every language name should appear
        for name in Language::valid_names() {
            assert!(
                display.contains(name),
                "valid_names_display() missing '{}': {}",
                name,
                display
            );
        }
    }

    #[test]
    fn test_language_def_stopwords_nonempty() {
        // Every language must provide at least one stopword
        for lang in Language::all_variants() {
            let def = lang.def();
            assert!(
                !def.stopwords.is_empty(),
                "Language {} has empty stopwords",
                lang
            );
        }
    }

    #[test]
    fn test_language_def_extract_return() {
        // Empty input should never produce a return type for any language
        for lang in Language::all_variants() {
            let result = (lang.def().extract_return_nl)("");
            assert_eq!(
                result, None,
                "extract_return_nl(\"\") should be None for {}",
                lang
            );
        }

        // Known signatures per language — verify extraction works through function pointers
        assert_eq!(
            (Language::Rust.def().extract_return_nl)("fn foo() -> String"),
            Some("Returns string".to_string())
        );
        assert_eq!(
            (Language::Python.def().extract_return_nl)("def foo() -> str:"),
            Some("Returns str".to_string())
        );
        assert_eq!(
            (Language::TypeScript.def().extract_return_nl)("function foo(): string"),
            Some("Returns string".to_string())
        );
        assert_eq!(
            (Language::JavaScript.def().extract_return_nl)("function foo()"),
            None
        );
        assert_eq!(
            (Language::Go.def().extract_return_nl)("func foo() string {"),
            Some("Returns string".to_string())
        );
        assert_eq!(
            (Language::C.def().extract_return_nl)("int add(int a, int b)"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            (Language::Java.def().extract_return_nl)("public String getName()"),
            Some("Returns string".to_string())
        );
        assert_eq!(
            (Language::CSharp.def().extract_return_nl)("public int Add(int a, int b)"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            (Language::Sql.def().extract_return_nl)(
                "CREATE FUNCTION dbo.fn_Calc(@id INT) RETURNS DECIMAL(10,2)"
            ),
            Some("Returns decimal".to_string())
        );
        assert_eq!(
            (Language::Sql.def().extract_return_nl)("CREATE PROCEDURE dbo.usp_Foo"),
            None
        );
        assert_eq!(
            (Language::Markdown.def().extract_return_nl)("any markdown content"),
            None
        );
        assert_eq!(
            (Language::Scala.def().extract_return_nl)("def foo(x: Int): String ="),
            Some("Returns string".to_string())
        );
        assert_eq!(
            (Language::Ruby.def().extract_return_nl)("def calculate(x, y)"),
            None
        );
        assert_eq!(
            (Language::Cpp.def().extract_return_nl)("int add(int a, int b)"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            (Language::Cpp.def().extract_return_nl)("auto foo() -> int"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            (Language::Bash.def().extract_return_nl)("function foo()"),
            None
        );
        assert_eq!(
            (Language::Hcl.def().extract_return_nl)("resource \"aws_instance\" \"web\""),
            None
        );
        assert_eq!(
            (Language::Kotlin.def().extract_return_nl)("fun add(a: Int, b: Int): Int {"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            (Language::Kotlin.def().extract_return_nl)("fun doSomething(): Unit {"),
            None
        );
        assert_eq!(
            (Language::Swift.def().extract_return_nl)("func greet(name: String) -> String {"),
            Some("Returns string".to_string())
        );
        assert_eq!(
            (Language::Swift.def().extract_return_nl)("func doSomething() {"),
            None
        );
        assert_eq!(
            (Language::ObjC.def().extract_return_nl)("- (void)greet"),
            None
        );
        assert_eq!(
            (Language::Protobuf.def().extract_return_nl)("message User {"),
            None
        );
        assert_eq!(
            (Language::GraphQL.def().extract_return_nl)("type User {"),
            None
        );
        assert_eq!(
            (Language::Php.def().extract_return_nl)("function add(int $a, int $b): int {"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            (Language::Php.def().extract_return_nl)("function doSomething(): void {"),
            None
        );
        assert_eq!(
            (Language::Lua.def().extract_return_nl)("function foo(x)"),
            None
        );
        assert_eq!(
            (Language::Zig.def().extract_return_nl)("pub fn add(a: i32, b: i32) i32 {"),
            Some("Returns i32".to_string())
        );
        assert_eq!(
            (Language::Zig.def().extract_return_nl)("pub fn main() void {"),
            None
        );
        assert_eq!(
            (Language::R.def().extract_return_nl)("greet <- function(name) {"),
            None
        );
        assert_eq!((Language::Yaml.def().extract_return_nl)("key: value"), None);
        assert_eq!((Language::Toml.def().extract_return_nl)("[section]"), None);
        assert_eq!(
            (Language::Elixir.def().extract_return_nl)("def greet(name) do"),
            None
        );
        assert_eq!(
            (Language::Erlang.def().extract_return_nl)("greet(Name) ->"),
            None
        );
        assert_eq!(
            (Language::Haskell.def().extract_return_nl)("greet :: String -> String"),
            Some("Returns string".to_string())
        );
        assert_eq!(
            (Language::Haskell.def().extract_return_nl)("main :: IO ()"),
            None
        );
        assert_eq!(
            (Language::OCaml.def().extract_return_nl)("val add : int -> int -> int"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            (Language::OCaml.def().extract_return_nl)("let add x y = x + y"),
            None
        );
        assert_eq!(
            (Language::Julia.def().extract_return_nl)("function add(x::Int, y::Int)::Int"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            (Language::Julia.def().extract_return_nl)("function greet(name)"),
            None
        );
        assert_eq!(
            (Language::Gleam.def().extract_return_nl)("pub fn add(x: Int, y: Int) -> Int {"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            (Language::Gleam.def().extract_return_nl)("pub fn main() -> Nil {"),
            None
        );
        // CSS — no return types
        assert_eq!(
            (Language::Css.def().extract_return_nl)(".class { color: red; }"),
            None
        );
        // Perl — no static return types
        assert_eq!((Language::Perl.def().extract_return_nl)("sub add {"), None);
        // HTML — no return types
        assert_eq!(
            (Language::Html.def().extract_return_nl)("<div>content</div>"),
            None
        );
        // JSON — no return types
        assert_eq!(
            (Language::Json.def().extract_return_nl)("\"key\": \"value\""),
            None
        );
        // XML — no return types
        assert_eq!((Language::Xml.def().extract_return_nl)("<element/>"), None);
        // INI — no return types
        assert_eq!((Language::Ini.def().extract_return_nl)("key = value"), None);
        // Nix — no type annotations
        assert_eq!((Language::Nix.def().extract_return_nl)("x: x * 2"), None);
        // Make — no return types
        assert_eq!(
            (Language::Make.def().extract_return_nl)("all: build test"),
            None
        );
        // LaTeX — no return types
        assert_eq!(
            (Language::Latex.def().extract_return_nl)("\\section{Intro}"),
            None
        );
        // Solidity — returns keyword
        assert_eq!(
            (Language::Solidity.def().extract_return_nl)(
                "function add(uint a, uint b) public pure returns (uint)"
            ),
            Some("Returns uint".to_string())
        );
        assert_eq!(
            (Language::Solidity.def().extract_return_nl)("function doSomething() public"),
            None
        );
        // CUDA — C++ style
        assert_eq!(
            (Language::Cuda.def().extract_return_nl)("__device__ float compute(float x)"),
            Some("Returns float".to_string())
        );
        assert_eq!(
            (Language::Cuda.def().extract_return_nl)("__global__ void kernel(int n)"),
            None
        );
        // GLSL — C style
        assert_eq!(
            (Language::Glsl.def().extract_return_nl)("vec4 applyLighting(vec3 normal)"),
            Some("Returns vec4".to_string())
        );
        assert_eq!(
            (Language::Glsl.def().extract_return_nl)("void main()"),
            None
        );
    }

    // ===== ChunkType tests =====

    #[test]
    fn test_chunk_type_from_str_valid() {
        assert_eq!(
            "function".parse::<ChunkType>().unwrap(),
            ChunkType::Function
        );
        assert_eq!("method".parse::<ChunkType>().unwrap(), ChunkType::Method);
        assert_eq!("class".parse::<ChunkType>().unwrap(), ChunkType::Class);
        assert_eq!("struct".parse::<ChunkType>().unwrap(), ChunkType::Struct);
        assert_eq!("enum".parse::<ChunkType>().unwrap(), ChunkType::Enum);
        assert_eq!("trait".parse::<ChunkType>().unwrap(), ChunkType::Trait);
        assert_eq!(
            "interface".parse::<ChunkType>().unwrap(),
            ChunkType::Interface
        );
        assert_eq!(
            "constant".parse::<ChunkType>().unwrap(),
            ChunkType::Constant
        );
        assert_eq!(
            "property".parse::<ChunkType>().unwrap(),
            ChunkType::Property
        );
        assert_eq!(
            "delegate".parse::<ChunkType>().unwrap(),
            ChunkType::Delegate
        );
        assert_eq!("event".parse::<ChunkType>().unwrap(), ChunkType::Event);
        assert_eq!("module".parse::<ChunkType>().unwrap(), ChunkType::Module);
        assert_eq!("macro".parse::<ChunkType>().unwrap(), ChunkType::Macro);
        assert_eq!("object".parse::<ChunkType>().unwrap(), ChunkType::Object);
        assert_eq!(
            "typealias".parse::<ChunkType>().unwrap(),
            ChunkType::TypeAlias
        );
    }

    #[test]
    fn test_chunk_type_from_str_case_insensitive() {
        assert_eq!(
            "FUNCTION".parse::<ChunkType>().unwrap(),
            ChunkType::Function
        );
        assert_eq!("Method".parse::<ChunkType>().unwrap(), ChunkType::Method);
        assert_eq!("CLASS".parse::<ChunkType>().unwrap(), ChunkType::Class);
    }

    #[test]
    fn test_chunk_type_from_str_invalid() {
        let result = "invalid".parse::<ChunkType>();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown chunk type"));
    }

    #[test]
    fn test_chunk_type_display_roundtrip() {
        // Verify Display and FromStr are inverses for ALL variants (macro-generated)
        for ct in ChunkType::ALL {
            let s = ct.to_string();
            let parsed: ChunkType = s.parse().unwrap();
            assert_eq!(*ct, parsed);
        }
    }

    #[test]
    fn test_chunk_type_valid_names_roundtrip() {
        // Every entry in valid_names() should parse and round-trip through Display
        for name in ChunkType::valid_names() {
            let ct: ChunkType = name.parse().unwrap_or_else(|_| {
                panic!("valid_names() entry '{}' should parse as ChunkType", name)
            });
            assert_eq!(
                &ct.to_string(),
                name,
                "Display for '{}' should round-trip",
                name
            );
        }
    }

    #[test]
    fn test_chunk_type_all_count_matches_valid_names() {
        assert_eq!(
            ChunkType::ALL.len(),
            ChunkType::valid_names().len(),
            "ALL and valid_names() should have the same count"
        );
    }

    #[test]
    fn test_callable_sql_list() {
        let list = ChunkType::callable_sql_list();
        assert!(list.contains("'function'"));
        assert!(list.contains("'method'"));
        assert!(list.contains("'property'"));
        assert!(!list.contains("'class'"));
        assert!(!list.contains("'delegate'"));
        assert!(!list.contains("'event'"));
        assert!(!list.contains("'module'"));
        assert!(list.contains("'macro'"));
        assert!(!list.contains("'object'"));
        assert!(!list.contains("'typealias'"));
    }

    // ===== Test markers / path patterns tests =====

    #[test]
    fn test_all_test_markers_nonempty() {
        let markers = REGISTRY.all_test_markers();
        // At least Rust (#[test]) and Java (@Test) should contribute markers
        assert!(
            markers.len() >= 2,
            "Expected at least 2 test markers, got {}",
            markers.len()
        );
        assert!(
            markers.contains(&"#[test]"),
            "Rust #[test] should be in all_test_markers"
        );
        assert!(
            markers.contains(&"@Test"),
            "Java @Test should be in all_test_markers"
        );
    }

    #[test]
    fn test_all_test_path_patterns_nonempty() {
        let patterns = REGISTRY.all_test_path_patterns();
        assert!(
            !patterns.is_empty(),
            "Expected at least 1 test path pattern"
        );
        assert!(
            patterns.contains(&"%/tests/%"),
            "%/tests/% should be in all_test_path_patterns"
        );
    }

    #[test]
    fn test_all_test_markers_no_duplicates() {
        let markers = REGISTRY.all_test_markers();
        let set: std::collections::HashSet<&str> = markers.iter().copied().collect();
        assert_eq!(
            markers.len(),
            set.len(),
            "all_test_markers() should have no duplicates"
        );
    }

    #[test]
    fn test_rust_test_markers() {
        let def = Language::Rust.def();
        assert!(def.test_markers.contains(&"#[test]"));
        assert!(def.test_markers.contains(&"#[cfg(test)]"));
    }

    #[test]
    fn test_structural_matchers_default_none() {
        // Most languages should default to None for structural_matchers
        for lang in Language::all_variants() {
            // Just verify the field is accessible without panicking
            let _matchers = lang.def().structural_matchers;
        }
    }
}
