//! MCP protocol integration tests

use serde_json::{json, Value};
use tempfile::TempDir;

// Re-export types we need to test
use cqs::mcp::JsonRpcRequest;

/// Helper to create a test MCP server with initialized index
fn setup_test_server() -> (TempDir, cqs::mcp::McpServer) {
    let dir = TempDir::new().unwrap();
    let project_root = dir.path().to_path_buf();

    // Create .cqs directory and empty index
    let cqs_dir = project_root.join(".cqs");
    std::fs::create_dir_all(&cqs_dir).unwrap();

    // Initialize store with empty database
    let index_path = cqs_dir.join("index.db");
    let store = cqs::store::Store::open(&index_path).unwrap();
    store
        .init(&cqs::store::ModelInfo {
            name: "intfloat/e5-base-v2".into(),
            dimensions: 769, // 768 model + 1 sentiment
            version: "1.0".into(),
        })
        .unwrap();

    let server = cqs::mcp::McpServer::new(project_root, false).unwrap();
    (dir, server)
}

/// Helper to create JSON-RPC request
fn make_request(method: &str, params: Option<Value>) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: method.into(),
        params,
    }
}

#[test]
fn test_initialize() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "initialize",
        Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "test-client",
                "version": "1.0.0"
            }
        })),
    );

    let response = server.handle_request(request);

    // Should succeed
    assert!(
        response.error.is_none(),
        "Expected success, got error: {:?}",
        response.error
    );
    assert!(response.result.is_some());

    let result = response.result.unwrap();
    assert_eq!(result["serverInfo"]["name"], "cqs");
    assert!(result["protocolVersion"].is_string());
    assert!(result["capabilities"]["tools"].is_object());
}

#[test]
fn test_tools_list() {
    let (_dir, server) = setup_test_server();

    let request = make_request("tools/list", None);
    let response = server.handle_request(request);

    assert!(response.error.is_none());
    let result = response.result.unwrap();

    // Should have tools array
    let tools = result["tools"].as_array().unwrap();
    assert!(tools.len() >= 2);

    // Should have cqs_search tool
    let search_tool = tools.iter().find(|t| t["name"] == "cqs_search");
    assert!(search_tool.is_some(), "Missing cqs_search tool");

    let search_tool = search_tool.unwrap();
    assert!(search_tool["description"].is_string());
    assert!(search_tool["inputSchema"]["properties"]["query"].is_object());

    // Should have cqs_stats tool
    let stats_tool = tools.iter().find(|t| t["name"] == "cqs_stats");
    assert!(stats_tool.is_some(), "Missing cqs_stats tool");
}

#[test]
fn test_tools_call_stats() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_stats",
            "arguments": {}
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "Stats call failed: {:?}",
        response.error
    );
    let result = response.result.unwrap();

    // Should have content array with text
    let content = result["content"].as_array().unwrap();
    assert!(!content.is_empty());
    assert_eq!(content[0]["type"], "text");

    // Text should contain stats info
    let text = content[0]["text"].as_str().unwrap();
    assert!(
        text.contains("chunks") || text.contains("Total"),
        "Stats text should mention chunks: {}",
        text
    );
}

#[test]
fn test_unknown_method() {
    let (_dir, server) = setup_test_server();

    let request = make_request("unknown/method", None);
    let response = server.handle_request(request);

    // Should return error
    assert!(response.error.is_some());
    let error = response.error.unwrap();
    assert!(error.message.contains("Unknown method"));
}

#[test]
fn test_tools_call_unknown_tool() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "unknown_tool",
            "arguments": {}
        })),
    );

    let response = server.handle_request(request);

    assert!(response.error.is_some());
    let error = response.error.unwrap();
    assert!(error.message.contains("Unknown tool"));
}

#[test]
fn test_tools_call_missing_params() {
    let (_dir, server) = setup_test_server();

    let request = make_request("tools/call", None);
    let response = server.handle_request(request);

    assert!(response.error.is_some());
    let error = response.error.unwrap();
    assert!(error.message.contains("Missing"));
}

#[test]
fn test_initialized_notification() {
    let (_dir, server) = setup_test_server();

    // initialized is a notification, should return null
    let request = make_request("initialized", None);
    let response = server.handle_request(request);

    assert!(response.error.is_none());
    assert_eq!(response.result, Some(Value::Null));
}

#[test]
fn test_response_has_id() {
    let (_dir, server) = setup_test_server();

    let request = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(42)),
        method: "tools/list".into(),
        params: None,
    };

    let response = server.handle_request(request);

    // Response ID should match request ID
    assert_eq!(response.id, Some(json!(42)));
}

#[test]
fn test_cqs_read_valid_file() {
    let (dir, server) = setup_test_server();

    // Create a test file
    let test_file = dir.path().join("test.rs");
    std::fs::write(&test_file, "fn main() {}").unwrap();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_read",
            "arguments": {"path": "test.rs"}
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "cqs_read failed: {:?}",
        response.error
    );
    let result = response.result.unwrap();
    let content = result["content"][0]["text"].as_str().unwrap();
    assert!(content.contains("fn main()"));
}

#[test]
fn test_cqs_read_path_traversal_blocked() {
    let (_dir, server) = setup_test_server();

    // Try to read /etc/passwd via path traversal
    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_read",
            "arguments": {"path": "../../../etc/passwd"}
        })),
    );

    let response = server.handle_request(request);

    // Should fail with error
    assert!(response.error.is_some(), "Path traversal should be blocked");
    let error = response.error.unwrap();
    assert!(
        error.message.contains("traversal") || error.message.contains("not found"),
        "Error should mention traversal or not found: {}",
        error.message
    );
}

#[test]
fn test_cqs_read_file_not_found() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_read",
            "arguments": {"path": "nonexistent.rs"}
        })),
    );

    let response = server.handle_request(request);

    assert!(response.error.is_some());
    let error = response.error.unwrap();
    assert!(error.message.contains("not found"));
}

#[test]
fn test_cqs_read_with_notes() {
    let (dir, server) = setup_test_server();

    // Create docs directory and notes.toml
    let docs_dir = dir.path().join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();
    std::fs::write(
        docs_dir.join("notes.toml"),
        r#"
[[note]]
sentiment = -0.8
text = "This is a warning note about test.rs"
mentions = ["test.rs"]
"#,
    )
    .unwrap();

    // Create the mentioned file
    std::fs::write(dir.path().join("test.rs"), "fn test() {}").unwrap();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_read",
            "arguments": {"path": "test.rs"}
        })),
    );

    let response = server.handle_request(request);

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    let content = result["content"][0]["text"].as_str().unwrap();

    // Should contain note context
    assert!(
        content.contains("[WARNING]") || content.contains("warning note"),
        "Should inject note context: {}",
        content
    );
    // Should also contain file content
    assert!(content.contains("fn test()"));
}

// ===== MCP Protocol Edge Case Tests =====

#[test]
fn test_initialize_with_old_protocol_version() {
    let (_dir, server) = setup_test_server();

    // Use an older protocol version - server should accept and respond with its version
    let request = make_request(
        "initialize",
        Some(json!({
            "protocolVersion": "2023-01-01",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        })),
    );

    let response = server.handle_request(request);

    // Should succeed - server ignores client version and returns its own
    assert!(response.error.is_none());
    let result = response.result.unwrap();
    // Server returns its supported version
    assert!(result["protocolVersion"].is_string());
}

#[test]
fn test_string_request_id() {
    let (_dir, server) = setup_test_server();

    let request = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!("string-id-123")),
        method: "tools/list".into(),
        params: None,
    };

    let response = server.handle_request(request);

    assert!(response.error.is_none());
    assert_eq!(response.id, Some(json!("string-id-123")));
}

#[test]
fn test_null_request_id_notification() {
    let (_dir, server) = setup_test_server();

    // Null ID is valid for notifications
    let request = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: None,
        method: "initialized".into(),
        params: None,
    };

    let response = server.handle_request(request);

    // Notification response - no error, result is null
    assert!(response.error.is_none());
}

#[test]
fn test_tools_call_wrong_param_type() {
    let (_dir, server) = setup_test_server();

    // Pass a string for arguments instead of object
    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_stats",
            "arguments": "not an object"
        })),
    );

    let response = server.handle_request(request);

    // Should handle gracefully - either succeed with default args or error
    // The important thing is it doesn't panic
    assert!(response.error.is_some() || response.result.is_some());
}

#[test]
fn test_tools_call_null_arguments() {
    let (_dir, server) = setup_test_server();

    // Null arguments should be treated as empty object
    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_stats",
            "arguments": null
        })),
    );

    let response = server.handle_request(request);

    // cqs_stats has no required args, should succeed
    assert!(
        response.error.is_none(),
        "Null arguments should work: {:?}",
        response.error
    );
}

#[test]
fn test_tools_call_missing_name() {
    let (_dir, server) = setup_test_server();

    // Missing required "name" field
    let request = make_request(
        "tools/call",
        Some(json!({
            "arguments": {}
        })),
    );

    let response = server.handle_request(request);

    assert!(response.error.is_some());
    let error = response.error.unwrap();
    // Should indicate missing field
    assert!(
        error.message.contains("Missing") || error.message.contains("name"),
        "Error should mention missing name: {}",
        error.message
    );
}

#[test]
fn test_empty_method() {
    let (_dir, server) = setup_test_server();

    let request = make_request("", None);
    let response = server.handle_request(request);

    assert!(response.error.is_some());
}

#[test]
fn test_very_long_query_rejected() {
    let (_dir, server) = setup_test_server();

    // Create a query longer than MAX_QUERY_LENGTH (10000 bytes)
    let long_query = "a".repeat(15000);

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {"query": long_query}
        })),
    );

    let response = server.handle_request(request);

    // Should reject with error about query length
    assert!(response.error.is_some());
    let error = response.error.unwrap();
    assert!(
        error.message.contains("too long") || error.message.contains("Query"),
        "Error should mention query length: {}",
        error.message
    );
}

// ===== MCP Response Format Test (#9) =====

#[test]
fn test_mcp_search_response_format() {
    let (_dir, server) = setup_test_server();

    // Make a search request (use name_only to avoid needing embedder)
    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {
                "query": "test_function",
                "name_only": true
            }
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "Search should succeed: {:?}",
        response.error
    );

    let result = response.result.unwrap();

    // Validate response structure
    let content = result["content"]
        .as_array()
        .expect("Response should have content array");
    assert!(!content.is_empty(), "Content array should not be empty");

    // First content item should be type "text"
    assert_eq!(
        content[0]["type"], "text",
        "First content item should be type 'text'"
    );

    // Text field should be valid JSON
    let text = content[0]["text"]
        .as_str()
        .expect("Content should have text field");
    let parsed: serde_json::Value =
        serde_json::from_str(text).expect("Text content should be valid JSON");

    // Should have results field (may be empty array)
    assert!(
        parsed.get("results").is_some(),
        "Parsed JSON should have 'results' field"
    );
}

#[test]
fn test_initialize_without_params() {
    let (_dir, server) = setup_test_server();

    // Initialize with no params - should use defaults
    let request = make_request("initialize", None);
    let response = server.handle_request(request);

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    assert_eq!(result["serverInfo"]["name"], "cqs");
}

#[test]
fn test_cqs_search_with_all_optional_params() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {
                "query": "test query",
                "limit": 10,
                "threshold": 0.5,
                "language": "rust",
                "path_pattern": "src/**",
                "name_boost": 0.3,
                "semantic_only": true
            }
        })),
    );

    let response = server.handle_request(request);

    // Should succeed (even if no results found)
    assert!(
        response.error.is_none(),
        "Search with all params failed: {:?}",
        response.error
    );
}

// ===== Additional Edge Case Tests =====

#[test]
fn test_deeply_nested_json() {
    let (_dir, server) = setup_test_server();

    // Create deeply nested JSON to test parser limits
    let mut nested = json!({"inner": "value"});
    for _ in 0..50 {
        nested = json!({"level": nested});
    }

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_stats",
            "arguments": nested
        })),
    );

    let response = server.handle_request(request);

    // Should handle gracefully - either succeed or return an error (not panic)
    // cqs_stats ignores extra arguments, so it may succeed
    assert!(
        response.error.is_some() || response.result.is_some(),
        "Should handle deeply nested JSON without panic"
    );
}

#[test]
fn test_unicode_in_query() {
    let (_dir, server) = setup_test_server();

    // Test various unicode characters in search query
    let unicode_queries = [
        "函数名称",                      // Chinese
        "функция",                       // Russian
        "関数",                          // Japanese
        "🔍 search emoji",               // Emoji
        "café résumé naïve",             // Accented Latin
        "αβγδ Greek letters",            // Greek
        "test\u{200B}hidden\u{FEFF}bom", // Zero-width chars and BOM
    ];

    for query in &unicode_queries {
        let request = make_request(
            "tools/call",
            Some(json!({
                "name": "cqs_search",
                "arguments": {"query": query}
            })),
        );

        let response = server.handle_request(request);

        // Should handle all unicode gracefully (not panic, may return empty results)
        assert!(
            response.error.is_none()
                || response
                    .error
                    .as_ref()
                    .is_some_and(|e| !e.message.contains("panic")),
            "Unicode query '{}' should not cause panic: {:?}",
            query,
            response.error
        );
    }
}

#[test]
fn test_concurrent_requests() {
    use std::sync::Arc;
    use std::thread;

    let (dir, server) = setup_test_server();
    let server = Arc::new(server);

    // Create a test file so cqs_read has something to read
    std::fs::write(dir.path().join("concurrent_test.rs"), "fn main() {}").unwrap();

    // Spawn multiple threads making concurrent requests
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let server = Arc::clone(&server);
            thread::spawn(move || {
                // Each thread makes multiple requests
                for j in 0..5 {
                    let request = JsonRpcRequest {
                        jsonrpc: "2.0".into(),
                        id: Some(json!(format!("thread-{}-req-{}", i, j))),
                        method: "tools/call".into(),
                        params: Some(json!({
                            "name": "cqs_stats",
                            "arguments": {}
                        })),
                    };

                    let response = server.handle_request(request);
                    assert!(
                        response.error.is_none(),
                        "Concurrent request failed: {:?}",
                        response.error
                    );
                }
            })
        })
        .collect();

    // Wait for all threads
    for handle in handles {
        handle
            .join()
            .expect("Thread panicked during concurrent test");
    }
}

#[test]
fn test_special_characters_in_path() {
    let (dir, server) = setup_test_server();

    // Test reading files with special characters in path
    // Create file with special name (but safe for filesystem)
    let special_name = "test-file_v2.0.rs";
    std::fs::write(dir.path().join(special_name), "fn special() {}").unwrap();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_read",
            "arguments": {"path": special_name}
        })),
    );

    let response = server.handle_request(request);
    assert!(
        response.error.is_none(),
        "Special chars in filename should work: {:?}",
        response.error
    );
}

#[test]
fn test_empty_query_string() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {"query": ""}
        })),
    );

    let response = server.handle_request(request);

    // Empty query should either return error or empty results (not panic)
    assert!(
        response.error.is_some() || response.result.is_some(),
        "Empty query should be handled gracefully"
    );
}

#[test]
fn test_whitespace_only_query() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {"query": "   \t\n   "}
        })),
    );

    let response = server.handle_request(request);

    // Whitespace-only query should be handled gracefully
    assert!(
        response.error.is_some() || response.result.is_some(),
        "Whitespace query should be handled gracefully"
    );
}

// =============================================================================
// Tool-specific tests: search, notes, callers/callees, audit, stats
// =============================================================================

// ----- cqs_search tool tests -----

#[test]
fn test_cqs_search_name_only_mode() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {
                "query": "test_function",
                "name_only": true
            }
        })),
    );

    let response = server.handle_request(request);

    // name_only mode should succeed (skips embedder, searches by name)
    assert!(
        response.error.is_none(),
        "name_only search failed: {:?}",
        response.error
    );
}

#[test]
fn test_cqs_search_limit_clamping_zero() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {
                "query": "test",
                "limit": 0,
                "name_only": true
            }
        })),
    );

    let response = server.handle_request(request);

    // limit=0 should be clamped to 1, not error
    assert!(
        response.error.is_none(),
        "limit=0 should be clamped, got error: {:?}",
        response.error
    );
}

#[test]
fn test_cqs_search_limit_clamping_high() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {
                "query": "test",
                "limit": 50,
                "name_only": true
            }
        })),
    );

    let response = server.handle_request(request);

    // limit=50 should be clamped to 20, not error
    assert!(
        response.error.is_none(),
        "limit=50 should be clamped, got error: {:?}",
        response.error
    );
}

#[test]
fn test_cqs_search_threshold_boundaries() {
    let (_dir, server) = setup_test_server();

    for threshold in &[0.0, 1.0] {
        let request = make_request(
            "tools/call",
            Some(json!({
                "name": "cqs_search",
                "arguments": {
                    "query": "test",
                    "threshold": threshold,
                    "name_only": true
                }
            })),
        );

        let response = server.handle_request(request);

        assert!(
            response.error.is_none(),
            "threshold={} should be accepted, got error: {:?}",
            threshold,
            response.error
        );
    }
}

#[test]
fn test_cqs_search_missing_query() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {}
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_some(),
        "Missing query should return error"
    );
}

#[test]
fn test_cqs_search_note_weight_param() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_search",
            "arguments": {
                "query": "test",
                "note_weight": 0.5,
                "name_only": true
            }
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "note_weight param should be accepted: {:?}",
        response.error
    );
}

// ----- cqs_add_note tool tests -----

#[test]
fn test_cqs_add_note_basic() {
    let (dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_add_note",
            "arguments": {
                "text": "This is a test note"
            }
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "add_note failed: {:?}",
        response.error
    );

    let result = response.result.unwrap();
    let content = result["content"][0]["text"].as_str().unwrap();

    // Should confirm note was added
    assert!(
        content.contains("added") || content.contains("status"),
        "Should confirm addition: {}",
        content
    );

    // docs/notes.toml should exist
    assert!(
        dir.path().join("docs/notes.toml").exists(),
        "docs/notes.toml should be created"
    );
}

#[test]
fn test_cqs_add_note_empty_text() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_add_note",
            "arguments": {
                "text": ""
            }
        })),
    );

    let response = server.handle_request(request);

    assert!(response.error.is_some(), "Empty text should return error");
}

#[test]
fn test_cqs_add_note_missing_text() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_add_note",
            "arguments": {}
        })),
    );

    let response = server.handle_request(request);

    assert!(response.error.is_some(), "Missing text should return error");
}

#[test]
fn test_cqs_add_note_with_mentions() {
    let (dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_add_note",
            "arguments": {
                "text": "Found a pattern in search",
                "mentions": ["search.rs", "index.rs"],
                "sentiment": 0.5
            }
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "add_note with mentions failed: {:?}",
        response.error
    );

    // Verify TOML contains mentions
    let toml_content = std::fs::read_to_string(dir.path().join("docs/notes.toml")).unwrap();
    assert!(
        toml_content.contains("search.rs"),
        "TOML should contain mention: {}",
        toml_content
    );
    assert!(
        toml_content.contains("index.rs"),
        "TOML should contain mention: {}",
        toml_content
    );
}

#[test]
fn test_cqs_add_note_sentiment_clamping() {
    let (dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_add_note",
            "arguments": {
                "text": "Extreme sentiment test",
                "sentiment": 5.0
            }
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "Extreme sentiment should be clamped, not error: {:?}",
        response.error
    );

    // Verify sentiment was clamped to 1.0
    let toml_content = std::fs::read_to_string(dir.path().join("docs/notes.toml")).unwrap();
    assert!(
        toml_content.contains("sentiment = 1") || toml_content.contains("sentiment = 1.0"),
        "Sentiment should be clamped to 1.0: {}",
        toml_content
    );
}

#[test]
fn test_cqs_add_note_toml_escaping() {
    let (dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_add_note",
            "arguments": {
                "text": "Note with \"quotes\" and\nnewlines"
            }
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "TOML escaping failed: {:?}",
        response.error
    );

    // File should be valid TOML
    let toml_content = std::fs::read_to_string(dir.path().join("docs/notes.toml")).unwrap();
    assert!(
        toml_content.parse::<toml::Table>().is_ok(),
        "notes.toml should be valid TOML: {}",
        toml_content
    );
}

#[test]
fn test_cqs_add_note_appends() {
    let (_dir, server) = setup_test_server();

    // Add first note
    let request1 = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_add_note",
            "arguments": { "text": "First note" }
        })),
    );
    let response1 = server.handle_request(request1);
    assert!(response1.error.is_none());

    // Add second note
    let request2 = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_add_note",
            "arguments": { "text": "Second note" }
        })),
    );
    let response2 = server.handle_request(request2);
    assert!(response2.error.is_none());

    // Response should indicate total_notes >= 2
    let result = response2.result.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("total_notes") || text.contains("2"),
        "Should show multiple notes: {}",
        text
    );
}

// ----- cqs_callers / cqs_callees tool tests -----

#[test]
fn test_cqs_callers_missing_name() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_callers",
            "arguments": {}
        })),
    );

    let response = server.handle_request(request);

    assert!(response.error.is_some(), "Missing name should return error");
}

#[test]
fn test_cqs_callers_nonexistent() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_callers",
            "arguments": { "name": "nonexistent_function_xyz" }
        })),
    );

    let response = server.handle_request(request);

    // Should succeed with empty results, not error
    assert!(
        response.error.is_none(),
        "Nonexistent function should not error: {:?}",
        response.error
    );
    let result = response.result.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("No callers") || text.contains("[]"),
        "Should indicate no callers found: {}",
        text
    );
}

#[test]
fn test_cqs_callees_missing_name() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_callees",
            "arguments": {}
        })),
    );

    let response = server.handle_request(request);

    assert!(response.error.is_some(), "Missing name should return error");
}

#[test]
fn test_cqs_callees_nonexistent() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_callees",
            "arguments": { "name": "nonexistent_function_xyz" }
        })),
    );

    let response = server.handle_request(request);

    // Should succeed with empty results
    assert!(
        response.error.is_none(),
        "Nonexistent function should not error: {:?}",
        response.error
    );
}

// ----- cqs_audit_mode tool tests -----

#[test]
fn test_cqs_audit_mode_query() {
    let (_dir, server) = setup_test_server();

    // Query without setting - should return current state
    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_audit_mode",
            "arguments": {}
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "Audit mode query failed: {:?}",
        response.error
    );
    let result = response.result.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("audit_mode"),
        "Should contain audit_mode status: {}",
        text
    );
}

#[test]
fn test_cqs_audit_mode_enable_disable() {
    let (_dir, server) = setup_test_server();

    // Enable
    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_audit_mode",
            "arguments": { "enabled": true }
        })),
    );

    let response = server.handle_request(request);
    assert!(
        response.error.is_none(),
        "Audit enable failed: {:?}",
        response.error
    );
    let result = response.result.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("true") || text.contains("enabled"),
        "Should confirm enabled: {}",
        text
    );

    // Disable
    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_audit_mode",
            "arguments": { "enabled": false }
        })),
    );

    let response = server.handle_request(request);
    assert!(
        response.error.is_none(),
        "Audit disable failed: {:?}",
        response.error
    );
}

#[test]
fn test_cqs_audit_mode_custom_duration() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_audit_mode",
            "arguments": {
                "enabled": true,
                "expires_in": "1h"
            }
        })),
    );

    let response = server.handle_request(request);

    assert!(
        response.error.is_none(),
        "Custom duration failed: {:?}",
        response.error
    );
}

// ----- cqs_stats tool tests (extended) -----

#[test]
fn test_cqs_stats_response_structure() {
    let (_dir, server) = setup_test_server();

    let request = make_request(
        "tools/call",
        Some(json!({
            "name": "cqs_stats",
            "arguments": {}
        })),
    );

    let response = server.handle_request(request);

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();

    // Should contain key stats fields
    assert!(
        text.contains("chunks") || text.contains("Total"),
        "Should have chunk info: {}",
        text
    );
    assert!(
        text.contains("model") || text.contains("e5"),
        "Should have model info: {}",
        text
    );
}

// =============================================================================
// Server Entry Point Tests (stdio transport)
// =============================================================================

mod server_tests {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    /// Setup a project directory with .cqs initialized
    fn setup_project() -> TempDir {
        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Initialize store
        let index_path = cqs_dir.join("index.db");
        let store = cqs::store::Store::open(&index_path).unwrap();
        store
            .init(&cqs::store::ModelInfo {
                name: "intfloat/e5-base-v2".into(),
                dimensions: 769,
                version: "1.0".into(),
            })
            .unwrap();

        dir
    }

    #[test]
    fn test_stdio_initialize_request() {
        let dir = setup_project();

        // Spawn cqs serve --transport stdio
        let mut child = Command::new(env!("CARGO_BIN_EXE_cqs"))
            .args(["serve", "--transport", "stdio", "--project"])
            .arg(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("Failed to spawn cqs serve");

        let mut stdin = child.stdin.take().expect("Failed to get stdin");
        let stdout = child.stdout.take().expect("Failed to get stdout");
        let mut reader = BufReader::new(stdout);

        // Send initialize request
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        writeln!(stdin, "{}", request).expect("Failed to write request");
        stdin.flush().expect("Failed to flush stdin");

        // Read response
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .expect("Failed to read response");

        // Verify response is valid JSON-RPC
        let json: serde_json::Value =
            serde_json::from_str(&response).expect("Response should be valid JSON");
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert!(json["result"].is_object(), "Should have result object");
        assert!(
            json["result"]["protocolVersion"].is_string(),
            "Should have protocol version"
        );

        // Clean up
        drop(stdin);
        let _ = child.wait();
    }

    #[test]
    fn test_stdio_list_tools_request() {
        let dir = setup_project();

        let mut child = Command::new(env!("CARGO_BIN_EXE_cqs"))
            .args(["serve", "--transport", "stdio", "--project"])
            .arg(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("Failed to spawn cqs serve");

        let mut stdin = child.stdin.take().expect("Failed to get stdin");
        let stdout = child.stdout.take().expect("Failed to get stdout");
        let mut reader = BufReader::new(stdout);

        // Initialize first
        let init_request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        writeln!(stdin, "{}", init_request).expect("Failed to write init request");
        stdin.flush().expect("Failed to flush");
        let mut _init_response = String::new();
        reader.read_line(&mut _init_response).unwrap();

        // Send tools/list request
        let tools_request = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        writeln!(stdin, "{}", tools_request).expect("Failed to write tools request");
        stdin.flush().expect("Failed to flush");

        // Read response
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .expect("Failed to read response");

        // Verify response contains tools
        let json: serde_json::Value = serde_json::from_str(&response).expect("Valid JSON");
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 2);
        assert!(
            json["result"]["tools"].is_array(),
            "Should have tools array"
        );

        let tools = json["result"]["tools"].as_array().unwrap();
        let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(
            tool_names.contains(&"cqs_search"),
            "Should have cqs_search tool"
        );
        assert!(
            tool_names.contains(&"cqs_stats"),
            "Should have cqs_stats tool"
        );

        drop(stdin);
        let _ = child.wait();
    }

    #[test]
    fn test_stdio_invalid_json_returns_error() {
        let dir = setup_project();

        let mut child = Command::new(env!("CARGO_BIN_EXE_cqs"))
            .args(["serve", "--transport", "stdio", "--project"])
            .arg(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("Failed to spawn cqs serve");

        let mut stdin = child.stdin.take().expect("Failed to get stdin");
        let stdout = child.stdout.take().expect("Failed to get stdout");
        let mut reader = BufReader::new(stdout);

        // Send invalid JSON
        writeln!(stdin, "{{not valid json}}").expect("Failed to write");
        stdin.flush().expect("Failed to flush");

        // Read response
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .expect("Failed to read response");

        // Should get a JSON-RPC error response
        let json: serde_json::Value = serde_json::from_str(&response).expect("Valid JSON");
        assert_eq!(json["jsonrpc"], "2.0");
        assert!(json["error"].is_object(), "Should have error object");

        drop(stdin);
        let _ = child.wait();
    }
}
