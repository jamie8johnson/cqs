//! MCP (Model Context Protocol) server implementation
//!
//! # Security
//!
//! JSON deserialization from untrusted input is bounded by:
//! - HTTP transport: 1MB request body limit (RequestBodyLimitLayer)
//! - Stdio transport: trusted client (Claude Code) with reasonable message sizes

mod audit_mode;
mod server;
mod tools;
mod transports;
mod types;
mod validation;

// Public API
pub use server::McpServer;
pub use transports::{serve_http, serve_stdio};
pub use validation::parse_duration;
// Types kept pub(crate) for integration tests; not part of public API
pub use types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

#[cfg(test)]
mod tests {
    mod fuzz {
        use super::super::types::JsonRpcRequest;
        use proptest::prelude::*;

        proptest! {
            /// Fuzz: JsonRpcRequest parsing should never panic
            #[test]
            fn fuzz_jsonrpc_parse_no_panic(input in "\\PC{0,1000}") {
                let _ = serde_json::from_str::<JsonRpcRequest>(&input);
            }

            /// Fuzz: JsonRpcRequest with JSON-like structure
            #[test]
            fn fuzz_jsonrpc_structured(
                jsonrpc in "(1\\.0|2\\.0|[0-9]\\.[0-9])",
                id in prop::option::of(0i64..1000),
                method in "[a-z/_]{1,30}",
            ) {
                let json = match id {
                    Some(id) => format!(
                        r#"{{"jsonrpc":"{}","id":{},"method":"{}"}}"#,
                        jsonrpc, id, method
                    ),
                    None => format!(
                        r#"{{"jsonrpc":"{}","method":"{}"}}"#,
                        jsonrpc, method
                    ),
                };
                let _ = serde_json::from_str::<JsonRpcRequest>(&json);
            }
        }
    }
}
