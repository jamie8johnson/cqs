//! MCP protocol integration tests

use serde_json::{json, Value};
use tempfile::TempDir;

// Re-export types we need to test
use cqs::mcp::JsonRpcRequest;

/// Helper to create a test MCP server with initialized index
fn setup_test_server() -> (TempDir, cqs::mcp::McpServer) {
    let dir = TempDir::new().unwrap();
    let project_root = dir.path().to_path_buf();

    // Create .cq directory and empty index
    let cq_dir = project_root.join(".cq");
    std::fs::create_dir_all(&cq_dir).unwrap();

    // Initialize store with empty database
    let index_path = cq_dir.join("index.db");
    let store = cqs::store::Store::open(&index_path).unwrap();
    store
        .init(&cqs::store::ModelInfo {
            name: "intfloat/e5-base-v2".into(),
            dimensions: 769, // 768 model + 1 sentiment
            version: "1.0".into(),
        })
        .unwrap();

    let server = cqs::mcp::McpServer::new(project_root).unwrap();
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
    let (_dir, mut server) = setup_test_server();

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
    let (_dir, mut server) = setup_test_server();

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
    let (_dir, mut server) = setup_test_server();

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
    let (_dir, mut server) = setup_test_server();

    let request = make_request("unknown/method", None);
    let response = server.handle_request(request);

    // Should return error
    assert!(response.error.is_some());
    let error = response.error.unwrap();
    assert!(error.message.contains("Unknown method"));
}

#[test]
fn test_tools_call_unknown_tool() {
    let (_dir, mut server) = setup_test_server();

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
    let (_dir, mut server) = setup_test_server();

    let request = make_request("tools/call", None);
    let response = server.handle_request(request);

    assert!(response.error.is_some());
    let error = response.error.unwrap();
    assert!(error.message.contains("Missing"));
}

#[test]
fn test_initialized_notification() {
    let (_dir, mut server) = setup_test_server();

    // initialized is a notification, should return null
    let request = make_request("initialized", None);
    let response = server.handle_request(request);

    assert!(response.error.is_none());
    assert_eq!(response.result, Some(Value::Null));
}

#[test]
fn test_response_has_id() {
    let (_dir, mut server) = setup_test_server();

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
    let (dir, mut server) = setup_test_server();

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
    let (_dir, mut server) = setup_test_server();

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
    let (_dir, mut server) = setup_test_server();

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
    let (dir, mut server) = setup_test_server();

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
