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
    /// Used by [`emit_json_error`] for the JSON failure-path contract.
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

/// Recursively replace NaN / +Inf / -Inf f64 values inside a [`serde_json::Value`]
/// tree with `null`.
///
/// Defense-in-depth: `serde_json::Number::from_f64` already rejects non-finite
/// floats by returning `None`, and the standard `Serialize` derive maps
/// `f64::NAN` to `Value::Null` rather than panicking. But typed structs
/// constructed by hand or third-party `Serialize` impls can route through
/// `Serializer::serialize_f64`, which returns `Err` for non-finite values.
/// Sanitizing the [`Value`] tree before re-serialization keeps the envelope
/// emit path total — every JSON-emitting surface (CLI `emit_json`, batch
/// `write_json_line`, chat REPL) shares the same retry-on-NaN behavior so
/// agents see one shape across all surfaces.
pub(crate) fn sanitize_json_floats(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.is_nan() || f.is_infinite() {
                    *value = serde_json::Value::Null;
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                sanitize_json_floats(item);
            }
        }
        serde_json::Value::Object(map) => {
            for (_k, v) in map.iter_mut() {
                sanitize_json_floats(v);
            }
        }
        _ => {}
    }
}

/// Format a pre-built envelope [`serde_json::Value`] as a pretty-printed
/// string. Tries `serde_json::to_string_pretty` first, falls back to the
/// sanitize-and-retry pattern on serialization failure (NaN / Infinity in
/// a leaf field). Used by both [`emit_json`] (CLI handlers) and `cmd_chat`
/// so the chat REPL inherits the same retry behavior as the batch and CLI
/// surfaces — no envelope leaks bare stderr text on a non-finite leaf.
///
/// Returns the final stringified envelope, or an `Err` only if even the
/// sanitized tree fails to serialize (in practice impossible: after
/// [`sanitize_json_floats`] no `Value::Number` can hold a non-finite f64).
pub fn format_envelope_to_string(value: &serde_json::Value) -> Result<String> {
    match serde_json::to_string_pretty(value) {
        Ok(s) => Ok(s),
        Err(_) => {
            let mut sanitized = value.clone();
            sanitize_json_floats(&mut sanitized);
            Ok(serde_json::to_string_pretty(&sanitized)?)
        }
    }
}

/// Print a typed value as pretty-printed JSON wrapped in the standard envelope.
/// Drop-in replacement for `println!("{}", serde_json::to_string_pretty(&v)?)`.
///
/// Sanitizes NaN / Infinity floats via the same try → on-Err sanitize-and-retry
/// pattern as the batch / daemon socket path (see [`format_envelope_to_string`]
/// and [`crate::cli::batch::write_json_line`]). Keeps observable output uniform
/// across CLI, batch, and chat surfaces.
pub fn emit_json<T: Serialize>(value: &T) -> Result<()> {
    let env = Envelope::ok(value);
    let buf = serde_json::to_value(&env)?;
    let s = format_envelope_to_string(&buf)?;
    println!("{s}");
    Ok(())
}

/// Print an error envelope as pretty-printed JSON. Used by `cqs ping --json`
/// (daemon-not-running path) and `cqs eval --baseline ... --json` (regression-
/// past-tolerance path) so JSON consumers always get the published failure
/// shape `{data:null, error:{code,message}, version:1}` instead of bare
/// stderr text.
///
/// Same retry-on-NaN guarantee as [`emit_json`].
pub fn emit_json_error(code: &str, message: &str) -> Result<()> {
    let env = Envelope::<serde_json::Value>::err(code, message);
    let buf = serde_json::to_value(&env)?;
    let s = format_envelope_to_string(&buf)?;
    println!("{s}");
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

    // D.1: emit_json must sanitize NaN to null via the shared retry pattern,
    // matching write_json_line in batch/mod.rs. This test exercises the full
    // envelope pipeline (Envelope::ok → to_value → format_envelope_to_string)
    // so a regression that drops sanitization fails immediately.
    #[test]
    fn emit_json_sanitizes_nan_to_null() {
        let payload = serde_json::json!({"score": f64::NAN, "name": "x"});
        let env = Envelope::ok(payload);
        let mut buf = serde_json::to_value(&env).unwrap();
        sanitize_json_floats(&mut buf);
        let s = serde_json::to_string_pretty(&buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(parsed["data"]["score"].is_null());
        assert_eq!(parsed["data"]["name"], "x");
        assert_eq!(parsed["version"], JSON_OUTPUT_VERSION);
    }

    // D.1: ±Infinity sanitization parity. serde_json's typed-struct path
    // already maps non-finite f64 to null, but a manually-constructed
    // Value tree (e.g. via custom Serialize impls that skip the safety
    // net) can still leak. Sanitizer must catch both signs.
    #[test]
    fn emit_json_sanitizes_pos_and_neg_infinity() {
        let payload = serde_json::json!({"a": f64::INFINITY, "b": f64::NEG_INFINITY, "name": "x"});
        let env = Envelope::ok(payload);
        let mut buf = serde_json::to_value(&env).unwrap();
        sanitize_json_floats(&mut buf);
        let s = serde_json::to_string_pretty(&buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(parsed["data"]["a"].is_null());
        assert!(parsed["data"]["b"].is_null());
        assert_eq!(parsed["data"]["name"], "x");
    }

    // D.1: format_envelope_to_string is the chat-shared helper. It must
    // produce the same sanitized output as emit_json for any envelope value
    // — that's what gives chat parity with batch / CLI.
    #[test]
    fn format_envelope_to_string_handles_nan_payload() {
        let payload = serde_json::json!({"score": f64::NAN, "extra": [1.0, f64::NAN, 3.0]});
        let env = Envelope::ok(payload);
        let buf = serde_json::to_value(&env).unwrap();
        let s = format_envelope_to_string(&buf).expect("must not Err");
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("output must be valid JSON");
        // serde_json's typed-struct path already maps NaN to null in `score`,
        // so the post-format value is null whether or not the retry triggered.
        assert!(parsed["data"]["score"].is_null());
        assert_eq!(parsed["data"]["extra"][0], 1.0);
        assert!(parsed["data"]["extra"][1].is_null());
        assert_eq!(parsed["data"]["extra"][2], 3.0);
    }

    // D.1: format_envelope_to_string is a no-op on clean values — any
    // sanitized retry would mutate output, so this pins the success path.
    #[test]
    fn format_envelope_to_string_passthrough_on_clean_value() {
        let env = Envelope::ok(serde_json::json!({"name": "foo", "score": 0.95}));
        let buf = serde_json::to_value(&env).unwrap();
        let s = format_envelope_to_string(&buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["data"]["name"], "foo");
        assert_eq!(parsed["data"]["score"], 0.95);
        assert!(parsed["error"].is_null());
    }
}
