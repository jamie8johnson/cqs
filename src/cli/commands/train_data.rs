use anyhow::Result;

use cqs::train_data::TrainDataConfig;

pub fn cmd_train_data(config: TrainDataConfig) -> Result<()> {
    let _span = tracing::info_span!("cmd_train_data").entered();
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
