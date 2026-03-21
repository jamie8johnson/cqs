use anyhow::Result;

use cqs::train_data::TrainDataConfig;

/// Generates training data for machine learning models from repository commits.
///
/// This function processes repositories to extract training triplets from commit data and outputs summary statistics about the generation process, including counts by programming language.
///
/// # Arguments
///
/// * `config` - Configuration object specifying parameters for training data generation, such as which repositories to process and filtering criteria.
///
/// # Returns
///
/// Returns `Ok(())` on successful completion, or an `Err` if training data generation fails.
///
/// # Errors
///
/// Returns an error if the underlying `generate_training_data` function encounters issues such as repository access failures or invalid commit data.
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
