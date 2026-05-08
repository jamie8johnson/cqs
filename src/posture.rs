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
/// **As of SNR Phase 4 (2026-05-08, this commit), the default is
/// [`Self::V2Bare`].** CLI direct success on a friendly-deployment
/// process now emits the bare JSON payload on stdout — no envelope
/// wrap. The legacy envelope shape is opt-in via `CQS_OUTPUT_FORMAT=v1`.
/// Integration tests and the eval harness pin themselves to `v1` via
/// env (see `tests/cli_*.rs` helpers and `evals/*.py` os.environ
/// overrides) so the flip-default doesn't break existing assertion
/// shapes; a follow-up PR migrates those consumers to expect the bare
/// shape natively.
///
/// **Posture interaction:** [`Posture::Adversarial`] overrides this —
/// the verbose envelope wins regardless of `OutputFormat`. The two
/// env vars compose: `CQS_ULTRASECURITY=1` ⇒ full envelope on every
/// surface; `CQS_OUTPUT_FORMAT=v1` AND not adversarial ⇒ legacy envelope
/// on the CLI direct success path (consumer-migration hedge); otherwise
/// (the new default) bare payload.
///
/// Batch / daemon JSONL is **not** affected by this — Phase 3 already
/// shipped the slim `{"data": ...}` / `{"error": {...}}` shape there
/// and the JSONL contract requires self-describing lines either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Legacy envelope shape: CLI direct success emits the full envelope
    /// `{data, error: null, version: 1, _meta: {...}}` to stdout. Selected
    /// by setting `CQS_OUTPUT_FORMAT=v1`. Hedge for consumer scripts that
    /// haven't migrated to the bare shape yet.
    V1Envelope,
    /// **Default as of SNR Phase 4 (2026-05-08):** CLI direct success
    /// emits the bare JSON payload to stdout (no envelope). Selected
    /// when `CQS_OUTPUT_FORMAT` is unset or set to anything other than
    /// `v1`. Restores the high-SNR baseline that the 79% → 6% search-
    /// rate decline measured.
    V2Bare,
}

impl OutputFormat {
    /// Read the env var once and return the corresponding format.
    /// Same one-syscall cost as [`Posture::current`]; intended to be
    /// called at the same dispatcher entry points.
    ///
    /// **SNR Phase 4 default flip (2026-05-08):** unset env or any value
    /// other than the literal `"v1"` ⇒ [`Self::V2Bare`] (bare payload).
    /// `CQS_OUTPUT_FORMAT=v1` ⇒ [`Self::V1Envelope`] (legacy envelope).
    /// Inverted polarity from the original opt-in landing — opt-out
    /// is now the legacy hedge for consumer scripts that haven't
    /// migrated.
    pub fn current() -> Self {
        if std::env::var("CQS_OUTPUT_FORMAT").as_deref() == Ok("v1") {
            Self::V1Envelope
        } else {
            Self::V2Bare
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

    // SNR Phase 4 default flip (2026-05-08): default is V2Bare.
    // CQS_OUTPUT_FORMAT=v1 opts back into the legacy envelope shape
    // (consumer-migration hedge). Tests + eval harness pin themselves
    // to v1 via env so the flip doesn't break existing assertion shapes.

    #[test]
    #[serial_test::serial]
    fn output_format_current_reads_env_var() {
        std::env::remove_var("CQS_OUTPUT_FORMAT");
        assert_eq!(
            OutputFormat::current(),
            OutputFormat::V2Bare,
            "default flip: unset env is V2Bare"
        );
        std::env::set_var("CQS_OUTPUT_FORMAT", "v1");
        assert_eq!(
            OutputFormat::current(),
            OutputFormat::V1Envelope,
            "explicit v1 opts into legacy envelope"
        );
        std::env::set_var("CQS_OUTPUT_FORMAT", "v2");
        assert_eq!(
            OutputFormat::current(),
            OutputFormat::V2Bare,
            "explicit v2 also yields V2Bare (idempotent with default)"
        );
        std::env::set_var("CQS_OUTPUT_FORMAT", "V1");
        assert_eq!(
            OutputFormat::current(),
            OutputFormat::V2Bare,
            "case-sensitive: only lowercase 'v1' opts back to envelope"
        );
        std::env::set_var("CQS_OUTPUT_FORMAT", "junk");
        assert_eq!(
            OutputFormat::current(),
            OutputFormat::V2Bare,
            "unrecognized value falls through to default V2Bare"
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
