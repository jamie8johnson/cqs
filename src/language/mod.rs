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
//! - `lang-java` - Java support (enabled by default)
//! - `lang-all` - All languages

use std::collections::HashMap;
use std::sync::LazyLock;

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
macro_rules! define_languages {
    (
        $(
            $(#[doc = $doc:expr])*
            $variant:ident => $name:literal, feature = $feature:literal, module = $module:ident;
        )+
    ) => {
        // Feature-gated module imports
        $(
            #[cfg(feature = $feature)]
            mod $module;
        )+

        /// Supported programming languages
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
                    reg.register($module::definition());
                )+
                reg
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Type definitions (prerequisites for language modules and macro expansion)
// ---------------------------------------------------------------------------

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
    /// Signature is built by the parser as a breadcrumb path (Markdown)
    Breadcrumb,
}

/// Type of code element extracted by the parser
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkType {
    /// Standalone function
    Function,
    /// Method (function inside a class/struct/impl)
    Method,
    /// Class definition (Python, TypeScript, JavaScript)
    Class,
    /// Struct definition (Rust, Go)
    Struct,
    /// Enum definition
    Enum,
    /// Trait definition (Rust)
    Trait,
    /// Interface definition (TypeScript, Go)
    Interface,
    /// Constant or static variable
    Constant,
    /// Documentation section (Markdown)
    Section,
    /// Property (C# get/set properties)
    Property,
    /// Delegate type declaration (C#)
    Delegate,
    /// Event declaration (C#)
    Event,
}

impl ChunkType {
    /// Returns true for types that have call graph connections (Function, Method, Property).
    pub fn is_callable(self) -> bool {
        matches!(
            self,
            ChunkType::Function | ChunkType::Method | ChunkType::Property
        )
    }

    /// SQL IN clause string for all callable chunk types.
    /// Derived from `is_callable()` — keep in sync when adding new callable variants.
    pub fn callable_sql_list() -> String {
        let callable = [ChunkType::Function, ChunkType::Method, ChunkType::Property];
        callable
            .iter()
            .map(|ct| format!("'{}'", ct))
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl std::fmt::Display for ChunkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChunkType::Function => write!(f, "function"),
            ChunkType::Method => write!(f, "method"),
            ChunkType::Class => write!(f, "class"),
            ChunkType::Struct => write!(f, "struct"),
            ChunkType::Enum => write!(f, "enum"),
            ChunkType::Trait => write!(f, "trait"),
            ChunkType::Interface => write!(f, "interface"),
            ChunkType::Constant => write!(f, "constant"),
            ChunkType::Section => write!(f, "section"),
            ChunkType::Property => write!(f, "property"),
            ChunkType::Delegate => write!(f, "delegate"),
            ChunkType::Event => write!(f, "event"),
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
        write!(
            f,
            "Unknown chunk type: '{}'. Valid options: function, method, class, struct, enum, trait, interface, constant, section, property, delegate, event",
            self.input
        )
    }
}

impl std::error::Error for ParseChunkTypeError {}

impl std::str::FromStr for ChunkType {
    type Err = ParseChunkTypeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "function" => Ok(ChunkType::Function),
            "method" => Ok(ChunkType::Method),
            "class" => Ok(ChunkType::Class),
            "struct" => Ok(ChunkType::Struct),
            "enum" => Ok(ChunkType::Enum),
            "trait" => Ok(ChunkType::Trait),
            "interface" => Ok(ChunkType::Interface),
            "constant" => Ok(ChunkType::Constant),
            "section" => Ok(ChunkType::Section),
            "property" => Ok(ChunkType::Property),
            "delegate" => Ok(ChunkType::Delegate),
            "event" => Ok(ChunkType::Event),
            _ => Err(ParseChunkTypeError {
                input: s.to_string(),
            }),
        }
    }
}

/// Error returned when parsing an invalid Language string
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLanguageError {
    /// The invalid input string
    pub input: String,
}

impl std::fmt::Display for ParseLanguageError {
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
}

// ---------------------------------------------------------------------------
// Language registration — one line per language
// ---------------------------------------------------------------------------

define_languages! {
    /// Rust (.rs files)
    Rust => "rust", feature = "lang-rust", module = rust;
    /// Python (.py, .pyi files)
    Python => "python", feature = "lang-python", module = python;
    /// TypeScript (.ts, .tsx files)
    TypeScript => "typescript", feature = "lang-typescript", module = typescript;
    /// JavaScript (.js, .jsx, .mjs, .cjs files)
    JavaScript => "javascript", feature = "lang-javascript", module = javascript;
    /// Go (.go files)
    Go => "go", feature = "lang-go", module = go;
    /// C (.c, .h files)
    C => "c", feature = "lang-c", module = c;
    /// Java (.java files)
    Java => "java", feature = "lang-java", module = java;
    /// SQL (.sql files)
    Sql => "sql", feature = "lang-sql", module = sql;
    /// Markdown (.md, .mdx files)
    Markdown => "markdown", feature = "lang-markdown", module = markdown;
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

    /// Get the tree-sitter grammar for this language.
    /// Panics if the language has no grammar (e.g., Markdown uses a custom parser).
    pub fn grammar(&self) -> tree_sitter::Language {
        let grammar_fn = self
            .def()
            .grammar
            .unwrap_or_else(|| panic!("{} has no tree-sitter grammar — use custom parser", self));
        grammar_fn()
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
        #[cfg(feature = "lang-sql")]
        assert!(REGISTRY.from_extension("sql").is_some());
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
        #[cfg(feature = "lang-sql")]
        {
            expected += 1;
        }
        #[cfg(feature = "lang-markdown")]
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
        assert_eq!(Language::from_extension("sql"), Some(Language::Sql));
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
        assert_eq!("sql".parse::<Language>().unwrap(), Language::Sql);
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
        assert_eq!(Language::Sql.to_string(), "sql");
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
        // Verify Display and FromStr are inverses
        let types = [
            ChunkType::Function,
            ChunkType::Method,
            ChunkType::Class,
            ChunkType::Struct,
            ChunkType::Enum,
            ChunkType::Trait,
            ChunkType::Interface,
            ChunkType::Constant,
            ChunkType::Section,
            ChunkType::Property,
            ChunkType::Delegate,
            ChunkType::Event,
        ];
        for ct in types {
            let s = ct.to_string();
            let parsed: ChunkType = s.parse().unwrap();
            assert_eq!(ct, parsed);
        }
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
    }
}
