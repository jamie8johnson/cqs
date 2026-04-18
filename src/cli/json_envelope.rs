//! Uniform JSON output envelope for all CLI and batch commands.
//!
//! Every JSON-emitting command wraps its payload in:
//! ```json
//! {"data": <payload>, "error": null, "version": 1}
//! ```
//! On failure:
//! ```json
//! {"data": null, "error": {"code": "...", "message": "..."}, "version": 1}
//! ```
//!
//! Agents parse one shape across all commands instead of per-command logic.
//! Bump [`JSON_OUTPUT_VERSION`] on any breaking schema change to the inner
//! `data` payloads (the envelope itself stays stable).
//!
//! ## Surfaces
//!
//! - **CLI**: handlers call [`emit_json`] (pretty-printed) instead of
//!   `println!("{}", serde_json::to_string_pretty(&out)?)`.
//! - **Batch / daemon socket**: [`crate::cli::batch::write_json_line`] wraps
//!   each handler's `serde_json::Value` via [`wrap_value`] / [`wrap_error`]
//!   before serializing one JSONL record.
//! - **Daemon socket transport**: the outer `{status, output}` framing in
//!   `watch.rs` is orthogonal — `output` carries this envelope as a string.

use anyhow::Result;
use serde::Serialize;

/// Wire-format version. Bump on any breaking change to the `data` payload
/// shapes for any command. The envelope structure itself (data/error/version
/// keys) is stable across versions.
pub const JSON_OUTPUT_VERSION: u32 = 1;

/// Standard error code taxonomy. Keep small; refine as new categories prove
/// necessary. Map anyhow chains to [`error_codes::INTERNAL`] by default.
///
/// `NOT_FOUND` and `IO_ERROR` are part of the published taxonomy but no CLI
/// site uses them yet (anyhow errors flow through main → stderr as text);
/// the `#[allow(dead_code)]` keeps them reachable for future error-path
/// migrations without warning noise.
#[allow(dead_code)]
pub mod error_codes {
    /// Requested entity does not exist (function name, file path, etc.).
    pub const NOT_FOUND: &str = "not_found";
    /// User-supplied input was malformed or out of range.
    pub const INVALID_INPUT: &str = "invalid_input";
    /// Failed to parse a command, query, or input file.
    pub const PARSE_ERROR: &str = "parse_error";
    /// Filesystem, network, or socket I/O failure.
    pub const IO_ERROR: &str = "io_error";
    /// Catch-all for unexpected internal errors. Carries the anyhow chain
    /// in `message` so the root cause stays visible.
    pub const INTERNAL: &str = "internal";
}

/// Standard envelope for all JSON-emitting commands.
#[derive(Debug, Serialize)]
pub struct Envelope<T: Serialize> {
    pub data: Option<T>,
    pub error: Option<JsonError>,
    pub version: u32,
}

/// Structured error reported in the `error` field of an [`Envelope`].
#[derive(Debug, Serialize)]
pub struct JsonError {
    pub code: String,
    pub message: String,
}

impl<T: Serialize> Envelope<T> {
    /// Build a success envelope wrapping `data`.
    pub fn ok(data: T) -> Self {
        Self {
            data: Some(data),
            error: None,
            version: JSON_OUTPUT_VERSION,
        }
    }
}

impl Envelope<serde_json::Value> {
    /// Build an error envelope. `Envelope<serde_json::Value>` is the canonical
    /// type for errors so the caller doesn't need to name a phantom data type.
    /// Used by tests today; will be the entry point once CLI error paths route
    /// through the envelope (the `#[allow(dead_code)]` covers the gap).
    #[allow(dead_code)]
    pub fn err(code: &str, message: impl Into<String>) -> Self {
        Self {
            data: None,
            error: Some(JsonError {
                code: code.to_string(),
                message: message.into(),
            }),
            version: JSON_OUTPUT_VERSION,
        }
    }
}

/// Wrap a raw [`serde_json::Value`] payload in the standard envelope.
/// Used by batch and pipeline paths that already work with untyped values.
pub fn wrap_value(payload: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "data": payload,
        "error": null,
        "version": JSON_OUTPUT_VERSION,
    })
}

/// Build an error envelope as a raw [`serde_json::Value`].
pub fn wrap_error(code: &str, message: &str) -> serde_json::Value {
    serde_json::json!({
        "data": null,
        "error": { "code": code, "message": message },
        "version": JSON_OUTPUT_VERSION,
    })
}

/// Print a typed value as pretty-printed JSON wrapped in the standard envelope.
/// Drop-in replacement for `println!("{}", serde_json::to_string_pretty(&v)?)`.
pub fn emit_json<T: Serialize>(value: &T) -> Result<()> {
    let env = Envelope::ok(value);
    println!("{}", serde_json::to_string_pretty(&env)?);
    Ok(())
}

/// Print an error envelope as pretty-printed JSON. CLI sites will use this
/// once JSON error paths replace today's `anyhow → stderr text` flow.
#[allow(dead_code)]
pub fn emit_json_error(code: &str, message: &str) -> Result<()> {
    let env = Envelope::<serde_json::Value>::err(code, message);
    println!("{}", serde_json::to_string_pretty(&env)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_envelope_shape() {
        let env = Envelope::ok(serde_json::json!({"foo": 1}));
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["data"]["foo"], 1);
        assert!(v["error"].is_null());
        assert_eq!(v["version"], JSON_OUTPUT_VERSION);
    }

    #[test]
    fn err_envelope_shape() {
        let env = Envelope::<serde_json::Value>::err(error_codes::NOT_FOUND, "missing");
        let v = serde_json::to_value(&env).unwrap();
        assert!(v["data"].is_null());
        assert_eq!(v["error"]["code"], "not_found");
        assert_eq!(v["error"]["message"], "missing");
        assert_eq!(v["version"], JSON_OUTPUT_VERSION);
    }

    #[test]
    fn wrap_value_shape() {
        let v = wrap_value(serde_json::json!([1, 2, 3]));
        assert_eq!(v["data"], serde_json::json!([1, 2, 3]));
        assert!(v["error"].is_null());
        assert_eq!(v["version"], JSON_OUTPUT_VERSION);
    }

    #[test]
    fn wrap_error_shape() {
        let v = wrap_error(error_codes::PARSE_ERROR, "bad token");
        assert!(v["data"].is_null());
        assert_eq!(v["error"]["code"], "parse_error");
        assert_eq!(v["error"]["message"], "bad token");
    }
}
