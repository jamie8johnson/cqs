//! Parser tests

use cqs::parser::{ChunkType, Language, Parser};

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

// ===== C and Java Parser Fixture Tests (#239) =====

#[test]
fn test_parse_c_fixture() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.c");
    let chunks = parser.parse_file(&path).unwrap();

    // C parser may not extract everything - check what we actually got
    assert!(
        !chunks.is_empty(),
        "Should find at least some chunks in C file"
    );

    // Verify language is correct
    for chunk in &chunks {
        assert_eq!(
            chunk.language,
            Language::C,
            "All chunks should be Language::C"
        );
    }

    // Note: C parser capabilities depend on tree-sitter-c
    // Just verify we can parse the file and extract at least some functions
    let function_count = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Function)
        .count();
    assert!(
        function_count > 0,
        "Should find at least one function in C file"
    );
}

#[test]
fn test_parse_java_fixture() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("Sample.java");
    let chunks = parser.parse_file(&path).unwrap();

    let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"TaskManager"),
        "Should find TaskManager class"
    );
    assert!(names.contains(&"addTask"), "Should find addTask method");
    assert!(
        names.contains(&"findByName"),
        "Should find findByName method"
    );
    assert!(
        names.contains(&"getHighPriority"),
        "Should find getHighPriority method"
    );
    assert!(names.contains(&"size"), "Should find size method");
    assert!(names.contains(&"Task"), "Should find Task class");

    // Verify language
    for chunk in &chunks {
        assert_eq!(
            chunk.language,
            Language::Java,
            "All chunks should be Language::Java"
        );
    }

    // Verify methods are detected correctly
    let add_task = chunks.iter().find(|c| c.name == "addTask");
    assert!(add_task.is_some(), "Should find addTask chunk");
    let add_task = add_task.unwrap();
    assert_eq!(
        add_task.chunk_type,
        ChunkType::Method,
        "addTask should be a method"
    );
    assert!(
        add_task.content.contains("tasks.add"),
        "addTask should contain tasks.add call"
    );

    // Verify doc comments are extracted
    let task_manager = chunks.iter().find(|c| c.name == "TaskManager");
    assert!(task_manager.is_some(), "Should find TaskManager chunk");
    let task_manager = task_manager.unwrap();
    if let Some(doc) = &task_manager.doc {
        assert!(doc.contains("task manager"), "Should extract doc comment");
    }
}

// ===== SQL tests =====

#[test]
fn test_parse_sql_fixture() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.sql");
    let chunks = parser.parse_file(&path).unwrap();

    // 2 procs + 1 function + 1 view = 4 chunks
    // (T-SQL trigger syntax not supported by grammar — PostgreSQL triggers only)
    assert!(
        chunks.len() >= 4,
        "Expected at least 4 chunks, got {}",
        chunks.len()
    );

    // Stored procedure
    let proc = chunks.iter().find(|c| c.name.contains("usp_GetOrders"));
    assert!(proc.is_some(), "Should find usp_GetOrders procedure");
    let proc = proc.unwrap();
    assert_eq!(proc.chunk_type, ChunkType::Function);
    assert_eq!(proc.language, Language::Sql);

    // Function
    let func = chunks.iter().find(|c| c.name.contains("fn_CalcTotal"));
    assert!(func.is_some(), "Should find fn_CalcTotal function");
    assert_eq!(func.unwrap().chunk_type, ChunkType::Function);

    // View
    let view = chunks
        .iter()
        .find(|c| c.name.contains("vw_ActiveCustomers"));
    assert!(view.is_some(), "Should find vw_ActiveCustomers view");
    assert_eq!(view.unwrap().chunk_type, ChunkType::Constant);
}

#[test]
fn test_sql_signature_extraction() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.sql");
    let chunks = parser.parse_file(&path).unwrap();

    let func = chunks
        .iter()
        .find(|c| c.name.contains("fn_CalcTotal"))
        .expect("Should find fn_CalcTotal");
    // Signature should stop at AS, not include the body
    assert!(
        func.signature.contains("fn_CalcTotal"),
        "Signature should contain function name: {}",
        func.signature
    );
    assert!(
        !func.signature.contains("BEGIN"),
        "Signature should stop before BEGIN: {}",
        func.signature
    );
}

#[test]
fn test_sql_schema_qualified_names() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.sql");
    let chunks = parser.parse_file(&path).unwrap();

    let proc = chunks
        .iter()
        .find(|c| c.name.contains("usp_GetOrders"))
        .expect("Should find usp_GetOrders");
    // Schema prefix should be preserved in the name
    assert!(
        proc.name.contains("dbo"),
        "Should preserve schema prefix: {}",
        proc.name
    );
}

#[test]
fn test_sql_go_separator() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.sql");
    let chunks = parser.parse_file(&path).unwrap();

    // GO separators should not prevent multi-batch parsing
    // 4 chunks span multiple GO-separated batches
    assert!(
        chunks.len() >= 4,
        "GO separators should not break parsing, got {} chunks",
        chunks.len()
    );
}

#[test]
fn test_sql_call_extraction() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.sql");
    let chunks = parser.parse_file(&path).unwrap();

    // usp_ProcessOrder should have EXEC call in its body
    let process = chunks
        .iter()
        .find(|c| c.name.contains("usp_ProcessOrder"))
        .expect("Should find usp_ProcessOrder");
    assert!(
        process.content.contains("EXEC"),
        "usp_ProcessOrder body should contain EXEC call"
    );

    // Extract actual calls from the procedure body
    let calls = parser.extract_calls(&process.content, Language::Sql, 0, process.content.len(), 0);
    assert!(
        !calls.is_empty(),
        "Should extract calls from usp_ProcessOrder"
    );
}

// ===== Markdown tests =====

#[test]
fn test_parse_markdown_fixture() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.md");
    let chunks = parser.parse_file(&path).unwrap();

    assert!(!chunks.is_empty(), "Should find chunks in markdown file");

    // All chunks should be Section type and Markdown language
    for chunk in &chunks {
        assert_eq!(chunk.chunk_type, ChunkType::Section);
        assert_eq!(chunk.language, Language::Markdown);
    }

    // Should have sections from the fixture
    let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"Getting Started"),
        "Should find 'Getting Started' section, got: {:?}",
        names
    );
    assert!(
        names.contains(&"Advanced Topics"),
        "Should find 'Advanced Topics' section, got: {:?}",
        names
    );
}

#[test]
fn test_markdown_breadcrumb_signatures() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.md");
    let chunks = parser.parse_file(&path).unwrap();

    // H2 sections should have title in breadcrumb
    let getting_started = chunks.iter().find(|c| c.name == "Getting Started");
    assert!(getting_started.is_some(), "Should find 'Getting Started'");
    let gs = getting_started.unwrap();
    assert!(
        gs.signature.contains("Sample Documentation"),
        "Breadcrumb should contain title, got: {}",
        gs.signature
    );
}

#[test]
fn test_markdown_code_block_headings_ignored() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.md");
    let chunks = parser.parse_file(&path).unwrap();

    let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();

    // Headings inside code blocks should NOT appear as chunk names
    assert!(
        !names.contains(&"This heading inside a code block should NOT be parsed"),
        "Should not parse heading inside code block, got: {:?}",
        names
    );
    assert!(
        !names.contains(&"Also not a heading"),
        "Should not parse heading inside code block, got: {:?}",
        names
    );
}

#[test]
fn test_markdown_cross_references() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.md");
    let refs = parser.parse_file_calls(&path).unwrap();

    let all_callees: Vec<&str> = refs
        .iter()
        .flat_map(|fc| fc.calls.iter().map(|c| c.callee_name.as_str()))
        .collect();

    // Should find markdown links
    assert!(
        all_callees.contains(&"Configuration Guide"),
        "Should extract link text, got: {:?}",
        all_callees
    );
    assert!(
        all_callees.contains(&"API Reference"),
        "Should extract link text, got: {:?}",
        all_callees
    );

    // Should find backtick function references
    assert!(
        all_callees.contains(&"TagRead"),
        "Should extract backtick function ref, got: {:?}",
        all_callees
    );
    assert!(
        all_callees.contains(&"Module.func"),
        "Should extract backtick module.func ref, got: {:?}",
        all_callees
    );
    assert!(
        all_callees.contains(&"Class::method"),
        "Should extract backtick Class::method ref, got: {:?}",
        all_callees
    );
}

#[test]
fn test_markdown_image_links_skipped() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.md");
    let refs = parser.parse_file_calls(&path).unwrap();

    let all_callees: Vec<&str> = refs
        .iter()
        .flat_map(|fc| fc.calls.iter().map(|c| c.callee_name.as_str()))
        .collect();

    // Image links should NOT be extracted as references
    assert!(
        !all_callees.contains(&"architecture diagram"),
        "Should NOT extract image link text, got: {:?}",
        all_callees
    );
}

#[test]
fn test_markdown_code_blocks_in_content() {
    let parser = Parser::new().unwrap();
    let path = fixtures_path().join("sample.md");
    let chunks = parser.parse_file(&path).unwrap();

    // Code blocks should stay in their parent chunk's content
    let has_code_block = chunks.iter().any(|c| c.content.contains("def example():"));
    assert!(
        has_code_block,
        "Code block content should be preserved in parent chunk"
    );
}

#[test]
fn test_markdown_supported_extensions() {
    let parser = Parser::new().unwrap();
    let exts = parser.supported_extensions();
    assert!(exts.contains(&"md"), "Should support .md extension");
    assert!(exts.contains(&"mdx"), "Should support .mdx extension");
}

#[test]
fn test_markdown_language_from_extension() {
    assert_eq!(Language::from_extension("md"), Some(Language::Markdown));
    assert_eq!(Language::from_extension("mdx"), Some(Language::Markdown));
}
