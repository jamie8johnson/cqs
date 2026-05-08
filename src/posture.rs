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
}
