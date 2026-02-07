//! Impact command — what breaks if you change a function

use anyhow::Result;

use std::collections::{HashMap, HashSet, VecDeque};

use cqs::Store;

use crate::cli::find_project_root;

use super::resolve::resolve_target;

pub(crate) fn cmd_impact(
    _cli: &crate::cli::Cli,
    name: &str,
    depth: usize,
    json: bool,
) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

    if !index_path.exists() {
        anyhow::bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;
    let depth = depth.clamp(1, 10);

    // Resolve target
    let (chunk, _) = resolve_target(&store, name)?;
    let target_name = chunk.name.clone();

    // Get callers with call-site context
    let callers_ctx = store.get_callers_with_context(&target_name)?;

    // Build caller info with snippets
    struct CallerDisplay {
        name: String,
        file: std::path::PathBuf,
        line: u32,
        call_line: u32,
        snippet: Option<String>,
    }

    let mut callers: Vec<CallerDisplay> = Vec::new();
    for caller in &callers_ctx {
        let snippet = store
            .search_by_name(&caller.name, 1)
            .ok()
            .and_then(|r| r.into_iter().next())
            .and_then(|r| {
                let lines: Vec<&str> = r.chunk.content.lines().collect();
                let offset = caller.call_line.saturating_sub(r.chunk.line_start) as usize;
                if offset < lines.len() {
                    let start = offset.saturating_sub(1);
                    let end = (offset + 2).min(lines.len());
                    Some(lines[start..end].join("\n"))
                } else {
                    None
                }
            });

        callers.push(CallerDisplay {
            name: caller.name.clone(),
            file: caller.file.clone(),
            line: caller.line,
            call_line: caller.call_line,
            snippet,
        });
    }

    // Find tests via reverse BFS
    let graph = store.get_call_graph()?;
    let test_chunks = store.find_test_chunks()?;

    // Reverse BFS from target to find all ancestors
    let mut ancestors: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    ancestors.insert(target_name.clone(), 0);
    queue.push_back((target_name.clone(), 0));

    while let Some((current, d)) = queue.pop_front() {
        if d >= 5 {
            continue;
        }
        if let Some(rev_callers) = graph.reverse.get(&current) {
            for caller in rev_callers {
                if !ancestors.contains_key(caller) {
                    ancestors.insert(caller.clone(), d + 1);
                    queue.push_back((caller.clone(), d + 1));
                }
            }
        }
    }

    // Intersect ancestors with test set
    struct TestDisplay {
        name: String,
        file: std::path::PathBuf,
        line: u32,
        call_depth: usize,
    }

    let mut tests: Vec<TestDisplay> = Vec::new();
    for test in &test_chunks {
        if let Some(&d) = ancestors.get(&test.name) {
            if d > 0 {
                tests.push(TestDisplay {
                    name: test.name.clone(),
                    file: test.file.clone(),
                    line: test.line_start,
                    call_depth: d,
                });
            }
        }
    }
    tests.sort_by_key(|t| t.call_depth);

    // Transitive callers (depth > 1)
    struct TransitiveCaller {
        name: String,
        file: std::path::PathBuf,
        line: u32,
        depth: usize,
    }

    let transitive_callers = if depth > 1 {
        let mut result: Vec<TransitiveCaller> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(target_name.clone());
        let mut q: VecDeque<(String, usize)> = VecDeque::new();
        q.push_back((target_name.clone(), 0));

        while let Some((current, d)) = q.pop_front() {
            if d >= depth {
                continue;
            }
            if let Some(rev_callers) = graph.reverse.get(&current) {
                for caller_name in rev_callers {
                    if visited.insert(caller_name.clone()) {
                        if let Some(r) = store
                            .search_by_name(caller_name, 1)
                            .ok()
                            .and_then(|r| r.into_iter().next())
                        {
                            result.push(TransitiveCaller {
                                name: caller_name.clone(),
                                file: r.chunk.file,
                                line: r.chunk.line_start,
                                depth: d + 1,
                            });
                        }
                        q.push_back((caller_name.clone(), d + 1));
                    }
                }
            }
        }
        result
    } else {
        Vec::new()
    };

    if json {
        let callers_json: Vec<_> = callers
            .iter()
            .map(|c| {
                let rel = c
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&c.file)
                    .to_string_lossy()
                    .replace('\\', "/");
                serde_json::json!({
                    "name": c.name,
                    "file": rel,
                    "line": c.line,
                    "call_line": c.call_line,
                    "snippet": c.snippet,
                })
            })
            .collect();

        let tests_json: Vec<_> = tests
            .iter()
            .map(|t| {
                let rel = t
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&t.file)
                    .to_string_lossy()
                    .replace('\\', "/");
                serde_json::json!({
                    "name": t.name,
                    "file": rel,
                    "line": t.line,
                    "call_depth": t.call_depth,
                })
            })
            .collect();

        let mut output = serde_json::json!({
            "function": chunk.name,
            "callers": callers_json,
            "tests": tests_json,
            "caller_count": callers_json.len(),
            "test_count": tests_json.len(),
        });

        if depth > 1 {
            let trans_json: Vec<_> = transitive_callers
                .iter()
                .map(|c| {
                    let rel = c
                        .file
                        .strip_prefix(&root)
                        .unwrap_or(&c.file)
                        .to_string_lossy()
                        .replace('\\', "/");
                    serde_json::json!({
                        "name": c.name,
                        "file": rel,
                        "line": c.line,
                        "depth": c.depth,
                    })
                })
                .collect();
            output
                .as_object_mut()
                .unwrap()
                .insert("transitive_callers".into(), serde_json::json!(trans_json));
        }

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;

        println!(
            "{} ({})",
            chunk.name.bold(),
            chunk
                .file
                .strip_prefix(&root)
                .unwrap_or(&chunk.file)
                .display()
        );

        // Direct callers
        if callers.is_empty() {
            println!();
            println!("{}", "No callers found.".dimmed());
        } else {
            println!();
            println!("{} ({}):", "Callers".cyan(), callers.len());
            for c in &callers {
                let rel = c.file.strip_prefix(&root).unwrap_or(&c.file);
                println!(
                    "  {} ({}:{}, call at line {})",
                    c.name,
                    rel.display(),
                    c.line,
                    c.call_line
                );
                if let Some(ref snippet) = c.snippet {
                    for line in snippet.lines() {
                        println!("    {}", line.dimmed());
                    }
                }
            }
        }

        // Transitive callers
        if depth > 1 && !transitive_callers.is_empty() {
            println!();
            println!(
                "{} ({}):",
                "Transitive Callers".cyan(),
                transitive_callers.len()
            );
            for c in &transitive_callers {
                let rel = c.file.strip_prefix(&root).unwrap_or(&c.file);
                println!(
                    "  {} ({}:{}) [depth {}]",
                    c.name,
                    rel.display(),
                    c.line,
                    c.depth
                );
            }
        }

        // Tests
        if tests.is_empty() {
            println!();
            println!("{}", "No affected tests found.".dimmed());
        } else {
            println!();
            println!("{} ({}):", "Affected Tests".yellow(), tests.len());
            for t in &tests {
                let rel = t.file.strip_prefix(&root).unwrap_or(&t.file);
                println!(
                    "  {} ({}:{}) [depth {}]",
                    t.name,
                    rel.display(),
                    t.line,
                    t.call_depth
                );
            }
        }
    }

    Ok(())
}
