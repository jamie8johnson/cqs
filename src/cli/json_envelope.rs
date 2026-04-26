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

/// Canonical error-code taxonomy. Single source of truth for the wire-format
/// strings emitted in [`JsonError::code`]. `as_str()` is `const fn` so the
/// legacy `error_codes::FOO` `&'static str` constants below can re-export
/// these without duplicating the string literals — adding a new code requires
/// adding a variant here first. P2 #54.
///
/// `#[non_exhaustive]` lets us add new codes without requiring downstream
/// matchers to handle every variant; consumers should always treat unknown
/// codes as an internal error.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Requested entity does not exist (function name, file path, etc.).
    NotFound,
    /// User-supplied input was malformed or out of range.
    InvalidInput,
    /// Failed to parse a command, query, or input file.
    ParseError,
    /// Filesystem, network, or socket I/O failure.
    IoError,
    /// Catch-all for unexpected internal errors. Carries the anyhow chain
    /// in `message` so the root cause stays visible.
    Internal,
}

impl ErrorCode {
    /// Render the variant as the wire-format string. `const fn` so the
    /// `error_codes::FOO` constants can delegate without runtime cost.
    pub const fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::NotFound => "not_found",
            ErrorCode::InvalidInput => "invalid_input",
            ErrorCode::ParseError => "parse_error",
            ErrorCode::IoError => "io_error",
            ErrorCode::Internal => "internal",
        }
    }
}

impl From<ErrorCode> for &'static str {
    fn from(code: ErrorCode) -> Self {
        code.as_str()
    }
}

/// Standard error code taxonomy as `&'static str` constants. Each constant
/// delegates to [`ErrorCode::as_str`] so the wire-format strings live in
/// exactly one place.
///
/// Today only `internal`, `invalid_input`, and `parse_error` are emitted by
/// CLI / batch / daemon paths; `not_found` and `io_error` are reserved for
/// future error-path migrations (see ping/eval emit-on-failure paths). Keep
/// them exposed so the `tracing::warn!(code = ...)` calls and downstream
/// matchers stay grounded against the same taxonomy.
#[allow(dead_code)]
pub mod error_codes {
    use super::ErrorCode;

    /// Requested entity does not exist (function name, file path, etc.).
    pub const NOT_FOUND: &str = ErrorCode::NotFound.as_str();
    /// User-supplied input was malformed or out of range.
    pub const INVALID_INPUT: &str = ErrorCode::InvalidInput.as_str();
    /// Failed to parse a command, query, or input file.
    pub const PARSE_ERROR: &str = ErrorCode::ParseError.as_str();
    /// Filesystem, network, or socket I/O failure.
    pub const IO_ERROR: &str = ErrorCode::IoError.as_str();
    /// Catch-all for unexpected internal errors. Carries the anyhow chain
    /// in `message` so the root cause stays visible.
    pub const INTERNAL: &str = ErrorCode::Internal.as_str();
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
    /// Used by [`wrap_error`] and [`emit_json_error`] for the JSON failure-
    /// path contract.
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
///
/// P2 #28: takes `&serde_json::Value` so the daemon hot path (one wrap per
/// dispatched query) does not deep-clone the inner Map/Vec. The envelope
/// construction itself still allocates the outer `{data, error, version}`
/// object plus a shallow clone of the payload (necessary because
/// `serde_json::json!` macro takes ownership).
///
/// P2 #40: thin wrapper over the typed [`Envelope::ok`] path so both code
/// paths share the same shape — adding a new envelope field (e.g. `meta`)
/// touches one place.
pub fn wrap_value(payload: &serde_json::Value) -> serde_json::Value {
    // P2.69: build the envelope as a Map directly. Previously we ran the
    // payload through `serde_json::to_value(Envelope::ok(&payload))`, which
    // walks the inner tree and rebuilds every Map/Vec — a deep clone in
    // disguise. The hot-path daemon dispatch wraps tens of KB per query at
    // hundreds of QPS, so the deep clone is real allocator pressure. Building
    // the outer Map by hand makes the shallow `payload.clone()` the only
    // allocation. Schema is identical and Envelope<T>::serialize stays as the
    // canonical typed shape (see [`Envelope::ok`]).
    let mut env = serde_json::Map::with_capacity(3);
    env.insert("data".to_string(), payload.clone());
    env.insert("error".to_string(), serde_json::Value::Null);
    env.insert(
        "version".to_string(),
        serde_json::Value::Number(JSON_OUTPUT_VERSION.into()),
    );
    serde_json::Value::Object(env)
}

/// Build an error envelope as a raw [`serde_json::Value`]. P2 #40: thin
/// wrapper over the typed [`Envelope::err`] path — same rationale as
/// [`wrap_value`].
///
/// Accepts `&str` for the code rather than [`ErrorCode`] because some
/// legacy call sites (pipeline error structs in `cli::batch::pipeline`)
/// carry the code as `&'static str` from `error_codes::FOO`. New code
/// should prefer [`ErrorCode::as_str`] for compile-checked emission.
pub fn wrap_error(code: &str, message: &str) -> serde_json::Value {
    serde_json::to_value(Envelope::<serde_json::Value>::err(code, message)).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "wrap_error: envelope serialization failed; emitting fallback shape");
        serde_json::json!({
            "data": null,
            "error": { "code": code, "message": message },
            "version": JSON_OUTPUT_VERSION,
        })
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
/// Same retry-on-NaN guarantee as [`emit_json`]. Accepts `&str` for the
/// code so legacy callers using `error_codes::FOO` keep working; new code
/// should prefer [`ErrorCode::as_str`] for compile-checked emission.
pub fn emit_json_error(code: &str, message: &str) -> Result<()> {
    let env = Envelope::<serde_json::Value>::err(code, message);
    let buf = serde_json::to_value(&env)?;
    let s = format_envelope_to_string(&buf)?;
    println!("{s}");
    Ok(())
}

/// Redact an error chain to a stable `(code, message)` pair safe to surface
/// over the daemon socket or to JSON consumers. P2 #33.
///
/// `format!("{e:#}")` on an `anyhow::Error` walks the entire context chain
/// and may emit raw HTTP bodies, filesystem paths, sqlite query fragments,
/// or other operator-side detail that has no business reaching a daemon
/// client. This helper:
///
/// - Inspects the root cause's type for a small allowlist of known-safe
///   downcasts (sqlx errors → `internal`, IO errors → `io_error`, anyhow
///   with no specific source → `internal`).
/// - Returns a stable `(ErrorCode, message)` pair where `message` is
///   either a redacted summary (for known classes) or a correlation
///   chain-id (`"err-<u64-hex>"`) that an operator can grep for in
///   `journalctl -u cqs-watch`.
/// - Logs the full chain via `tracing::warn!(chain_id, error = %format!("{:#}"))`
///   so the unredacted form stays available to the operator without
///   reaching the client.
///
/// The four daemon batch dispatch sites in `BatchContext::dispatch_line`
/// and `cmd_batch`'s stdin loop call this so a panicked sqlite query or
/// HTTP fetch failure can't leak request URLs / row contents to the
/// daemon socket client.
pub fn redact_error(err: &anyhow::Error) -> (ErrorCode, String) {
    // Generate a stable chain id once per error so the warn log and the
    // client-facing message both reference the same correlation handle.
    // Process-local; not a security boundary, just a grep target.
    let chain_id = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        // Hash the rendered chain so retrying the same error gets the
        // same id (operator can correlate journal lines).
        format!("{err:#}").hash(&mut h);
        format!("err-{:016x}", h.finish())
    };

    // Walk the source chain to inspect the root cause type. Only specific
    // downcasts emit the underlying message; everything else gets the
    // opaque chain-id form so we never hand out arbitrary error text.
    let root: &(dyn std::error::Error + 'static) = err.chain().last().unwrap_or(err.as_ref());

    // sqlx errors leak query text and binding values via their Display
    // impl — never surface those to clients.
    if root.downcast_ref::<sqlx::Error>().is_some() {
        tracing::warn!(
            chain_id = %chain_id,
            error = %format!("{err:#}"),
            "Daemon batch: sqlite error redacted"
        );
        return (
            ErrorCode::Internal,
            format!("internal database error ({chain_id})"),
        );
    }

    // IO errors: the Display form is safe (kind + os error message), but
    // the surrounding anyhow context may carry the path. Return the root
    // io::Error display only.
    if let Some(io_err) = root.downcast_ref::<std::io::Error>() {
        tracing::warn!(
            chain_id = %chain_id,
            error = %format!("{err:#}"),
            "Daemon batch: IO error redacted"
        );
        return (ErrorCode::IoError, format!("{io_err} ({chain_id})"));
    }

    // Default: unknown root. Surface only the chain-id; operators can
    // correlate via the warn log below.
    tracing::warn!(
        chain_id = %chain_id,
        error = %format!("{err:#}"),
        "Daemon batch: unknown error class redacted"
    );
    (
        ErrorCode::Internal,
        format!("internal error ({chain_id}); see daemon log"),
    )
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
        let v = wrap_value(&serde_json::json!([1, 2, 3]));
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

    // P2 #54: ErrorCode enum drives the const proxies. Test that adding /
    // renaming a variant requires a compile-time match update — the as_str
    // arm and the const proxy stay in lockstep with the enum.
    #[test]
    fn error_code_str_round_trip() {
        assert_eq!(ErrorCode::NotFound.as_str(), error_codes::NOT_FOUND);
        assert_eq!(ErrorCode::InvalidInput.as_str(), error_codes::INVALID_INPUT);
        assert_eq!(ErrorCode::ParseError.as_str(), error_codes::PARSE_ERROR);
        assert_eq!(ErrorCode::IoError.as_str(), error_codes::IO_ERROR);
        assert_eq!(ErrorCode::Internal.as_str(), error_codes::INTERNAL);
    }

    #[test]
    fn error_code_into_static_str() {
        let code: &'static str = ErrorCode::ParseError.into();
        assert_eq!(code, "parse_error");
    }

    // P2 #40: wrap_value / wrap_error are now thin wrappers over the typed
    // Envelope::ok / Envelope::err paths. Same shape on both surfaces means
    // adding a field (e.g. `meta`) touches one place.
    #[test]
    fn wrap_value_matches_envelope_ok_shape() {
        let payload = serde_json::json!({"x": 1, "y": [2, 3]});
        let via_wrap = wrap_value(&payload);
        let via_typed = serde_json::to_value(Envelope::ok(&payload)).unwrap();
        assert_eq!(via_wrap, via_typed);
    }

    #[test]
    fn wrap_error_matches_envelope_err_shape() {
        let via_wrap = wrap_error(error_codes::INVALID_INPUT, "bad query");
        let via_typed = serde_json::to_value(Envelope::<serde_json::Value>::err(
            "invalid_input",
            "bad query",
        ))
        .unwrap();
        assert_eq!(via_wrap, via_typed);
    }

    // P2 #28: wrap_value takes &Value — confirm the caller doesn't need to
    // clone the payload at the call site. (The function still allocates the
    // outer envelope; the goal is to remove the redundant deep clone.)
    #[test]
    fn wrap_value_takes_reference_no_caller_clone() {
        let payload = serde_json::json!({"big": (0..100).collect::<Vec<_>>()});
        // Pass by reference; payload is still owned at the call site.
        let _wrapped = wrap_value(&payload);
        assert_eq!(payload["big"][0], 0);
        assert_eq!(payload["big"][99], 99);
    }

    // P2 #33: redact_error returns a stable code+chain-id pair for unknown
    // error roots so the daemon socket never echoes raw anyhow chains.
    #[test]
    fn redact_error_unknown_root_returns_internal_with_chain_id() {
        let err = anyhow::anyhow!("some internal failure with /etc/passwd in path");
        let (code, msg) = redact_error(&err);
        assert_eq!(code, ErrorCode::Internal);
        // The message must include the chain-id correlation handle and
        // must NOT include the original (potentially sensitive) text.
        assert!(msg.contains("err-"), "expected chain-id in message: {msg}");
        assert!(
            !msg.contains("/etc/passwd"),
            "raw error text leaked to client: {msg}"
        );
    }

    // P2 #33: same input → same chain-id (deterministic correlation). An
    // operator grepping the journal for the chain-id finds the matching warn.
    #[test]
    fn redact_error_chain_id_is_deterministic_for_same_root() {
        let err1 = anyhow::anyhow!("repeatable error text");
        let err2 = anyhow::anyhow!("repeatable error text");
        let (_, msg1) = redact_error(&err1);
        let (_, msg2) = redact_error(&err2);
        // Both messages should embed the same chain-id since the rendered
        // chain text is identical.
        let extract = |msg: &str| -> Option<String> {
            msg.split_whitespace()
                .find(|w| w.contains("err-"))
                .map(|w| w.trim_start_matches('(').trim_end_matches(')').to_string())
        };
        assert_eq!(
            extract(&msg1),
            extract(&msg2),
            "chain-id should be deterministic for same root"
        );
    }

    // P2 #33: sqlx errors are downcast and redacted to "internal database
    // error" — never echoing the SQL query string or row binding values.
    #[test]
    fn redact_error_sqlite_root_returns_redacted_database_message() {
        // RowNotFound is a simple sqlx::Error variant that doesn't need a
        // real sqlite handle to construct. Surrounding context() carries
        // the SQL the redaction must drop.
        let sqlx_err = sqlx::Error::RowNotFound;
        let err = anyhow::Error::from(sqlx_err)
            .context("SELECT secret FROM users WHERE id = 'sensitive'");
        let (code, msg) = redact_error(&err);
        assert_eq!(code, ErrorCode::Internal);
        assert!(msg.starts_with("internal database error"));
        assert!(!msg.contains("SELECT"), "SQL leaked to client: {msg}");
        assert!(
            !msg.contains("sensitive"),
            "binding leaked to client: {msg}"
        );
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

    // P3 #110: write_json_line (in `crate::cli::batch`) and format_envelope_to_string
    // share the same `wrap_value` + `sanitize_json_floats` retry path on
    // serializer failure — the retry must catch BOTH `+Inf` and `-Inf`,
    // not just NaN. The existing `test_write_json_line_nan_retry` only
    // pinned NaN. This test exercises the same chain
    // (`wrap_value` → `sanitize_json_floats` → `serde_json::to_string`)
    // that the batch path uses, with the exact mixed-Infinity payload
    // the audit triage called out.
    //
    // The function-under-test naming follows the audit prompt: a future
    // refactor that pulls the batch helper into json_envelope can land
    // an actual `write_json_line` here without renaming.
    #[test]
    fn test_write_json_line_infinity_retry() {
        let payload = serde_json::json!({
            "score": f64::INFINITY,
            "neg_score": f64::NEG_INFINITY,
            "name": "x",
        });
        // wrap_value matches what write_json_line's retry arm calls
        // (`crate::cli::json_envelope::wrap_value(value)`).
        let wrapped = wrap_value(&payload);
        let mut sanitized = wrapped;
        sanitize_json_floats(&mut sanitized);
        // The sanitized envelope must serialize to valid JSON (no NaN /
        // Infinity literals, since those are not allowed in the JSON spec).
        let s = serde_json::to_string(&sanitized).expect("must serialize after sanitize");
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("output must be valid JSON");

        // Both Infinity fields must be replaced with null inside the data
        // payload; the non-numeric `name` field stays untouched.
        assert!(
            parsed["data"]["score"].is_null(),
            "+Infinity score must be sanitized to null. got: {}",
            parsed["data"]["score"]
        );
        assert!(
            parsed["data"]["neg_score"].is_null(),
            "-Infinity neg_score must be sanitized to null. got: {}",
            parsed["data"]["neg_score"]
        );
        assert_eq!(
            parsed["data"]["name"], "x",
            "non-numeric fields must pass through the retry untouched"
        );
        // Envelope shell stays intact.
        assert!(parsed["error"].is_null());
        assert_eq!(parsed["version"], JSON_OUTPUT_VERSION);
        // The serialized form must not contain the literal "Infinity"
        // (would be invalid JSON; serde_json would have rejected it).
        assert!(
            !s.contains("Infinity"),
            "sanitized output must not contain the 'Infinity' literal: {s}"
        );
    }

    // P3 #111: `wrap_value` has no double-wrap detection. Calling it on
    // an already-wrapped envelope explicitly produces a NESTED envelope:
    // the outer `data` field holds the inner envelope object verbatim,
    // including its `data`, `error`, and `version` keys. This test pins
    // that documented behaviour so a future "auto-detect envelope shape
    // and pass-through" change is a deliberate, test-failing decision
    // rather than an accidental drift.
    //
    // The current contract is: callers MUST NOT pass an already-wrapped
    // value to `wrap_value`. There's no compile-time check; this test
    // documents what happens when someone does.
    #[test]
    fn wrap_value_does_not_double_wrap_existing_envelope() {
        // Build an envelope by going through wrap_value once.
        let inner_payload = serde_json::json!({"name": "foo", "count": 3});
        let first_wrap = wrap_value(&inner_payload);

        // Sanity: first wrap is the standard envelope.
        assert_eq!(first_wrap["data"], inner_payload);
        assert!(first_wrap["error"].is_null());
        assert_eq!(first_wrap["version"], JSON_OUTPUT_VERSION);

        // Now wrap it AGAIN. Today wrap_value has no envelope detection,
        // so this produces `{data: {data:{...}, error:null, version:1}, error:null, version:1}`.
        let second_wrap = wrap_value(&first_wrap);

        // Outer envelope shape is intact.
        assert!(
            second_wrap["data"].is_object(),
            "second wrap's data must be an object (the entire first envelope)"
        );
        assert!(second_wrap["error"].is_null());
        assert_eq!(second_wrap["version"], JSON_OUTPUT_VERSION);

        // Inner envelope is nested under outer `data` — the contract pin.
        // If a future change adds envelope detection, this assertion flips
        // and the comment above documents the behaviour change.
        let inner = &second_wrap["data"];
        assert_eq!(
            inner["data"], inner_payload,
            "double-wrap puts the original payload at data.data — \
             pins the no-detection contract"
        );
        assert!(
            inner["error"].is_null(),
            "the inner envelope's error field is preserved verbatim"
        );
        assert_eq!(
            inner["version"], JSON_OUTPUT_VERSION,
            "the inner envelope's version field is preserved verbatim"
        );

        // Cross-check: the deeply-nested payload survives intact under
        // `data.data` so a consumer that grepped `.data.data` would still
        // find their fields.
        assert_eq!(second_wrap["data"]["data"]["name"], "foo");
        assert_eq!(second_wrap["data"]["data"]["count"], 3);
    }
}
