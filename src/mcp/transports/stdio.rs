//! Stdio transport for MCP server
//!
//! Reads JSON-RPC requests from stdin and writes responses to stdout.
//! Used by Claude Code for direct integration.

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::Result;

use super::super::server::McpServer;
use super::super::types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

/// Run the MCP server with stdio transport
///
/// Reads JSON-RPC requests from stdin and writes responses to stdout.
/// Used by Claude Code for direct integration.
///
/// # Arguments
/// * `project_root` - Root directory of the project to index
/// * `use_gpu` - Whether to use GPU acceleration for embeddings
pub fn serve_stdio(project_root: impl AsRef<Path>, use_gpu: bool) -> Result<()> {
    let server = McpServer::new(project_root, use_gpu)?;

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;

        if line.trim().is_empty() {
            continue;
        }

        // SAFETY: Stdio transport is from trusted client (Claude Code)
        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let error_response = JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {}", e),
                        data: None,
                    }),
                };
                let response_json = serde_json::to_string(&error_response)?;
                writeln!(stdout, "{}", response_json)?;
                stdout.flush()?;
                continue;
            }
        };

        let response = server.handle_request(request);

        // Skip response for notifications (no id)
        if response.id.is_none()
            && response
                .result
                .as_ref()
                .map(|v| v.is_null())
                .unwrap_or(false)
        {
            continue;
        }

        let response_json = serde_json::to_string(&response)?;
        writeln!(stdout, "{}", response_json)?;
        stdout.flush()?;
    }

    Ok(())
}
