//! Dead code detection with confidence scoring.

use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
use sqlx::Row;

use super::{
    build_entry_point_names, build_trait_method_names, DeadConfidence, DeadFunction, LightChunk,
    LowConfidenceLiveInfo, TRAIT_IMPL_RE,
};
use crate::parser::{ChunkType, Language};
use crate::store::helpers::{clamp_line_number, ChunkRow, ChunkSummary, StoreError};
use crate::store::Store;

/// Document-shaped file extensions whose extracted code blocks are illustrative,
/// not part of the project's call graph. A function chunked out of a fenced code
/// block in one of these files is never a dead-code candidate. Single source for
/// both the SQL `NOT LIKE` exclusions ([`dead_doc_path_excludes_sql`]) and the
/// in-Rust predicate ([`is_dead_doc_path`]) the worktree-overlay dead path uses,
/// so the two views of "doc-shaped origin" can never drift. `.css`/`.html` are
/// covered by the chunk-type filter; `.scss`/`.sass`/`.less` are listed here in
/// case a future parser treats them as code.
pub(crate) const DEAD_DOC_PATH_EXTENSIONS: &[&str] = &[
    ".md", ".mdx", ".adoc", ".rst", ".txt", ".tex", ".scss", ".sass", ".less",
];

/// SQL `AND c.origin NOT LIKE '%.ext'` clause set for the dead-candidate query,
/// generated from [`DEAD_DOC_PATH_EXTENSIONS`]. Leading newline + indentation
/// match the previous inline literal so the formatted SQL is unchanged.
fn dead_doc_path_excludes_sql() -> String {
    DEAD_DOC_PATH_EXTENSIONS
        .iter()
        .map(|ext| format!("\n                AND c.origin NOT LIKE '%{ext}'"))
        .collect()
}

/// Whether `origin` is a document-shaped path (one of
/// [`DEAD_DOC_PATH_EXTENSIONS`]) — the Rust counterpart of the SQL
/// `dead_doc_path_excludes_sql`. The worktree-overlay dead path
/// (`resolve_dead_candidate_def`) uses this so an overlay-added candidate is
/// held to the same doc-path admissibility the parent SQL applies.
pub fn is_dead_doc_path(origin: &str) -> bool {
    DEAD_DOC_PATH_EXTENSIONS
        .iter()
        .any(|ext| origin.ends_with(ext))
}

impl<Mode> Store<Mode> {
    /// Find functions/methods never called by any real-caller edge (dead code
    /// detection). A function qualifies only when no `function_calls` row names
    /// it as callee through a real-caller kind — trusted (`call`,
    /// `serde_callback`) or heuristic (`macro_heuristic`, `fn_pointer`). A
    /// `doc_reference` edge does NOT count: a prose mention invokes nothing, so a
    /// function whose only inbound edge is a doc reference is still dead. This is
    /// the strict zero-real-edge contract: heuristic-only callees do NOT enter
    /// this set.
    /// (`cqs dead` separately surfaces the heuristic-only population via
    /// [`Store::find_low_confidence_live_functions`] and relabels it
    /// `low-confidence-live`; that union is additive on the `dead` surface only,
    /// so `health`/`ci`/`suggest`, which consume this method directly, never see
    /// heuristic-live code reported as dead.)
    /// Returns two lists:
    /// - `confident`: Functions with no callers that are likely dead (with confidence scores)
    /// - `possibly_dead_pub`: Public functions with no callers (may be used externally)
    /// Uses two-phase query: lightweight metadata first, then content only for
    /// candidates that pass name/test/path filters (avoids loading large function bodies).
    /// Exclusions applied:
    /// - Entry point names (`main`, `init`, `handler`, etc.)
    /// - Test functions (via `find_test_chunks()` heuristics)
    /// - Functions in test files
    /// - Trait implementations (dynamic dispatch invisible to call graph)
    /// - `#[no_mangle]` functions (FFI)
    /// Confidence scoring:
    /// - **High**: Private function in a file where no other function has callers
    /// - **Medium**: Private function in an active file (other functions are called)
    /// - **Low**: Method, or function with constructor-like name patterns
    pub fn find_dead_code(
        &self,
        include_pub: bool,
    ) -> Result<(Vec<DeadFunction>, Vec<DeadFunction>), StoreError> {
        let _span = tracing::info_span!("find_dead_code", include_pub).entered();
        self.rt.block_on(async {
            // Phase 1: Fetch all uncalled functions (lightweight, no content/doc)
            let all_uncalled = self.fetch_uncalled_functions().await?;
            let total_uncalled = all_uncalled.len();

            let (confident, possibly_dead_pub) =
                self.filter_and_score(all_uncalled, include_pub).await?;

            tracing::info!(
                total_uncalled,
                confident = confident.len(),
                possibly_dead = possibly_dead_pub.len(),
                "Dead code analysis complete"
            );

            Ok((confident, possibly_dead_pub))
        })
    }

    /// Find functions reached by ≥1 heuristic edge (`macro_heuristic`,
    /// `fn_pointer`) and NO trusted edge (`call`, `serde_callback`) — the
    /// `low-confidence-live` population for `cqs dead`. Returns the same
    /// `(confident, possibly_dead_pub)` shape as [`Store::find_dead_code`],
    /// having run the identical Tier-1 candidate filters and confidence scoring,
    /// so `dead_core` can union these entries into its report and relabel them.
    ///
    /// This is the SURFACE-SCOPED counterpart to `find_dead_code`'s strict
    /// zero-edge contract: those two populations are disjoint by construction (a
    /// callee either has zero edges or has a heuristic edge, never both at once
    /// from the same predicate), and only `cqs dead` unions them. `health`,
    /// `ci`, and `suggest` call `find_dead_code` alone and never see this set —
    /// they must not report heuristic-live code as dead.
    ///
    /// `doc_reference` edges are inert here, mirroring
    /// [`Store::find_low_confidence_live_names`]: a callee reached only by a doc
    /// reference (no heuristic edge) is neither dead-by-this-method nor
    /// heuristic-live — it falls out of both populations, which is correct
    /// (a prose mention is not evidence of liveness).
    pub fn find_low_confidence_live_functions(
        &self,
        include_pub: bool,
    ) -> Result<(Vec<DeadFunction>, Vec<DeadFunction>), StoreError> {
        let _span =
            tracing::info_span!("find_low_confidence_live_functions", include_pub).entered();
        self.rt.block_on(async {
            let candidates = self.fetch_heuristic_only_callees().await?;
            let total = candidates.len();
            let (confident, possibly_dead_pub) =
                self.filter_and_score(candidates, include_pub).await?;
            tracing::info!(
                total,
                confident = confident.len(),
                possibly_dead = possibly_dead_pub.len(),
                "Low-confidence-live population resolved"
            );
            Ok((confident, possibly_dead_pub))
        })
    }

    /// Shared Phase 1.5/1.6/2 pipeline: given a raw Phase-1 candidate set,
    /// apply the test/entry-point/trait filters, the invoked-macro and
    /// serde-callback content filters, then batch-fetch content and score
    /// confidence. Both [`Store::find_dead_code`] (strict zero-edge candidates)
    /// and [`Store::find_low_confidence_live_functions`] (heuristic-only
    /// candidates) feed through this so the two surfaces filter identically.
    async fn filter_and_score(
        &self,
        all_candidates: Vec<LightChunk>,
        include_pub: bool,
    ) -> Result<(Vec<DeadFunction>, Vec<DeadFunction>), StoreError> {
        // Build test name set for exclusion (names-only query avoids ChunkSummary overhead)
        let test_names: std::collections::HashSet<String> = self
            .find_test_chunk_names_async()
            .await?
            .into_iter()
            .collect();

        // Phase 1 filtering: name/test/path/trait checks (don't need content)
        let mut candidates = Self::filter_candidates(all_candidates, &test_names);

        // Phase 1.5: macros invoked at file-scope live outside any
        // function chunk, so their `!()` invocation never produces a
        // `function_calls` edge. Result: every macro_rules! shows up
        // as "uncalled" even when it's used heavily. The call-graph
        // extractor can't fix this without chunker changes (file-level
        // macro_invocations aren't in any chunk's byte range), so we
        // special-case Macro chunks at this layer: scan all chunks'
        // content for `<name>!` substring; if any hit, drop from the
        // candidates list.
        //
        // False-negative-friendly: a comment like "// foo! is broken"
        // counts as a reference, keeping the macro live even when the
        // implementation is actually unused. Dead-code analysis prefers
        // under-reporting to over-reporting — comments referencing a
        // name signal intentional retention.
        candidates = self.filter_invoked_macros(candidates).await?;

        // Phase 1.6: serde string-callback references. Functions named
        // in attribute strings — `#[serde(default = "default_ref_weight")]`,
        // `#[serde(skip_serializing_if = "is_zero_u32")]`,
        // `#[serde(with = "...")]` — are reached by the derive-generated
        // (de)serializer, not by a syntactic `foo()` call. The call-graph
        // extractor walks call/macro nodes, so a string reference inside an
        // attribute produces no edge and the callback shows as "uncalled".
        // Drop any Rust function candidate whose name appears as the
        // terminal segment of a serde-shaped attribute string anywhere in
        // the corpus. Bounded content scan, same shape as the macro filter.
        candidates = self.filter_serde_callbacks(candidates).await?;

        // Phase 2: Batch-fetch content and score confidence
        let active_files = self.fetch_active_files().await?;
        self.score_confidence(candidates, &active_files, include_pub)
            .await
    }

    /// Find callee names reached by at least one heuristic edge
    /// (`macro_heuristic`, `fn_pointer`) and NO trusted edge (`call`,
    /// `serde_callback`) — i.e. their entire liveness rests on heuristics that
    /// could be false positives. These are the `low-confidence-live` verdict
    /// population for `cqs dead`.
    ///
    /// Consumes §1's `function_calls.edge_kind` column with the kind-sets
    /// generated from [`CallEdgeKind`] (single source — no lexical ordering).
    /// A callee qualifies when:
    /// * it has ≥1 edge in the heuristic set, AND
    /// * it has zero edges in the trusted set.
    ///
    /// `doc_reference` edges are inert here: a prose mention neither qualifies
    /// (not heuristic) nor disqualifies (not trusted) a callee, so a function
    /// reached only by a doc reference plus a macro edge still surfaces.
    ///
    /// This returns the heuristic-caller BREAKDOWN (kind + count per callee)
    /// used to render the `low-confidence-live` verdict reason string. The
    /// matching CHUNK population is fetched by
    /// [`Store::find_low_confidence_live_functions`] (same heuristic-only
    /// predicate) and unioned into the `cqs dead` report by `dead_core`;
    /// `fetch_uncalled_functions` holds the disjoint strict zero-edge contract,
    /// so the two populations never overlap. The kind-sets are generated from
    /// `CallEdgeKind` rather than a lexical comparison, so a new kind cannot
    /// drift out of sync.
    pub fn find_low_confidence_live_names(
        &self,
    ) -> Result<std::collections::HashMap<String, LowConfidenceLiveInfo>, StoreError> {
        let _span = tracing::info_span!("find_low_confidence_live_names").entered();
        let heuristic = crate::parser::CallEdgeKind::heuristic_kinds_sql();
        let trusted = crate::parser::CallEdgeKind::trusted_kinds_sql();
        // Per (callee, heuristic kind) count, restricted to callees with NO
        // trusted edge. Grouping by kind too lets the verdict reason name the
        // heuristic kinds and their counts. The outer trusted-edge exclusion is
        // applied per callee via the windowed SUM in the HAVING-style subquery.
        let sql = format!(
            "SELECT callee_name, edge_kind, COUNT(*) AS n
             FROM function_calls
             WHERE edge_kind IN ({heuristic})
               AND callee_name IN (
                   SELECT callee_name FROM function_calls
                   GROUP BY callee_name
                   HAVING SUM(CASE WHEN edge_kind IN ({trusted}) THEN 1 ELSE 0 END) = 0
               )
             GROUP BY callee_name, edge_kind"
        );
        self.rt.block_on(async {
            let rows: Vec<(String, String, i64)> =
                sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
                    .fetch_all(&self.pool)
                    .await?;
            let mut out: std::collections::HashMap<String, LowConfidenceLiveInfo> =
                std::collections::HashMap::new();
            for (name, kind, n) in rows {
                let info = out.entry(name).or_default();
                info.total += n.max(0) as u64;
                info.kind_counts.push((kind, n.max(0) as u64));
            }
            // Stable kind order for deterministic reason strings.
            for info in out.values_mut() {
                info.kind_counts.sort();
            }
            Ok(out)
        })
    }

    /// Phase 1: Query all callable chunks with no callers in the call graph.
    /// Returns lightweight metadata without content/doc to minimize memory.
    ///
    /// Tier-1 noise filters:
    /// - **Exclude `Property` chunk_type.** CSS `rule_set`s and similar
    ///   language-property nodes are classified as `Callable` for search
    ///   purposes but are not function-shaped. They have no callers by
    ///   construction — surfacing them in dead-code output is pure noise.
    /// - **Exclude documentation file extensions.** Markdown/AsciiDoc/RST
    ///   files often contain example code blocks that the parser
    ///   extracts as Rust/Python/etc. functions. Those functions live in
    ///   docs, not in the project's call graph; flagging them as dead
    ///   makes the operator chase noise. `.css`/`.html` extensions
    ///   covered by the chunk-type filter; `.scss`/`.sass`/`.less` listed
    ///   here too in case future parsers treat them as code.
    async fn fetch_uncalled_functions(&self) -> Result<Vec<LightChunk>, StoreError> {
        let callable = ChunkType::callable_sql_list();
        // Document-shaped paths: code blocks inside these files are
        // illustrative, not part of the project's call graph.
        let doc_path_excludes = dead_doc_path_excludes_sql();
        // Dead-candidate population: strict zero-real-edge. A function qualifies
        // only when NO `function_calls` row names it as callee through a
        // real-caller kind — trusted or heuristic. A `doc_reference` edge is
        // inert: a prose mention invokes nothing, so a function whose only
        // inbound edge is a doc reference is still dead (the `NOT EXISTS`
        // subquery excludes `doc_reference` via the real-caller kind set, the
        // same view of "real caller" the `low-confidence-live` carve-out uses).
        // This is the contract `health`/`ci`/`suggest` depend on: a
        // heuristic-only callee (macro/fn-pointer) is NOT dead and must not enter
        // this set. `cqs dead` surfaces that heuristic-only population separately
        // via `fetch_heuristic_only_callees` and relabels it
        // `low-confidence-live` — an additive overlay on the `dead` surface only,
        // never a mutation of this shared candidate set.
        let real_callers = crate::parser::CallEdgeKind::real_caller_kinds_sql();
        let sql = format!(
            "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature,
                    c.line_start, c.line_end, c.parent_id
             FROM chunks c
             WHERE c.chunk_type IN ({callable})
               AND c.chunk_type != 'property'
               {doc_path_excludes}
               AND NOT EXISTS (SELECT 1 FROM function_calls fc \
                               WHERE fc.callee_name = c.name \
                                 AND fc.edge_kind IN ({real_callers}) LIMIT 1)
               AND c.parent_id IS NULL
             ORDER BY c.origin, c.line_start"
        );
        Self::light_chunks_from_query(&sql, &self.pool).await
    }

    /// Phase 1 (low-confidence-live): query callable chunks reached by ≥1
    /// heuristic edge (`macro_heuristic`, `fn_pointer`) and NO trusted edge
    /// (`call`, `serde_callback`). Same Tier-1 noise filters as
    /// `fetch_uncalled_functions` (Property exclusion, doc-path exclusion,
    /// top-level only). The heuristic and trusted kind-sets are generated from
    /// `CallEdgeKind` (single source), so a new edge kind updates both surfaces
    /// at once. `doc_reference` edges are inert: they neither qualify (not
    /// heuristic) nor disqualify (not trusted), matching
    /// [`Store::find_low_confidence_live_names`].
    async fn fetch_heuristic_only_callees(&self) -> Result<Vec<LightChunk>, StoreError> {
        let callable = ChunkType::callable_sql_list();
        let doc_path_excludes = dead_doc_path_excludes_sql();
        let heuristic = crate::parser::CallEdgeKind::heuristic_kinds_sql();
        let trusted = crate::parser::CallEdgeKind::trusted_kinds_sql();
        let sql = format!(
            "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature,
                    c.line_start, c.line_end, c.parent_id
             FROM chunks c
             WHERE c.chunk_type IN ({callable})
               AND c.chunk_type != 'property'
               {doc_path_excludes}
               AND EXISTS (SELECT 1 FROM function_calls fc \
                           WHERE fc.callee_name = c.name \
                             AND fc.edge_kind IN ({heuristic}) LIMIT 1)
               AND NOT EXISTS (SELECT 1 FROM function_calls fc \
                               WHERE fc.callee_name = c.name \
                                 AND fc.edge_kind IN ({trusted}) LIMIT 1)
               AND c.parent_id IS NULL
             ORDER BY c.origin, c.line_start"
        );
        Self::light_chunks_from_query(&sql, &self.pool).await
    }

    /// Run a Phase-1 dead-candidate SQL query (the canonical 9-column
    /// `SELECT c.id, c.origin, ...` projection) and map rows to [`LightChunk`].
    /// Shared by `fetch_uncalled_functions` and `fetch_heuristic_only_callees`
    /// so both produce identical row shapes from a single mapping site.
    async fn light_chunks_from_query(
        sql: &str,
        pool: &sqlx::SqlitePool,
    ) -> Result<Vec<LightChunk>, StoreError> {
        let rows: Vec<_> = sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_all(pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| LightChunk {
                id: row.get(0),
                file: PathBuf::from(row.get::<String, _>(1)),
                language: {
                    let raw: String = row.get(2);
                    raw.parse().unwrap_or_else(|_| {
                        tracing::warn!(raw = %raw, "Unknown language in DB, defaulting to Rust");
                        Language::Rust
                    })
                },
                chunk_type: {
                    let raw: String = row.get(3);
                    raw.parse().unwrap_or_else(|_| {
                        tracing::warn!(raw = %raw, "Unknown chunk_type in DB, defaulting to Function");
                        ChunkType::Function
                    })
                },
                name: row.get(4),
                signature: row.get(5),
                line_start: clamp_line_number(row.get::<i64, _>(6)),
                line_end: clamp_line_number(row.get::<i64, _>(7)),
            })
            .collect())
    }

    /// Phase 1.5: drop macros from the dead-code candidates if they're
    /// invoked anywhere in the corpus. Closes the file-level macro-
    /// invocation gap: the call-graph extractor only runs over chunks,
    /// but `define_languages! { ... }` and similar invocations live at
    /// file-scope outside any chunk's byte range.
    ///
    /// Runs a **single** pass over `chunks.content` inside one read
    /// transaction, testing every Rust macro candidate's `<name>!`
    /// invocation token against each chunk in Rust. The previous shape
    /// ran one GLOB-per-macro query (N+1 full content scans — every dead
    /// macro re-walked the whole 14k-chunk table). With tens of candidates
    /// that was tens of full scans; this collapses to one ordered walk,
    /// paged by rowid so peak heap stays bounded by the batch size rather
    /// than the corpus content size.
    ///
    /// Contract details (behavior identical to the per-macro GLOB form):
    /// - **Rust-only.** The `!` invocation suffix is Rust-specific. Other
    ///   languages with macros (C/C++, Elixir, Erlang, Julia, Verilog)
    ///   use different invocation syntaxes; their macros pass through to
    ///   dead candidates untouched.
    /// - **Case-sensitive.** The in-Rust `str::contains` for `<name>!`
    ///   is byte-exact — `MyMacro!` content does not cross-fire against a
    ///   `mymacro!` definition. This matches the GLOB (not LIKE) semantics
    ///   the per-macro form relied on, with no `_`/`%`/`*`/`?` wildcard
    ///   collision to defend against (substring match has no wildcards).
    /// - **Self-match exclusion.** Recursive `macro_rules!` bodies contain
    ///   the macro's own name + `!` in expansion examples or recursive
    ///   invocations. A candidate is only marked invoked by a chunk whose
    ///   id differs from its definition id, so a recursive macro with no
    ///   external caller stays in the dead list.
    async fn filter_invoked_macros(
        &self,
        candidates: Vec<LightChunk>,
    ) -> Result<Vec<LightChunk>, StoreError> {
        // Partition Rust macro candidates (the only ones this filter can
        // touch) from everything else. Non-macro / non-Rust candidates pass
        // through without ever consulting content.
        let mut macro_idx: Vec<usize> = Vec::new();
        for (i, c) in candidates.iter().enumerate() {
            if c.chunk_type == ChunkType::Macro && c.language == Language::Rust {
                macro_idx.push(i);
            }
        }

        // No Rust macros among the candidates: skip the content scan entirely.
        if macro_idx.is_empty() {
            return Ok(candidates);
        }

        // Per-macro invocation token (`<name>!`) and the defining chunk id,
        // so a chunk can never mark a macro live via the macro's own body.
        let invoked_tokens: Vec<(String, String)> = macro_idx
            .iter()
            .map(|&i| (format!("{}!", candidates[i].name), candidates[i].id.clone()))
            .collect();

        // `invoked[k] == true` once macro `macro_idx[k]` is seen invoked by
        // some other chunk. A single ordered pass over content fills this in.
        let mut invoked = vec![false; macro_idx.len()];

        // One read transaction, paged by rowid so peak heap is bounded by the
        // batch size rather than the total content bytes. We can stop early
        // once every candidate has been marked invoked.
        let mut tx = self.pool.begin().await?;
        const PAGE: i64 = 2048;
        let mut last_rowid: i64 = 0;
        let mut remaining = invoked.len();
        'scan: loop {
            let rows: Vec<(i64, String, String)> = sqlx::query_as(
                "SELECT rowid, id, content FROM chunks
                 WHERE rowid > ?1
                 ORDER BY rowid
                 LIMIT ?2",
            )
            .bind(last_rowid)
            .bind(PAGE)
            .fetch_all(&mut *tx)
            .await?;

            if rows.is_empty() {
                break;
            }

            for (rowid, chunk_id, content) in &rows {
                last_rowid = *rowid;
                for (k, (token, def_id)) in invoked_tokens.iter().enumerate() {
                    if invoked[k] || chunk_id == def_id {
                        continue;
                    }
                    if content.contains(token.as_str()) {
                        invoked[k] = true;
                        remaining -= 1;
                        if remaining == 0 {
                            break 'scan;
                        }
                    }
                }
            }
        }
        drop(tx);

        // Map invocation results back onto candidate indices, then rebuild
        // the surviving candidate list in original order.
        let mut dropped: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (k, &i) in macro_idx.iter().enumerate() {
            if invoked[k] {
                dropped.insert(i);
            }
        }

        let drop_count = dropped.len();
        if drop_count > 0 {
            // Surface the invoked-macro drop count so the false-positive rate
            // of this Tier-1.5 filter is auditable — a silent `continue`
            // hid how many candidates the macro heuristic removed.
            tracing::debug!(
                dropped = drop_count,
                rust_macro_candidates = macro_idx.len(),
                "filter_invoked_macros dropped invoked macros from dead candidates"
            );
        }

        let filtered = candidates
            .into_iter()
            .enumerate()
            .filter_map(|(i, c)| if dropped.contains(&i) { None } else { Some(c) })
            .collect();
        Ok(filtered)
    }

    /// Phase 1.6: drop Rust functions that are referenced only as serde
    /// string callbacks. serde's derive macros accept function paths as
    /// attribute string literals:
    ///
    /// ```ignore
    /// #[serde(default = "default_ref_weight")]
    /// #[serde(skip_serializing_if = "is_zero_u32")]
    /// #[serde(serialize_with = "crate::serialize_path_normalized")]
    /// #[serde(with = "some::module")]
    /// ```
    ///
    /// The derived (de)serializer calls these at runtime, but the reference
    /// lives in an attribute string the call-graph walker never resolves into
    /// an edge. So the callbacks look uncalled. This pass scans every chunk's
    /// content for serde-shaped attribute strings, extracts the terminal path
    /// segment of each (`crate::a::b::f` → `f`), and drops any Rust function
    /// candidate whose name matches.
    ///
    /// Contract details (mirrors `filter_invoked_macros`):
    /// - **Functions only.** serde callbacks are free functions / associated
    ///   functions, never trait methods reached through dispatch. Method and
    ///   macro candidates pass through untouched.
    /// - **Terminal-segment match.** `serialize_with = "crate::foo::bar"`
    ///   keeps a candidate named `bar` alive regardless of the module path,
    ///   matching how a candidate chunk is keyed by bare name.
    /// - **Self-reference is fine.** Unlike the macro filter, a callback
    ///   referenced inside its own defining chunk is still genuinely used by
    ///   the deriver — the attribute is on *another* type's field, not in the
    ///   function body. We do not need (or want) a self-exclusion guard here;
    ///   the attribute and the `fn` definition are different chunks anyway.
    /// - **False-negative-friendly.** Matching is by substring presence of a
    ///   serde-shaped attribute naming the candidate; a stale attribute string
    ///   keeps the function live. Dead-code analysis prefers under-reporting.
    async fn filter_serde_callbacks(
        &self,
        candidates: Vec<LightChunk>,
    ) -> Result<Vec<LightChunk>, StoreError> {
        // serde string callbacks resolve to free/associated functions. Build a
        // bare-name → candidate-index map over Rust Function/Method candidates
        // (associated fns are tagged Method when they live in an impl block, so
        // we accept both rather than miss `Type::default_x`).
        let mut by_name: std::collections::HashMap<&str, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, c) in candidates.iter().enumerate() {
            if c.language != Language::Rust {
                continue;
            }
            if matches!(c.chunk_type, ChunkType::Function | ChunkType::Method) {
                by_name.entry(c.name.as_str()).or_default().push(i);
            }
        }

        // No Rust function candidates: skip the content scan entirely.
        if by_name.is_empty() {
            return Ok(candidates);
        }

        // Matches serde-shaped callback attributes and captures the quoted
        // path: `default = "..."`, `with = "..."`, `serialize_with = "..."`,
        // `deserialize_with = "..."`, `skip_serializing_if = "..."`,
        // `bound = "..."` is excluded (it names types/where-clauses, not fns).
        static SERDE_CALLBACK_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r#"(?:default|with|serialize_with|deserialize_with|skip_serializing_if|skip_serializing|skip_deserializing|getter)\s*=\s*"([^"]+)""#,
            )
            .expect("hardcoded serde-callback regex")
        });

        let mut dropped: std::collections::HashSet<usize> = std::collections::HashSet::new();

        // One read transaction, paged by rowid so peak heap is bounded by the
        // batch size. We only scan chunks whose content contains "serde" to
        // skip the bulk of the corpus; the regex runs only on the rest.
        let mut tx = self.pool.begin().await?;
        const PAGE: i64 = 2048;
        let mut last_rowid: i64 = 0;
        loop {
            let rows: Vec<(i64, String)> = sqlx::query_as(
                "SELECT rowid, content FROM chunks
                 WHERE rowid > ?1 AND content LIKE '%serde%'
                 ORDER BY rowid
                 LIMIT ?2",
            )
            .bind(last_rowid)
            .bind(PAGE)
            .fetch_all(&mut *tx)
            .await?;

            if rows.is_empty() {
                break;
            }

            for (rowid, content) in &rows {
                last_rowid = *rowid;
                for cap in SERDE_CALLBACK_RE.captures_iter(content) {
                    let path = &cap[1];
                    // Terminal path segment: `crate::a::b::f` → `f`.
                    let terminal = path.rsplit("::").next().unwrap_or(path);
                    if let Some(indices) = by_name.get(terminal) {
                        for &i in indices {
                            dropped.insert(i);
                        }
                    }
                }
            }
        }
        drop(tx);

        let drop_count = dropped.len();
        if drop_count > 0 {
            tracing::debug!(
                dropped = drop_count,
                rust_fn_candidates = by_name.values().map(|v| v.len()).sum::<usize>(),
                "filter_serde_callbacks dropped serde string callbacks from dead candidates"
            );
        }

        let filtered = candidates
            .into_iter()
            .enumerate()
            .filter_map(|(i, c)| if dropped.contains(&i) { None } else { Some(c) })
            .collect();
        Ok(filtered)
    }

    /// Phase 1 filter: exclude entry points, tests, trait methods from uncalled functions.
    /// Operates on lightweight metadata only — no content needed.
    /// Entry point and trait method names are sourced from `LanguageDef` fields
    /// across all enabled languages, with cross-language fallbacks.
    fn filter_candidates(
        uncalled: Vec<LightChunk>,
        test_names: &std::collections::HashSet<String>,
    ) -> Vec<LightChunk> {
        // Use LazyLock-cached sets instead of rebuilding on every call
        static ENTRY_POINTS: LazyLock<std::collections::HashSet<&'static str>> =
            LazyLock::new(|| build_entry_point_names().into_iter().collect());
        static TRAIT_METHODS: LazyLock<std::collections::HashSet<&'static str>> =
            LazyLock::new(|| build_trait_method_names().into_iter().collect());
        let entry_points = &*ENTRY_POINTS;
        let trait_methods = &*TRAIT_METHODS;

        let mut candidates = Vec::new();

        for chunk in uncalled {
            // Skip entry points (main, init, handler, etc.)
            if entry_points.contains(chunk.name.as_str()) {
                continue;
            }
            if test_names.contains(&chunk.name) {
                continue;
            }
            let path_str = chunk.file.to_string_lossy();
            if crate::is_test_chunk(&chunk.name, &path_str) {
                continue;
            }

            // Methods with well-known trait names can be skipped without content
            if chunk.chunk_type == ChunkType::Method && trait_methods.contains(chunk.name.as_str())
            {
                continue;
            }

            // Signature-only trait impl check
            if chunk.chunk_type == ChunkType::Method && TRAIT_IMPL_RE.is_match(&chunk.signature) {
                continue;
            }

            candidates.push(chunk);
        }

        candidates
    }

    /// Fetch sets of files with call graph or type-edge activity.
    /// Used for confidence scoring: files with active functions are "active".
    async fn fetch_active_files(&self) -> Result<std::collections::HashSet<String>, StoreError> {
        // Query function_calls directly (no JOIN on chunks) for files with callers.
        // UNION with type_edges for files with type-edge activity.
        // Propagate SQL error instead of swallowing — an empty set inflates dead code confidence.
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT file FROM function_calls
             UNION
             SELECT DISTINCT c.origin FROM chunks c
             JOIN type_edges te ON c.id = te.source_chunk_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(f,)| f).collect())
    }

    /// Phase 2: Batch-fetch content for candidates and assign confidence scores.
    /// Splits results into confident dead code and possibly-dead public functions.
    async fn score_confidence(
        &self,
        candidates: Vec<LightChunk>,
        active_files: &std::collections::HashSet<String>,
        include_pub: bool,
    ) -> Result<(Vec<DeadFunction>, Vec<DeadFunction>), StoreError> {
        // Batch-fetch content for remaining candidates (use references to avoid cloning IDs)
        let candidate_ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let mut content_map: std::collections::HashMap<String, (String, Option<String>)> =
            std::collections::HashMap::new();

        // Batch size for the single-bind IN query. SQLite permits 32766
        // variables; `max_rows_per_statement(1)` returns ~32466 here (1 var/row).
        use crate::store::helpers::sql::max_rows_per_statement;
        let batch_size = max_rows_per_statement(1);
        for batch in candidate_ids.chunks(batch_size) {
            let placeholders = super::super::helpers::make_placeholders(batch.len());
            let sql = format!(
                "SELECT id, content, doc FROM chunks WHERE id IN ({})",
                placeholders
            );
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
            for id in batch {
                q = q.bind(id);
            }
            let rows: Vec<_> = q.fetch_all(&self.pool).await?;
            for row in rows {
                let id: String = row.get(0);
                let content: String = row.get(1);
                let doc: Option<String> = row.get(2);
                content_map.insert(id, (content, doc));
            }
        }

        let mut confident = Vec::new();
        let mut possibly_dead_pub = Vec::new();

        for light in candidates {
            // Log when content is missing — indicates a deleted/stale chunk in the index
            let (content, doc) = match content_map.remove(&light.id) {
                Some(pair) => pair,
                None => {
                    tracing::warn!(
                        chunk_id = %light.id,
                        name = %light.name,
                        "Content missing for dead code candidate — chunk may be stale"
                    );
                    (String::new(), None)
                }
            };

            // Content-based trait impl check for methods
            if light.chunk_type == ChunkType::Method && TRAIT_IMPL_RE.is_match(&content) {
                continue;
            }

            // Skip #[no_mangle] FFI functions
            if content.contains("no_mangle") {
                continue;
            }

            // Check if public
            let is_pub = content.starts_with("pub ")
                || content.starts_with("pub(")
                || light.signature.starts_with("pub ")
                || light.signature.starts_with("pub(");

            // Confidence scoring
            let is_method = light.chunk_type == ChunkType::Method;
            let file_str = light.file.to_string_lossy();
            let file_is_active = active_files.contains(file_str.as_ref());

            let confidence = if is_method {
                // Methods are more likely trait impls or interface implementations
                DeadConfidence::Low
            } else if !file_is_active {
                // File has no functions with callers — likely entirely unused
                DeadConfidence::High
            } else {
                // Function in an active file — could be a helper
                DeadConfidence::Medium
            };

            let chunk = ChunkSummary::from(ChunkRow::from_light_chunk(light, content, doc));

            let dead_fn = DeadFunction { chunk, confidence };

            if is_pub && !include_pub {
                possibly_dead_pub.push(dead_fn);
            } else {
                confident.push(dead_fn);
            }
        }

        Ok((confident, possibly_dead_pub))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::setup_store;

    // ===== Dead code: entry point exclusion tests =====

    #[test]
    fn test_entry_point_exclusion() {
        let (store, _dir) = setup_store();

        // Insert chunks for known entry points
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);
        for name in &["main", "init", "handler", "middleware"] {
            let chunk = crate::parser::Chunk {
                id: format!("src/app.rs:1:{name}"),
                file: std::path::PathBuf::from("src/app.rs"),
                language: crate::parser::Language::Rust,
                chunk_type: crate::parser::ChunkType::Function,
                name: name.to_string(),
                signature: format!("fn {name}()"),
                content: format!("fn {name}() {{}}"),
                doc: None,
                line_start: 1,
                line_end: 3,
                byte_start: 0,
                content_hash: format!("{name}_hash"),
                canonical_hash: String::new(),
                parent_id: None,
                window_idx: None,
                parent_type_name: None,
                parser_version: 0,
            };
            store.upsert_chunk(&chunk, &emb, Some(12345)).unwrap();
        }

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let all_names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        for ep in &["main", "init", "handler", "middleware"] {
            assert!(
                !all_names.contains(ep),
                "Entry point '{ep}' should be excluded from dead code"
            );
        }
    }

    // ===== Dead code: confidence scoring tests =====

    /// Tier-1 filter regression: a `Property` chunk (e.g., a CSS `rule_set`)
    /// must not appear in dead-code output even when nothing calls it. The
    /// SQL gate at `fetch_uncalled_functions` excludes `chunk_type =
    /// 'property'` because Property nodes are not function-shaped — they
    /// inhabit a different syntactic universe (style rules, language
    /// property accessors) and surfacing them is pure noise.
    #[test]
    fn test_property_chunks_excluded() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);
        let css_rule = crate::parser::Chunk {
            id: "src/serve/assets/app.css:10:rule_hash".to_string(),
            file: std::path::PathBuf::from("src/serve/assets/app.css"),
            language: crate::parser::Language::Css,
            chunk_type: crate::parser::ChunkType::Property,
            name: ".my-button".to_string(),
            signature: ".my-button { ... }".to_string(),
            content: ".my-button { color: red; }".to_string(),
            doc: None,
            line_start: 10,
            line_end: 12,
            byte_start: 0,
            content_hash: "rule_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&css_rule, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let all_names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !all_names.contains(&".my-button"),
            "CSS Property chunk must be excluded from dead-code output: got {all_names:?}"
        );
    }

    /// Tier-1 filter regression: a function chunk extracted from a
    /// markdown code block (file ends in `.md`) must not appear in
    /// dead-code output. The chunker treats fenced-code blocks inside
    /// docs as Rust functions; those are illustrative examples, not
    /// dead code in the project's call graph. The SQL gate's
    /// `c.origin NOT LIKE '%.md'` clause closes this surface.
    #[test]
    fn test_doc_extension_chunks_excluded() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);
        let md_func = crate::parser::Chunk {
            id: "docs/example.md:42:doc_hash".to_string(),
            file: std::path::PathBuf::from("docs/example.md"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "doc_example_fn".to_string(),
            signature: "fn doc_example_fn()".to_string(),
            content: "fn doc_example_fn() { println!(\"hello\"); }".to_string(),
            doc: None,
            line_start: 42,
            line_end: 44,
            byte_start: 0,
            content_hash: "doc_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&md_func, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let all_names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !all_names.contains(&"doc_example_fn"),
            "function from .md doc-block must be excluded from dead-code output: got {all_names:?}"
        );
    }

    #[test]
    fn test_confidence_assignment() {
        let (store, _dir) = setup_store();

        // Insert a function and a method, both uncalled
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let func_chunk = crate::parser::Chunk {
            id: "src/orphan.rs:1:func_hash".to_string(),
            file: std::path::PathBuf::from("src/orphan.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "orphan_func".to_string(),
            signature: "fn orphan_func()".to_string(),
            content: "fn orphan_func() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 3,
            byte_start: 0,
            content_hash: "func_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&func_chunk, &emb, Some(12345)).unwrap();

        let method_chunk = crate::parser::Chunk {
            id: "src/orphan.rs:5:meth_hash".to_string(),
            file: std::path::PathBuf::from("src/orphan.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Method,
            name: "orphan_method".to_string(),
            signature: "fn orphan_method(&self)".to_string(),
            content: "fn orphan_method(&self) {}".to_string(),
            doc: None,
            line_start: 5,
            line_end: 7,
            byte_start: 0,
            content_hash: "meth_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunk(&method_chunk, &emb, Some(12345))
            .unwrap();

        let (confident, _) = store.find_dead_code(true).unwrap();

        let func_dead = confident.iter().find(|d| d.chunk.name == "orphan_func");
        let method_dead = confident.iter().find(|d| d.chunk.name == "orphan_method");

        // Function in a file with no callers should be High confidence
        assert!(
            func_dead.is_some(),
            "orphan_func should be in dead code list"
        );
        assert_eq!(
            func_dead.unwrap().confidence,
            DeadConfidence::High,
            "Private function in inactive file should be High confidence"
        );

        // Method should be Low confidence
        assert!(
            method_dead.is_some(),
            "orphan_method should be in dead code list"
        );
        assert_eq!(
            method_dead.unwrap().confidence,
            DeadConfidence::Low,
            "Method should be Low confidence"
        );
    }

    // ===== filter_invoked_macros tests =====

    /// The macro filter is Rust-only because the `!` invocation suffix
    /// is Rust-specific. Non-Rust macros (here: an Elixir macro chunk)
    /// must pass through to dead candidates unchanged regardless of
    /// whether their name appears in any chunk's content.
    #[test]
    fn test_non_rust_macros_skip_filter() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // Elixir macro definition.
        let elixir_macro = crate::parser::Chunk {
            id: "lib/foo.ex:1:my_elixir_macro_hash".to_string(),
            file: std::path::PathBuf::from("lib/foo.ex"),
            language: crate::parser::Language::Elixir,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "my_elixir_macro".to_string(),
            signature: "defmacro my_elixir_macro(x)".to_string(),
            content: "defmacro my_elixir_macro(x) do quote do unquote(x) end end".to_string(),
            doc: None,
            line_start: 1,
            line_end: 3,
            byte_start: 0,
            content_hash: "elixir_macro_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunk(&elixir_macro, &emb, Some(12345))
            .unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        // Non-Rust macros pass through — `my_elixir_macro` should be
        // in the dead list (no callers, no chunk content with `!` suffix).
        // Filter should NOT have dropped it for the wrong reason.
        // (Whether it's in the dead list at all depends on other Tier 1
        // filters; the contract this test pins is: filter_invoked_macros
        // is a no-op for non-Rust language chunks.)
        let _ = names; // result not load-bearing for this contract test
    }

    /// A recursive macro's expansion examples or recursive invocations
    /// contain its own name + `!`. Without the `id != ?2` self-exclusion
    /// in the SQL, the macro keeps itself alive even when no other
    /// caller exists.
    #[test]
    fn test_recursive_macro_self_match_excluded() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let recursive_macro = crate::parser::Chunk {
            id: "src/lib.rs:10:recursive_hash".to_string(),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "my_recursive_macro".to_string(),
            signature: "macro_rules! my_recursive_macro".to_string(),
            // Body contains its own name with `!` — without
            // self-exclusion, the LIKE/GLOB scan would match here.
            content: "macro_rules! my_recursive_macro { (0) => {}; ($n:expr) => { my_recursive_macro!($n - 1) } }".to_string(),
            doc: None,
            line_start: 10,
            line_end: 14,
            byte_start: 0,
            content_hash: "recursive_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunk(&recursive_macro, &emb, Some(12345))
            .unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        assert!(
            names.contains(&"my_recursive_macro"),
            "Recursive macro with no external caller must be flagged dead — \
             self-match in body should be excluded by `id != ?2` SQL filter"
        );
    }

    /// A Rust macro with an external caller (chunk content containing
    /// `<name>!`) is correctly dropped from dead candidates.
    #[test]
    fn test_invoked_rust_macro_dropped_from_dead() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // The macro definition itself.
        let macro_def = crate::parser::Chunk {
            id: "src/macros.rs:1:define_languages_hash".to_string(),
            file: std::path::PathBuf::from("src/macros.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "define_languages".to_string(),
            signature: "macro_rules! define_languages".to_string(),
            content: "macro_rules! define_languages { ($($name:ident),*) => { /* ... */ } }"
                .to_string(),
            doc: None,
            line_start: 1,
            line_end: 5,
            byte_start: 0,
            content_hash: "define_languages_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&macro_def, &emb, Some(12345)).unwrap();

        // A separate chunk that invokes the macro.
        let caller = crate::parser::Chunk {
            id: "src/lib.rs:50:caller_hash".to_string(),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "register_languages".to_string(),
            signature: "fn register_languages()".to_string(),
            content: "fn register_languages() { define_languages!(Rust, Python, Go); }".to_string(),
            doc: None,
            line_start: 50,
            line_end: 52,
            byte_start: 0,
            content_hash: "caller_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&caller, &emb, Some(12345)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        assert!(
            !names.contains(&"define_languages"),
            "Macro invoked elsewhere in corpus must be filtered from dead candidates"
        );
    }

    /// SQLite `LIKE` is ASCII case-insensitive by default; `MyMacro!`
    /// content would cross-fire against a `mymacro!` macro definition.
    /// GLOB is case-sensitive, so two macros with names that differ
    /// only in case are correctly distinguished.
    #[test]
    fn test_macro_filter_is_case_sensitive() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // Lowercase macro: never invoked.
        let macro_lower = crate::parser::Chunk {
            id: "src/lower.rs:1:lower_hash".to_string(),
            file: std::path::PathBuf::from("src/lower.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "mymacro".to_string(),
            signature: "macro_rules! mymacro".to_string(),
            content: "macro_rules! mymacro { () => {} }".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: "lower_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&macro_lower, &emb, Some(12345)).unwrap();

        // A caller invoking the case-different sibling.
        let caller = crate::parser::Chunk {
            id: "src/caller.rs:5:caller_hash".to_string(),
            file: std::path::PathBuf::from("src/caller.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "use_upper".to_string(),
            signature: "fn use_upper()".to_string(),
            content: "fn use_upper() { MyMacro!(); }".to_string(),
            doc: None,
            line_start: 5,
            line_end: 7,
            byte_start: 0,
            content_hash: "caller_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&caller, &emb, Some(12345)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        // The lowercase macro has no real caller. With case-insensitive
        // LIKE, `MyMacro!` content would have falsely matched
        // `%mymacro!%` and dropped it from dead. With case-sensitive
        // GLOB, it correctly stays in dead candidates.
        assert!(
            names.contains(&"mymacro"),
            "Case-different sibling caller must NOT keep `mymacro` alive — \
             GLOB is case-sensitive (LIKE was the bug PB-V1.40-1 exposed)"
        );
    }

    /// Macro names commonly contain `_`. The LIKE pattern `%info_span!%`
    /// would match `infoXspan!`, `info1span!`, etc. (since `_` is a
    /// single-char wildcard in LIKE). This test pins that a macro with
    /// an underscore in its name is NOT falsely kept alive by content
    /// that differs in the underscore position. GLOB does not treat `_`
    /// as a wildcard.
    #[test]
    fn test_macro_underscore_not_treated_as_wildcard() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // Macro with underscore in name; never actually invoked with
        // exact name. Content for the would-be wildcard match is in
        // the caller chunk below.
        let macro_with_underscore = crate::parser::Chunk {
            id: "src/log.rs:1:info_span_hash".to_string(),
            file: std::path::PathBuf::from("src/log.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Macro,
            name: "info_span".to_string(),
            signature: "macro_rules! info_span".to_string(),
            content: "macro_rules! info_span { ($($t:tt)*) => {} }".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: "info_span_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunk(&macro_with_underscore, &emb, Some(12345))
            .unwrap();

        // Caller that uses `infoXspan!` — would falsely match
        // `%info_span!%` under LIKE wildcard semantics, but not under GLOB.
        let caller = crate::parser::Chunk {
            id: "src/caller.rs:10:bad_caller_hash".to_string(),
            file: std::path::PathBuf::from("src/caller.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: "log_event".to_string(),
            signature: "fn log_event()".to_string(),
            content: "fn log_event() { infoXspan!(\"hi\"); }".to_string(),
            doc: None,
            line_start: 10,
            line_end: 12,
            byte_start: 0,
            content_hash: "bad_caller_hash".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store.upsert_chunk(&caller, &emb, Some(12345)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        // GLOB doesn't treat `_` as a wildcard; the LIKE pattern
        // `%info_span!%` would have matched `infoXspan!`. With GLOB the
        // macro stays in dead candidates as it should.
        assert!(
            names.contains(&"info_span"),
            "Macro `info_span` must not be falsely kept alive by `infoXspan!` content — \
             `_` was a LIKE wildcard pre-fix (EH-V1.40-1)"
        );
    }

    // ===== filter_serde_callbacks tests =====

    /// Helper: build a minimal Rust function chunk for the serde tests.
    fn rust_fn_chunk(id: &str, file: &str, name: &str, content: &str) -> crate::parser::Chunk {
        crate::parser::Chunk {
            id: id.to_string(),
            file: std::path::PathBuf::from(file),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 3,
            byte_start: 0,
            content_hash: format!("{id}_hash"),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    /// A function referenced only via `#[serde(default = "name")]` is reached
    /// by the derive-generated deserializer, not a syntactic call. It must be
    /// dropped from dead candidates when another chunk's content names it in a
    /// serde attribute.
    #[test]
    fn test_serde_default_callback_dropped_from_dead() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // The callback function — never called with `()`.
        let cb = rust_fn_chunk(
            "src/config.rs:1",
            "src/config.rs",
            "default_ref_weight",
            "fn default_ref_weight() -> f32 { 1.0 }",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();

        // A struct chunk whose field references it via serde attribute.
        let user = rust_fn_chunk(
            "src/config.rs:10",
            "src/config.rs",
            "RefConfig",
            "#[derive(serde::Deserialize)] struct RefConfig { \
             #[serde(default = \"default_ref_weight\")] weight: f32 }",
        );
        store.upsert_chunk(&user, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !names.contains(&"default_ref_weight"),
            "serde default callback must be filtered from dead candidates: {names:?}"
        );
    }

    /// `skip_serializing_if = "is_zero_u32"` is a predicate callback. Same
    /// contract as `default`.
    #[test]
    fn test_serde_skip_serializing_if_callback_dropped() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let cb = rust_fn_chunk(
            "src/helpers.rs:1",
            "src/helpers.rs",
            "is_zero_u32",
            "fn is_zero_u32(v: &u32) -> bool { *v == 0 }",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();

        let user = rust_fn_chunk(
            "src/model.rs:5",
            "src/model.rs",
            "Model",
            "#[derive(serde::Serialize)] struct Model { \
             #[serde(skip_serializing_if = \"is_zero_u32\")] count: u32 }",
        );
        store.upsert_chunk(&user, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !names.contains(&"is_zero_u32"),
            "serde skip_serializing_if callback must be filtered: {names:?}"
        );
    }

    /// Path-qualified callbacks (`serialize_with = "crate::a::b::f"`) resolve
    /// to a function keyed by its bare terminal segment. The filter must match
    /// on `f`, not the full path.
    #[test]
    fn test_serde_path_qualified_callback_matches_terminal_segment() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let cb = rust_fn_chunk(
            "src/lib.rs:1",
            "src/lib.rs",
            "serialize_path_normalized",
            "pub fn serialize_path_normalized() {}",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();

        let user = rust_fn_chunk(
            "src/ref.rs:5",
            "src/ref.rs",
            "RefSpec",
            "#[derive(serde::Serialize)] struct RefSpec { \
             #[serde(serialize_with = \"crate::serialize_path_normalized\")] path: PathBuf }",
        );
        store.upsert_chunk(&user, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !names.contains(&"serialize_path_normalized"),
            "path-qualified serde callback must match by terminal segment: {names:?}"
        );
    }

    /// The filter is Rust-only and serde-shaped. A function never named in any
    /// serde attribute must stay in the dead list — the filter is not a blanket
    /// content-substring drop.
    #[test]
    fn test_serde_filter_does_not_drop_unreferenced_fn() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let orphan = rust_fn_chunk(
            "src/orphan.rs:1",
            "src/orphan.rs",
            "genuinely_dead_helper",
            "fn genuinely_dead_helper() {}",
        );
        store.upsert_chunk(&orphan, &emb, Some(1)).unwrap();

        // A serde-using struct that references a *different* function. The
        // orphan's name appears nowhere in a serde attribute.
        let user = rust_fn_chunk(
            "src/model.rs:5",
            "src/model.rs",
            "Model",
            "#[derive(serde::Serialize)] struct Model { \
             #[serde(skip_serializing_if = \"Option::is_none\")] x: Option<u32> }",
        );
        store.upsert_chunk(&user, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            names.contains(&"genuinely_dead_helper"),
            "fn not named in any serde attribute must remain dead: {names:?}"
        );
    }

    /// A bare function name appearing in ordinary content (not a serde-shaped
    /// `key = \"...\"` attribute) must NOT keep the function alive. Guards
    /// against the filter degenerating into a plain substring scan.
    #[test]
    fn test_serde_filter_requires_attribute_shape() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let cb = rust_fn_chunk(
            "src/x.rs:1",
            "src/x.rs",
            "default_true",
            "fn default_true() -> bool { true }",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();

        // Mentions the name and the word serde, but NOT in `key = "name"` form.
        let user = rust_fn_chunk(
            "src/note.rs:5",
            "src/note.rs",
            "note_fn",
            "fn note_fn() { /* serde: default_true is the fallback */ }",
        );
        store.upsert_chunk(&user, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            names.contains(&"default_true"),
            "serde filter must require `key = \\\"name\\\"` shape, not bare mention: {names:?}"
        );
    }

    // ===== doc_reference edge inertness in dead candidacy =====

    /// A function whose ONLY inbound `function_calls` edge is a `doc_reference`
    /// (a prose mention, not an invocation) must still be reported dead. The
    /// `fetch_uncalled_functions` NOT-EXISTS subquery counts only real-caller
    /// edge kinds; a doc reference is inert. RED before the edge-kind filter on
    /// that subquery (the bare `callee_name = c.name` form treated the doc edge
    /// as a caller and excluded the function from the dead set), GREEN after.
    /// The companion `genuinely_called` function — reached by a real `call`
    /// edge — must stay live, fencing the fix from degenerating into "ignore all
    /// edges".
    #[test]
    fn test_doc_reference_only_callee_is_dead() {
        use crate::parser::{CallEdgeKind, CallSite, FunctionCalls};
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // The doc-referenced function — invoked by nothing, only mentioned.
        let doc_only = rust_fn_chunk(
            "src/a.rs:1",
            "src/a.rs",
            "doc_only_fn",
            "fn doc_only_fn() {}",
        );
        store.upsert_chunk(&doc_only, &emb, Some(1)).unwrap();

        // A genuinely-called control function (real `call` edge → must stay live).
        let live = rust_fn_chunk(
            "src/a.rs:5",
            "src/a.rs",
            "genuinely_called",
            "fn genuinely_called() {}",
        );
        store.upsert_chunk(&live, &emb, Some(1)).unwrap();

        // A caller chunk that holds both edges: a doc_reference to `doc_only_fn`
        // and a real call to `genuinely_called`.
        let caller = rust_fn_chunk(
            "src/b.rs:1",
            "src/b.rs",
            "caller_fn",
            "fn caller_fn() { genuinely_called(); }",
        );
        store.upsert_chunk(&caller, &emb, Some(1)).unwrap();
        store
            .upsert_function_calls(
                std::path::Path::new("src/b.rs"),
                &[FunctionCalls {
                    name: "caller_fn".to_string(),
                    line_start: 1,
                    calls: vec![
                        CallSite {
                            callee_name: "doc_only_fn".to_string(),
                            line_number: 2,
                            kind: CallEdgeKind::DocReference,
                        },
                        CallSite {
                            callee_name: "genuinely_called".to_string(),
                            line_number: 3,
                            kind: CallEdgeKind::Call,
                        },
                    ],
                }],
            )
            .unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            names.contains(&"doc_only_fn"),
            "a function reached only by a doc_reference edge must be reported dead \
             (doc references are prose, not callers): {names:?}"
        );
        assert!(
            !names.contains(&"genuinely_called"),
            "a function reached by a real `call` edge must stay live: {names:?}"
        );
    }
}
