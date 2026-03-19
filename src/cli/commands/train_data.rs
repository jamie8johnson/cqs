use std::path::PathBuf;

use anyhow::Result;

pub fn cmd_train_data(
    repos: Vec<PathBuf>,
    output: PathBuf,
    max_commits: usize,
    min_msg_len: usize,
    max_files: usize,
    dedup_cap: usize,
    resume: bool,
    verbose: bool,
) -> Result<()> {
    let config = cqs::train_data::TrainDataConfig {
        repos,
        output,
        max_commits,
        min_msg_len,
        max_files,
        dedup_cap,
        resume,
        verbose,
    };
    let stats = cqs::train_data::generate_training_data(&config).map_err(|e| anyhow::anyhow!(e))?;

    println!(
        "Generated {} triplets from {} repos ({} commits processed, {} skipped)",
        stats.total_triplets, stats.repos_processed, stats.commits_processed, stats.commits_skipped
    );
    if stats.parse_failures > 0 {
        println!("  {} parse failures", stats.parse_failures);
    }
    for (lang, count) in &stats.language_counts {
        println!("  {}: {} triplets", lang, count);
    }
    Ok(())
}
