//! Batch command handlers — one function per BatchCmd variant.

use std::collections::HashMap;

use anyhow::{Context, Result};

use super::commands::BatchInput;
use super::BatchContext;
use crate::cli::{validate_finite_f32, DeadConfidenceLevel};

// ─── Handlers ────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_search(
    ctx: &BatchContext,
    query: &str,
    limit: usize,
    name_only: bool,
    semantic_only: bool,
    rerank: bool,
    lang: Option<String>,
    path: Option<String>,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_search", query).entered();

    if name_only {
        let results = ctx.store.search_by_name(query, limit.clamp(1, 100))?;
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.chunk.name,
                    "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                    "line_start": r.chunk.line_start,
                    "line_end": r.chunk.line_end,
                    "language": r.chunk.language.to_string(),
                    "chunk_type": r.chunk.chunk_type.to_string(),
                    "signature": r.chunk.signature,
                    "score": r.score,
                })
            })
            .collect();
        return Ok(serde_json::json!({
            "results": json_results,
            "query": query,
            "total": json_results.len(),
        }));
    }

    let embedder = ctx.embedder()?;
    let query_embedding = embedder
        .embed_query(query)
        .context("Failed to embed query")?;

    let languages = match &lang {
        Some(l) => Some(vec![l
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid language '{}'", l))?]),
        None => None,
    };

    let limit = limit.clamp(1, 100);
    let effective_limit = if rerank { (limit * 4).min(100) } else { limit };

    let filter = cqs::SearchFilter {
        languages,
        chunk_types: None,
        path_pattern: path,
        name_boost: 0.2,
        query_text: query.to_string(),
        enable_rrf: !semantic_only,
        note_weight: 1.0,
        note_only: false,
    };

    // Check audit mode (cached per session)
    let audit_mode = ctx.audit_state();
    let index = ctx.vector_index()?;

    let results = if audit_mode.is_active() {
        let code_results = ctx.store.search_filtered_with_index(
            &query_embedding,
            &filter,
            effective_limit,
            0.3,
            index,
        )?;
        code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect()
    } else {
        ctx.store.search_unified_with_index(
            &query_embedding,
            &filter,
            effective_limit,
            0.3,
            index,
        )?
    };

    // Re-rank if requested
    let results = if rerank && results.len() > 1 {
        let mut code_results = Vec::new();
        let mut note_results = Vec::new();
        for r in results {
            match r {
                cqs::store::UnifiedResult::Code(sr) => code_results.push(sr),
                note @ cqs::store::UnifiedResult::Note(_) => note_results.push(note),
            }
        }
        if code_results.len() > 1 {
            let reranker = ctx.reranker()?;
            reranker
                .rerank(query, &mut code_results, limit)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        }
        let mut out: Vec<cqs::store::UnifiedResult> = code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect();
        out.extend(note_results);
        out.truncate(limit);
        out
    } else {
        results
    };

    // Token-budget packing
    let (results, token_info) = if let Some(budget) = tokens {
        let embedder = ctx.embedder()?;
        let texts: Vec<&str> = results
            .iter()
            .map(|r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr.chunk.content.as_str(),
                cqs::store::UnifiedResult::Note(nr) => nr.note.text.as_str(),
            })
            .collect();
        let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
        let (packed, used) = crate::cli::commands::token_pack(
            results,
            &counts,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
            |r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr.score,
                cqs::store::UnifiedResult::Note(nr) => nr.score,
            },
        );
        (packed, Some((used, budget)))
    } else {
        (results, None)
    };

    let json_results: Vec<serde_json::Value> = results
        .iter()
        .map(|r| match r {
            cqs::store::UnifiedResult::Code(sr) => serde_json::json!({
                "name": sr.chunk.name,
                "file": sr.chunk.file.to_string_lossy().replace('\\', "/"),
                "line_start": sr.chunk.line_start,
                "line_end": sr.chunk.line_end,
                "language": sr.chunk.language.to_string(),
                "chunk_type": sr.chunk.chunk_type.to_string(),
                "signature": sr.chunk.signature,
                "score": sr.score,
                "content": sr.chunk.content,
            }),
            cqs::store::UnifiedResult::Note(nr) => serde_json::json!({
                "type": "note",
                "text": nr.note.text,
                "score": nr.score,
                "sentiment": nr.note.sentiment,
            }),
        })
        .collect();

    let mut response = serde_json::json!({
        "results": json_results,
        "query": query,
        "total": json_results.len(),
    });
    if let Some((used, budget)) = token_info {
        response["token_count"] = serde_json::json!(used);
        response["token_budget"] = serde_json::json!(budget);
    }
    Ok(response)
}

pub(super) fn dispatch_deps(
    ctx: &BatchContext,
    name: &str,
    reverse: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_deps", name, reverse).entered();

    if reverse {
        let types = ctx.store.get_types_used_by(name)?;
        Ok(serde_json::json!({
            "function": name,
            "types": types.iter().map(|t| {
                serde_json::json!({"type_name": t.type_name, "edge_kind": t.edge_kind})
            }).collect::<Vec<_>>(),
            "count": types.len(),
        }))
    } else {
        let users = ctx.store.get_type_users(name)?;
        let json_users: Vec<serde_json::Value> = users
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "file": cqs::rel_display(&c.file, &ctx.root),
                    "line_start": c.line_start,
                    "chunk_type": c.chunk_type.to_string(),
                })
            })
            .collect();
        Ok(serde_json::json!(json_users))
    }
}

pub(super) fn dispatch_callers(ctx: &BatchContext, name: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_callers", name).entered();
    let callers = ctx.store.get_callers_full(name)?;
    let json_callers: Vec<serde_json::Value> = callers
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": c.file.to_string_lossy().replace('\\', "/"),
                "line": c.line,
            })
        })
        .collect();
    Ok(serde_json::json!(json_callers))
}

pub(super) fn dispatch_callees(ctx: &BatchContext, name: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_callees", name).entered();
    let callees = ctx.store.get_callees_full(name, None)?;
    Ok(serde_json::json!({
        "function": name,
        "calls": callees.iter().map(|(n, line)| {
            serde_json::json!({"name": n, "line": line})
        }).collect::<Vec<_>>(),
        "count": callees.len(),
    }))
}

pub(super) fn dispatch_explain(
    ctx: &BatchContext,
    target: &str,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_explain", target).entered();

    let resolved = cqs::resolve_target(&ctx.store, target)?;
    let chunk = &resolved.chunk;

    let callers = match ctx.store.get_callers_full(&chunk.name) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, name = chunk.name, "Failed to get callers in explain");
            vec![]
        }
    };
    let chunk_file = chunk.file.to_string_lossy();
    let callees = match ctx.store.get_callees_full(&chunk.name, Some(&chunk_file)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, name = chunk.name, "Failed to get callees in explain");
            vec![]
        }
    };

    // Similar (top 3)
    let similar = match ctx.store.get_chunk_with_embedding(&chunk.id)? {
        Some((_, embedding)) => {
            let filter = cqs::SearchFilter {
                languages: None,
                chunk_types: None,
                path_pattern: None,
                name_boost: 0.0,
                query_text: String::new(),
                enable_rrf: false,
                note_weight: 0.0,
                note_only: false,
            };
            let index = ctx.vector_index()?;
            let sim_results = ctx
                .store
                .search_filtered_with_index(&embedding, &filter, 4, 0.3, index)?;
            sim_results
                .into_iter()
                .filter(|r| r.chunk.id != chunk.id)
                .take(3)
                .collect::<Vec<_>>()
        }
        None => vec![],
    };

    // Token budget: decide which content to include
    let (include_target_content, similar_content_set, token_info) = if let Some(budget) = tokens {
        let embedder = ctx.embedder()?;
        let target_tokens =
            crate::cli::commands::count_tokens(embedder, &chunk.content, &chunk.name);
        let remaining = budget.saturating_sub(target_tokens);

        let indexed: Vec<(usize, f32)> = similar
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.score))
            .collect();
        let texts: Vec<&str> = indexed
            .iter()
            .map(|&(i, _)| similar[i].chunk.content.as_str())
            .collect();
        let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
        let (packed, sim_used) =
            crate::cli::commands::token_pack(indexed, &counts, remaining, 0, |&(_, score)| score);
        let sim_included: std::collections::HashSet<String> = packed
            .into_iter()
            .map(|(i, _)| similar[i].chunk.id.clone())
            .collect();

        let used = target_tokens + sim_used;
        (true, Some(sim_included), Some((used, budget)))
    } else {
        (false, None, None)
    };

    let callers_json: Vec<_> = callers
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": c.file.to_string_lossy().replace('\\', "/"),
                "line": c.line,
            })
        })
        .collect();

    let callees_json: Vec<_> = callees
        .iter()
        .map(|(name, line)| serde_json::json!({"name": name, "line": line}))
        .collect();

    let similar_json: Vec<_> = similar
        .iter()
        .map(|r| {
            let mut obj = serde_json::json!({
                "name": r.chunk.name,
                "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                "score": r.score,
            });
            if let Some(ref set) = similar_content_set {
                if set.contains(&r.chunk.id) {
                    obj["content"] = serde_json::json!(r.chunk.content);
                }
            }
            obj
        })
        .collect();

    let rel_file = cqs::rel_display(&chunk.file, &ctx.root);

    let mut output = serde_json::json!({
        "name": chunk.name,
        "file": rel_file,
        "language": chunk.language.to_string(),
        "chunk_type": chunk.chunk_type.to_string(),
        "lines": [chunk.line_start, chunk.line_end],
        "signature": chunk.signature,
        "doc": chunk.doc,
        "callers": callers_json,
        "callees": callees_json,
        "similar": similar_json,
    });
    if include_target_content {
        output["content"] = serde_json::json!(chunk.content);
    }
    if let Some((used, budget)) = token_info {
        output["token_count"] = serde_json::json!(used);
        output["token_budget"] = serde_json::json!(budget);
    }
    Ok(output)
}

pub(super) fn dispatch_similar(
    ctx: &BatchContext,
    target: &str,
    limit: usize,
    threshold: f32,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_similar", target).entered();
    let threshold = validate_finite_f32(threshold, "threshold")?;
    let limit = limit.clamp(1, 100);

    let resolved = cqs::resolve_target(&ctx.store, target)?;
    let chunk = &resolved.chunk;

    let (source_chunk, embedding) = ctx
        .store
        .get_chunk_with_embedding(&chunk.id)?
        .ok_or_else(|| anyhow::anyhow!("Could not load embedding for '{}'", chunk.name))?;

    let filter = cqs::SearchFilter {
        languages: None,
        chunk_types: None,
        path_pattern: None,
        name_boost: 0.0,
        query_text: String::new(),
        enable_rrf: false,
        note_weight: 0.0,
        note_only: false,
    };

    let index = ctx.vector_index()?;
    let results = ctx.store.search_filtered_with_index(
        &embedding,
        &filter,
        limit.saturating_add(1),
        threshold,
        index,
    )?;

    let filtered: Vec<_> = results
        .into_iter()
        .filter(|r| r.chunk.id != source_chunk.id)
        .take(limit)
        .collect();

    let json_results: Vec<serde_json::Value> = filtered
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.chunk.name,
                "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                "score": r.score,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "results": json_results,
        "target": chunk.name,
        "total": json_results.len(),
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_gather(
    ctx: &BatchContext,
    query: &str,
    expand: usize,
    direction: &str,
    limit: usize,
    tokens: Option<usize>,
    ref_name: Option<&str>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_gather", query, ?ref_name).entered();

    let embedder = ctx.embedder()?;
    let query_embedding = embedder
        .embed_query(query)
        .context("Failed to embed query")?;

    let dir: cqs::GatherDirection = direction
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{e}"))?;

    let opts = cqs::GatherOptions {
        expand_depth: expand.clamp(0, 5),
        direction: dir,
        limit: limit.clamp(1, 100),
        ..cqs::GatherOptions::default()
    };

    let result = if let Some(rn) = ref_name {
        ctx.get_ref(rn)?;
        let ref_idx = ctx
            .borrow_ref(rn)
            .ok_or_else(|| anyhow::anyhow!("Reference '{}' not loaded", rn))?;
        cqs::gather_cross_index(
            &ctx.store,
            &ref_idx,
            &query_embedding,
            query,
            &opts,
            &ctx.root,
        )?
    } else {
        cqs::gather(&ctx.store, &query_embedding, query, &opts, &ctx.root)?
    };

    // Token-budget packing
    let (chunks, token_info) = if let Some(budget) = tokens {
        let embedder = ctx.embedder()?;
        let texts: Vec<&str> = result.chunks.iter().map(|c| c.content.as_str()).collect();
        let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
        let (packed, used) = crate::cli::commands::token_pack(
            result.chunks,
            &counts,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
            |c| c.score,
        );
        (packed, Some((used, budget)))
    } else {
        (result.chunks, None)
    };

    let json_chunks: Vec<serde_json::Value> = chunks
        .iter()
        .map(|c| {
            let mut chunk_json = serde_json::json!({
                "name": c.name,
                "file": c.file.to_string_lossy().replace('\\', "/"),
                "line_start": c.line_start,
                "line_end": c.line_end,
                "language": c.language.to_string(),
                "chunk_type": c.chunk_type.to_string(),
                "signature": c.signature,
                "score": c.score,
                "depth": c.depth,
                "content": c.content,
            });
            if let Some(ref src) = c.source {
                chunk_json["source"] = serde_json::json!(src);
            }
            chunk_json
        })
        .collect();

    let mut response = serde_json::json!({
        "query": query,
        "chunks": json_chunks,
        "expansion_capped": result.expansion_capped,
        "search_degraded": result.search_degraded,
    });
    if let Some((used, budget)) = token_info {
        response["token_count"] = serde_json::json!(used);
        response["token_budget"] = serde_json::json!(budget);
    }
    Ok(response)
}

pub(super) fn dispatch_impact(
    ctx: &BatchContext,
    name: &str,
    depth: usize,
    do_suggest_tests: bool,
    include_types: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_impact", name).entered();

    let resolved = cqs::resolve_target(&ctx.store, name)?;
    let chunk = &resolved.chunk;
    let depth = depth.clamp(1, 10);

    let result = cqs::analyze_impact(&ctx.store, &chunk.name, depth, include_types)?;

    let mut json = cqs::impact_to_json(&result, &ctx.root);

    if do_suggest_tests {
        let suggestions = cqs::suggest_tests(&ctx.store, &result);
        let suggestions_json: Vec<_> = suggestions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "test_name": s.test_name,
                    "suggested_file": s.suggested_file,
                    "for_function": s.for_function,
                    "pattern_source": s.pattern_source,
                    "inline": s.inline,
                })
            })
            .collect();
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "test_suggestions".into(),
                serde_json::json!(suggestions_json),
            );
        }
    }

    Ok(json)
}

pub(super) fn dispatch_test_map(
    ctx: &BatchContext,
    name: &str,
    max_depth: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_test_map", name).entered();

    let resolved = cqs::resolve_target(&ctx.store, name)?;
    let target_name = resolved.chunk.name.clone();

    let graph = ctx.call_graph()?;
    let test_chunks = ctx.store.find_test_chunks()?;

    // Reverse BFS from target
    let mut ancestors: HashMap<String, (usize, String)> = HashMap::new();
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    ancestors.insert(target_name.clone(), (0, String::new()));
    queue.push_back((target_name.clone(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(&current) {
            for caller in callers {
                if !ancestors.contains_key(caller) {
                    ancestors.insert(caller.clone(), (depth + 1, current.clone()));
                    queue.push_back((caller.clone(), depth + 1));
                }
            }
        }
    }

    struct TestMatch {
        name: String,
        file: String,
        line: u32,
        depth: usize,
        chain: Vec<String>,
    }

    let mut matches: Vec<TestMatch> = Vec::new();
    for test in &test_chunks {
        if let Some((depth, _)) = ancestors.get(&test.name) {
            if *depth > 0 {
                let mut chain = Vec::new();
                let mut current = test.name.clone();
                while !current.is_empty() {
                    chain.push(current.clone());
                    if current == target_name {
                        break;
                    }
                    current = ancestors
                        .get(&current)
                        .map(|(_, p)| p.clone())
                        .unwrap_or_default();
                }
                let rel_file = cqs::rel_display(&test.file, &ctx.root);
                matches.push(TestMatch {
                    name: test.name.clone(),
                    file: rel_file,
                    line: test.line_start,
                    depth: *depth,
                    chain,
                });
            }
        }
    }

    matches.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.name.cmp(&b.name)));

    let tests_json: Vec<_> = matches
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "file": m.file,
                "line": m.line,
                "call_depth": m.depth,
                "call_chain": m.chain,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "function": target_name,
        "tests": tests_json,
        "test_count": matches.len(),
    }))
}

pub(super) fn dispatch_trace(
    ctx: &BatchContext,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_trace", source, target).entered();

    let source_resolved = cqs::resolve_target(&ctx.store, source)?;
    let target_resolved = cqs::resolve_target(&ctx.store, target)?;
    let source_name = source_resolved.chunk.name.clone();
    let target_name = target_resolved.chunk.name.clone();

    if source_name == target_name {
        let rel_file = cqs::rel_display(&source_resolved.chunk.file, &ctx.root);
        return Ok(serde_json::json!({
            "source": source_name,
            "target": target_name,
            "path": [{
                "name": source_name,
                "file": rel_file,
                "line": source_resolved.chunk.line_start,
                "signature": source_resolved.chunk.signature,
            }],
            "depth": 0,
        }));
    }

    let graph = ctx.call_graph()?;

    // BFS shortest path
    let mut visited: HashMap<String, String> = HashMap::new();
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    visited.insert(source_name.clone(), String::new());
    queue.push_back((source_name.clone(), 0));
    let mut found_path: Option<Vec<String>> = None;

    while let Some((current, depth)) = queue.pop_front() {
        if current == target_name {
            let mut path = vec![current.clone()];
            let mut node = &current;
            while let Some(pred) = visited.get(node) {
                if pred.is_empty() {
                    break;
                }
                path.push(pred.clone());
                node = pred;
            }
            path.reverse();
            found_path = Some(path);
            break;
        }
        if depth >= max_depth {
            continue;
        }
        if let Some(callees) = graph.forward.get(&current) {
            for callee in callees {
                if !visited.contains_key(callee) {
                    visited.insert(callee.clone(), current.clone());
                    queue.push_back((callee.clone(), depth + 1));
                }
            }
        }
    }

    match found_path {
        Some(names) => {
            // Batch lookup instead of N+1 queries (PERF-20)
            let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            let batch_results = ctx.store.search_by_names_batch(&name_refs, 1)?;

            let mut path_json = Vec::new();
            for name in &names {
                let entry = match batch_results.get(name.as_str()).and_then(|v| v.first()) {
                    Some(r) => {
                        let rel = cqs::rel_display(&r.chunk.file, &ctx.root);
                        serde_json::json!({
                            "name": name,
                            "file": rel,
                            "line": r.chunk.line_start,
                            "signature": r.chunk.signature,
                        })
                    }
                    None => serde_json::json!({"name": name}),
                };
                path_json.push(entry);
            }

            Ok(serde_json::json!({
                "source": source_name,
                "target": target_name,
                "path": path_json,
                "depth": names.len() - 1,
            }))
        }
        None => Ok(serde_json::json!({
            "source": source_name,
            "target": target_name,
            "path": null,
            "message": format!("No call path found within depth {}", max_depth),
        })),
    }
}

pub(super) fn dispatch_dead(
    ctx: &BatchContext,
    include_pub: bool,
    min_confidence: &DeadConfidenceLevel,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_dead").entered();

    let min_level: cqs::store::DeadConfidence = min_confidence.into();
    let (confident, possibly_pub) = ctx.store.find_dead_code(include_pub)?;

    let confident: Vec<_> = confident
        .into_iter()
        .filter(|d| d.confidence >= min_level)
        .collect();
    let possibly_pub: Vec<_> = possibly_pub
        .into_iter()
        .filter(|d| d.confidence >= min_level)
        .collect();

    let format_dead = |dead: &cqs::store::DeadFunction| {
        let confidence = match dead.confidence {
            cqs::store::DeadConfidence::High => "high",
            cqs::store::DeadConfidence::Medium => "medium",
            cqs::store::DeadConfidence::Low => "low",
        };
        serde_json::json!({
            "name": dead.chunk.name,
            "file": cqs::rel_display(&dead.chunk.file, &ctx.root),
            "line_start": dead.chunk.line_start,
            "line_end": dead.chunk.line_end,
            "chunk_type": dead.chunk.chunk_type.to_string(),
            "signature": dead.chunk.signature,
            "language": dead.chunk.language.to_string(),
            "confidence": confidence,
        })
    };

    Ok(serde_json::json!({
        "dead": confident.iter().map(&format_dead).collect::<Vec<_>>(),
        "possibly_dead_pub": possibly_pub.iter().map(&format_dead).collect::<Vec<_>>(),
        "total_dead": confident.len(),
        "total_possibly_dead_pub": possibly_pub.len(),
    }))
}

pub(super) fn dispatch_related(
    ctx: &BatchContext,
    name: &str,
    limit: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_related", name).entered();
    let limit = limit.clamp(1, 100);

    let result = cqs::find_related(&ctx.store, name, limit)?;

    let to_json = |items: &[cqs::RelatedFunction]| -> Vec<serde_json::Value> {
        items
            .iter()
            .map(|r| {
                let rel = cqs::rel_display(&r.file, &ctx.root);
                serde_json::json!({
                    "name": r.name,
                    "file": rel,
                    "line": r.line,
                    "overlap_count": r.overlap_count,
                })
            })
            .collect()
    };

    Ok(serde_json::json!({
        "target": result.target,
        "shared_callers": to_json(&result.shared_callers),
        "shared_callees": to_json(&result.shared_callees),
        "shared_types": to_json(&result.shared_types),
    }))
}

pub(super) fn dispatch_context(
    ctx: &BatchContext,
    path: &str,
    summary: bool,
    compact: bool,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_context", path).entered();

    let chunks = ctx.store.get_chunks_by_origin(path)?;
    if chunks.is_empty() {
        anyhow::bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    if compact {
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = ctx.store.get_caller_counts_batch(&names)?;
        let callee_counts = ctx.store.get_callee_counts_batch(&names)?;

        let entries: Vec<_> = chunks
            .iter()
            .map(|c| {
                let cc = caller_counts.get(&c.name).copied().unwrap_or(0);
                let ce = callee_counts.get(&c.name).copied().unwrap_or(0);
                serde_json::json!({
                    "name": c.name,
                    "chunk_type": c.chunk_type.to_string(),
                    "signature": c.signature,
                    "lines": [c.line_start, c.line_end],
                    "caller_count": cc,
                    "callee_count": ce,
                })
            })
            .collect();

        return Ok(serde_json::json!({
            "file": path,
            "chunks": entries,
            "total": entries.len(),
        }));
    }

    if summary {
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = ctx.store.get_caller_counts_batch(&names)?;
        let callee_counts = ctx.store.get_callee_counts_batch(&names)?;
        let total_callers: u64 = caller_counts.values().sum();
        let total_callees: u64 = callee_counts.values().sum();

        return Ok(serde_json::json!({
            "file": path,
            "chunk_count": chunks.len(),
            "total_callers": total_callers,
            "total_callees": total_callees,
        }));
    }

    // Full context — with optional token packing
    let (chunks, token_info) = if let Some(budget) = tokens {
        let embedder = ctx.embedder()?;
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = ctx.store.get_caller_counts_batch(&names)?;

        let indexed: Vec<(usize, f32)> = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let cc = caller_counts.get(&c.name).copied().unwrap_or(0) as f32;
                (i, cc)
            })
            .collect();
        let texts: Vec<&str> = indexed
            .iter()
            .map(|&(i, _)| chunks[i].content.as_str())
            .collect();
        let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
        let (packed, used) = crate::cli::commands::token_pack(
            indexed,
            &counts,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
            |&(_, cc)| cc,
        );
        let keep: std::collections::HashSet<usize> = packed.into_iter().map(|(i, _)| i).collect();
        let filtered: Vec<_> = chunks
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep.contains(i))
            .map(|(_, c)| c)
            .collect();
        (filtered, Some((used, budget)))
    } else {
        (chunks, None)
    };

    let entries: Vec<_> = chunks
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "chunk_type": c.chunk_type.to_string(),
                "language": c.language.to_string(),
                "lines": [c.line_start, c.line_end],
                "signature": c.signature,
                "content": c.content,
            })
        })
        .collect();

    let mut response = serde_json::json!({
        "file": path,
        "chunks": entries,
        "total": entries.len(),
    });
    if let Some((used, budget)) = token_info {
        response["token_count"] = serde_json::json!(used);
        response["token_budget"] = serde_json::json!(budget);
    }
    Ok(response)
}

pub(super) fn dispatch_stats(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stats").entered();
    let stats = ctx.store.stats()?;
    let note_count = ctx.store.note_count()?;
    let fc_stats = ctx.store.function_call_stats()?;
    let te_stats = ctx.store.type_edge_stats()?;
    let errors = ctx.error_count.load(std::sync::atomic::Ordering::Relaxed);

    Ok(serde_json::json!({
        "total_chunks": stats.total_chunks,
        "total_files": stats.total_files,
        "notes": note_count,
        "errors": errors,
        "call_graph": {
            "total_calls": fc_stats.total_calls,
            "unique_callers": fc_stats.unique_callers,
            "unique_callees": fc_stats.unique_callees,
        },
        "type_graph": {
            "total_edges": te_stats.total_edges,
            "unique_types": te_stats.unique_types,
        },
        "by_language": stats.chunks_by_language.iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect::<HashMap<String, _>>(),
        "by_type": stats.chunks_by_type.iter()
            .map(|(t, c)| (t.to_string(), c))
            .collect::<HashMap<String, _>>(),
        "model": stats.model_name,
        "schema_version": stats.schema_version,
    }))
}

pub(super) fn dispatch_onboard(
    ctx: &BatchContext,
    query: &str,
    depth: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_onboard", query, depth).entered();
    let embedder = ctx.embedder()?;
    let depth = depth.clamp(1, 5);
    let result = cqs::onboard(&ctx.store, embedder, query, &ctx.root, depth)?;

    if tokens.is_none() {
        return cqs::onboard_to_json(&result)
            .map_err(|e| anyhow::anyhow!("Failed to serialize onboard: {e}"));
    }
    let budget = tokens.unwrap();

    // Flatten entries with depth-based scores
    let mut entries: Vec<(String, f32)> = Vec::new();
    entries.push((result.entry_point.name.clone(), 1.0));
    for (i, c) in result.call_chain.iter().enumerate() {
        entries.push((c.name.clone(), 1.0 / (i as f32 + 2.0)));
    }
    for c in &result.callers {
        entries.push((c.name.clone(), 0.3));
    }

    // Batch-fetch content
    let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
    let chunks_by_name = match ctx.store.get_chunks_by_names_batch(&names) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-fetch chunks for onboard token packing");
            HashMap::new()
        }
    };

    let items: Vec<(String, String, f32)> = entries
        .into_iter()
        .filter_map(|(name, score)| {
            let content = chunks_by_name.get(name.as_str())?.first()?.content.clone();
            Some((name, content, score))
        })
        .collect();

    let texts: Vec<&str> = items.iter().map(|(_, c, _)| c.as_str()).collect();
    let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
    let (packed, used) =
        crate::cli::commands::token_pack(items, &counts, budget, 0, |&(_, _, s)| s);
    let content_map: HashMap<String, String> = packed
        .into_iter()
        .map(|(name, content, _)| (name, content))
        .collect();

    // Build JSON, injecting content for packed entries
    let mut json = cqs::onboard_to_json(&result)
        .map_err(|e| anyhow::anyhow!("Failed to serialize onboard: {e}"))?;

    // Inject content into entry_point
    if let Some(content) = content_map.get(&result.entry_point.name) {
        json["entry_point"]["content"] = serde_json::json!(content);
    }
    // Inject into call_chain
    if let Some(chain) = json.get_mut("call_chain").and_then(|v| v.as_array_mut()) {
        for (i, entry) in chain.iter_mut().enumerate() {
            if let Some(c) = result.call_chain.get(i) {
                if let Some(content) = content_map.get(&c.name) {
                    entry["content"] = serde_json::json!(content);
                }
            }
        }
    }
    // Inject into callers
    if let Some(callers) = json.get_mut("callers").and_then(|v| v.as_array_mut()) {
        for (i, entry) in callers.iter_mut().enumerate() {
            if let Some(c) = result.callers.get(i) {
                if let Some(content) = content_map.get(&c.name) {
                    entry["content"] = serde_json::json!(content);
                }
            }
        }
    }

    json["token_count"] = serde_json::json!(used);
    json["token_budget"] = serde_json::json!(budget);
    Ok(json)
}

pub(super) fn dispatch_scout(
    ctx: &BatchContext,
    query: &str,
    limit: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_scout", query).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 50);
    let result = cqs::scout(&ctx.store, embedder, query, &ctx.root, limit)?;

    if tokens.is_none() {
        return Ok(cqs::scout_to_json(&result, &ctx.root));
    }
    let budget = tokens.unwrap();

    // Batch-fetch content for all chunks
    let all_names: Vec<&str> = result
        .file_groups
        .iter()
        .flat_map(|g| g.chunks.iter().map(|c| c.name.as_str()))
        .collect();
    let chunks_by_name = match ctx.store.get_chunks_by_names_batch(&all_names) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-fetch chunks for scout token packing");
            HashMap::new()
        }
    };

    // Build (name, content, score) triples for packing
    let items: Vec<(String, String, f32)> = result
        .file_groups
        .iter()
        .flat_map(|g| {
            g.chunks.iter().filter_map(|c| {
                let content = chunks_by_name
                    .get(c.name.as_str())?
                    .first()?
                    .content
                    .clone();
                Some((c.name.clone(), content, g.relevance_score * c.search_score))
            })
        })
        .collect();

    let texts: Vec<&str> = items
        .iter()
        .map(|(_, content, _)| content.as_str())
        .collect();
    let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
    let (packed, used) =
        crate::cli::commands::token_pack(items, &counts, budget, 0, |&(_, _, score)| score);
    let content_map: HashMap<String, String> = packed
        .into_iter()
        .map(|(name, content, _)| (name, content))
        .collect();

    // Build JSON with content for packed items
    let mut json = cqs::scout_to_json(&result, &ctx.root);
    if let Some(groups) = json.get_mut("file_groups").and_then(|v| v.as_array_mut()) {
        for group in groups {
            if let Some(chunks) = group.get_mut("chunks").and_then(|v| v.as_array_mut()) {
                for chunk in chunks {
                    if let Some(name) = chunk.get("name").and_then(|v| v.as_str()) {
                        if let Some(content) = content_map.get(name) {
                            chunk["content"] = serde_json::json!(content);
                        }
                    }
                }
            }
        }
    }
    json["token_count"] = serde_json::json!(used);
    json["token_budget"] = serde_json::json!(budget);
    Ok(json)
}

pub(super) fn dispatch_where(
    ctx: &BatchContext,
    description: &str,
    limit: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_where", description).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 10);
    let result = cqs::suggest_placement(&ctx.store, embedder, description, limit)?;

    let suggestions_json: Vec<_> = result
        .suggestions
        .iter()
        .map(|s| {
            let rel = cqs::rel_display(&s.file, &ctx.root);
            serde_json::json!({
                "file": rel,
                "score": s.score,
                "insertion_line": s.insertion_line,
                "near_function": s.near_function,
                "reason": s.reason,
                "patterns": {
                    "imports": s.patterns.imports,
                    "error_handling": s.patterns.error_handling,
                    "naming_convention": s.patterns.naming_convention,
                    "visibility": s.patterns.visibility,
                    "has_inline_tests": s.patterns.has_inline_tests,
                }
            })
        })
        .collect();

    Ok(serde_json::json!({
        "description": description,
        "suggestions": suggestions_json,
    }))
}

pub(super) fn dispatch_read(
    ctx: &BatchContext,
    path: &str,
    focus: Option<&str>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_read", path).entered();

    // Focused read mode
    if let Some(focus) = focus {
        return dispatch_read_focused(ctx, focus);
    }

    let (file_path, content) = crate::cli::commands::read::validate_and_read_file(&ctx.root, path)?;

    let audit_state = ctx.audit_state();
    let (header, notes_injected) = crate::cli::commands::read::build_file_note_header(
        path,
        &file_path,
        audit_state,
        ctx.notes(),
    );

    let enriched = if header.is_empty() {
        content
    } else {
        format!("{}{}", header, content)
    };

    Ok(serde_json::json!({
        "path": path,
        "content": enriched,
        "notes_injected": notes_injected,
    }))
}

fn dispatch_read_focused(ctx: &BatchContext, focus: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_read_focused", focus).entered();

    let audit_state = ctx.audit_state();
    let result = crate::cli::commands::read::build_focused_output(
        &ctx.store,
        focus,
        &ctx.root,
        audit_state,
        ctx.notes(),
    )?;

    let mut json = serde_json::json!({
        "focus": focus,
        "content": result.output,
    });
    if let Some(ref h) = result.hints {
        json["hints"] = serde_json::json!({
            "caller_count": h.caller_count,
            "test_count": h.test_count,
            "no_callers": h.caller_count == 0,
            "no_tests": h.test_count == 0,
        });
    }

    Ok(json)
}

pub(super) fn dispatch_stale(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stale").entered();

    let file_set = ctx.file_set()?;
    let report = ctx.store.list_stale_files(file_set)?;

    let stale_json: Vec<_> = report
        .stale
        .iter()
        .map(|f| {
            serde_json::json!({
                "origin": f.origin,
                "stored_mtime": f.stored_mtime,
                "current_mtime": f.current_mtime,
            })
        })
        .collect();

    let missing_json: Vec<_> = report
        .missing
        .iter()
        .map(|origin| serde_json::json!(origin))
        .collect();

    Ok(serde_json::json!({
        "stale": stale_json,
        "missing": missing_json,
        "total_indexed": report.total_indexed,
        "stale_count": report.stale.len(),
        "missing_count": report.missing.len(),
    }))
}

pub(super) fn dispatch_health(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_health").entered();

    let file_set = ctx.file_set()?;
    let report = cqs::health::health_check(&ctx.store, file_set, &ctx.cqs_dir)?;

    let hotspots_json: Vec<_> = report
        .hotspots
        .iter()
        .map(|(name, count)| serde_json::json!({"name": name, "caller_count": count}))
        .collect();

    let untested_json: Vec<_> = report
        .untested_hotspots
        .iter()
        .map(|(name, count)| serde_json::json!({"name": name, "caller_count": count}))
        .collect();

    Ok(serde_json::json!({
        "stats": {
            "total_chunks": report.stats.total_chunks,
            "total_files": report.stats.total_files,
        },
        "stale_count": report.stale_count,
        "missing_count": report.missing_count,
        "dead_confident": report.dead_confident,
        "dead_possible": report.dead_possible,
        "hotspots": hotspots_json,
        "untested_hotspots": untested_json,
        "note_count": report.note_count,
        "note_warnings": report.note_warnings,
        "warnings": report.warnings,
    }))
}

pub(super) fn dispatch_drift(
    ctx: &BatchContext,
    reference: &str,
    threshold: f32,
    min_drift: f32,
    lang: Option<&str>,
    limit: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_drift", reference).entered();
    let threshold = validate_finite_f32(threshold, "threshold")?;
    let min_drift = validate_finite_f32(min_drift, "min_drift")?;

    // Use cached reference store (PERF-27/RM-17)
    ctx.get_ref(reference)?;
    let ref_idx = ctx
        .borrow_ref(reference)
        .ok_or_else(|| anyhow::anyhow!("Reference '{}' not loaded", reference))?;

    let result = cqs::drift::detect_drift(
        &ref_idx.store,
        &ctx.store,
        reference,
        threshold,
        min_drift,
        lang,
    )?;

    let mut drifted_json: Vec<_> = result
        .drifted
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file,
                "chunk_type": e.chunk_type,
                "similarity": e.similarity,
                "drift": e.drift,
            })
        })
        .collect();
    if let Some(lim) = limit {
        drifted_json.truncate(lim);
    }

    Ok(serde_json::json!({
        "reference": result.reference,
        "threshold": result.threshold,
        "min_drift": result.min_drift,
        "drifted": drifted_json,
        "total_compared": result.total_compared,
        "unchanged": result.unchanged,
    }))
}

pub(super) fn dispatch_notes(
    ctx: &BatchContext,
    warnings: bool,
    patterns: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_notes", warnings, patterns).entered();

    let notes = ctx.notes();
    let filtered: Vec<_> = notes
        .iter()
        .filter(|n| {
            if warnings {
                n.is_warning()
            } else if patterns {
                n.is_pattern()
            } else {
                true
            }
        })
        .map(|n| {
            serde_json::json!({
                "text": n.text,
                "sentiment": n.sentiment,
                "sentiment_label": n.sentiment_label(),
                "mentions": n.mentions,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "notes": filtered,
        "total": filtered.len(),
    }))
}

pub(super) fn dispatch_task(
    ctx: &BatchContext,
    description: &str,
    limit: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_task", description).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 10);
    let result = cqs::task(&ctx.store, embedder, description, &ctx.root, limit)?;

    let mut json = cqs::task_to_json(&result, &ctx.root);

    // Apply token budgeting if requested (simplified: pack code section only)
    if let Some(budget) = tokens {
        let code_texts: Vec<&str> = result.code.iter().map(|c| c.content.as_str()).collect();
        let counts = crate::cli::commands::count_tokens_batch(embedder, &code_texts);
        let (packed_idx, used) = crate::cli::commands::token_pack(
            (0..result.code.len()).collect::<Vec<_>>(),
            &counts,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
            |&i| result.code[i].score,
        );

        // Replace code array with packed subset
        let code_json: Vec<serde_json::Value> = packed_idx
            .iter()
            .map(|&i| {
                let c = &result.code[i];
                serde_json::json!({
                    "name": c.name,
                    "file": cqs::rel_display(&c.file, &ctx.root),
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
            .collect();
        json["code"] = serde_json::json!(code_json);
        json["token_count"] = serde_json::json!(used);
        json["token_budget"] = serde_json::json!(budget);
    }

    Ok(json)
}

pub(super) fn dispatch_help() -> Result<serde_json::Value> {
    use clap::CommandFactory;
    let mut buf = Vec::new();
    BatchInput::command().write_help(&mut buf)?;
    let help_text = String::from_utf8_lossy(&buf).to_string();
    Ok(serde_json::json!({"help": help_text}))
}
