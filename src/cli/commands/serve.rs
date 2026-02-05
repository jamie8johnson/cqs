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
    pub api_key_file: Option<PathBuf>,
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

    // Resolve API key from either --api-key or --api-key-file
    let api_key = match (&config.api_key, &config.api_key_file) {
        (Some(_), Some(_)) => {
            bail!("Cannot specify both --api-key and --api-key-file");
        }
        (Some(key), None) => Some(key.clone()),
        (None, Some(path)) => {
            let key = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("Failed to read API key file: {}", e))?;
            Some(key.trim().to_string())
        }
        (None, None) => None,
    };

    // Require API key for non-localhost HTTP binds
    if !is_localhost && config.transport == "http" && api_key.is_none() {
        bail!(
            "API key required for non-localhost HTTP bind.\n\
             Set --api-key <key>, --api-key-file <path>, or CQS_API_KEY environment variable."
        );
    }

    let root = config.project.unwrap_or_else(find_project_root);

    match config.transport.as_str() {
        "stdio" => cqs::serve_stdio(root, config.gpu),
        "http" => cqs::serve_http(root, &config.bind, config.port, api_key, config.gpu),
        // Keep sse as alias for backwards compatibility
        "sse" => cqs::serve_http(root, &config.bind, config.port, api_key, config.gpu),
        _ => {
            bail!(
                "Unknown transport: {}. Use 'stdio' or 'http'.",
                config.transport
            );
        }
    }
}
