//! Task command — one-shot implementation context for a task description.

use anyhow::Result;
use colored::Colorize;

use cqs::{task, task_to_json, Embedder};

pub(crate) fn cmd_task(
    _cli: &crate::cli::Cli,
    description: &str,
    limit: usize,
    json: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_task", ?max_tokens).entered();
    let (store, root, _) = crate::cli::open_project_store()?;
    let embedder = Embedder::new()?;
    let limit = limit.clamp(1, 10);

    let result = task(&store, &embedder, description, &root, limit)?;

    if let Some(budget) = max_tokens {
        output_with_budget(&result, &root, &embedder, budget, json)?;
    } else if json {
        let output = task_to_json(&result, &root);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        output_text(&result, &root);
    }

    Ok(())
}

/// Greedy index-based packing: sort items by score desc, pack until budget.
/// Returns (kept_indices_in_original_order, tokens_used).
fn index_pack(
    token_counts: &[usize],
    budget: usize,
    overhead_per_item: usize,
    score_fn: impl Fn(usize) -> f32,
) -> (Vec<usize>, usize) {
    if token_counts.is_empty() {
        return (Vec::new(), 0);
    }
    let mut order: Vec<usize> = (0..token_counts.len()).collect();
    order.sort_by(|&a, &b| {
        score_fn(b)
            .partial_cmp(&score_fn(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut used = 0;
    let mut kept = Vec::new();
    for idx in order {
        let cost = token_counts[idx] + overhead_per_item;
        if used + cost > budget && !kept.is_empty() {
            break;
        }
        used += cost;
        kept.push(idx);
    }
    kept.sort(); // preserve original order
    (kept, used)
}

/// Waterfall token budgeting: allocate budget across sections, surplus flows forward.
fn output_with_budget(
    result: &cqs::TaskResult,
    root: &std::path::Path,
    embedder: &Embedder,
    budget: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("waterfall_budget", budget).entered();

    let overhead = if json {
        super::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    };
    let mut remaining = budget;

    // 1. Scout section (15%) — pack file groups by relevance
    let scout_budget = (budget as f64 * 0.15) as usize;
    let group_texts: Vec<String> = result
        .scout
        .file_groups
        .iter()
        .map(|g| {
            g.chunks
                .iter()
                .map(|c| c.signature.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    let group_text_refs: Vec<&str> = group_texts.iter().map(|s| s.as_str()).collect();
    let group_counts = super::count_tokens_batch(embedder, &group_text_refs);
    let (scout_indices, scout_used) = index_pack(&group_counts, scout_budget, overhead, |i| {
        result.scout.file_groups[i].relevance_score
    });
    remaining = remaining.saturating_sub(scout_used);

    // 2. Code section (50% + surplus) — pack gathered chunks by score
    let code_budget =
        (budget as f64 * 0.50) as usize + remaining.min(scout_budget.saturating_sub(scout_used));
    let code_texts: Vec<String> = result.code.iter().map(|c| c.content.clone()).collect();
    let code_text_refs: Vec<&str> = code_texts.iter().map(|s| s.as_str()).collect();
    let code_counts = super::count_tokens_batch(embedder, &code_text_refs);
    let (code_indices, code_used) = index_pack(&code_counts, code_budget, overhead, |i| {
        result.code[i].score
    });
    remaining = remaining.saturating_sub(code_used);

    // 3. Impact section (15% + surplus) — risk by score, tests by depth
    let impact_budget =
        (budget as f64 * 0.15) as usize + remaining.min(code_budget.saturating_sub(code_used));
    let risk_texts: Vec<String> = result
        .risk
        .iter()
        .map(|(name, r)| {
            format!(
                "{}: {:?} score:{:.1} callers:{} cov:{:.0}%",
                name,
                r.risk_level,
                r.score,
                r.caller_count,
                r.coverage * 100.0
            )
        })
        .collect();
    let risk_text_refs: Vec<&str> = risk_texts.iter().map(|s| s.as_str()).collect();
    let risk_counts = super::count_tokens_batch(embedder, &risk_text_refs);
    let (risk_indices, risk_used) = index_pack(&risk_counts, impact_budget, overhead, |i| {
        result.risk[i].1.score
    });

    let tests_budget = impact_budget.saturating_sub(risk_used);
    let test_texts: Vec<String> = result
        .tests
        .iter()
        .map(|t| {
            format!(
                "{} {}:{} depth:{}",
                t.name,
                t.file.display(),
                t.line,
                t.call_depth
            )
        })
        .collect();
    let test_text_refs: Vec<&str> = test_texts.iter().map(|s| s.as_str()).collect();
    let test_counts = super::count_tokens_batch(embedder, &test_text_refs);
    let (test_indices, tests_used) = index_pack(&test_counts, tests_budget, overhead, |i| {
        1.0 / (result.tests[i].call_depth as f32 + 1.0)
    });
    remaining = remaining.saturating_sub(risk_used + tests_used);

    // 4. Placement section (10% + surplus)
    let placement_budget = (budget as f64 * 0.10) as usize
        + remaining.min(impact_budget.saturating_sub(risk_used + tests_used));
    let placement_texts: Vec<String> = result
        .placement
        .iter()
        .map(|s| {
            format!(
                "{}: {} line:{} near:{}",
                s.file.display(),
                s.reason,
                s.insertion_line,
                s.near_function
            )
        })
        .collect();
    let placement_text_refs: Vec<&str> = placement_texts.iter().map(|s| s.as_str()).collect();
    let placement_counts = super::count_tokens_batch(embedder, &placement_text_refs);
    let (placement_indices, placement_used) =
        index_pack(&placement_counts, placement_budget, overhead, |i| {
            result.placement[i].score
        });
    remaining = remaining.saturating_sub(placement_used);

    // 5. Notes section (10% + surplus)
    let notes_budget = (budget as f64 * 0.10) as usize + remaining;
    let note_texts: Vec<&str> = result
        .scout
        .relevant_notes
        .iter()
        .map(|n| n.text.as_str())
        .collect();
    let note_counts = super::count_tokens_batch(embedder, &note_texts);
    let (note_indices, notes_used) = index_pack(&note_counts, notes_budget, overhead, |i| {
        result.scout.relevant_notes[i].sentiment.abs()
    });

    let total_used = scout_used + code_used + risk_used + tests_used + placement_used + notes_used;

    tracing::info!(
        total = total_used,
        budget,
        scout = scout_used,
        code = code_used,
        risk = risk_used,
        tests = tests_used,
        placement = placement_used,
        notes = notes_used,
        "Waterfall budget complete"
    );

    let packed = PackedSections {
        scout: scout_indices,
        code: code_indices,
        risk: risk_indices,
        tests: test_indices,
        placement: placement_indices,
        notes: note_indices,
        total_used,
        budget,
    };

    if json {
        output_json_budgeted(result, root, &packed)?;
    } else {
        output_text_budgeted(result, root, &packed);
    }

    Ok(())
}

/// Packed section indices from waterfall budgeting.
struct PackedSections {
    scout: Vec<usize>,
    code: Vec<usize>,
    risk: Vec<usize>,
    tests: Vec<usize>,
    placement: Vec<usize>,
    notes: Vec<usize>,
    total_used: usize,
    budget: usize,
}

fn output_json_budgeted(
    result: &cqs::TaskResult,
    root: &std::path::Path,
    packed: &PackedSections,
) -> Result<()> {
    let scout_json = build_scout_json(result, root, &packed.scout);
    let code_json = build_code_json(result, root, &packed.code);
    let risk_json = build_risk_json(result, &packed.risk);
    let tests_json = build_tests_json(result, root, &packed.tests);
    let placement_json = build_placement_json(result, root, &packed.placement);
    let notes_json = build_notes_json(result, &packed.notes);

    let output = serde_json::json!({
        "description": result.description,
        "scout": scout_json,
        "code": code_json,
        "risk": risk_json,
        "tests": tests_json,
        "placement": placement_json,
        "notes": notes_json,
        "summary": {
            "total_files": result.summary.total_files,
            "total_functions": result.summary.total_functions,
            "modify_targets": result.summary.modify_targets,
            "high_risk_count": result.summary.high_risk_count,
            "test_count": result.summary.test_count,
            "stale_count": result.summary.stale_count,
        },
        "token_count": packed.total_used,
        "token_budget": packed.budget,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn build_scout_json(
    result: &cqs::TaskResult,
    root: &std::path::Path,
    indices: &[usize],
) -> serde_json::Value {
    let groups: Vec<serde_json::Value> = indices
        .iter()
        .map(|&i| {
            let g = &result.scout.file_groups[i];
            let chunks: Vec<serde_json::Value> = g
                .chunks
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "name": c.name,
                        "chunk_type": c.chunk_type.to_string(),
                        "signature": c.signature,
                        "line_start": c.line_start,
                        "role": match c.role {
                            cqs::ChunkRole::ModifyTarget => "modify_target",
                            cqs::ChunkRole::TestToUpdate => "test_to_update",
                            cqs::ChunkRole::Dependency => "dependency",
                        },
                        "caller_count": c.caller_count,
                        "test_count": c.test_count,
                        "search_score": c.search_score,
                    })
                })
                .collect();
            serde_json::json!({
                "file": cqs::rel_display(&g.file, root),
                "relevance_score": g.relevance_score,
                "is_stale": g.is_stale,
                "chunks": chunks,
            })
        })
        .collect();
    serde_json::json!({
        "file_groups": groups,
        "summary": {
            "total_files": result.scout.summary.total_files,
            "total_functions": result.scout.summary.total_functions,
            "untested_count": result.scout.summary.untested_count,
            "stale_count": result.scout.summary.stale_count,
        }
    })
}

fn build_code_json(
    result: &cqs::TaskResult,
    root: &std::path::Path,
    indices: &[usize],
) -> Vec<serde_json::Value> {
    indices
        .iter()
        .map(|&i| {
            let c = &result.code[i];
            serde_json::json!({
                "name": c.name,
                "file": cqs::rel_display(&c.file, root),
                "line_start": c.line_start,
                "line_end": c.line_end,
                "language": c.language.to_string(),
                "chunk_type": c.chunk_type.to_string(),
                "signature": c.signature,
                "content": c.content,
                "score": c.score,
                "depth": c.depth,
            })
        })
        .collect()
}

fn build_risk_json(result: &cqs::TaskResult, indices: &[usize]) -> Vec<serde_json::Value> {
    indices
        .iter()
        .map(|&i| {
            let (name, r) = &result.risk[i];
            serde_json::json!({
                "name": name,
                "risk_level": format!("{:?}", r.risk_level),
                "blast_radius": format!("{:?}", r.blast_radius),
                "score": r.score,
                "caller_count": r.caller_count,
                "test_count": r.test_count,
                "coverage": r.coverage,
            })
        })
        .collect()
}

fn build_tests_json(
    result: &cqs::TaskResult,
    root: &std::path::Path,
    indices: &[usize],
) -> Vec<serde_json::Value> {
    indices
        .iter()
        .map(|&i| {
            let t = &result.tests[i];
            serde_json::json!({
                "name": t.name,
                "file": cqs::rel_display(&t.file, root),
                "line": t.line,
                "call_depth": t.call_depth,
            })
        })
        .collect()
}

fn build_placement_json(
    result: &cqs::TaskResult,
    root: &std::path::Path,
    indices: &[usize],
) -> Vec<serde_json::Value> {
    indices
        .iter()
        .map(|&i| {
            let s = &result.placement[i];
            serde_json::json!({
                "file": cqs::rel_display(&s.file, root),
                "score": s.score,
                "insertion_line": s.insertion_line,
                "near_function": s.near_function,
                "reason": s.reason,
            })
        })
        .collect()
}

fn build_notes_json(result: &cqs::TaskResult, indices: &[usize]) -> Vec<serde_json::Value> {
    indices
        .iter()
        .map(|&i| {
            let n = &result.scout.relevant_notes[i];
            serde_json::json!({
                "text": n.text,
                "sentiment": n.sentiment,
                "mentions": n.mentions,
            })
        })
        .collect()
}

fn output_text_budgeted(result: &cqs::TaskResult, root: &std::path::Path, packed: &PackedSections) {
    print_header(
        &result.description,
        &result.summary,
        packed.total_used,
        packed.budget,
    );
    print_scout_section(result, root, &packed.scout);
    print_code_section_idx(&result.code, root, &packed.code, result.code.len());
    print_impact_section_idx(&result.risk, &result.tests, &packed.risk, &packed.tests);
    print_placement_section_idx(
        &result.placement,
        root,
        &packed.placement,
        result.placement.len(),
    );
    print_notes_section_idx(
        &result.scout.relevant_notes,
        &packed.notes,
        result.scout.relevant_notes.len(),
    );
}

fn output_text(result: &cqs::TaskResult, root: &std::path::Path) {
    let all_scout: Vec<usize> = (0..result.scout.file_groups.len()).collect();
    print_header(&result.description, &result.summary, 0, 0);
    print_scout_section(result, root, &all_scout);

    let all_code: Vec<usize> = (0..result.code.len()).collect();
    print_code_section_idx(&result.code, root, &all_code, result.code.len());

    let all_risk: Vec<usize> = (0..result.risk.len()).collect();
    let all_tests: Vec<usize> = (0..result.tests.len()).collect();
    print_impact_section_idx(&result.risk, &result.tests, &all_risk, &all_tests);

    let all_placement: Vec<usize> = (0..result.placement.len()).collect();
    print_placement_section_idx(
        &result.placement,
        root,
        &all_placement,
        result.placement.len(),
    );

    let all_notes: Vec<usize> = (0..result.scout.relevant_notes.len()).collect();
    print_notes_section_idx(
        &result.scout.relevant_notes,
        &all_notes,
        result.scout.relevant_notes.len(),
    );
}

fn print_header(description: &str, summary: &cqs::TaskSummary, used: usize, budget: usize) {
    let token_label = if budget > 0 {
        format!(" ({} of {} tokens)", used, budget)
    } else {
        String::new()
    };
    println!(
        "{} {}{}",
        "═══ Task:".cyan().bold(),
        description.bold(),
        token_label.dimmed()
    );
    println!(
        "  {} targets | {} files | {} tests | {} high-risk",
        summary.modify_targets.to_string().bold(),
        summary.total_files,
        summary.test_count,
        summary.high_risk_count
    );
}

fn print_scout_section(result: &cqs::TaskResult, root: &std::path::Path, indices: &[usize]) {
    if indices.is_empty() {
        return;
    }
    println!();
    println!("{}", "── Scout ──────────────────────────────".cyan());
    let total = result.scout.file_groups.len();
    for &i in indices {
        let g = &result.scout.file_groups[i];
        let rel = cqs::rel_display(&g.file, root);
        print!(
            "  {} {}",
            rel.bold(),
            format!("({:.2})", g.relevance_score).dimmed()
        );
        if g.is_stale {
            print!(" {}", "[STALE]".yellow().bold());
        }
        println!();
        for c in &g.chunks {
            let role = match c.role {
                cqs::ChunkRole::ModifyTarget => "modify",
                cqs::ChunkRole::TestToUpdate => "test",
                cqs::ChunkRole::Dependency => "dep",
            };
            println!(
                "    {} {} {} {}",
                "▸".dimmed(),
                c.name,
                format!("({})", role).dimmed(),
                format!("callers:{} tests:{}", c.caller_count, c.test_count).dimmed()
            );
        }
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more files truncated)", total - indices.len()).dimmed()
        );
    }
}

fn print_code_section_idx(
    code: &[cqs::GatheredChunk],
    root: &std::path::Path,
    indices: &[usize],
    total: usize,
) {
    if indices.is_empty() {
        return;
    }
    println!();
    println!("{}", "── Code ───────────────────────────────".cyan());
    for &i in indices {
        let c = &code[i];
        let rel = cqs::rel_display(&c.file, root);
        println!("  {} {}:{}", c.name.bold(), rel, c.line_start);
        if !c.signature.is_empty() {
            println!("    {}", c.signature.dimmed());
        }
        let lines: Vec<&str> = c.content.lines().take(5).collect();
        for line in &lines {
            println!("    {}", line);
        }
        if c.content.lines().count() > 5 {
            println!("    {}", "...".dimmed());
        }
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more items truncated)", total - indices.len()).dimmed()
        );
    }
}

fn print_impact_section_idx(
    risk: &[(String, cqs::RiskScore)],
    tests: &[cqs::TestInfo],
    risk_idx: &[usize],
    test_idx: &[usize],
) {
    if risk_idx.is_empty() && test_idx.is_empty() {
        return;
    }
    if !risk_idx.is_empty() {
        println!();
        println!("{}", "── Impact ─────────────────────────────".cyan());
        for &i in risk_idx {
            let (name, r) = &risk[i];
            let level = match r.risk_level {
                cqs::RiskLevel::High => format!("{:?}", r.risk_level).red().bold().to_string(),
                cqs::RiskLevel::Medium => format!("{:?}", r.risk_level).yellow().to_string(),
                cqs::RiskLevel::Low => format!("{:?}", r.risk_level).green().to_string(),
            };
            println!(
                "  {}: {} {}",
                name,
                level,
                format!(
                    "(score: {:.1}, callers: {}, coverage: {:.0}%)",
                    r.score,
                    r.caller_count,
                    r.coverage * 100.0
                )
                .dimmed()
            );
        }
        if risk_idx.len() < risk.len() {
            println!(
                "  {}",
                format!(
                    "({} more risk entries truncated)",
                    risk.len() - risk_idx.len()
                )
                .dimmed()
            );
        }
    }

    if !test_idx.is_empty() {
        println!();
        println!("{}", "── Tests ──────────────────────────────".cyan());
        for &i in test_idx {
            let t = &tests[i];
            let rel = cqs::rel_display(&t.file, std::path::Path::new(""));
            println!(
                "  {} {}:{} {}",
                t.name,
                rel,
                t.line,
                format!("depth:{}", t.call_depth).dimmed()
            );
        }
        if test_idx.len() < tests.len() {
            println!(
                "  {}",
                format!("({} more tests truncated)", tests.len() - test_idx.len()).dimmed()
            );
        }
    }
}

fn print_placement_section_idx(
    placement: &[cqs::FileSuggestion],
    root: &std::path::Path,
    indices: &[usize],
    total: usize,
) {
    if indices.is_empty() {
        return;
    }
    println!();
    println!("{}", "── Placement ──────────────────────────".cyan());
    for &i in indices {
        let s = &placement[i];
        let rel = cqs::rel_display(&s.file, root);
        println!("  {} — {}", rel.bold(), s.reason.dimmed());
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more suggestions truncated)", total - indices.len()).dimmed()
        );
    }
}

fn print_notes_section_idx(notes: &[cqs::store::NoteSummary], indices: &[usize], total: usize) {
    if indices.is_empty() {
        return;
    }
    println!();
    println!("{}", "── Notes ──────────────────────────────".cyan());
    for &i in indices {
        let n = &notes[i];
        let sentiment = if n.sentiment < 0.0 {
            format!("[{:.1}]", n.sentiment).red().to_string()
        } else if n.sentiment > 0.0 {
            format!("[+{:.1}]", n.sentiment).green().to_string()
        } else {
            "[0.0]".dimmed().to_string()
        };
        let text = if n.text.len() > 80 {
            format!("{}...", &n.text[..n.text.floor_char_boundary(77)])
        } else {
            n.text.clone()
        };
        println!("  {} {}", sentiment, text.dimmed());
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more notes truncated)", total - indices.len()).dimmed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_waterfall_allocation_percentages() {
        let total = 0.15 + 0.50 + 0.15 + 0.10 + 0.10;
        assert!(
            (total - 1.0_f64).abs() < 0.001,
            "Budget percentages must sum to 1.0"
        );
    }

    #[test]
    fn test_waterfall_section_budgets() {
        let budget: usize = 1000;
        let scout = (budget as f64 * 0.15) as usize;
        let code = (budget as f64 * 0.50) as usize;
        let impact = (budget as f64 * 0.15) as usize;
        let placement = (budget as f64 * 0.10) as usize;
        let notes = (budget as f64 * 0.10) as usize;
        assert_eq!(scout + code + impact + placement + notes, budget);
    }

    #[test]
    fn test_index_pack_empty() {
        let (indices, used) = index_pack(&[], 100, 0, |_| 1.0);
        assert!(indices.is_empty());
        assert_eq!(used, 0);
    }

    #[test]
    fn test_index_pack_all_fit() {
        let counts = vec![10, 20, 30];
        let (indices, used) = index_pack(&counts, 100, 0, |_| 1.0);
        assert_eq!(indices, vec![0, 1, 2]);
        assert_eq!(used, 60);
    }

    #[test]
    fn test_index_pack_budget_forces_selection() {
        let counts = vec![10, 10, 10, 10, 10];
        // Scores: 0=1.0, 1=5.0, 2=3.0, 3=4.0, 4=2.0
        // Budget 30 fits 3 items → picks indices 1, 3, 2 (by score), sorted → [1, 2, 3]
        let (indices, used) = index_pack(&counts, 30, 0, |i| match i {
            0 => 1.0,
            1 => 5.0,
            2 => 3.0,
            3 => 4.0,
            4 => 2.0,
            _ => 0.0,
        });
        assert_eq!(indices.len(), 3);
        assert_eq!(used, 30);
        assert!(indices.contains(&1));
        assert!(indices.contains(&2));
        assert!(indices.contains(&3));
    }

    #[test]
    fn test_index_pack_preserves_order() {
        let counts = vec![10, 10, 10];
        // Budget fits 2 → picks highest score items, returned in original order
        let (indices, _) = index_pack(&counts, 20, 0, |i| match i {
            0 => 1.0,
            1 => 3.0,
            2 => 2.0,
            _ => 0.0,
        });
        assert_eq!(indices, vec![1, 2]); // original order, not score order
    }

    #[test]
    fn test_index_pack_always_includes_one() {
        let counts = vec![100]; // over budget
        let (indices, used) = index_pack(&counts, 10, 0, |_| 1.0);
        assert_eq!(indices, vec![0]);
        assert_eq!(used, 100);
    }

    #[test]
    fn test_index_pack_with_overhead() {
        let counts = vec![10, 10, 10];
        // With overhead 35, each item costs 45. Budget 100 fits 2 (90), not 3 (135)
        let (indices, used) = index_pack(&counts, 100, 35, |_| 1.0);
        assert_eq!(indices.len(), 2);
        assert_eq!(used, 90);
    }
}
