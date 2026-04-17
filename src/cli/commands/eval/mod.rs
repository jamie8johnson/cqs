//! `cqs eval` — first-class A/B harness for measuring search quality.
//!
//! Replaces the friction of the Python eval harness (subprocess env
//! inheritance, batch-flag drift, gold-matching reinvented per script) with
//! a Rust subcommand that runs the production search path against a JSON
//! query set and prints R@K aggregates.
//!
//! Workflow:
//!   `cqs eval evals/queries/v3_test.json` — run + print
//!   `cqs eval evals/queries/v3_test.json --json` — machine-readable
//!   `cqs eval evals/queries/v3_test.json --save baseline.json` — capture
//!   `cqs eval evals/queries/v3_test.json --baseline baseline.json` — diff
//!     (Task C2 will implement the diff body; today it errors)
//!
//! Future Task C1 (`--with-model X`) will build a temp side-index and
//! reuse this module's runner — see `runner::run_eval` for the seam.

mod baseline;
mod runner;

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use cqs::store::ReadOnly;

use crate::cli::CommandContext;

pub(crate) use runner::EvalReport;

/// CLI args for `cqs eval`.
///
/// Kept as a flat struct on `Commands::Eval` instead of a shared
/// `args::EvalArgs` because there is no batch handler for eval — `cqs eval`
/// is CLI-only by design (long-running, progress to stderr, file I/O).
/// Adding a batch handler later is a one-line move into `args.rs`.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct EvalCmdArgs {
    /// Path to the queries JSON file (v3 schema)
    pub query_file: PathBuf,

    /// Output as JSON instead of text
    #[arg(long)]
    pub json: bool,

    /// Max results retrieved per query (used for R@K denominator cap)
    #[arg(long, default_value = "20")]
    pub limit: usize,

    /// Restrict the run to one category (e.g. `multi_step`)
    #[arg(long)]
    pub category: Option<String>,

    /// Save the resulting report to this path (JSON)
    #[arg(long)]
    pub save: Option<PathBuf>,

    /// Compare current run against a saved baseline (Task C2 — stub today)
    #[arg(long)]
    pub baseline: Option<PathBuf>,

    /// Tolerance for `--baseline` diff (percentage points; default 1.0)
    #[arg(long, default_value = "1.0")]
    pub tolerance: f64,
}

/// CLI handler for `cqs eval`.
pub(crate) fn cmd_eval(ctx: &CommandContext<'_, ReadOnly>, args: &EvalCmdArgs) -> Result<()> {
    let _span = tracing::info_span!(
        "cmd_eval",
        query_file = %args.query_file.display(),
        category = ?args.category,
        limit = args.limit,
    )
    .entered();

    if args.limit == 0 {
        anyhow::bail!("--limit must be at least 1");
    }
    if !args.tolerance.is_finite() || args.tolerance < 0.0 {
        anyhow::bail!(
            "--tolerance must be a finite non-negative number, got {}",
            args.tolerance
        );
    }

    let report = runner::run_eval(ctx, &args.query_file, args.category.as_deref(), args.limit)?;

    // When --baseline is set, prefer the diff output over the raw report —
    // a CI-shaped invocation just wants the diff. The raw report still
    // lands on disk via --save below if requested.
    if args.baseline.is_none() {
        // Output (text or JSON) before --save so the user sees results even
        // if --save's directory is missing or unwritable.
        if args.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print_text_report(&report);
        }
    }

    if let Some(save_path) = &args.save {
        let bytes =
            serde_json::to_vec_pretty(&report).context("Failed to serialize eval report")?;
        std::fs::write(save_path, &bytes)
            .with_context(|| format!("Failed to write baseline to {}", save_path.display()))?;
        eprintln!("[eval] saved baseline to {}", save_path.display());
    }

    if let Some(baseline_path) = &args.baseline {
        let diff = baseline::compare_against_baseline(&report, baseline_path, args.tolerance)?;
        baseline::print_diff_report(&diff, args.json);
        if !diff.regressions.is_empty() {
            // Per-category regression past tolerance → CI-friendly exit 1.
            // Stderr summary so a wrapping shell script can grep for it
            // even when stdout is consumed by --json.
            eprintln!(
                "[eval] {} regression(s) past tolerance \u{00b1}{:.1}pp — exit 1",
                diff.regressions.len(),
                diff.tolerance_pp
            );
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Print the eval report in human-readable text.
///
/// Format mirrors the spec exactly so a user comparing old python output
/// against `cqs eval` output can eyeball the same shape.
fn print_text_report(report: &EvalReport) {
    println!(
        "=== eval results: {} (N={}) ===",
        report.query_file, report.overall.n
    );
    println!(
        "OVERALL: R@1={}  R@5={}  R@20={}",
        pct(report.overall.r_at_1),
        pct(report.overall.r_at_5),
        pct(report.overall.r_at_20)
    );
    if report.skipped > 0 {
        println!("(skipped {} queries with no gold_chunk)", report.skipped);
    }
    println!();

    if !report.by_category.is_empty() {
        println!(
            "{:<24} {:>5} {:>7} {:>7} {:>7}",
            "category", "N", "R@1", "R@5", "R@20"
        );
        for (cat, stats) in &report.by_category {
            println!(
                "{:<24} {:>5} {:>7} {:>7} {:>7}",
                cat,
                stats.n,
                pct(stats.r_at_1),
                pct(stats.r_at_5),
                pct(stats.r_at_20),
            );
        }
        println!();
    }

    println!(
        "(eval took {:.1}s, {:.1} queries/sec, model={})",
        report.elapsed_secs, report.queries_per_sec, report.index_model
    );
}

/// Format a fraction in [0.0, 1.0] as a percentage with one decimal place,
/// e.g. 0.4220 → "42.2%". Same formatting the python eval used.
fn pct(x: f64) -> String {
    format!("{:>5.1}%", x * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pct` formats fractions consistently across the spectrum so the
    /// text report aligns in columns.
    #[test]
    fn test_pct_formatting() {
        assert_eq!(pct(0.0), "  0.0%");
        assert_eq!(pct(0.422), " 42.2%");
        assert_eq!(pct(1.0), "100.0%");
        assert_eq!(pct(0.5), " 50.0%");
    }

    /// Validate args: --limit 0 must fail before running anything.
    /// We can't construct a `CommandContext` here without an indexed store,
    /// so the limit guard is implicitly tested via the cmd_eval entry —
    /// integration tests in tests/eval_subcommand_test.rs cover the live path.
    #[test]
    fn test_args_default_limit_is_20() {
        // Mirror clap's default_value
        use clap::Parser;
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            args: EvalCmdArgs,
        }
        let w = Wrapper::try_parse_from(["test", "queries.json"]).unwrap();
        assert_eq!(w.args.limit, 20);
        assert!(!w.args.json);
        assert!(w.args.category.is_none());
        assert!(w.args.save.is_none());
        assert!(w.args.baseline.is_none());
        assert!((w.args.tolerance - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_args_parse_all_flags() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            args: EvalCmdArgs,
        }
        let w = Wrapper::try_parse_from([
            "test",
            "queries.json",
            "--json",
            "--limit",
            "50",
            "--category",
            "structural_search",
            "--save",
            "out.json",
            "--baseline",
            "base.json",
            "--tolerance",
            "2.5",
        ])
        .unwrap();
        assert!(w.args.json);
        assert_eq!(w.args.limit, 50);
        assert_eq!(w.args.category.as_deref(), Some("structural_search"));
        assert_eq!(w.args.save.unwrap().to_str().unwrap(), "out.json");
        assert_eq!(w.args.baseline.unwrap().to_str().unwrap(), "base.json");
        assert!((w.args.tolerance - 2.5).abs() < 1e-9);
    }
}
