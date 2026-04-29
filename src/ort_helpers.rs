//! Shared ORT error mapping for the embedder, reranker, and SPLADE backends.
//!
//! Three near-identical `fn ort_err` helpers used to live in
//! [`crate::embedder::provider`], [`crate::reranker`], and
//! [`crate::splade`]; each wrapped `ort::Error` into the calling module's
//! `*Error::Inference{,Failed}` variant via `.to_string()`. Audit finding
//! CQ-V1.30.1-5 (P3-CQ-2): consolidated here as a single helper backed by
//! a private trait.
//!
//! The trait approach (rather than `E: From<String>`) avoids the
//! reflexive `From<T> for T` ambiguity that breaks `.map_err(ort_err)`
//! type inference: a generic `ort_err<E: From<String>>(e: ...)` can't
//! decide whether the closure's output should be `String` or the
//! enclosing `*Error` — both satisfy `From<String>`. A bespoke trait
//! has only the three target impls, so inference is unambiguous from
//! the call site's surrounding `Result<_, *Error>` return type.

/// Implemented by error types that can carry a stringified ORT error
/// in their `Inference{,Failed}` variant. Sealed inside the crate;
/// adding a new ORT-using error type means one new impl in its module.
pub(crate) trait FromOrtMessage {
    fn from_ort_message(msg: String) -> Self;
}

/// Convert any ORT error into the caller's error type via `.to_string()`.
///
/// Accepts anything `Display` so `ort::Error`, `ort::Error<T>`, or any
/// other ort error wrapper resolve through the same call. The
/// surrounding `Result<_, *Error>` return type is enough for the
/// compiler to pick the right `FromOrtMessage` impl when used as
/// `.map_err(ort_err)`.
pub(crate) fn ort_err<E>(e: impl std::fmt::Display) -> E
where
    E: FromOrtMessage,
{
    E::from_ort_message(e.to_string())
}
