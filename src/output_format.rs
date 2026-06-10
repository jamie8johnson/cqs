//! Wire-format selector for JSON output.
//!
//! - **Process-lifetime caching.** [`OutputFormat::current`] reads the env
//!   once via `OnceLock` and memoizes. A daemon serving thousands of
//!   emissions per minute does one syscall per process lifetime, not per
//!   emit. Side benefit: no TOCTOU race — a daemon thread that calls
//!   `set_var` mid-stream can't flip the envelope shape because the cached
//!   value is already pinned.
//!
//! - **Recognized-value parsing + warn on unrecognized values.** The
//!   parser recognizes `v1` / `v2` (case-insensitive, whitespace trimmed)
//!   and logs `tracing::warn!` for any value set but unrecognized so the
//!   operator sees their typo.
//!
//! The lean default wire shape (bare payload on CLI direct, slim
//! `{"data": ...}` on batch/daemon) always emits the security-relevant
//! signals where they matter: leaf serializers emit `trust_level` whenever
//! it is non-default and `injection_flags` whenever non-empty. There is no
//! "force-emit absence" posture knob — absent means default, which any
//! consuming agent handles. `CQS_OUTPUT_FORMAT=v1` restores the full
//! `{data, error, version, _meta}` envelope for consumers that want the
//! wrapped shape.

/// Wire-format selector for CLI direct (`emit_json`) success path.
///
/// The default is [`Self::V2Bare`]: CLI direct success emits the bare JSON
/// payload on stdout — no envelope wrap. The v1 envelope shape is opt-in via
/// `CQS_OUTPUT_FORMAT=v1`; the eval harness pins itself to `v1` via env
/// (see `evals/*.py` os.environ overrides) and a small named compat test
/// set asserts the v1 shape still resolves.
///
/// Batch / daemon JSONL is **not** affected by this — it always uses the
/// slim `{"data": ...}` / `{"error": {...}}` shape (the JSONL contract
/// requires self-describing lines either way).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// v1 envelope shape: CLI direct success emits the full envelope
    /// `{data, error: null, version: 1, _meta: {...}}` to stdout. Selected
    /// by setting `CQS_OUTPUT_FORMAT=v1`. For consumer scripts that want the
    /// wrapped shape.
    V1Envelope,
    /// Default: CLI direct success emits the bare JSON payload to stdout
    /// (no envelope). Selected when `CQS_OUTPUT_FORMAT` is unset or set to
    /// anything other than `v1`.
    V2Bare,
}

/// Process-lifetime cache for the resolved output format. First `current()`
/// call reads env, subsequent calls hit the cache. Avoids a per-emit
/// `std::env::var` syscall and pins the value so a daemon thread can't flip
/// the envelope shape mid-stream via `set_var`.
static OUTPUT_FORMAT: std::sync::OnceLock<OutputFormat> = std::sync::OnceLock::new();

impl OutputFormat {
    /// Resolve once and cache for the process lifetime. Intended to be
    /// called at request entry points (CLI dispatcher, batch dispatcher,
    /// daemon handler).
    ///
    /// Unset env or any value other than `"v1"` (case-insensitive after
    /// trim) ⇒ [`Self::V2Bare`] (bare payload). `CQS_OUTPUT_FORMAT=v1` ⇒
    /// [`Self::V1Envelope`] (v1 envelope).
    pub fn current() -> Self {
        *OUTPUT_FORMAT.get_or_init(Self::resolve_from_env)
    }

    /// Read the env var, parse, and log on first call.
    fn resolve_from_env() -> Self {
        let raw = std::env::var("CQS_OUTPUT_FORMAT").ok();
        let format = Self::resolve_from_str(raw.as_deref());
        tracing::info!(
            format = ?format,
            raw = ?raw.as_deref().unwrap_or("<unset>"),
            "OutputFormat resolved from CQS_OUTPUT_FORMAT"
        );
        format
    }

    /// Pure parsing helper. `v1` (case-insensitive, whitespace trimmed)
    /// ⇒ V1Envelope; `v2` or empty/unset ⇒ V2Bare; unrecognized ⇒
    /// V2Bare + `tracing::warn!`.
    fn resolve_from_str(raw: Option<&str>) -> Self {
        let trimmed = raw.unwrap_or("").trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "v1" => Self::V1Envelope,
            "" | "v2" => Self::V2Bare,
            other => {
                tracing::warn!(
                    raw = %other,
                    "CQS_OUTPUT_FORMAT value not recognized; defaulting to V2Bare. \
                     Recognized values: v1 (legacy envelope), v2 (bare payload)."
                );
                Self::V2Bare
            }
        }
    }

    /// `true` when the bare-payload wire shape should be used on the
    /// CLI direct success path (i.e. [`Self::V2Bare`]).
    pub fn emits_bare_payload(self) -> bool {
        matches!(self, Self::V2Bare)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ───────────────────────────────────────────────────────────────────
    // OutputFormat::resolve_from_str — pure parser tests
    //
    // `current()` uses process-lifetime `OnceLock` caching, so env-mutating
    // tests against it are racy: the first test that runs in the binary
    // wins the cache for the entire process. Pure-function tests on the
    // parser side-step the cache and are deterministic.
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn output_format_resolve_unset_is_v2_bare() {
        assert_eq!(OutputFormat::resolve_from_str(None), OutputFormat::V2Bare);
        assert_eq!(
            OutputFormat::resolve_from_str(Some("")),
            OutputFormat::V2Bare
        );
    }

    #[test]
    fn output_format_resolve_v1_recognized_case_insensitive() {
        // `v1` recognition is case-insensitive and whitespace-trimmed.
        for v in &["v1", "V1", "  v1  ", "\tv1\n", "V1"] {
            assert_eq!(
                OutputFormat::resolve_from_str(Some(v)),
                OutputFormat::V1Envelope,
                "expected v1 alias {v:?} → V1Envelope"
            );
        }
    }

    #[test]
    fn output_format_resolve_v2_yields_bare() {
        for v in &["v2", "V2", " v2 "] {
            assert_eq!(
                OutputFormat::resolve_from_str(Some(v)),
                OutputFormat::V2Bare,
                "expected v2 alias {v:?} → V2Bare"
            );
        }
    }

    #[test]
    fn output_format_resolve_unknown_falls_through_to_v2() {
        // Default is V2Bare. Anything we don't recognize falls through to
        // V2Bare + tracing::warn (so a typo doesn't silently select the
        // v1 envelope shape).
        assert_eq!(
            OutputFormat::resolve_from_str(Some("v3")),
            OutputFormat::V2Bare
        );
        assert_eq!(
            OutputFormat::resolve_from_str(Some("envelope")),
            OutputFormat::V2Bare
        );
        assert_eq!(
            OutputFormat::resolve_from_str(Some("junk")),
            OutputFormat::V2Bare
        );
    }

    #[test]
    fn output_format_emits_bare_payload_under_v2() {
        assert!(OutputFormat::V2Bare.emits_bare_payload());
    }

    #[test]
    fn output_format_skips_bare_under_v1() {
        assert!(!OutputFormat::V1Envelope.emits_bare_payload());
    }
}
