//! Language registry for code parsing
//!
//! This module provides a registry of supported programming languages,
//! each with its own tree-sitter grammar, query patterns, and extraction rules.
//!
//! Languages are registered at compile time based on feature flags.
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

#[cfg(feature = "lang-c")]
mod c;
#[cfg(feature = "lang-go")]
mod go;
#[cfg(feature = "lang-java")]
mod java;
#[cfg(feature = "lang-javascript")]
mod javascript;
#[cfg(feature = "lang-python")]
mod python;
#[cfg(feature = "lang-rust")]
mod rust;
#[cfg(feature = "lang-typescript")]
mod typescript;

/// A language definition with all parsing configuration
pub struct LanguageDef {
    /// Language name (e.g., "rust", "python")
    pub name: &'static str,
    /// Function to get the tree-sitter grammar
    pub grammar: fn() -> tree_sitter::Language,
    /// File extensions for this language
    pub extensions: &'static [&'static str],
    /// Tree-sitter query for extracting code chunks
    pub chunk_query: &'static str,
    /// Tree-sitter query for extracting function calls (optional)
    pub call_query: Option<&'static str>,
    /// How to extract signatures
    pub signature_style: SignatureStyle,
    /// Mapping from tree-sitter capture names to chunk types
    pub type_map: &'static [(&'static str, ChunkType)],
    /// Node types that contain doc comments
    pub doc_nodes: &'static [&'static str],
    /// Node kinds that are themselves methods (e.g., Go's "method_declaration")
    pub method_node_kinds: &'static [&'static str],
    /// Parent node kinds that make a child function a method (e.g., Rust's "impl_item")
    pub method_containers: &'static [&'static str],
}

/// How to extract function signatures
#[derive(Debug, Clone, Copy, Default)]
pub enum SignatureStyle {
    /// Extract until opening brace `{` (Rust, Go, JS, TS)
    #[default]
    UntilBrace,
    /// Extract until colon `:` (Python)
    UntilColon,
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
            "Unknown chunk type: '{}'. Valid options: function, method, class, struct, enum, trait, interface, constant",
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
            _ => Err(ParseChunkTypeError {
                input: s.to_string(),
            }),
        }
    }
}

/// Supported programming languages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// Rust (.rs files)
    Rust,
    /// Python (.py, .pyi files)
    Python,
    /// TypeScript (.ts, .tsx files)
    TypeScript,
    /// JavaScript (.js, .jsx, .mjs, .cjs files)
    JavaScript,
    /// Go (.go files)
    Go,
    /// C (.c, .h files)
    C,
    /// Java (.java files)
    Java,
}

impl Language {
    /// Get the language definition from the registry
    pub fn def(&self) -> &'static LanguageDef {
        REGISTRY
            .get(&self.to_string())
            .expect("language not in registry — check feature flags")
    }

    /// Look up a language by file extension
    pub fn from_extension(ext: &str) -> Option<Self> {
        REGISTRY
            .from_extension(ext)
            .and_then(|def| def.name.parse().ok())
    }

    /// Get the tree-sitter grammar for this language
    pub fn grammar(&self) -> tree_sitter::Language {
        (self.def().grammar)()
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
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::Python => write!(f, "python"),
            Language::TypeScript => write!(f, "typescript"),
            Language::JavaScript => write!(f, "javascript"),
            Language::Go => write!(f, "go"),
            Language::C => write!(f, "c"),
            Language::Java => write!(f, "java"),
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
            "Unknown language: '{}'. Valid options: rust, python, typescript, javascript, go, c, java",
            self.input
        )
    }
}

impl std::error::Error for ParseLanguageError {}

impl std::str::FromStr for Language {
    type Err = ParseLanguageError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rust" => Ok(Language::Rust),
            "python" => Ok(Language::Python),
            "typescript" => Ok(Language::TypeScript),
            "javascript" => Ok(Language::JavaScript),
            "go" => Ok(Language::Go),
            "c" => Ok(Language::C),
            "java" => Ok(Language::Java),
            _ => Err(ParseLanguageError {
                input: s.to_string(),
            }),
        }
    }
}

/// Global language registry
pub static REGISTRY: LazyLock<LanguageRegistry> = LazyLock::new(LanguageRegistry::new);

/// Registry of all supported languages
pub struct LanguageRegistry {
    /// Languages indexed by name
    by_name: HashMap<&'static str, &'static LanguageDef>,
    /// Languages indexed by extension
    by_extension: HashMap<&'static str, &'static LanguageDef>,
}

impl LanguageRegistry {
    /// Create a new registry with all enabled languages
    fn new() -> Self {
        let mut reg = Self {
            by_name: HashMap::new(),
            by_extension: HashMap::new(),
        };

        // Register all enabled languages based on feature flags
        #[cfg(feature = "lang-rust")]
        reg.register(rust::definition());

        #[cfg(feature = "lang-python")]
        reg.register(python::definition());

        #[cfg(feature = "lang-typescript")]
        reg.register(typescript::definition());

        #[cfg(feature = "lang-javascript")]
        reg.register(javascript::definition());

        #[cfg(feature = "lang-go")]
        reg.register(go::definition());

        #[cfg(feature = "lang-c")]
        reg.register(c::definition());

        #[cfg(feature = "lang-java")]
        reg.register(java::definition());

        reg
    }

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
        assert_eq!(all.len(), expected);
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn test_language_grammar() {
        // Verify we can get grammars
        let rust = REGISTRY.get("rust").unwrap();
        let grammar = (rust.grammar)();
        // Just verify grammar is valid by checking ABI version
        assert!(grammar.abi_version() > 0);
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
    }

    #[test]
    fn test_language_def_bridge() {
        // Verify def() returns the correct LanguageDef for each language
        assert_eq!(Language::Rust.def().name, "rust");
        assert_eq!(Language::Python.def().name, "python");
        assert_eq!(Language::Go.def().name, "go");
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
        ];
        for ct in types {
            let s = ct.to_string();
            let parsed: ChunkType = s.parse().unwrap();
            assert_eq!(ct, parsed);
        }
    }
}
