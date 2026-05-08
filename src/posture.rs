//! Caller-decided emission posture for JSON output.
//!
//! Lives in the lib crate so leaf serializers (e.g.
//! `crate::store::helpers::types::SearchResult::build_chunk_json_inner`)
//! can take a [`Posture`] parameter without depending on the bin
//! (`cqs::cli::json_envelope`) layer. The bin's `cli::json_envelope`
//! re-exports this same type for convenience.
//!
//! See `docs/json-snr-restoration.md` for the migration plan.

/// Caller-decided emission posture, threaded from request entry points
/// down to leaf serializers. Replaces ad-hoc `std::env::var` reads in
/// leaf functions with a parameter so:
/// - leaf serializers stay process-state-independent (deterministic in
///   tests, no surprise behavior under env mutation),
/// - the env var is read **once** per request at the dispatcher layer
///   instead of N times per emitted result, and
/// - the verbosity contract becomes a typed value the compiler tracks
///   instead of a string-keyed env-var lookup.
///
/// `Friendly` (default) emits the lean wire shape: `_meta.handling_advice`
/// is omitted, per-result advisory fields skip-when-default. `Adversarial`
/// (set via `CQS_ULTRASECURITY=1` at process start) restores the full
/// verbose envelope expected by adversarial-deployment consumers (cqs as
/// a remote MCP server reading user-uploaded code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// Lean wire shape — `handling_advice` omitted, security signals
    /// skip-when-default. Default for friendly-deployment agents.
    Friendly,
    /// Verbose wire shape — full envelope with advisory + force-emitted
    /// security signals. Selected via `CQS_ULTRASECURITY=1`.
    Adversarial,
}

impl Posture {
    /// Read the env var once and return the corresponding posture.
    /// Cheap (one syscall); intended to be called at request entry
    /// points (CLI dispatcher, batch dispatcher, daemon handler) so the
    /// posture flows through the request as a typed value.
    pub fn current() -> Self {
        if std::env::var("CQS_ULTRASECURITY").as_deref() == Ok("1") {
            Self::Adversarial
        } else {
            Self::Friendly
        }
    }

    /// `true` when the verbose envelope should be emitted (force-emit
    /// security signals, include `_meta.handling_advice`, etc.).
    pub fn is_adversarial(self) -> bool {
        matches!(self, Self::Adversarial)
    }
}

/// Wire-format selector for CLI direct (`emit_json`) success path.
///
/// SNR Phase 4 ships this as opt-in via `CQS_OUTPUT_FORMAT=v2`; the
/// default stays `V1Envelope` to keep the existing 21+ integration
/// test files and 50+ eval harness Python scripts unmodified. A
/// future release flips the default to `V2Bare` once consumer
/// migration is complete (deferred work; tracked in
/// `docs/json-snr-restoration.md` Phase 4 pickup notes).
///
/// **Posture interaction:** [`Posture::Adversarial`] overrides this —
/// the verbose envelope wins regardless of `OutputFormat`. The two
/// env vars compose: `CQS_ULTRASECURITY=1` ⇒ full envelope on every
/// surface; `CQS_OUTPUT_FORMAT=v2` AND not adversarial ⇒ bare
/// payload on the CLI direct success path; otherwise current envelope
/// behavior.
///
/// Batch / daemon JSONL is **not** affected by this — Phase 3 already
/// shipped the slim `{"data": ...}` / `{"error": {...}}` shape there
/// and the JSONL contract requires self-describing lines either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Current behavior: CLI direct success emits the full envelope
    /// `{data, error: null, version: 1, _meta: {...}}` to stdout.
    /// Default; selected when `CQS_OUTPUT_FORMAT` is unset or any
    /// value other than `v2`.
    V1Envelope,
    /// SNR Phase 4 target shape: CLI direct success emits the bare
    /// JSON payload to stdout (no envelope). Failure path emits a
    /// structured error to stderr + non-zero exit. Selected via
    /// `CQS_OUTPUT_FORMAT=v2`.
    V2Bare,
}

impl OutputFormat {
    /// Read the env var once and return the corresponding format.
    /// Same one-syscall cost as [`Posture::current`]; intended to be
    /// called at the same dispatcher entry points.
    pub fn current() -> Self {
        if std::env::var("CQS_OUTPUT_FORMAT").as_deref() == Ok("v2") {
            Self::V2Bare
        } else {
            Self::V1Envelope
        }
    }

    /// `true` when the bare-payload wire shape should be used on the
    /// CLI direct success path. Returns `false` when [`Posture`] is
    /// [`Posture::Adversarial`] — adversarial consumers always get
    /// the full envelope regardless of `OutputFormat`.
    pub fn emits_bare_payload(self, posture: Posture) -> bool {
        matches!(self, Self::V2Bare) && !posture.is_adversarial()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_adversarial_classifies_correctly() {
        assert!(Posture::Adversarial.is_adversarial());
        assert!(!Posture::Friendly.is_adversarial());
    }

    #[test]
    #[serial_test::serial]
    fn current_reads_env_var() {
        std::env::remove_var("CQS_ULTRASECURITY");
        assert_eq!(Posture::current(), Posture::Friendly);
        std::env::set_var("CQS_ULTRASECURITY", "1");
        assert_eq!(Posture::current(), Posture::Adversarial);
        std::env::set_var("CQS_ULTRASECURITY", "0");
        assert_eq!(
            Posture::current(),
            Posture::Friendly,
            "any value other than '1' is Friendly"
        );
        std::env::remove_var("CQS_ULTRASECURITY");
    }

    // SNR Phase 4 plumbing: OutputFormat::V2Bare opt-in via
    // CQS_OUTPUT_FORMAT=v2; default V1Envelope preserves existing
    // tests + eval harness behavior.

    #[test]
    #[serial_test::serial]
    fn output_format_current_reads_env_var() {
        std::env::remove_var("CQS_OUTPUT_FORMAT");
        assert_eq!(OutputFormat::current(), OutputFormat::V1Envelope);
        std::env::set_var("CQS_OUTPUT_FORMAT", "v2");
        assert_eq!(OutputFormat::current(), OutputFormat::V2Bare);
        std::env::set_var("CQS_OUTPUT_FORMAT", "v1");
        assert_eq!(
            OutputFormat::current(),
            OutputFormat::V1Envelope,
            "explicit v1 is also envelope (legacy hedge)"
        );
        std::env::set_var("CQS_OUTPUT_FORMAT", "V2");
        assert_eq!(
            OutputFormat::current(),
            OutputFormat::V1Envelope,
            "case-sensitive: only lowercase 'v2' opts in"
        );
        std::env::remove_var("CQS_OUTPUT_FORMAT");
    }

    #[test]
    fn output_format_emits_bare_payload_under_v2_friendly() {
        assert!(OutputFormat::V2Bare.emits_bare_payload(Posture::Friendly));
    }

    #[test]
    fn output_format_skips_bare_under_v1() {
        assert!(!OutputFormat::V1Envelope.emits_bare_payload(Posture::Friendly));
        assert!(!OutputFormat::V1Envelope.emits_bare_payload(Posture::Adversarial));
    }

    #[test]
    fn output_format_adversarial_overrides_v2() {
        // Posture::Adversarial wins — verbose envelope on every surface.
        assert!(!OutputFormat::V2Bare.emits_bare_payload(Posture::Adversarial));
    }
}
