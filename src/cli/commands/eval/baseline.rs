//! Baseline diff support for `cqs eval --baseline X`.
//!
//! Task C2 will implement the full diff: load a saved `EvalReport` from disk,
//! compare against the just-run report, surface per-category deltas, and
//! gate on `--tolerance N` percentage points. This module is the entry
//! point both for the placeholder error today and the real diff later — by
//! routing through `compare_against_baseline` from day one, C2 is a
//! single-function implementation rather than a CLI re-plumb.

use std::path::Path;

use anyhow::{bail, Result};

use super::runner::EvalReport;

/// Compare `current` against the `EvalReport` saved at `baseline_path`.
///
/// Returns `Ok(())` when the diff falls inside `tolerance_pp` percentage
/// points on every category and the overall metric (Task C2 contract).
/// Returns an error otherwise. Today this is a stub that always errors —
/// the parse-and-call site exists so C2 only needs to fill in the body.
///
/// Loads but does not interpret the baseline: the I/O lives here so C2
/// gets the bytes without re-touching the dispatch path.
pub(crate) fn compare_against_baseline(
    _current: &EvalReport,
    baseline_path: &Path,
    _tolerance_pp: f64,
) -> Result<()> {
    let _span =
        tracing::info_span!("compare_against_baseline", path = %baseline_path.display()).entered();
    bail!(
        "--baseline is not yet implemented (Task C2). Saved baseline path was: {}. \
         Run `cqs eval ... --save baseline.json` to capture the current run; \
         once C2 lands, `--baseline baseline.json --tolerance 1.0` will diff against it.",
        baseline_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use super::super::runner::Overall;

    fn dummy_report() -> EvalReport {
        EvalReport {
            query_count: 1,
            skipped: 0,
            elapsed_secs: 1.0,
            queries_per_sec: 1.0,
            overall: Overall {
                n: 1,
                r_at_1: 1.0,
                r_at_5: 1.0,
                r_at_20: 1.0,
            },
            by_category: BTreeMap::new(),
            index_model: "test".into(),
            cqs_version: "0.0.0".into(),
            query_file: "noop.json".into(),
            limit: 20,
            category_filter: None,
        }
    }

    /// C2 not implemented: the call must error. When C2 lands, this test
    /// flips to assert the success path; the failure mode pins today's
    /// behavior so a half-finished C2 doesn't silently degrade to "passes
    /// because nobody checked".
    #[test]
    fn test_baseline_stub_errors_until_c2() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let report = dummy_report();
        let result = compare_against_baseline(&report, tmp.path(), 1.0);
        assert!(result.is_err(), "C2 stub must error until implemented");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not yet implemented"),
            "Error should call out C2: {err}"
        );
    }
}
