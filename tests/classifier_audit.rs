//! Classifier audit: run `classify_query` on v3 dev queries and report
//! a confusion matrix vs the consensus labels. One-shot, read-only —
//! prints findings via `println!`, always passes.
//!
//! Run: `cargo test --test classifier_audit --release --features gpu-index -- --nocapture`

use std::collections::{BTreeMap, HashMap};
use std::fs;

use cqs::search::router::{classify_query, QueryCategory};
use serde::Deserialize;

#[derive(Deserialize)]
struct V3DevEntry {
    query: String,
    category: String,
}

#[derive(Deserialize)]
struct V3DevFile {
    queries: Vec<V3DevEntry>,
}

fn v3_label_to_category(s: &str) -> Option<QueryCategory> {
    // The v3 eval uses the snake_case names produced by Gemma/Claude; cqs
    // uses the same via QueryCategory::from_snake_case, which already
    // handles both "structural" and "structural_search" aliases etc.
    QueryCategory::from_snake_case(s)
}

#[test]
fn audit_classifier_on_v3_dev() {
    let path = "evals/queries/v3_dev.json";
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            // Skip if the eval file isn't present (CI environments without the dataset).
            eprintln!("skipping audit: {path} not readable ({e})");
            return;
        }
    };
    let data: V3DevFile = serde_json::from_str(&text).expect("v3 dev must parse");

    let mut total = 0usize;
    let mut correct = 0usize;
    let mut unknown_labeled = 0usize; // cqs said Unknown
    let mut mismatch_nonunknown: Vec<(String, QueryCategory, QueryCategory)> = Vec::new();
    let mut confusion: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut per_label_total: HashMap<String, usize> = HashMap::new();
    let mut per_label_fired: HashMap<String, usize> = HashMap::new();
    let mut per_label_correct: HashMap<String, usize> = HashMap::new();

    for row in &data.queries {
        let Some(gold) = v3_label_to_category(&row.category) else {
            continue;
        };
        total += 1;
        *per_label_total.entry(row.category.clone()).or_default() += 1;

        let classification = classify_query(&row.query);
        let predicted = classification.category;
        let pred_str = predicted.to_string();

        *confusion
            .entry((row.category.clone(), pred_str.clone()))
            .or_insert(0) += 1;

        if predicted == QueryCategory::Unknown {
            unknown_labeled += 1;
        } else {
            *per_label_fired.entry(row.category.clone()).or_default() += 1;
        }

        if predicted == gold {
            correct += 1;
            *per_label_correct.entry(row.category.clone()).or_default() += 1;
        } else if predicted != QueryCategory::Unknown {
            mismatch_nonunknown.push((row.query.clone(), gold, predicted));
        }
    }

    println!("\n=== classifier audit on v3 dev ===");
    println!("total queries         : {total}");
    println!(
        "classifier correct    : {correct} ({:.1}%)",
        100.0 * correct as f64 / total as f64
    );
    println!(
        "landed in Unknown     : {unknown_labeled} ({:.1}%)",
        100.0 * unknown_labeled as f64 / total as f64
    );
    println!(
        "fired wrong (not Unknown, not gold): {}  ← fix targets",
        mismatch_nonunknown.len()
    );

    println!("\n--- per v3-label recall (any category) / precision (gold match) ---");
    println!(
        "{:<22} {:>4}  {:>10}  {:>10}  {:>8}",
        "v3 label", "N", "fired%", "correct%", "prec%"
    );
    let mut labels: Vec<_> = per_label_total.keys().cloned().collect();
    labels.sort();
    for lab in &labels {
        let n = per_label_total[lab];
        let fired = per_label_fired.get(lab).copied().unwrap_or(0);
        let corr = per_label_correct.get(lab).copied().unwrap_or(0);
        let prec = if fired > 0 {
            100.0 * corr as f64 / fired as f64
        } else {
            0.0
        };
        println!(
            "  {:<20} {:>4}  {:>9.1}%  {:>9.1}%  {:>7.1}%",
            lab,
            n,
            100.0 * fired as f64 / n as f64,
            100.0 * corr as f64 / n as f64,
            prec,
        );
    }

    println!("\n--- confusion matrix (rows = v3 label, cols = classifier prediction) ---");
    let mut cols: std::collections::BTreeSet<String> =
        confusion.keys().map(|(_, c)| c.clone()).collect();
    cols.insert("unknown".to_string());
    let cols: Vec<String> = cols.into_iter().collect();
    print!("{:<22}", "label \\ pred");
    for c in &cols {
        print!(" {:>10}", c);
    }
    println!();
    for lab in &labels {
        print!("  {:<20}", lab);
        for c in &cols {
            let n = confusion
                .get(&(lab.clone(), c.clone()))
                .copied()
                .unwrap_or(0);
            if n > 0 {
                print!(" {:>10}", n);
            } else {
                print!(" {:>10}", ".");
            }
        }
        println!();
    }

    println!("\n--- mismatched non-Unknown predictions (fix targets) ---");
    if mismatch_nonunknown.is_empty() {
        println!("  (none — every non-Unknown prediction matched gold)");
    } else {
        for (q, gold, pred) in &mismatch_nonunknown {
            let short = if q.len() > 78 {
                format!("{}…", &q[..78])
            } else {
                q.clone()
            };
            println!(
                "  [gold={:<22} pred={:<22}] {}",
                gold.to_string(),
                pred.to_string(),
                short
            );
        }
    }
}
