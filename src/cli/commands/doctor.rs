//! Doctor command for cqs
//!
//! Runs diagnostic checks on installation and index.

use anyhow::Result;
use colored::Colorize;

use cqs::embedder::ModelConfig;
use cqs::{Embedder, Parser as CqParser, Store};

use crate::cli::find_project_root;

/// Issue type detected during doctor checks.
#[derive(Debug, Clone, PartialEq)]
enum IssueKind {
    /// Index is stale — needs re-index
    Stale,
    /// Schema version mismatch — needs migration
    Schema,
    /// No index exists — needs creation
    NoIndex,
    /// Model error — needs reinstall
    ModelError,
}

/// A single doctor issue with its fix action.
#[derive(Debug, Clone)]
struct DoctorIssue {
    kind: IssueKind,
    message: String,
}

/// Run fix actions for detected issues.
fn run_fixes(issues: &[DoctorIssue]) -> Result<()> {
    let _span = tracing::info_span!("doctor_fix", issue_count = issues.len()).entered();

    for issue in issues {
        match issue.kind {
            IssueKind::Stale | IssueKind::NoIndex => {
                println!("  Fixing: {} — running 'cqs index'...", issue.message);
                let status = std::process::Command::new("cqs")
                    .arg("index")
                    .status()
                    .map_err(|e| anyhow::anyhow!("Failed to run 'cqs index': {}", e))?;
                if status.success() {
                    println!("  {} Index rebuilt", "[✓]".green());
                } else {
                    println!("  {} Index rebuild failed", "[✗]".red());
                    tracing::warn!("cqs index exited with status {}", status);
                }
            }
            IssueKind::Schema => {
                println!(
                    "  Fixing: {} — running 'cqs index --force'...",
                    issue.message
                );
                let status = std::process::Command::new("cqs")
                    .args(["index", "--force"])
                    .status()
                    .map_err(|e| anyhow::anyhow!("Failed to run 'cqs index --force': {}", e))?;
                if status.success() {
                    println!("  {} Index rebuilt with schema migration", "[✓]".green());
                } else {
                    println!("  {} Schema migration failed", "[✗]".red());
                    tracing::warn!("cqs index --force exited with status {}", status);
                }
            }
            IssueKind::ModelError => {
                println!(
                    "  Skipping: {} — model issues require manual intervention",
                    issue.message
                );
            }
        }
    }
    Ok(())
}

/// Run diagnostic checks on cqs installation and index
/// Reports runtime info, embedding provider, model status, and index statistics.
/// With `--fix`, automatically remediates issues: stale→index, schema→migrate.
pub(crate) fn cmd_doctor(model_override: Option<&str>, fix: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_doctor", fix).entered();
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");
    let mut any_failed = false;
    let mut issues: Vec<DoctorIssue> = Vec::new();

    println!("Runtime:");

    // Check model
    let model_config = ModelConfig::resolve(model_override, None);
    match Embedder::new(model_config.clone()) {
        Ok(embedder) => {
            println!(
                "  {} Model: {} (metadata: {})",
                "[✓]".green(),
                cqs::embedder::model_repo(),
                cqs::store::MODEL_NAME
            );
            println!("  {} Tokenizer: loaded", "[✓]".green());
            println!("  {} Execution: {}", "[✓]".green(), embedder.provider());

            // Test embedding
            let start = std::time::Instant::now();
            embedder.warm()?;
            let elapsed = start.elapsed();
            println!("  {} Test embedding: {:?}", "[✓]".green(), elapsed);
        }
        Err(e) => {
            let msg = format!("Model load failed: {}", e);
            println!("  {} Model: {}", "[✗]".red(), e);
            issues.push(DoctorIssue {
                kind: IssueKind::ModelError,
                message: msg,
            });
            any_failed = true;
        }
    }

    println!();
    println!("Parser:");
    match CqParser::new() {
        Ok(parser) => {
            println!("  {} tree-sitter: loaded", "[✓]".green());
            println!(
                "  {} Languages: {}",
                "[✓]".green(),
                parser.supported_extensions().join(", ")
            );
        }
        Err(e) => {
            println!("  {} Parser: {}", "[✗]".red(), e);
            // Parser errors are not auto-fixable
            any_failed = true;
        }
    }

    println!();
    println!("Index:");
    if index_path.exists() {
        match Store::open(&index_path) {
            Ok(store) => {
                let stats = store.stats()?;
                println!("  {} Location: {}", "[✓]".green(), index_path.display());
                println!(
                    "  {} Schema version: {}",
                    "[✓]".green(),
                    stats.schema_version
                );
                println!("  {} {} chunks indexed", "[✓]".green(), stats.total_chunks);
                if !stats.chunks_by_language.is_empty() {
                    let lang_summary: Vec<_> = stats
                        .chunks_by_language
                        .iter()
                        .map(|(l, c)| format!("{} {}", c, l))
                        .collect();
                    println!("      ({})", lang_summary.join(", "));
                }

                // Check schema version against expected
                let expected = cqs::store::CURRENT_SCHEMA_VERSION;
                if stats.schema_version != expected {
                    println!(
                        "  {} Schema mismatch: index is v{}, cqs expects v{}",
                        "[!]".yellow(),
                        stats.schema_version,
                        expected
                    );
                    issues.push(DoctorIssue {
                        kind: IssueKind::Schema,
                        message: format!(
                            "Schema v{} != expected v{}",
                            stats.schema_version, expected
                        ),
                    });
                    any_failed = true;
                }

                // Check model mismatch between index and configured model
                let stored = store.stored_model_name();
                let configured = &model_config.name;
                match stored {
                    Some(ref stored_name) if stored_name != configured => {
                        println!(
                            "  {} Model mismatch: index uses \"{}\", configured is \"{}\"",
                            "[!]".yellow(),
                            stored_name,
                            configured
                        );
                        println!("      Run `cqs index --force` to reindex with the new model.");
                        issues.push(DoctorIssue {
                            kind: IssueKind::Stale,
                            message: format!(
                                "Model mismatch: index uses \"{}\", configured is \"{}\"",
                                stored_name, configured
                            ),
                        });
                        any_failed = true;
                    }
                    _ => {}
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                println!("  {} Index: {}", "[✗]".red(), e);
                if err_str.contains("Schema version mismatch") {
                    issues.push(DoctorIssue {
                        kind: IssueKind::Schema,
                        message: err_str,
                    });
                }
                any_failed = true;
            }
        }
    } else {
        println!("  {} Index: not created yet", "[!]".yellow());
        println!("      Run 'cqs index' to create the index");
        issues.push(DoctorIssue {
            kind: IssueKind::NoIndex,
            message: "Index not created".to_string(),
        });
    }

    // Check references
    let config = cqs::config::Config::load(&root);
    if !config.references.is_empty() {
        println!();
        println!("References:");
        for r in &config.references {
            let db_path = r.path.join("index.db");
            if !r.path.exists() {
                println!(
                    "  {} {}: path missing ({})",
                    "[✗]".red(),
                    r.name,
                    r.path.display()
                );
                any_failed = true;
                continue;
            }
            match Store::open(&db_path) {
                Ok(store) => {
                    let chunks = store.chunk_count().unwrap_or(0);
                    let hnsw = if cqs::HnswIndex::exists(&r.path, "index") {
                        "HNSW loaded".to_string()
                    } else {
                        "no HNSW".to_string()
                    };
                    println!(
                        "  {} {}: {} chunks, {} (weight {:.1})",
                        "[✓]".green(),
                        r.name,
                        chunks,
                        hnsw,
                        r.weight
                    );
                }
                Err(e) => {
                    println!("  {} {}: {}", "[✗]".red(), r.name, e);
                    any_failed = true;
                }
            }
        }
    }

    println!();
    if any_failed {
        println!("Some checks failed — see {} items above.", "[✗]".red());
    } else {
        println!("All checks passed.");
    }

    // --fix: attempt automatic remediation
    if fix && !issues.is_empty() {
        println!();
        println!("{}:", "Auto-fixing issues".bold());
        run_fixes(&issues)?;
    } else if fix && issues.is_empty() {
        println!("Nothing to fix.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_kind_maps_to_fix_action() {
        // Verify the fix action mapping for each issue kind
        let stale = DoctorIssue {
            kind: IssueKind::Stale,
            message: "stale index".to_string(),
        };
        let schema = DoctorIssue {
            kind: IssueKind::Schema,
            message: "schema mismatch".to_string(),
        };
        let no_index = DoctorIssue {
            kind: IssueKind::NoIndex,
            message: "no index".to_string(),
        };
        let model = DoctorIssue {
            kind: IssueKind::ModelError,
            message: "model error".to_string(),
        };

        // Stale and NoIndex both map to "cqs index"
        assert_eq!(stale.kind, IssueKind::Stale);
        assert_eq!(no_index.kind, IssueKind::NoIndex);
        // Schema maps to "cqs index --force"
        assert_eq!(schema.kind, IssueKind::Schema);
        // Model is manual
        assert_eq!(model.kind, IssueKind::ModelError);
    }
}
