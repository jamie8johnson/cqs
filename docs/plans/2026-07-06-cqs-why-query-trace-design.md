# `cqs why` — query-trace design (the ROADMAP "cqs trace \"query\"" item)

Status: design locked pending review. Investigation basis: full pipeline map with file:line anchors (inv-trace brief, 2026-07-06); anchors cited here were verified against origin/main tip.

## Problem

Understanding why a query ranked (or buried) a result requires `RUST_LOG=debug` + log grepping — and even that is incomplete. The routing decisions are made CLI-side in `prepare_query` (src/cli/commands/search/query.rs:530) and **discarded**: `Classification` never reaches `PreparedQuery`, only its derivatives (α, type boosts, index choice). Several decisions are invisible at any log level:

1. RRF fusion internals — `rrf_fuse` has zero tracing (no fused count, no leg overlap, no FTS-leg count).
2. Name/note/importance/type-boost score mutations (all silent).
3. Threshold-cut, parent-dedup, final-truncate drop counts (finalize steps 1–5).
4. Centroid **decline** — ran-but-under-margin vs never-ran are indistinguishable (router.rs:1627-1656 returns silent `None`).
5. The CLI-final SPLADE decision for the explicit-α / `--splade`-forced arms + the centroid α-floor bump (query.rs:703-711).
6. SPLADE query term content, per-query synonym expansion, resolved ef_search, cosine-reuse decision, reranker preamble toggle.

Consumers: agents debugging their own retrieval ("why didn't X surface?"), and us tuning the pipeline. This is the query-level complement of per-result `rank_signals` (#1847).

## Cornerstone decisions

### Name: new command `cqs why "query"` (wire verb `why`, MCP tool `cqs_why`)

- `trace` is load-bearing and stays untouched: the graph command ships two required positionals, kind-fallback routing, an MCP `cqs_trace` tool, parity/schema/snapshot pins, and ~8 README sites. Overloading it (arg-shape dispatch) or renaming it has the widest blast radius for zero user value.
- A `--trace` mode on `search` is rejected on the SNR lesson: search's output contract is the most adoption-sensitive surface we have (the 79%→6% search-rate collapse tracked response-shape growth). A giant optional trace section in `SearchOutput` + its MCP outputSchema is exactly the field accumulation we un-shipped in the SNR restoration. Diagnostic verbosity belongs to a command you only get when you ask for it.
- `why` is short, agent-mnemonic ("why did these results rank"), collision-free, and reads naturally next to `explain` (chunk-level) and `rank_signals` (result-level).

### Fidelity rule: same-execution side channel, never re-derivation

`why` must report the execution it actually ran, not a reconstruction. The proven pattern is `capture_legs`: `search_hybrid_legs` (src/search/query.rs:787-800) materializes `SearchLegs` as a side channel of the SAME execution — results byte-identical, no re-run — and `search_legs_core` (query.rs:1175) already rides the production `prepare_query`. `why` extends that pattern; it never calls router functions standalone (drift risk: a re-derived trace can disagree with the real path and lie).

Consequence (pinned by test, see Gates): `cqs why "q" --limit N` returns the **identical result list** `cqs search "q" --limit N` returns under the same flags, plus the trace.

### Capture architecture: two-layer `QueryTrace`

- **CLI-side layer (stages 1–8, 13) — cheap, no Store signature changes.** `prepare_query`'s decisions are captured where they're made: widen `Prepared`/`PreparedQuery` (or thread an `Option<&mut RoutingTrace>`) to carry: classification (category/confidence/strategy, rule vs centroid, centroid ran/filled/declined-with-margin), α resolution arm (`per_cat_env`/`global_env`/`slot_toml`/`default`/explicit/forced) + the centroid α-floor bump, NameOnly short-circuit taken/fallback, base-vs-enriched index choice, embed-cache hit/miss, SPLADE encode fallback, and post-store steps (pattern filter, rerank applied, overlay merge counts).
- **Store-side layer (stages 10–12) — extends existing side channels.** Legs: reuse `SearchLegs` verbatim (dense/sparse/fused entries already carry raw_cosine, minmax, splade_dot, ranks, present_in_pool). New capture, mirroring the `record_rank_signals` opt-in shape: RRF leg-rank maps + fused/dropped counts in `finalize_results` steps 1–5 (threshold-cut, parent-dedup, truncate counts), boost application counts (name/note/importance/parent/type), MMR reorder yes/no. `rrf_fuse` gains an optional capture output; `signals_for`'s dense/fts rank maps and the trace share one source so rank_signals and `why` can never disagree.

### Scope boundary (v1 in / out)

- **In:** everything above, plus surface honesty: output carries `surface: "cli" | "daemon"` and the divergent knobs (`fts_first` — the daemon never takes the NameOnly short-circuit; `always_route`), so a trace states which path it describes. `_meta.overlay_graph`/`worktree_overlay` markers per the search conventions; overlay merge reported as a finalize step.
- **Out (v2, separate design):** per-token SPLADE attribution — nothing exists today (products summed and discarded in `SpladeIndex::search_with_filter`, src/splade/index.rs:251-261; `postings` private; no single-chunk sparse fetch; `decode_token` caller-less). Needs its own plumbing decision. Also out: per-result before/after rank deltas across every stage (v1 reports stage-level counts + final rank_signals; a full per-chunk rank ledger is a data-model question for v2).

### Relationship to `search-legs` (#2060)

`why`'s core subsumes the orphan daemon-only `search-legs` verb: `why_core` = `search_legs_core` + routing capture + finalize capture. The `search-legs` wire verb and serve's `/api/search_legs` client stay as-is for the viz (serve can migrate to `why_core` opportunistically; no compat break either way).

## Output shape (sketch)

Typed `WhyOutput` (Serialize + JsonSchema, skip-when-empty house rules):

```
{
  "query": "...",
  "surface": "daemon",
  "routing": { "category": "behavioral", "confidence": "high", "strategy": "Hybrid",
               "classifier": "rule",            // rule | centroid | skipped(reason)
               "centroid": {"ran": true, "filled": false, "margin": 0.04},
               "alpha": 0.8, "alpha_source": "slot_toml", "alpha_floor_applied": false,
               "index": "enriched", "embed_cache": "memory-hit",
               "fts_first_short_circuit": false },
  "legs": { ...SearchLegs verbatim... },        // omitted when fusion didn't run, same as search-legs
  "fusion": { "kind": "linear-alpha", "alpha": 0.8, "dense_n": 500, "sparse_n": 214,
              "overlap": 87, "candidate_count": 500 },
  "finalize": { "rrf": {"ran": true, "fts_n": 30, "fused_n": 20},
                "boosts": {"name_match": 3, "note_boost": 1, "parent_boost": 2, "type_boost": 0},
                "drops": {"threshold": 12, "parent_dedup": 4, "truncate": 6},
                "mmr": false, "rerank": false, "overlay_merged": 2 },
  "results": [ {"name": ..., "file": ..., "score": ..., "rank_signals": [...]} ],  // slim; no content relay
  "token_count": ...
}
```

Text mode renders `── Routing ── / ── Legs ── / ── Finalize ──` sections (`explain`/`onboard` precedent) with `--tokens N` budget packing. Results are slim (name/file/score/signals) — **no chunk content relay**, so `why` introduces no new injection-relay surface; the standard trust fields apply to what it does emit.

## Surface plan

Full command-core chain in one PR (the house four-hand rule): `WhyCoreArgs`/`WhyOutput` + `why_core` (`info_span!` at entry) → `cmd_why` + `cmd_why_dispatch` shim → `dispatch_why` + `for_each_batch_cmd!` row → JSON-args arm + `JSON_ARGS_CAPABLE_COMMANDS` → MCP `ToolDef` + `command_core_map!` + `command_output_map!` rows (read tool, unconditional; SAFE_EXPOSE review per the relay-tool convention — expected trivial given no content relay) → `EXPECTED_SUBCOMMANDS` append. Env knobs: none new in v1 (all capture is per-invocation).

## Gates (what "done" means)

1. **Byte-identity pin:** a test asserting `why`'s embedded result list == `search`'s result list for the same (query, flags), on both surfaces — the fidelity rule made non-regressable.
2. **Coverage pin:** a checklist test asserting the trace covers the six unlogged-decision classes above (construct fixtures where each fires: a centroid-decline query, an α-floor query, a threshold-drop query, ...). The trace must show them; RUST_LOG stays optional.
3. **Parity tiers:** daemon-vs-core, json-args-vs-argv, binary integration — same three tiers as every core command.
4. **Schema:** typed outputSchema (no bare objects — #2203 discipline); JSON-noise guard compliance (skip-when-empty on every Option).
5. Docs: README command row + env-table n/a note, CLAUDE.md/skills dispatcher row, CHANGELOG, MCP tool-count guard bump (`canonical_docs_read_tool_count_matches_registry`).

## Carried open questions (v2 ledger)

- Per-token SPLADE attribution (needs postings accessor or single-chunk sparse fetch; pairs with the viz teaching layer, docs/plans/2026-06-25-splade-viz-design.md).
- Per-result rank ledger across stages (before/after rank per chunk per stage).
- Whether serve's `/api/search_legs` migrates onto `why_core`.
- Whether `scout`/`gather`/`task` seeds should accept a `--why` passthrough for their seed search.
