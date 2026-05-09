//! Caller-decided emission posture for JSON output.
//!
//! Lives in the lib crate so leaf serializers (e.g.
//! `crate::store::helpers::types::SearchResult::build_chunk_json_inner`)
//! can take a [`Posture`] parameter without depending on the bin
//! (`cqs::cli::json_envelope`) layer. The bin's `cli::json_envelope`
//! re-exports this same type for convenience.
//!
//! See `docs/json-snr-restoration.md` for the migration plan.
//!
//! Audit Cluster B (post-v1.40.0):
//!
//! - **Process-lifetime caching** (CQ-V1.40-7, RM-V1.40-8, PERF-V1.40-3,
//!   PERF-V1.40-4, DS-V1.40-5, SEC-V1.40-6). [`Posture::current`] and
//!   [`OutputFormat::current`] read the env once via `OnceLock` and
//!   memoize. A daemon serving thousands of emissions per minute does
//!   one syscall per env var per process lifetime, not per emit. Side
//!   benefit: no TOCTOU race — a daemon thread that calls `set_var`
//!   mid-stream can't flip the envelope shape because the cached value
//!   is already pinned.
//!
//! - **Truthy/falsy alias recognition + warn on unrecognized values**
//!   (EH-V1.40-2, API-V1.40-8, OB-V1.40-3, PB-V1.40-3). Pre-fix,
//!   `CQS_ULTRASECURITY=true` silently fell through to `Friendly`,
//!   disabling the security advisory the operator believed they had
//!   enabled. Post-fix, the parser recognizes the conventional truthy
//!   set (`1`, `true`, `on`, `yes` case-insensitive, with whitespace
//!   trimmed) and logs `tracing::warn!` for any value set but
//!   unrecognized so the operator sees their typo.

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

/// Process-lifetime cache of the resolved posture. First call to
/// [`Posture::current`] reads the env via [`Posture::resolve_from_env`]
/// (with the warn-on-unrecognized-value parser) and stores the result;
/// subsequent calls are an `Atomic` load. Avoids the per-emit
/// `std::env::var` syscall + the TOCTOU race where a daemon thread can
/// `set_var` mid-stream and flip the envelope shape on the next emit.
static POSTURE: std::sync::OnceLock<Posture> = std::sync::OnceLock::new();

impl Posture {
    /// Resolve the posture once (first call reads env, subsequent calls
    /// hit the cache). Intended to be called at request entry points
    /// (CLI dispatcher, batch dispatcher, daemon handler) so the
    /// posture flows through the request as a typed value.
    ///
    /// Audit Cluster B (DS-V1.40-5 + SEC-V1.40-6 + perf): the cache
    /// pins the resolved posture for the process lifetime, so a daemon
    /// thread can't flip the envelope shape mid-request via `set_var`,
    /// and a hot emit path doesn't pay for `env::var` per call.
    pub fn current() -> Self {
        *POSTURE.get_or_init(Self::resolve_from_env)
    }

    /// Read the env var, parse via [`Self::resolve_from_str`], and log
    /// the resolved value. Pure side-effect of "first call" — subsequent
    /// `current()` calls hit the cache and don't log again.
    fn resolve_from_env() -> Self {
        let raw = std::env::var("CQS_ULTRASECURITY").ok();
        let posture = Self::resolve_from_str(raw.as_deref());
        tracing::info!(
            posture = ?posture,
            raw = ?raw.as_deref().unwrap_or("<unset>"),
            "Posture resolved from CQS_ULTRASECURITY"
        );
        posture
    }

    /// Pure parsing helper: map an env-value to a [`Posture`].
    ///
    /// `None` → `Friendly` (default, env unset).
    /// `Some(v)` recognized as truthy (`1`, `true`, `on`, `yes`,
    /// case-insensitive, whitespace trimmed) → `Adversarial`.
    /// `Some(v)` recognized as falsy (`0`, `false`, `off`, `no`,
    /// empty after trim) → `Friendly`.
    /// `Some(v)` unrecognized → `Friendly` + `tracing::warn!` so the
    /// operator sees their typo.
    fn resolve_from_str(raw: Option<&str>) -> Self {
        let trimmed = raw.unwrap_or("").trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => Self::Adversarial,
            "" | "0" | "false" | "off" | "no" => Self::Friendly,
            other => {
                tracing::warn!(
                    raw = %other,
                    "CQS_ULTRASECURITY value not recognized as truthy or falsy; \
                     defaulting to Friendly. Recognized truthy: 1, true, on, yes \
                     (case-insensitive). Recognized falsy: 0, false, off, no."
                );
                Self::Friendly
            }
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

/// Process-lifetime cache for the resolved output format. Same shape
/// as [`POSTURE`] — first `current()` call reads env, subsequent calls
/// hit the cache. See [`POSTURE`] doc for the rationale.
static OUTPUT_FORMAT: std::sync::OnceLock<OutputFormat> = std::sync::OnceLock::new();

impl OutputFormat {
    /// Resolve once and cache for the process lifetime. Same caching
    /// shape as [`Posture::current`]; intended to be called at the
    /// same dispatcher entry points.
    ///
    /// **SNR Phase 4 default flip (2026-05-08):** unset env or any value
    /// other than `"v1"` (case-insensitive after trim) ⇒ [`Self::V2Bare`]
    /// (bare payload). `CQS_OUTPUT_FORMAT=v1` ⇒ [`Self::V1Envelope`]
    /// (legacy envelope). Inverted polarity from the original opt-in
    /// landing — opt-out is now the legacy hedge for consumer scripts
    /// that haven't migrated.
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

    // ───────────────────────────────────────────────────────────────────
    // Posture::resolve_from_str — pure parser tests (audit Cluster B)
    //
    // Replaces the pre-Cluster-B `current_reads_env_var` test. After
    // `current()` adopted process-lifetime `OnceLock` caching (DS-V1.40-5
    // / SEC-V1.40-6 / perf cluster), env-mutating tests against
    // `current()` are racy: the first test that runs in the binary wins
    // the cache for the entire process. Pure-function tests on the
    // parser side-step the cache and are deterministic.
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn posture_resolve_unset_is_friendly() {
        assert_eq!(Posture::resolve_from_str(None), Posture::Friendly);
        assert_eq!(Posture::resolve_from_str(Some("")), Posture::Friendly);
    }

    #[test]
    fn posture_resolve_truthy_aliases_are_adversarial() {
        for v in &[
            "1", "true", "on", "yes", "TRUE", "True", "On", "ON", "Yes", "YES",
            // whitespace-trimmed
            " 1 ", "  true\t", "\non\n",
        ] {
            assert_eq!(
                Posture::resolve_from_str(Some(v)),
                Posture::Adversarial,
                "expected truthy alias {v:?} → Adversarial"
            );
        }
    }

    #[test]
    fn posture_resolve_falsy_aliases_are_friendly() {
        for v in &[
            "0", "false", "off", "no", "FALSE", "False", " 0 ", "\toff\n",
        ] {
            assert_eq!(
                Posture::resolve_from_str(Some(v)),
                Posture::Friendly,
                "expected falsy alias {v:?} → Friendly"
            );
        }
    }

    #[test]
    fn posture_resolve_unknown_value_is_friendly() {
        // Audit EH-V1.40-2: pre-Cluster-B `current()` silently dropped
        // `Friendly` on any non-`"1"` value, including likely typos.
        // Post-Cluster-B `resolve_from_str` still resolves to Friendly
        // (the safe default), but emits `tracing::warn!` so the operator
        // sees their typo. The test pins the resolution side; the warn
        // side is observable via `tracing-test` if needed but isn't a
        // load-bearing pin.
        assert_eq!(
            Posture::resolve_from_str(Some("enable")),
            Posture::Friendly,
            "unknown value falls through to Friendly (safe default)"
        );
        assert_eq!(
            Posture::resolve_from_str(Some("adversarial")),
            Posture::Friendly
        );
    }

    // ───────────────────────────────────────────────────────────────────
    // OutputFormat::resolve_from_str — pure parser tests
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
        // Pre-Cluster-B: `"V1"`, `"V1 "`, `"v1\n"` all silently fell
        // through to V2Bare because the parser used strict `==`.
        // Post-Cluster-B: case-insensitive + trimmed.
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
        // SNR Phase 4 default = V2Bare. Anything we don't recognize
        // falls through to V2Bare + tracing::warn (so a typo doesn't
        // silently re-enable the legacy envelope shape).
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

    // ───────────────────────────────────────────────────────────────────
    // Compose-contract tests (TC-HAP-V1.40-9: CQS_ULTRASECURITY ×
    // CQS_OUTPUT_FORMAT). These pin the matrix at the typed level —
    // since `current()` is cached, any compose-contract test against
    // `current()` itself is racy across the test binary. Test the
    // composition operator directly.
    // ───────────────────────────────────────────────────────────────────

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
