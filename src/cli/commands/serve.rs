//! Serve command for cqs
//!
//! Starts the MCP server for IDE integration.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::cli::find_project_root;

/// Configuration for the MCP server
pub(crate) struct ServeConfig {
    pub transport: String,
    pub bind: String,
    pub port: u16,
    pub project: Option<PathBuf>,
    pub gpu: bool,
    pub api_key: Option<String>,
    pub dangerously_allow_network_bind: bool,
}

/// Start the MCP server for IDE integration
pub(crate) fn cmd_serve(config: ServeConfig) -> Result<()> {
    // Block non-localhost bind unless explicitly allowed
    let is_localhost =
        config.bind == "127.0.0.1" || config.bind == "localhost" || config.bind == "::1";
    if !is_localhost && !config.dangerously_allow_network_bind {
        bail!(
            "Binding to '{}' would expose your codebase to the network.\n\
             If this is intentional, add --dangerously-allow-network-bind",
            config.bind
        );
    }

    // Require API key for non-localhost HTTP binds
    if !is_localhost && config.transport == "http" && config.api_key.is_none() {
        bail!(
            "API key required for non-localhost HTTP bind.\n\
             Set --api-key <key> or CQS_API_KEY environment variable."
        );
    }

    let root = config.project.unwrap_or_else(find_project_root);

    match config.transport.as_str() {
        "stdio" => cqs::serve_stdio(root, config.gpu),
        "http" => cqs::serve_http(root, &config.bind, config.port, config.api_key, config.gpu),
        // Keep sse as alias for backwards compatibility
        "sse" => cqs::serve_http(root, &config.bind, config.port, config.api_key, config.gpu),
        _ => {
            bail!(
                "Unknown transport: {}. Use 'stdio' or 'http'.",
                config.transport
            );
        }
    }
}
