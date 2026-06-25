//! Explain command — generate a function card
//!
//! Core logic is in `build_explain_data()` so batch mode can reuse it
//! without duplicating ~130 lines.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;

use cqs::index::VectorIndex;
use cqs::store::{CallerInfo, ChunkSummary, SearchResult, Store};
use cqs::{compute_hints, rel_display, FunctionHints, HnswIndex, SearchFilter};

use super::callers::{CalleeEntry, CallerEntry};
use crate::cli::staleness;

// ─── Wire core (MCP-ready, input-only) ──────────────────────────────────────

/// Surface-agnostic request core for `cqs explain` — the struct the daemon
/// JSON-args path (`cqs_explain`) deserializes the relayed `arguments` into,
/// then projects onto the clap-side [`crate::cli::args::ExplainArgs`] for
/// dispatch. Distinct from that clap struct because the clap one flattens the
/// shared `LimitArg` (not `Deserialize`/`JsonSchema`); this flat core mirrors
/// the `WhereArgs` / `PlanArgs` wire-core precedent.
///
/// Input-only: it derives `Deserialize` + `JsonSchema` for the wire, never
/// `Serialize`. The command's JSON OUTPUT is the separate [`ExplainOutput`], so
/// adding these derives cannot change the output wire shape. `name` is the lone
/// required field (the target to explain); `limit` defaults to the clap
/// `LimitArg` default and `tokens` is optional, so a wire caller can supply just
/// `name`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExplainArgs {
    /// Function name or `file:function` to explain.
    pub name: String,
    /// Cap on the callers / callees / similar lists in the function card
    /// (clamped to `GRAPH_LIMIT_CAP` in the adapter). Defaults to 5, matching
    /// the CLI `-n` default.
    #[serde(default = "default_explain_limit")]
    pub limit: usize,
    /// Maximum token budget. When set, relevant source bodies (the target plus
    /// each similar chunk that fits) are packed within the budget.
    #[serde(default)]
    pub tokens: Option<usize>,
}

/// Default for [`ExplainArgs::limit`] — mirrors the clap `LimitArg` default (5),
/// so an omitted `limit` over the wire matches `cqs explain <fn>` with no `-n`.
fn default_explain_limit() -> usize {
    crate::cli::args::DEFAULT_LIMIT
}

// ─── Shared core ────────────────────────────────────────────────────────────

/// All data needed to render an explain card (JSON or terminal).
pub(crate) struct ExplainData {
    pub chunk: ChunkSummary,
    pub callers: Vec<CallerInfo>,
    pub callees: Vec<(String, u32)>,
    pub similar: Vec<SearchResult>,
    pub hints: Option<FunctionHints>,
    /// When true, the target chunk's content should be included in output.
    pub include_target_content: bool,
    /// IDs of similar chunks whose content fits within the token budget.
    pub similar_content_ids: Option<HashSet<String>>,
    /// (tokens_used, budget) if `--tokens` was requested.
    pub token_info: Option<(usize, usize)>,
}

/// Build explain data: resolve target, fetch callers/callees/similar, compute hints,
/// and optionally pack content within a token budget.
/// Shared between CLI `cmd_explain` and batch `dispatch_explain`.
/// * `index` — pre-loaded vector index (batch passes its cached one, CLI passes `None`
///   to load fresh).
/// * `embedder` — required only when `max_tokens` is `Some`. Batch passes its cached one;
///   CLI passes `None` to create a fresh one internally.
pub(crate) fn build_explain_data<Mode>(
    store: &Store<Mode>,
    cqs_dir: &Path,
    target: &str,
    max_tokens: Option<usize>,
    index: Option<Option<&dyn VectorIndex>>,
    embedder: Option<&cqs::Embedder>,
    model_config: &cqs::embedder::ModelConfig,
) -> Result<ExplainData> {
    let _span = tracing::info_span!("build_explain_data", target).entered();
    // Resolve target
    let resolved = cqs::resolve_target(store, target)?;
    let chunk = resolved.chunk;

    // Get callers
    let callers = match store.get_callers_full(&chunk.name) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, name = chunk.name, "Failed to get callers in explain");
            Vec::new()
        }
    };

    // Get callees — scope to the resolved chunk's file to avoid ambiguity.
    // Explain's surface doesn't carry edge_kind, so project CalleeInfo to the
    // (name, line) tuple shape its CalleeEntry expects.
    let chunk_file = chunk.file.to_string_lossy();
    let callees = match store.get_callees_full(&chunk.name, Some(&chunk_file), None) {
        Ok(c) => c.into_iter().map(|ci| (ci.name, ci.line)).collect(),
        Err(e) => {
            tracing::warn!(error = %e, name = chunk.name, "Failed to get callees in explain");
            Vec::new()
        }
    };

    // Get similar (top 3) using embedding
    let similar = match store.get_chunk_with_embedding(&chunk.id)? {
        Some((_, embedding)) => {
            let filter = SearchFilter::default();
            // Use caller-provided index or load fresh
            let owned_index;
            let idx: Option<&dyn VectorIndex> = match index {
                Some(idx) => idx,
                None => {
                    owned_index = HnswIndex::try_load_with_ef(cqs_dir, None, store.dim());
                    owned_index.as_deref()
                }
            };
            let sim_results = store.search_filtered_with_index(
                &embedding, &filter, 4, // +1 to exclude self
                0.3, idx,
            )?;
            sim_results
                .into_iter()
                .filter(|r| r.chunk.id != chunk.id)
                .take(3)
                .collect::<Vec<_>>()
        }
        None => vec![],
    };

    // Compute hints (only for function/method chunk types)
    let hints = if chunk.chunk_type.is_callable() {
        match compute_hints(store, &chunk.name, Some(callers.len())) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(function = %chunk.name, error = %e, "Failed to compute hints");
                None
            }
        }
    } else {
        None
    };

    // Token budget: compute which content fits
    let (include_target_content, similar_content_ids, token_info) = if let Some(budget) = max_tokens
    {
        // Need an embedder for token counting
        let owned_embedder;
        let emb = match embedder {
            Some(e) => e,
            None => {
                owned_embedder = cqs::Embedder::new(model_config.clone())?;
                &owned_embedder
            }
        };
        let _pack_span = tracing::info_span!("token_pack_explain", budget).entered();

        // Priority 1: target chunk content (always included)
        let target_tokens = crate::cli::commands::count_tokens(emb, &chunk.content, &chunk.name);

        // Priority 2: similar chunks' content — pack remaining budget
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
        let token_counts = crate::cli::commands::count_tokens_batch(emb, &texts);
        let (packed, sim_used) = crate::cli::commands::token_pack(
            indexed,
            &token_counts,
            remaining,
            0,
            |&(_, score)| score,
        );
        let sim_included: HashSet<String> = packed
            .into_iter()
            .map(|(i, _)| similar[i].chunk.id.clone())
            .collect();

        let used = target_tokens + sim_used;
        tracing::info!(
            tokens = used,
            budget,
            similar_with_content = sim_included.len(),
            "Token-budgeted explain"
        );
        (true, Some(sim_included), Some((used, budget)))
    } else {
        (false, None, None)
    };

    Ok(ExplainData {
        chunk,
        callers,
        callees,
        similar,
        hints,
        include_target_content,
        similar_content_ids,
        token_info,
    })
}

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct SimilarEntry {
    pub name: String,
    pub file: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Injection heuristics that fired on this similar chunk's relayed body.
    /// A similar chunk's content is relayed verbatim only under `--tokens`
    /// (when it fits the budget), so this is populated only when `content` is
    /// present — the scan tracks exactly what is emitted. Skip-when-empty,
    /// mirroring the per-result `injection_flags` search and read carry.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub injection_flags: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct HintsOutput {
    pub caller_count: usize,
    pub test_count: usize,
    pub no_callers: bool,
    pub no_tests: bool,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ExplainOutput {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub chunk_type: String,
    pub language: String,
    pub signature: String,
    pub doc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub callers: Vec<CallerEntry>,
    pub callees: Vec<CalleeEntry>,
    pub similar: Vec<SimilarEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<HintsOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<usize>,
    /// `cqs explain` reads from the project store; a vendored target downgrades
    /// to "vendored-code", everything else is the default "user-code".
    /// Skip-when-default (absence means "user-code"), per the SECURITY.md
    /// trust-signal contract and the per-chunk search shape.
    #[serde(skip_serializing_if = "is_user_code")]
    pub trust_level: &'static str,
    /// Per-chunk injection-heuristic flags scanned over exactly the relayed
    /// surfaces: `doc` and `signature` always, `content` only when it is
    /// emitted (`--tokens`). A doc-borne payload must surface here, not just a
    /// content-borne one; an un-relayed surface must not, so the flags never
    /// over-report on text the response did not carry. Skip-when-empty (no
    /// heuristic fired — the common case), matching the search/read carry.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub injection_flags: Vec<String>,
}

/// `serde` skip predicate: a `"user-code"` trust level is the default and is
/// omitted from the wire shape, matching the per-chunk search skip-when-default
/// rule and `read`'s `FocusedReadJsonOutput`.
fn is_user_code(level: &&'static str) -> bool {
    *level == "user-code"
}

/// Build typed explain output from explain data.
/// Shared between CLI `cmd_explain --json` and batch `dispatch_explain`.
pub(crate) fn build_explain_output(data: &ExplainData, root: &Path) -> ExplainOutput {
    let _span = tracing::info_span!("build_explain_output", name = %data.chunk.name).entered();
    let chunk = &data.chunk;

    let callers: Vec<CallerEntry> = data
        .callers
        .iter()
        .map(|c| CallerEntry {
            name: c.name.clone(),
            file: rel_display(&c.file, root),
            line_start: c.line,
            project: String::new(),
            edge_kind: super::callers::edge_kind_field(c.edge_kind),
            // explain renders the bare-name caller list — no Type::method
            // receiver resolution here.
            attribution: String::new(),
        })
        .collect();

    let callees: Vec<CalleeEntry> = data
        .callees
        .iter()
        .map(|(name, line)| CalleeEntry {
            name: name.clone(),
            line_start: *line,
            project: String::new(),
            // Explain's callees are projected to (name, line) tuples upstream;
            // edge_kind is not carried, so default to the omitted `call`.
            edge_kind: String::new(),
        })
        .collect();

    let similar: Vec<SimilarEntry> = data
        .similar
        .iter()
        .map(|r| {
            let content = data.similar_content_ids.as_ref().and_then(|set| {
                if set.contains(&r.chunk.id) {
                    Some(r.chunk.content.clone())
                } else {
                    None
                }
            });
            // Scan only what is relayed: a similar chunk's body is emitted
            // verbatim only when its `content` is present (it fit the token
            // budget), so the injection scan runs on exactly that surface and
            // is empty otherwise.
            let injection_flags = match &content {
                Some(body) => cqs::llm::validation::detect_all_injection_patterns(body)
                    .into_iter()
                    .map(String::from)
                    .collect(),
                None => Vec::new(),
            };
            SimilarEntry {
                name: r.chunk.name.clone(),
                file: rel_display(&r.chunk.file, root),
                score: r.score,
                content,
                injection_flags,
            }
        })
        .collect();

    let hints = data.hints.as_ref().map(|h| HintsOutput {
        caller_count: h.caller_count,
        test_count: h.test_count,
        no_callers: h.caller_count == 0,
        no_tests: h.test_count == 0,
    });

    let (token_count, token_budget) = match data.token_info {
        Some((used, budget)) => (Some(used), Some(budget)),
        None => (None, None),
    };

    // Scan exactly the relayed surfaces. `doc` and `signature` are relayed
    // verbatim unconditionally; `content` is relayed only under `--tokens`
    // (`include_target_content`). A payload in any emitted surface must fire,
    // and a surface that is NOT emitted must not — flagging un-relayed content
    // would over-report on a response that never carried it.
    let doc = chunk.doc.as_deref().unwrap_or("");
    let scan_text = if data.include_target_content {
        format!("{doc}\n{}\n{}", chunk.signature, chunk.content)
    } else {
        format!("{doc}\n{}", chunk.signature)
    };
    let injection_flags: Vec<String> =
        cqs::llm::validation::detect_all_injection_patterns(&scan_text)
            .into_iter()
            .map(String::from)
            .collect();
    let trust_level = if chunk.vendored {
        "vendored-code"
    } else {
        "user-code"
    };

    ExplainOutput {
        name: chunk.name.clone(),
        file: cqs::rel_display(&chunk.file, root).to_string(),
        line_start: chunk.line_start,
        line_end: chunk.line_end,
        chunk_type: chunk.chunk_type.to_string(),
        language: chunk.language.to_string(),
        signature: chunk.signature.clone(),
        doc: chunk.doc.clone(),
        content: if data.include_target_content {
            Some(chunk.content.clone())
        } else {
            None
        },
        callers,
        callees,
        similar,
        hints,
        token_count,
        token_budget,
        trust_level,
        injection_flags,
    }
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_explain(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    target: &str,
    limit: usize,
    json: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_explain", target, limit).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;
    // Cap on callers/callees/similar lists in the function card.
    let limit = limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    let embedder = if max_tokens.is_some() {
        Some(ctx.embedder()?)
    } else {
        None
    };

    let mut data = build_explain_data(
        store,
        cqs_dir,
        target,
        max_tokens,
        None,
        embedder,
        ctx.cli.try_model_config()?,
    )?;

    // Truncate per-section AFTER hints (caller_count is computed from the
    // un-truncated callers list inside `build_explain_data`, so the displayed
    // hint counts remain accurate even when the user passes a small `--limit`).
    data.callers.truncate(limit);
    data.callees.truncate(limit);
    data.similar.truncate(limit);

    // Proactive staleness warning
    if !ctx.cli.quiet && !ctx.cli.no_stale_check {
        if let Some(file_str) = data.chunk.file.to_str() {
            staleness::warn_stale_results(store, &[file_str], root);
        }
    }

    if json {
        let output = build_explain_output(&data, root);
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        print_explain_terminal(&data, root);
    }

    Ok(())
}

fn print_explain_terminal(data: &ExplainData, root: &Path) {
    use colored::Colorize;

    let chunk = &data.chunk;
    let rel_file = cqs::rel_display(&chunk.file, root);

    let token_label = match data.token_info {
        Some((used, budget)) => format!(" ({} of {} tokens)", used, budget),
        None => String::new(),
    };
    println!(
        "{} ({} {}){}",
        chunk.name.bold(),
        chunk.chunk_type,
        chunk.language,
        token_label,
    );
    println!("{}:{}-{}", rel_file, chunk.line_start, chunk.line_end);

    if let Some(ref h) = data.hints {
        if h.caller_count == 0 || h.test_count == 0 {
            let caller_part = if h.caller_count == 0 {
                format!("{}", "0 callers".yellow())
            } else {
                format!("{} callers", h.caller_count)
            };
            let test_part = if h.test_count == 0 {
                format!("{}", "0 tests".yellow())
            } else {
                format!("{} tests", h.test_count)
            };
            println!("{} | {}", caller_part, test_part);
        } else {
            println!("{} callers | {} tests", h.caller_count, h.test_count);
        }
    }

    if !chunk.signature.is_empty() {
        println!();
        println!("{}", chunk.signature.dimmed());
    }

    if let Some(ref doc) = chunk.doc {
        println!();
        println!("{}", doc.green());
    }

    // Print target content if --tokens is set
    if data.include_target_content {
        println!();
        println!("{}", "\u{2500}".repeat(50));
        println!(
            "{}",
            crate::cli::display::sanitize_for_terminal(&chunk.content)
        );
    }

    if !data.callers.is_empty() {
        println!();
        println!("{}", "Callers:".cyan());
        for c in &data.callers {
            let rel = cqs::rel_display(&c.file, root);
            println!("  {} ({}:{})", c.name, rel, c.line);
        }
    }

    if !data.callees.is_empty() {
        println!();
        println!("{}", "Callees:".cyan());
        for (name, _) in &data.callees {
            println!("  {}", name);
        }
    }

    if !data.similar.is_empty() {
        println!();
        println!("{}", "Similar:".cyan());
        for r in &data.similar {
            let rel = cqs::rel_display(&r.chunk.file, root);
            println!(
                "  {} ({}:{}) [{:.2}]",
                r.chunk.name, rel, r.chunk.line_start, r.score
            );
            // Print similar content if within token budget
            if let Some(ref set) = data.similar_content_ids {
                if set.contains(&r.chunk.id) {
                    println!("{}", "\u{2500}".repeat(40));
                    println!(
                        "{}",
                        crate::cli::display::sanitize_for_terminal(&r.chunk.content)
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;

    // ─── RT-RELAY guard: scan == relayed on `explain` (red-team #2039) ───────
    //
    // These guards pin the trust boundary that makes `cqs explain` safe to
    // expose as an MCP read tool: the injection scan covers EXACTLY the
    // surfaces the response relays — doc + signature always, target `content`
    // only under `--tokens`. They drive `build_explain_output` (the real scan
    // path), not a hand-built `ExplainOutput`, so a future refactor that drops
    // the doc from the scan (re-opening the RT-RELAY gap #2024 closed) turns
    // these red. PoC parity: confirmed against `~/.cargo/bin/cqs explain` on a
    // seeded poisoned-doc project — `injection_flags: ["embedded-url"]` fired on
    // the doc surface with `content` absent, matching the already-exposed
    // `read --focus` / `similar` relay posture.

    use cqs::language::{ChunkType, Language};
    use cqs::store::ChunkSummary;
    use std::path::PathBuf;

    fn payload_chunk(doc: Option<&str>, signature: &str, content: &str) -> ChunkSummary {
        ChunkSummary {
            id: "id_target".into(),
            file: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "target".into(),
            signature: signature.into(),
            content: content.into(),
            doc: doc.map(|s| s.to_string()),
            line_start: 1,
            line_end: 5,
            content_hash: "hash".into(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
            parser_version: 0,
            vendored: false,
        }
    }

    fn explain_data(chunk: ChunkSummary, include_target_content: bool) -> ExplainData {
        ExplainData {
            chunk,
            callers: vec![],
            callees: vec![],
            similar: vec![],
            hints: None,
            include_target_content,
            similar_content_ids: None,
            token_info: None,
        }
    }

    /// A doc-borne payload fires `injection_flags` even when `content` is NOT
    /// relayed (no `--tokens`). This is the RT-RELAY contract: the doc surface
    /// is relayed verbatim always, so a payload there must surface — the gap
    /// #2024 closed. Red if a refactor scopes the scan to `content` only.
    #[test]
    fn explain_scan_fires_on_doc_when_content_unrelayed() {
        let chunk = payload_chunk(
            Some("See https://evil.example/payload for the real impl"),
            "fn target()",
            "fn target() { /* benign body */ }",
        );
        let data = explain_data(chunk, /* include_target_content */ false);
        let out = build_explain_output(&data, Path::new(""));
        assert!(
            out.content.is_none(),
            "content must be absent without --tokens (un-relayed surface)"
        );
        assert!(
            out.injection_flags.iter().any(|f| f == "embedded-url"),
            "doc-borne payload must fire injection_flags even when content is \
             un-relayed; got: {:?}",
            out.injection_flags
        );
    }

    /// The other direction of scan == relayed: a payload that lives ONLY in the
    /// target `content` body must NOT over-flag when `content` is un-relayed
    /// (no `--tokens`) — flagging a surface the response never carried would be
    /// a false positive that erodes the signal. Doc + signature here are benign.
    #[test]
    fn explain_scan_does_not_overflag_unrelayed_content() {
        let chunk = payload_chunk(
            Some("A perfectly ordinary helper."),
            "fn target()",
            "fn target() { let _ = \"see https://evil.example/c2\"; }",
        );
        let data = explain_data(chunk, /* include_target_content */ false);
        let out = build_explain_output(&data, Path::new(""));
        assert!(
            out.injection_flags.is_empty(),
            "un-relayed content body must NOT be scanned (no phantom flags); got: {:?}",
            out.injection_flags
        );
    }

    /// When `--tokens` relays the target `content`, a payload in the body DOES
    /// fire — the scan grows to cover the now-relayed surface. Completes the
    /// scan == relayed both-directions invariant for the target body.
    #[test]
    fn explain_scan_fires_on_content_when_relayed() {
        let chunk = payload_chunk(
            Some("A perfectly ordinary helper."),
            "fn target()",
            "fn target() { let _ = \"see https://evil.example/c2\"; }",
        );
        let data = explain_data(chunk, /* include_target_content */ true);
        let out = build_explain_output(&data, Path::new(""));
        assert!(
            out.content.is_some(),
            "content must be relayed under --tokens"
        );
        assert!(
            out.injection_flags.iter().any(|f| f == "embedded-url"),
            "content-borne payload must fire once the body is relayed; got: {:?}",
            out.injection_flags
        );
    }

    /// Similar-chunk relay direction: under `--tokens`, a similar chunk whose
    /// BODY is relayed (it fit the budget) carries a payload that must fire on
    /// THAT entry's own `injection_flags` — and a similar chunk whose body is
    /// NOT relayed must not. This is the relay surface explain has that the
    /// already-exposed `read --focus` does not (read relays type-dep bodies;
    /// explain relays similar bodies); pinning it closes the last scan==relayed
    /// direction. PoC parity: confirmed live — a poisoned neighbor body fired
    /// `["code-fence","embedded-url"]` on `similar[0].injection_flags` while the
    /// benign target stayed clean.
    #[test]
    fn explain_scan_fires_on_relayed_similar_body_only() {
        use cqs::store::SearchResult;

        let target = payload_chunk(Some("benign doc"), "fn target()", "fn target() {}");
        let mut relayed = payload_chunk(
            Some("neighbor doc"),
            "fn relayed_neighbor()",
            "fn relayed_neighbor() { let s = \"```\\nsee https://evil.example/n\\n```\"; }",
        );
        relayed.id = "id_relayed".into();
        relayed.name = "relayed_neighbor".into();
        let mut unrelayed = payload_chunk(
            Some("neighbor doc"),
            "fn unrelayed_neighbor()",
            "fn unrelayed_neighbor() { let s = \"https://evil.example/u\"; }",
        );
        unrelayed.id = "id_unrelayed".into();
        unrelayed.name = "unrelayed_neighbor".into();

        // Only the first neighbor's body fits the budget → only it is relayed.
        let relayed_ids: std::collections::HashSet<String> =
            ["id_relayed".to_string()].into_iter().collect();
        let data = ExplainData {
            chunk: target,
            callers: vec![],
            callees: vec![],
            similar: vec![
                SearchResult::new(relayed, 0.9),
                SearchResult::new(unrelayed, 0.8),
            ],
            hints: None,
            include_target_content: false,
            similar_content_ids: Some(relayed_ids),
            token_info: Some((10, 2000)),
        };
        let out = build_explain_output(&data, Path::new(""));
        let relayed_entry = out
            .similar
            .iter()
            .find(|s| s.name == "relayed_neighbor")
            .expect("relayed neighbor present");
        assert!(
            relayed_entry.content.is_some(),
            "relayed neighbor body must be emitted"
        );
        assert!(
            relayed_entry
                .injection_flags
                .iter()
                .any(|f| f == "embedded-url")
                && relayed_entry
                    .injection_flags
                    .iter()
                    .any(|f| f == "code-fence"),
            "relayed similar-chunk body payload must fire on its own injection_flags; got: {:?}",
            relayed_entry.injection_flags
        );
        let unrelayed_entry = out
            .similar
            .iter()
            .find(|s| s.name == "unrelayed_neighbor")
            .expect("un-relayed neighbor present");
        assert!(
            unrelayed_entry.content.is_none(),
            "un-relayed neighbor body must be absent"
        );
        assert!(
            unrelayed_entry.injection_flags.is_empty(),
            "un-relayed similar body must NOT be scanned (no phantom flag); got: {:?}",
            unrelayed_entry.injection_flags
        );
    }

    #[test]
    fn test_explain_output_field_names() {
        let output = ExplainOutput {
            name: "foo".into(),
            file: "src/lib.rs".into(),
            line_start: 10,
            line_end: 20,
            chunk_type: "function".into(),
            language: "rust".into(),
            signature: "fn foo()".into(),
            doc: None,
            content: None,
            callers: vec![],
            callees: vec![],
            similar: vec![],
            hints: None,
            token_count: None,
            token_budget: None,
            trust_level: "user-code",
            injection_flags: Vec::new(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line_end").is_some());
        assert!(json.get("line").is_none());
        // None fields should be omitted
        assert!(json.get("content").is_none());
        assert!(json.get("hints").is_none());
        assert!(json.get("token_count").is_none());
        assert!(json.get("token_budget").is_none());
        // Skip-when-default trust signals: user-code + no fired heuristic omit
        // both keys (absence means the default), matching the search/read shape.
        assert!(
            json.get("trust_level").is_none(),
            "user-code explain omits trust_level (skip-when-default), got: {json}"
        );
        assert!(
            json.get("injection_flags").is_none(),
            "benign explain omits injection_flags (skip-when-empty), got: {json}"
        );
    }

    /// Non-default trust signals serialize: a vendored target emits
    /// `trust_level: "vendored-code"` and a fired heuristic emits a populated
    /// `injection_flags` array. Pins the other half of skip-when-default.
    #[test]
    fn test_explain_output_emits_nondefault_trust_signals() {
        let output = ExplainOutput {
            name: "vendored_fn".into(),
            file: "vendor/lib.rs".into(),
            line_start: 1,
            line_end: 5,
            chunk_type: "function".into(),
            language: "rust".into(),
            signature: "fn vendored_fn()".into(),
            doc: None,
            content: None,
            callers: vec![],
            callees: vec![],
            similar: vec![],
            hints: None,
            token_count: None,
            token_budget: None,
            trust_level: "vendored-code",
            injection_flags: vec!["embedded-url".into()],
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(
            json["trust_level"], "vendored-code",
            "non-default trust_level must serialize"
        );
        let flags = json["injection_flags"]
            .as_array()
            .expect("non-empty injection_flags must serialize as an array");
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0], "embedded-url");
    }

    #[test]
    fn test_explain_output_with_hints() {
        let output = ExplainOutput {
            name: "bar".into(),
            file: "src/bar.rs".into(),
            line_start: 1,
            line_end: 5,
            chunk_type: "function".into(),
            language: "rust".into(),
            signature: "fn bar()".into(),
            doc: Some("A doc comment".into()),
            content: Some("fn bar() {}".into()),
            callers: vec![CallerEntry {
                name: "caller_a".into(),
                file: "src/a.rs".into(),
                line_start: 42,
                project: String::new(),
                edge_kind: String::new(),
                attribution: String::new(),
            }],
            callees: vec![CalleeEntry {
                name: "callee_b".into(),
                line_start: 3,
                project: String::new(),
                edge_kind: String::new(),
            }],
            similar: vec![SimilarEntry {
                name: "baz".into(),
                file: "src/baz.rs".into(),
                score: 0.85,
                content: None,
                injection_flags: Vec::new(),
            }],
            hints: Some(HintsOutput {
                caller_count: 1,
                test_count: 0,
                no_callers: false,
                no_tests: true,
            }),
            token_count: Some(100),
            token_budget: Some(500),
            trust_level: "user-code",
            injection_flags: Vec::new(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "bar");
        assert_eq!(json["callers"][0]["line_start"], 42);
        assert!(json["callers"][0].get("line").is_none());
        assert_eq!(json["callees"][0]["line_start"], 3);
        assert!(json["callees"][0].get("line").is_none());
        assert_eq!(json["hints"]["no_tests"], true);
        assert_eq!(json["token_count"], 100);
        assert_eq!(json["token_budget"], 500);
        assert!(json.get("content").is_some());
        // similar entry should not have content (None)
        assert!(json["similar"][0].get("content").is_none());
    }

    // Special characters in name/signature/doc survive serde round-trip
    #[test]
    fn test_explain_output_special_characters() {
        let output = ExplainOutput {
            name: "parse<T>".into(),
            file: "src/parser.rs".into(),
            line_start: 1,
            line_end: 10,
            chunk_type: "function".into(),
            language: "rust".into(),
            signature: "fn foo<T: Debug>(x: &T) -> Vec<T>".into(),
            doc: Some("returns \"best\" result with <html> & entities".into()),
            content: Some("fn foo<T>() { let x = \"hello\\nworld\"; }".into()),
            callers: vec![CallerEntry {
                name: "call<U>".into(),
                file: "src/a.rs".into(),
                line_start: 5,
                project: String::new(),
                edge_kind: String::new(),
                attribution: String::new(),
            }],
            callees: vec![],
            similar: vec![],
            hints: None,
            token_count: None,
            token_budget: None,
            trust_level: "user-code",
            injection_flags: Vec::new(),
        };

        // Serialize to JSON Value
        let json = serde_json::to_value(&output).unwrap();

        // Verify special characters survive
        assert_eq!(json["name"], "parse<T>");
        assert_eq!(json["signature"], "fn foo<T: Debug>(x: &T) -> Vec<T>");
        assert_eq!(
            json["doc"],
            "returns \"best\" result with <html> & entities"
        );
        assert_eq!(json["callers"][0]["name"], "call<U>");

        // Verify round-trip through string serialization
        let json_str = serde_json::to_string(&output).unwrap();
        let roundtrip: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(roundtrip["name"], "parse<T>");
        assert_eq!(
            roundtrip["doc"],
            "returns \"best\" result with <html> & entities"
        );
    }

    // Unicode in name and doc fields
    #[test]
    fn test_explain_output_unicode() {
        let output = ExplainOutput {
            name: "calculate_\u{03b1}\u{03b2}".into(),
            file: "src/math.rs".into(),
            line_start: 1,
            line_end: 5,
            chunk_type: "function".into(),
            language: "rust".into(),
            signature: "fn calculate_\u{03b1}\u{03b2}()".into(),
            doc: Some("Computes \u{03b1} + \u{03b2} coefficient".into()),
            content: None,
            callers: vec![],
            callees: vec![],
            similar: vec![],
            hints: None,
            token_count: None,
            token_budget: None,
            trust_level: "user-code",
            injection_flags: Vec::new(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "calculate_\u{03b1}\u{03b2}");
        assert_eq!(json["doc"], "Computes \u{03b1} + \u{03b2} coefficient");
    }

    // ─── RT-RELAY scan==relayed guards (red-team #2039 follow-up) ────────────
    //
    // These drive the *real* scan wiring in `build_explain_output` (the
    // `detect_all_injection_patterns` calls at the target and per-similar sites)
    // rather than a hand-built `ExplainOutput`. They pin the contract SECURITY.md
    // states for explain: a surface that is RELAYED is SCANNED (a payload there
    // surfaces in `injection_flags`), and a surface that is NOT relayed (un-emitted
    // `content` in a compact response) is NOT scanned (no phantom over-flagging).
    // This is exactly the boundary the already-exposed `read --focus` / `trace`
    // siblings carry; exposing `cqs_explain` adds no unscanned-relay surface.

    fn rt_chunk(
        id: &str,
        name: &str,
        signature: &str,
        doc: Option<&str>,
        content: &str,
    ) -> ChunkSummary {
        ChunkSummary {
            id: id.to_string(),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: cqs::parser::Language::Rust,
            chunk_type: cqs::parser::ChunkType::Function,
            name: name.to_string(),
            signature: signature.to_string(),
            content: content.to_string(),
            doc: doc.map(String::from),
            line_start: 1,
            line_end: 5,
            content_hash: "deadbeef".to_string(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
            parser_version: 0,
            vendored: false,
        }
    }

    fn rt_data(
        chunk: ChunkSummary,
        similar: Vec<SearchResult>,
        include_target_content: bool,
        similar_content_ids: Option<HashSet<String>>,
    ) -> ExplainData {
        ExplainData {
            chunk,
            callers: vec![],
            callees: vec![],
            similar,
            hints: None,
            include_target_content,
            similar_content_ids,
            token_info: None,
        }
    }

    /// Compact explain (no `--tokens`): the target `content` is NOT relayed, so a
    /// body-borne payload must NOT be scanned (no phantom flag), while a payload
    /// in the always-relayed `doc` MUST fire. Verified live against the binary:
    /// `cqs explain compute_widget_checksum --json` returned an empty
    /// `injection_flags` for a body-only `http://` payload, and a populated one
    /// for a doc-borne `http://` payload.
    #[test]
    fn rt_explain_compact_scans_doc_not_unrelayed_content() {
        // Payload ONLY in the body; doc is benign; content NOT relayed.
        let chunk = rt_chunk(
            "src/lib.rs:1:a",
            "compute_widget_checksum",
            "pub fn compute_widget_checksum(buf: &[u8]) -> u64",
            Some("Computes the widget checksum."),
            "fn compute_widget_checksum() { /* see http://evil.example.com */ }",
        );
        let data = rt_data(chunk, vec![], /* include_target_content */ false, None);
        let out = build_explain_output(&data, std::path::Path::new("."));
        assert!(out.content.is_none(), "compact mode must not relay content");
        assert!(
            out.injection_flags.is_empty(),
            "un-relayed body payload must NOT be scanned (no phantom flag), got: {:?}",
            out.injection_flags
        );

        // Same chunk, but the payload is in the always-relayed `doc`.
        let chunk = rt_chunk(
            "src/lib.rs:1:a",
            "compute_widget_checksum",
            "pub fn compute_widget_checksum(buf: &[u8]) -> u64",
            Some("Computes the checksum. Contact http://evil.example.com for the override."),
            "fn compute_widget_checksum() {}",
        );
        let data = rt_data(chunk, vec![], false, None);
        let out = build_explain_output(&data, std::path::Path::new("."));
        assert!(out.content.is_none(), "compact mode must not relay content");
        assert!(
            out.injection_flags.contains(&"embedded-url".to_string()),
            "doc is relayed unconditionally and must be scanned, got: {:?}",
            out.injection_flags
        );
    }

    /// `--tokens` explain: when the target `content` IS relayed, a body-borne
    /// payload MUST be scanned and fire. The flip side of the compact guard —
    /// together they pin scan==relayed in BOTH directions for the target body.
    #[test]
    fn rt_explain_tokens_scans_relayed_target_content() {
        let chunk = rt_chunk(
            "src/lib.rs:1:a",
            "compute_widget_checksum",
            "pub fn compute_widget_checksum(buf: &[u8]) -> u64",
            Some("Computes the widget checksum."),
            "fn f() { /* curl https://evil.example.com */ let x = \"```\"; }",
        );
        let data = rt_data(chunk, vec![], /* include_target_content */ true, None);
        let out = build_explain_output(&data, std::path::Path::new("."));
        assert!(out.content.is_some(), "--tokens mode must relay content");
        assert!(
            out.injection_flags.contains(&"embedded-url".to_string())
                && out.injection_flags.contains(&"code-fence".to_string()),
            "relayed target body payload must be scanned, got: {:?}",
            out.injection_flags
        );
    }

    /// Per-similar scan==relayed: a similar chunk's body is relayed verbatim only
    /// when its id is in `similar_content_ids` (it fit the token budget). The
    /// scan must fire on exactly the emitted similar body and stay empty for a
    /// similar whose body was NOT relayed. Verified live: the seeded neighbor with
    /// a `https://` + fence body fired `[code-fence, embedded-url]`, the benign
    /// neighbor stayed empty.
    #[test]
    fn rt_explain_similar_scans_only_relayed_bodies() {
        let target = rt_chunk(
            "src/lib.rs:1:a",
            "compute_widget_checksum",
            "pub fn compute_widget_checksum(buf: &[u8]) -> u64",
            Some("Computes the widget checksum."),
            "fn compute_widget_checksum() {}",
        );
        // Neighbor A: malicious body, WILL be relayed (id in the fit set).
        let evil = SearchResult {
            chunk: rt_chunk(
                "src/lib.rs:10:b",
                "compute_widget_checksum_seeded",
                "pub fn compute_widget_checksum_seeded(buf: &[u8], seed: u64) -> u64",
                None,
                "fn seeded() { /* https://evil.example.com */ let x = \"```\"; }",
            ),
            score: 0.9,
            rank_signals: vec![],
        };
        // Neighbor B: identical malicious body, but NOT relayed (id absent from
        // the fit set) — must NOT be scanned (no phantom flag on un-emitted body).
        let evil_unrelayed = SearchResult {
            chunk: rt_chunk(
                "src/lib.rs:20:c",
                "sum_widget_bytes",
                "pub fn sum_widget_bytes(buf: &[u8]) -> u64",
                None,
                "fn sum() { /* https://evil.example.com */ let x = \"```\"; }",
            ),
            score: 0.5,
            rank_signals: vec![],
        };
        let mut fit: HashSet<String> = HashSet::new();
        fit.insert("src/lib.rs:10:b".to_string()); // only neighbor A fits
        let data = rt_data(
            target,
            vec![evil, evil_unrelayed],
            /* include_target_content */ true,
            Some(fit),
        );
        let out = build_explain_output(&data, std::path::Path::new("."));

        let a = out
            .similar
            .iter()
            .find(|s| s.name == "compute_widget_checksum_seeded")
            .expect("neighbor A present");
        assert!(a.content.is_some(), "neighbor A body must be relayed");
        assert!(
            a.injection_flags.contains(&"embedded-url".to_string())
                && a.injection_flags.contains(&"code-fence".to_string()),
            "relayed similar body must be scanned, got: {:?}",
            a.injection_flags
        );

        let b = out
            .similar
            .iter()
            .find(|s| s.name == "sum_widget_bytes")
            .expect("neighbor B present");
        assert!(b.content.is_none(), "neighbor B body must NOT be relayed");
        assert!(
            b.injection_flags.is_empty(),
            "un-relayed similar body must NOT be scanned (no phantom flag), got: {:?}",
            b.injection_flags
        );
    }
}
