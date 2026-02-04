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
//! - `lang-all` - All languages

use std::collections::HashMap;
use std::sync::LazyLock;

#[cfg(feature = "lang-go")]
mod go;
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

impl std::str::FromStr for ChunkType {
    type Err = anyhow::Error;
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
            _ => anyhow::bail!(
                "Unknown chunk type: '{}'. Valid options: function, method, class, struct, enum, trait, interface, constant",
                s
            ),
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
}
