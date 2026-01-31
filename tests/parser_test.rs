//! Parser tests

use cqs::parser::{ChunkType, Language, Parser};
use std::path::Path;

fn fixtures_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn test_rust_function_extraction() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.rs");
    let chunks = parser.parse_file(&path).unwrap();

    // Should find add, subtract, new, add, get functions
    assert!(
        chunks.len() >= 5,
        "Expected at least 5 chunks, got {}",
        chunks.len()
    );

    // Check for specific function
    let add_fn = chunks
        .iter()
        .find(|c| c.name == "add" && c.chunk_type == ChunkType::Function);
    assert!(add_fn.is_some(), "Should find 'add' function");

    let add_fn = add_fn.unwrap();
    assert_eq!(add_fn.language, Language::Rust);
    assert!(add_fn.content.contains("a + b"));
}

#[test]
fn test_rust_method_detection() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.rs");
    let chunks = parser.parse_file(&path).unwrap();

    // Methods inside impl block
    let methods: Vec<_> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Method)
        .collect();
    assert!(!methods.is_empty(), "Should find methods in impl block");

    // Check Calculator::new is a method
    let new_method = chunks
        .iter()
        .find(|c| c.name == "new" && c.chunk_type == ChunkType::Method);
    assert!(new_method.is_some(), "Calculator::new should be a method");
}

#[test]
fn test_python_function_extraction() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.py");
    let chunks = parser.parse_file(&path).unwrap();

    assert!(!chunks.is_empty(), "Should find chunks in Python file");

    let greet_fn = chunks.iter().find(|c| c.name == "greet");
    assert!(greet_fn.is_some(), "Should find 'greet' function");

    let greet_fn = greet_fn.unwrap();
    assert_eq!(greet_fn.language, Language::Python);
}

#[test]
fn test_python_method_detection() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.py");
    let chunks = parser.parse_file(&path).unwrap();

    // Methods inside class
    let methods: Vec<_> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Method)
        .collect();
    assert!(!methods.is_empty(), "Should find methods in Python class");

    // Check increment is a method
    let increment = chunks.iter().find(|c| c.name == "increment");
    assert!(increment.is_some(), "Should find 'increment' method");
    assert_eq!(increment.unwrap().chunk_type, ChunkType::Method);
}

#[test]
fn test_typescript_function_extraction() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.ts");
    let chunks = parser.parse_file(&path).unwrap();

    assert!(!chunks.is_empty(), "Should find chunks in TypeScript file");

    let format_fn = chunks.iter().find(|c| c.name == "formatName");
    assert!(format_fn.is_some(), "Should find 'formatName' function");

    let format_fn = format_fn.unwrap();
    assert_eq!(format_fn.language, Language::TypeScript);
}

#[test]
fn test_typescript_arrow_function() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.ts");
    let chunks = parser.parse_file(&path).unwrap();

    let double_fn = chunks.iter().find(|c| c.name == "double");
    assert!(double_fn.is_some(), "Should find 'double' arrow function");
}

#[test]
fn test_javascript_function_extraction() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.js");
    let chunks = parser.parse_file(&path).unwrap();

    assert!(!chunks.is_empty(), "Should find chunks in JavaScript file");

    let validate_fn = chunks.iter().find(|c| c.name == "validateEmail");
    assert!(
        validate_fn.is_some(),
        "Should find 'validateEmail' function"
    );

    let validate_fn = validate_fn.unwrap();
    assert_eq!(validate_fn.language, Language::JavaScript);
}

#[test]
fn test_go_function_extraction() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.go");
    let chunks = parser.parse_file(&path).unwrap();

    assert!(!chunks.is_empty(), "Should find chunks in Go file");

    let greet_fn = chunks.iter().find(|c| c.name == "Greet");
    assert!(greet_fn.is_some(), "Should find 'Greet' function");

    let greet_fn = greet_fn.unwrap();
    assert_eq!(greet_fn.language, Language::Go);
    assert_eq!(greet_fn.chunk_type, ChunkType::Function);
}

#[test]
fn test_go_method_detection() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.go");
    let chunks = parser.parse_file(&path).unwrap();

    // Methods on Stack
    let push = chunks.iter().find(|c| c.name == "Push");
    assert!(push.is_some(), "Should find 'Push' method");
    assert_eq!(push.unwrap().chunk_type, ChunkType::Method);
}

#[test]
fn test_signature_extraction() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.rs");
    let chunks = parser.parse_file(&path).unwrap();

    let add_fn = chunks
        .iter()
        .find(|c| c.name == "add" && c.chunk_type == ChunkType::Function)
        .unwrap();

    // Signature should be normalized (single space)
    assert!(
        add_fn.signature.contains("pub fn add"),
        "Signature should contain function declaration"
    );
    assert!(
        !add_fn.signature.contains('{'),
        "Signature should not contain body"
    );
}

#[test]
fn test_doc_comment_extraction() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.rs");
    let chunks = parser.parse_file(&path).unwrap();

    let add_fn = chunks
        .iter()
        .find(|c| c.name == "add" && c.chunk_type == ChunkType::Function)
        .unwrap();

    assert!(add_fn.doc.is_some(), "Should extract doc comment");
    let doc = add_fn.doc.as_ref().unwrap();
    assert!(
        doc.contains("Adds two numbers"),
        "Doc should contain description"
    );
}

#[test]
fn test_language_from_extension() {
    assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
    assert_eq!(Language::from_extension("py"), Some(Language::Python));
    assert_eq!(Language::from_extension("pyi"), Some(Language::Python));
    assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
    assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
    assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
    assert_eq!(Language::from_extension("jsx"), Some(Language::JavaScript));
    assert_eq!(Language::from_extension("mjs"), Some(Language::JavaScript));
    assert_eq!(Language::from_extension("go"), Some(Language::Go));
    assert_eq!(Language::from_extension("txt"), None);
}

#[test]
fn test_supported_extensions() {
    let parser = Parser::new().unwrap();
    let exts = parser.supported_extensions();

    assert!(exts.contains(&"rs"));
    assert!(exts.contains(&"py"));
    assert!(exts.contains(&"ts"));
    assert!(exts.contains(&"js"));
    assert!(exts.contains(&"go"));
}
