//! Context command — module-level understanding
//!
//! Core logic is in shared functions (`build_compact_data`, `build_full_data`,
//! `compact_to_json`, `full_to_json`) so batch mode can reuse them without
//! duplicating ~120 lines.

use anyhow::{bail, Context as _, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use cqs::store::{ChunkSummary, Store};

use crate::cli::staleness;

// ─── Shared core ────────────────────────────────────────────────────────────

/// Compact mode data: signatures + caller/callee counts per chunk.
pub(crate) struct CompactData {
    pub chunks: Vec<ChunkSummary>,
    pub caller_counts: HashMap<String, u64>,
    pub callee_counts: HashMap<String, u64>,
}

/// Build compact-mode data: chunks with caller/callee counts.
pub(crate) fn build_compact_data(store: &Store, path: &str) -> Result<CompactData> {
    let _span = tracing::info_span!("build_compact_data", path).entered();
    let chunks = store
        .get_chunks_by_origin(path)
        .context("Failed to load chunks for file")?;
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }
    let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    let caller_counts = store.get_caller_counts_batch(&names)?;
    let callee_counts = store.get_callee_counts_batch(&names)?;
    Ok(CompactData {
        chunks,
        caller_counts,
        callee_counts,
    })
}

/// Typed output for compact context mode.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CompactOutput<'a> {
    pub file: &'a str,
    pub chunk_count: usize,
    pub chunks: Vec<CompactChunkEntry>,
}

/// A single chunk in compact context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CompactChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub signature: String,
    pub line_start: u32,
    pub line_end: u32,
    pub caller_count: u64,
    pub callee_count: u64,
}

/// Serialize compact data to JSON.
pub(crate) fn compact_to_json(data: &CompactData, path: &str) -> serde_json::Value {
    let entries: Vec<_> = data
        .chunks
        .iter()
        .map(|c| {
            let cc = data.caller_counts.get(&c.name).copied().unwrap_or(0);
            let ce = data.callee_counts.get(&c.name).copied().unwrap_or(0);
            CompactChunkEntry {
                name: c.name.clone(),
                chunk_type: c.chunk_type.to_string(),
                signature: c.signature.clone(),
                line_start: c.line_start,
                line_end: c.line_end,
                caller_count: cc,
                callee_count: ce,
            }
        })
        .collect();
    let output = CompactOutput {
        file: path,
        chunk_count: data.chunks.len(),
        chunks: entries,
    };
    serde_json::to_value(&output).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to serialize CompactOutput");
        serde_json::json!({})
    })
}

/// Full mode data: chunks with external callers, callees, and dependent files.
pub(crate) struct FullData {
    pub chunks: Vec<ChunkSummary>,
    /// (caller_name, caller_file_rel, callee_name, line)
    pub external_callers: Vec<(String, String, String, u32)>,
    /// (callee_name, called_from)
    pub external_callees: Vec<(String, String)>,
    pub dependent_files: HashSet<String>,
}

/// Build full-mode data: chunks with external callers/callees/dependent files.
/// Shared between CLI summary mode (uses counts) and full mode (uses details).
pub(crate) fn build_full_data(store: &Store, path: &str, root: &Path) -> Result<FullData> {
    let _span = tracing::info_span!("build_full_data", path).entered();
    let chunks = store
        .get_chunks_by_origin(path)
        .context("Failed to load chunks for file")?;
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    let chunk_names: HashSet<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    let names_vec: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();

    // Batch-fetch callers and callees for all chunks
    let callers_by_callee = store
        .get_callers_full_batch(&names_vec)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch callers for context");
            HashMap::new()
        });
    let callees_by_caller = store
        .get_callees_full_batch(&names_vec)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch callees for context");
            HashMap::new()
        });

    // Collect external callers
    let mut external_callers = Vec::new();
    let mut dependent_files: HashSet<String> = HashSet::new();
    for chunk in &chunks {
        let callers = callers_by_callee
            .get(&chunk.name)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for caller in callers {
            let caller_origin = caller.file.to_string_lossy().to_string();
            if !caller_origin.ends_with(path) {
                let rel = cqs::rel_display(&caller.file, root);
                external_callers.push((
                    caller.name.clone(),
                    rel.clone(),
                    chunk.name.clone(),
                    caller.line,
                ));
                dependent_files.insert(rel);
            }
        }
    }

    // Collect external callees
    let mut external_callees = Vec::new();
    let mut seen_callees: HashSet<String> = HashSet::new();
    for chunk in &chunks {
        let callees = callees_by_caller
            .get(&chunk.name)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for (callee_name, _) in callees {
            if !chunk_names.contains(callee_name.as_str())
                && seen_callees.insert(callee_name.clone())
            {
                external_callees.push((callee_name.clone(), chunk.name.clone()));
            }
        }
    }

    Ok(FullData {
        chunks,
        external_callers,
        external_callees,
        dependent_files,
    })
}

/// Typed output for full context mode.
#[derive(Debug, serde::Serialize)]
pub(crate) struct FullOutput<'a> {
    pub file: &'a str,
    pub chunks: Vec<FullChunkEntry>,
    pub external_callers: Vec<ExternalCallerEntry>,
    pub external_callees: Vec<ExternalCalleeEntry>,
    pub dependent_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<usize>,
}

/// A chunk in full context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct FullChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub signature: String,
    pub line_start: u32,
    pub line_end: u32,
    pub doc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// An external caller in full context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ExternalCallerEntry {
    pub name: String,
    pub file: String,
    pub calls: String,
    pub line_start: u32,
}

/// An external callee in full context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ExternalCalleeEntry {
    pub name: String,
    pub called_from: String,
}

/// Serialize full data to JSON, optionally including content within a token budget.
/// When `content_set` is `Some`, only chunks whose names are in the set include content.
/// When `None`, no content is included.
pub(crate) fn full_to_json(
    data: &FullData,
    path: &str,
    content_set: Option<&HashSet<String>>,
    token_info: Option<(usize, usize)>,
) -> serde_json::Value {
    let chunks: Vec<_> = data
        .chunks
        .iter()
        .map(|c| {
            let content =
                content_set.and_then(|set| set.contains(&c.name).then(|| c.content.clone()));
            FullChunkEntry {
                name: c.name.clone(),
                chunk_type: c.chunk_type.to_string(),
                signature: c.signature.clone(),
                line_start: c.line_start,
                line_end: c.line_end,
                doc: c.doc.clone(),
                content,
            }
        })
        .collect();
    let callers: Vec<_> = data
        .external_callers
        .iter()
        .map(|(caller_name, file, calls, line)| ExternalCallerEntry {
            name: caller_name.clone(),
            file: file.clone(),
            calls: calls.clone(),
            line_start: *line,
        })
        .collect();
    let callees: Vec<_> = data
        .external_callees
        .iter()
        .map(|(callee_name, from)| ExternalCalleeEntry {
            name: callee_name.clone(),
            called_from: from.clone(),
        })
        .collect();
    let mut dep_files: Vec<String> = data.dependent_files.iter().cloned().collect();
    dep_files.sort();

    let output = FullOutput {
        file: path,
        chunks,
        external_callers: callers,
        external_callees: callees,
        dependent_files: dep_files,
        token_count: token_info.map(|(used, _)| used),
        token_budget: token_info.map(|(_, budget)| budget),
    };
    serde_json::to_value(&output).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to serialize FullOutput");
        serde_json::json!({})
    })
}

/// Pack chunks by relevance (caller count descending) within a token budget.
/// Returns the set of included chunk names and total tokens used.
pub(crate) fn pack_by_relevance(
    chunks: &[ChunkSummary],
    caller_counts: &HashMap<String, u64>,
    budget: usize,
    embedder: &cqs::Embedder,
) -> (HashSet<String>, usize) {
    let _pack_span = tracing::info_span!("token_pack_context", budget).entered();

    // Build (index, caller_count) pairs for token_pack to sort by
    let indexed: Vec<(usize, u64)> = (0..chunks.len())
        .map(|i| {
            let cc = caller_counts.get(&chunks[i].name).copied().unwrap_or(0);
            (i, cc)
        })
        .collect();
    let texts: Vec<&str> = indexed
        .iter()
        .map(|&(i, _)| chunks[i].content.as_str())
        .collect();
    let token_counts = crate::cli::commands::count_tokens_batch(embedder, &texts);

    let (packed, used) =
        crate::cli::commands::token_pack(indexed, &token_counts, budget, 0, |&(_, cc)| cc as f32);

    let included: HashSet<String> = packed
        .into_iter()
        .map(|(i, _)| chunks[i].name.clone())
        .collect();
    (included, used)
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_context(
    ctx: &crate::cli::CommandContext,
    path: &str,
    json: bool,
    summary: bool,
    compact: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_context", path, ?max_tokens).entered();
    let store = &ctx.store;
    let root = &ctx.root;

    // --tokens is incompatible with --compact and --summary (those modes are deliberately minimal)
    if max_tokens.is_some() && (compact || summary) {
        bail!("--tokens cannot be used with --compact or --summary");
    }

    // Compact mode: signatures-only TOC with caller/callee counts
    if compact {
        let data = build_compact_data(store, path)?;

        // Proactive staleness warning
        if !ctx.cli.quiet && !ctx.cli.no_stale_check {
            staleness::warn_stale_results(store, &[path], root);
        }

        if json {
            let output = compact_to_json(&data, path);
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            print_compact_terminal(&data, path);
        }
        return Ok(());
    }

    // Summary and full modes need external caller/callee data
    let data = build_full_data(store, path, root)?;

    // Proactive staleness warning
    if !ctx.cli.quiet && !ctx.cli.no_stale_check {
        staleness::warn_stale_results(store, &[path], root);
    }

    if summary {
        if json {
            let output = summary_to_json(&data, path);
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            print_summary_terminal(&data, path);
        }
    } else if json {
        let (content_set, token_info) =
            build_token_pack(store, &data.chunks, max_tokens, ctx.model_config())?;
        let output = full_to_json(&data, path, content_set.as_ref(), token_info);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let (content_set, token_info) =
            build_token_pack(store, &data.chunks, max_tokens, ctx.model_config())?;
        print_full_terminal(&data, path, content_set.as_ref(), token_info);
    }

    Ok(())
}

/// Build token-packed content set if max_tokens is requested.
#[allow(clippy::type_complexity)]
fn build_token_pack(
    store: &Store,
    chunks: &[ChunkSummary],
    max_tokens: Option<usize>,
    model_config: &cqs::embedder::ModelConfig,
) -> Result<(Option<HashSet<String>>, Option<(usize, usize)>)> {
    let Some(budget) = max_tokens else {
        return Ok((None, None));
    };
    let embedder = cqs::Embedder::new(model_config.clone())?;
    let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    let caller_counts = store.get_caller_counts_batch(&names).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to fetch caller counts for token packing");
        HashMap::new()
    });
    let (included, used) = pack_by_relevance(chunks, &caller_counts, budget, &embedder);
    tracing::info!(
        chunks = included.len(),
        tokens = used,
        budget,
        "Token-budgeted context"
    );
    Ok((Some(included), Some((used, budget))))
}

/// Typed output for summary context mode.
#[derive(Debug, serde::Serialize)]
pub(crate) struct SummaryOutput<'a> {
    pub file: &'a str,
    pub chunk_count: usize,
    pub chunks: Vec<SummaryChunkEntry>,
    pub external_caller_count: usize,
    pub external_callee_count: usize,
    pub dependent_files: Vec<String>,
}

/// A chunk in summary context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct SummaryChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub line_start: u32,
    pub line_end: u32,
}

pub(crate) fn summary_to_json(data: &FullData, path: &str) -> serde_json::Value {
    let chunks: Vec<_> = data
        .chunks
        .iter()
        .map(|c| SummaryChunkEntry {
            name: c.name.clone(),
            chunk_type: c.chunk_type.to_string(),
            line_start: c.line_start,
            line_end: c.line_end,
        })
        .collect();
    let mut dep_files: Vec<String> = data.dependent_files.iter().cloned().collect();
    dep_files.sort();
    let output = SummaryOutput {
        file: path,
        chunk_count: data.chunks.len(),
        chunks,
        external_caller_count: data.external_callers.len(),
        external_callee_count: data.external_callees.len(),
        dependent_files: dep_files,
    };
    serde_json::to_value(&output).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to serialize SummaryOutput");
        serde_json::json!({})
    })
}

fn print_compact_terminal(data: &CompactData, path: &str) {
    use colored::Colorize;
    println!("{} ({} chunks)", path.bold(), data.chunks.len());
    for c in &data.chunks {
        let cc = data.caller_counts.get(&c.name).copied().unwrap_or(0);
        let ce = data.callee_counts.get(&c.name).copied().unwrap_or(0);
        let sig = if c.signature.is_empty() {
            c.name.clone()
        } else {
            c.signature.clone()
        };
        let caller_label = if cc == 1 { "caller" } else { "callers" };
        let callee_label = if ce == 1 { "callee" } else { "callees" };
        println!(
            "  {}  [{} {}, {} {}]",
            sig.dimmed(),
            cc,
            caller_label,
            ce,
            callee_label,
        );
    }
}

fn print_summary_terminal(data: &FullData, path: &str) {
    use colored::Colorize;
    println!("{} {}", "Context summary:".cyan(), path.bold());
    println!("  Chunks: {}", data.chunks.len());
    for c in &data.chunks {
        println!(
            "    {} {} (:{}-{})",
            c.chunk_type, c.name, c.line_start, c.line_end
        );
    }
    println!("  External callers: {}", data.external_callers.len());
    println!("  External callees: {}", data.external_callees.len());
    if !data.dependent_files.is_empty() {
        let mut dep_files: Vec<&String> = data.dependent_files.iter().collect();
        dep_files.sort();
        println!("  Dependent files:");
        for f in dep_files {
            println!("    {}", f);
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::language::{ChunkType, Language};
    use cqs::store::ChunkSummary;
    use std::path::PathBuf;

    fn make_chunk(name: &str, line_start: u32, line_end: u32) -> ChunkSummary {
        ChunkSummary {
            id: format!("id_{name}"),
            file: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content: format!("fn {name}() {{ }}"),
            doc: Some(format!("Doc for {name}")),
            line_start,
            line_end,
            content_hash: format!("hash_{name}"),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
        }
    }

    // ===== HP-1: compact_to_json tests =====

    #[test]
    fn hp1_compact_to_json_all_fields() {
        let chunks = vec![make_chunk("foo", 1, 10), make_chunk("bar", 12, 20)];
        let mut caller_counts = HashMap::new();
        caller_counts.insert("foo".to_string(), 3);
        caller_counts.insert("bar".to_string(), 0);
        let mut callee_counts = HashMap::new();
        callee_counts.insert("foo".to_string(), 1);
        callee_counts.insert("bar".to_string(), 5);

        let data = CompactData {
            chunks,
            caller_counts,
            callee_counts,
        };
        let json = compact_to_json(&data, "src/lib.rs");

        // Top-level fields
        assert_eq!(json["file"], "src/lib.rs");
        assert_eq!(json["chunk_count"], 2);

        // First chunk: verify all CompactChunkEntry fields
        let c0 = &json["chunks"][0];
        assert_eq!(c0["name"], "foo");
        assert_eq!(c0["chunk_type"], "function");
        assert_eq!(c0["signature"], "fn foo()");
        assert_eq!(c0["line_start"], 1);
        assert_eq!(c0["line_end"], 10);
        assert_eq!(c0["caller_count"], 3);
        assert_eq!(c0["callee_count"], 1);

        // Second chunk: zero callers
        let c1 = &json["chunks"][1];
        assert_eq!(c1["name"], "bar");
        assert_eq!(c1["caller_count"], 0);
        assert_eq!(c1["callee_count"], 5);
    }

    #[test]
    fn hp1_compact_to_json_missing_counts_default_to_zero() {
        // When caller/callee counts maps are empty, should default to 0
        let chunks = vec![make_chunk("orphan", 1, 5)];
        let data = CompactData {
            chunks,
            caller_counts: HashMap::new(),
            callee_counts: HashMap::new(),
        };
        let json = compact_to_json(&data, "src/orphan.rs");

        assert_eq!(json["chunks"][0]["caller_count"], 0);
        assert_eq!(json["chunks"][0]["callee_count"], 0);
    }

    // ===== HP-1: full_to_json tests =====

    #[test]
    fn hp1_full_to_json_all_fields() {
        let chunks = vec![make_chunk("process", 5, 25)];
        let external_callers = vec![(
            "main".to_string(),
            "src/main.rs".to_string(),
            "process".to_string(),
            3u32,
        )];
        let external_callees = vec![("validate".to_string(), "process".to_string())];
        let mut dependent_files = HashSet::new();
        dependent_files.insert("src/main.rs".to_string());

        let data = FullData {
            chunks,
            external_callers,
            external_callees,
            dependent_files,
        };
        let json = full_to_json(&data, "src/lib.rs", None, None);

        // Top-level
        assert_eq!(json["file"], "src/lib.rs");

        // Chunks
        let chunks_arr = json["chunks"].as_array().unwrap();
        assert_eq!(chunks_arr.len(), 1);
        let c0 = &chunks_arr[0];
        assert_eq!(c0["name"], "process");
        assert_eq!(c0["chunk_type"], "function");
        assert_eq!(c0["signature"], "fn process()");
        assert_eq!(c0["line_start"], 5);
        assert_eq!(c0["line_end"], 25);
        assert_eq!(c0["doc"], "Doc for process");
        assert!(
            c0.get("content").is_none(),
            "content should be absent when content_set is None"
        );

        // External callers
        let callers = json["external_callers"].as_array().unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["name"], "main");
        assert_eq!(callers[0]["file"], "src/main.rs");
        assert_eq!(callers[0]["calls"], "process");
        assert_eq!(callers[0]["line_start"], 3);

        // External callees
        let callees = json["external_callees"].as_array().unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0]["name"], "validate");
        assert_eq!(callees[0]["called_from"], "process");

        // Dependent files
        let dep = json["dependent_files"].as_array().unwrap();
        assert_eq!(dep.len(), 1);
        assert_eq!(dep[0], "src/main.rs");

        // Token fields absent when not provided
        assert!(
            json.get("token_count").is_none(),
            "token_count should be absent when token_info is None"
        );
        assert!(
            json.get("token_budget").is_none(),
            "token_budget should be absent when token_info is None"
        );
    }

    #[test]
    fn hp1_full_to_json_with_token_info() {
        let data = FullData {
            chunks: vec![make_chunk("f", 1, 5)],
            external_callers: vec![],
            external_callees: vec![],
            dependent_files: HashSet::new(),
        };
        let json = full_to_json(&data, "src/lib.rs", None, Some((150, 500)));

        assert_eq!(json["token_count"], 150);
        assert_eq!(json["token_budget"], 500);
    }

    #[test]
    fn hp1_full_to_json_with_content_set() {
        let chunks = vec![
            make_chunk("included", 1, 10),
            make_chunk("excluded", 12, 20),
        ];
        let data = FullData {
            chunks,
            external_callers: vec![],
            external_callees: vec![],
            dependent_files: HashSet::new(),
        };
        let mut content_set = HashSet::new();
        content_set.insert("included".to_string());

        let json = full_to_json(&data, "src/lib.rs", Some(&content_set), None);

        let chunks_arr = json["chunks"].as_array().unwrap();
        assert!(
            chunks_arr[0]["content"].is_string(),
            "included chunk should have content"
        );
        assert_eq!(chunks_arr[0]["content"], "fn included() { }");
        assert!(
            chunks_arr[1].get("content").is_none(),
            "excluded chunk should not have content"
        );
    }

    // ===== HP-1: summary_to_json tests =====

    #[test]
    fn hp1_summary_to_json_all_fields() {
        let data = FullData {
            chunks: vec![make_chunk("a", 1, 10), make_chunk("b", 15, 30)],
            external_callers: vec![
                (
                    "caller1".to_string(),
                    "src/c.rs".to_string(),
                    "a".to_string(),
                    5,
                ),
                (
                    "caller2".to_string(),
                    "src/d.rs".to_string(),
                    "b".to_string(),
                    8,
                ),
            ],
            external_callees: vec![("ext_fn".to_string(), "a".to_string())],
            dependent_files: {
                let mut s = HashSet::new();
                s.insert("src/c.rs".to_string());
                s.insert("src/d.rs".to_string());
                s
            },
        };
        let json = summary_to_json(&data, "src/lib.rs");

        assert_eq!(json["file"], "src/lib.rs");
        assert_eq!(json["chunk_count"], 2);
        assert_eq!(json["external_caller_count"], 2);
        assert_eq!(json["external_callee_count"], 1);

        // Chunks have minimal fields
        let chunks = json["chunks"].as_array().unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0]["name"], "a");
        assert_eq!(chunks[0]["chunk_type"], "function");
        assert_eq!(chunks[0]["line_start"], 1);
        assert_eq!(chunks[0]["line_end"], 10);
        // Summary chunks should NOT have signature, doc, content, caller_count etc.
        assert!(chunks[0].get("signature").is_none());
        assert!(chunks[0].get("doc").is_none());
        assert!(chunks[0].get("content").is_none());

        // Dependent files sorted
        let dep = json["dependent_files"].as_array().unwrap();
        assert_eq!(dep.len(), 2);
        assert_eq!(dep[0], "src/c.rs");
        assert_eq!(dep[1], "src/d.rs");
    }

    #[test]
    fn hp1_compact_chunk_count_matches_array_length() {
        // HP-5 gap: chunk_count should match chunks array length
        let chunks = vec![
            make_chunk("a", 1, 5),
            make_chunk("b", 6, 10),
            make_chunk("c", 11, 15),
        ];
        let data = CompactData {
            chunks,
            caller_counts: HashMap::new(),
            callee_counts: HashMap::new(),
        };
        let json = compact_to_json(&data, "src/lib.rs");

        let arr_len = json["chunks"].as_array().unwrap().len();
        let count_field = json["chunk_count"].as_u64().unwrap();
        assert_eq!(
            count_field, arr_len as u64,
            "chunk_count field must equal chunks array length"
        );
    }
}

fn print_full_terminal(
    data: &FullData,
    path: &str,
    content_set: Option<&HashSet<String>>,
    token_info: Option<(usize, usize)>,
) {
    use colored::Colorize;

    let token_label = match token_info {
        Some((used, budget)) => format!(" ({} of {} tokens)", used, budget),
        None => String::new(),
    };
    println!("{} {}{}", "Context for:".cyan(), path.bold(), token_label);
    println!();

    println!("{}", "Chunks:".cyan());
    for c in &data.chunks {
        println!(
            "  {} {} (:{}-{})",
            c.chunk_type,
            c.name.bold(),
            c.line_start,
            c.line_end
        );
        if !c.signature.is_empty() {
            println!("    {}", c.signature.dimmed());
        }
        // Print content if within token budget
        if let Some(included) = content_set {
            if included.contains(&c.name) {
                println!("{}", "\u{2500}".repeat(50));
                println!("{}", c.content);
                println!();
            }
        }
    }

    if !data.external_callers.is_empty() {
        println!();
        println!("{}", "External callers:".cyan());
        for (name, file, calls, line) in &data.external_callers {
            println!("  {} ({}:{}) -> {}", name, file, line, calls);
        }
    }

    if !data.external_callees.is_empty() {
        println!();
        println!("{}", "External callees:".cyan());
        for (name, from) in &data.external_callees {
            println!("  {} <- {}", name, from);
        }
    }

    if !data.dependent_files.is_empty() {
        println!();
        println!("{}", "Dependent files:".cyan());
        let mut files: Vec<&String> = data.dependent_files.iter().collect();
        files.sort();
        for f in files {
            println!("  {}", f);
        }
    }
}
