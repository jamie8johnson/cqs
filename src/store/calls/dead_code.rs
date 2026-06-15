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
    /// This returns the low-confidence-live BREAKDOWN (kind + count per callee)
    /// used to render the `low-confidence-live` verdict reason string. It folds
    /// TWO populations into the same map, both restricted to callees with NO
    /// trusted `function_calls` edge:
    /// * heuristic `function_calls` edges (`macro_heuristic` / `fn_pointer`) →
    ///   `total` + `kind_counts`,
    /// * `candidate_edges` (Lane 2) references → `candidate_total` +
    ///   `candidate_counts`.
    ///
    /// The matching CHUNK population is fetched by
    /// [`Store::find_low_confidence_live_functions`] (same predicate) and unioned
    /// into the `cqs dead` report by `dead_core`; `fetch_uncalled_functions`
    /// holds the disjoint strict zero-evidence contract, so the two populations
    /// never overlap. The `function_calls` kind-sets are generated from
    /// `CallEdgeKind` rather than a lexical comparison, so a new edge kind cannot
    /// drift out of sync; candidate kinds are reported transparently from the
    /// side-table rows so a new Lane-2 kind surfaces with no query change.
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
        let heuristic_sql = format!(
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
        // Per (callee, candidate kind) count over the `candidate_edges`
        // side-table (Lane 2), restricted to callees with NO trusted
        // `function_calls` edge. A callee that already has a trusted edge is
        // genuinely live and must NOT be relabeled low-confidence-live, so the
        // same trusted-exclusion subquery gates the candidate counts. A
        // candidate-only callee has zero `function_calls` rows, so the
        // `NOT EXISTS` (rather than the heuristic query's `IN` over a
        // function_calls-derived set) is what admits it.
        let candidate_sql = format!(
            "SELECT callee_name, candidate_kind, COUNT(*) AS n
             FROM candidate_edges ce
             WHERE NOT EXISTS (
                   SELECT 1 FROM function_calls fc
                   WHERE fc.callee_name = ce.callee_name
                     AND fc.edge_kind IN ({trusted}) LIMIT 1
               )
             GROUP BY callee_name, candidate_kind"
        );
        self.rt.block_on(async {
            let mut out: std::collections::HashMap<String, LowConfidenceLiveInfo> =
                std::collections::HashMap::new();

            let heuristic_rows: Vec<(String, String, i64)> =
                sqlx::query_as(sqlx::AssertSqlSafe(heuristic_sql.as_str()))
                    .fetch_all(&self.pool)
                    .await?;
            for (name, kind, n) in heuristic_rows {
                let info = out.entry(name).or_default();
                info.total += n.max(0) as u64;
                info.kind_counts.push((kind, n.max(0) as u64));
            }

            let candidate_rows: Vec<(String, String, i64)> =
                sqlx::query_as(sqlx::AssertSqlSafe(candidate_sql.as_str()))
                    .fetch_all(&self.pool)
                    .await?;
            for (name, kind, n) in candidate_rows {
                let info = out.entry(name).or_default();
                info.candidate_total += n.max(0) as u64;
                info.candidate_counts.push((kind, n.max(0) as u64));
            }

            // Stable kind order for deterministic reason strings.
            for info in out.values_mut() {
                info.kind_counts.sort();
                info.candidate_counts.sort();
            }
            Ok(out)
        })
    }

    /// Raw per-row `candidate_edges` contributions: `(callee_name, candidate_kind,
    /// reference-origin file)` for every candidate row in this store. The `file`
    /// column is the CALLER/reference origin (where the bare-name / macro-arg /
    /// serde linkage appears), symmetric to `function_calls.file` — so it is the
    /// masking key for the worktree-overlay merge ([`build_overlay_candidate_map`]),
    /// which masks parent rows whose reference origin is delta-touched and unions
    /// the overlay store's rows. Unlike [`Store::find_low_confidence_live_names`]'s
    /// pre-aggregated candidate arm, this exposes the row-level `file` the merge
    /// needs and applies NO trusted-edge exclusion: the merge consults this map
    /// only to relabel a function already computed dead over the merged caller
    /// graph (zero real callers, so zero trusted callers by construction), where a
    /// parent trusted edge — if any — lived in a masked origin and is no longer
    /// authoritative.
    pub fn candidate_edge_contributions(
        &self,
    ) -> Result<Vec<(String, String, String)>, StoreError> {
        let _span = tracing::info_span!("candidate_edge_contributions").entered();
        self.rt.block_on(async {
            let rows: Vec<(String, String, String)> =
                sqlx::query_as("SELECT callee_name, candidate_kind, file FROM candidate_edges")
                    .fetch_all(&self.pool)
                    .await?;
            Ok(rows)
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
        //
        // A `candidate_edges` (Lane 2) reference is also NOT real death: a bare
        // fn-pointer / macro arg or a serde container/with-module linkage that
        // the confident extractor declined to resolve into a `function_calls`
        // edge still points at this function. So a callee PRESENT in
        // `candidate_edges` is excluded here (it is low-confidence-live, surfaced
        // by `fetch_heuristic_only_callees`) — the truly-dead set requires zero
        // real `function_calls` edges AND zero candidate references. This keeps
        // truly-dead and low-confidence-live DISJOINT with candidates added: a
        // candidate-only callee leaves this set and enters the other.
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
               AND NOT EXISTS (SELECT 1 FROM candidate_edges ce \
                               WHERE ce.callee_name = c.name LIMIT 1)
               AND c.parent_id IS NULL
             ORDER BY c.origin, c.line_start"
        );
        Self::light_chunks_from_query(&sql, &self.pool).await
    }

    /// Phase 1 (low-confidence-live): query callable chunks that have NO trusted
    /// edge (`call`, `serde_callback`) and at least one of: a heuristic
    /// `function_calls` edge (`macro_heuristic` / `fn_pointer`), OR a
    /// `candidate_edges` (Lane 2) reference. A candidate-ONLY callee — zero
    /// `function_calls` edges, present only in the side-table — enters here via
    /// the candidate `EXISTS` arm. Same Tier-1 noise filters as
    /// `fetch_uncalled_functions` (Property exclusion, doc-path exclusion,
    /// top-level only). The heuristic and trusted kind-sets are generated from
    /// `CallEdgeKind` (single source), so a new edge kind updates both surfaces
    /// at once. `doc_reference` edges are inert: they neither qualify (not
    /// heuristic) nor disqualify (not trusted), matching
    /// [`Store::find_low_confidence_live_names`].
    ///
    /// Disjointness with `fetch_uncalled_functions` (truly-dead) is preserved
    /// with candidates added: truly-dead requires zero real `function_calls`
    /// edges AND zero candidate references; this set requires (heuristic edge OR
    /// candidate) AND zero trusted edge. The two predicates are mutually
    /// exclusive — a callee with any candidate is excluded from truly-dead and
    /// admitted here, with no overlap.
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
               AND (EXISTS (SELECT 1 FROM function_calls fc \
                            WHERE fc.callee_name = c.name \
                              AND fc.edge_kind IN ({heuristic}) LIMIT 1)
                    OR EXISTS (SELECT 1 FROM candidate_edges ce \
                               WHERE ce.callee_name = c.name LIMIT 1))
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

    /// Phase 1.6: drop Rust functions referenced only by the serde-callback keys
    /// the parser's confident pass and Lane-2 candidate emit do NOT cover —
    /// `skip_serializing` / `skip_deserializing`, plus `with = "..."`. serde's
    /// derive macros accept function paths as attribute string literals:
    ///
    /// ```ignore
    /// #[serde(skip_serializing = "is_zero_u32")]
    /// #[serde(skip_deserializing = "always_zero")]
    /// #[serde(with = "some::module")]
    /// ```
    ///
    /// **Retirement (candidate-edge campaign Lane 3).** This pass was the sole
    /// backstop for every serde-callback key. Now that the parser emits
    /// `function_calls` edges (FIELD-level `default` / `serialize_with` /
    /// `deserialize_with` / `skip_serializing_if` / `getter` / `with` → a
    /// `serde_callback` edge) AND `candidate_edges` (CONTAINER-level callbacks →
    /// `serde_container`; `with = "module"` inner fns → `serde_with_module`),
    /// and `fetch_uncalled_functions` excludes any callee present in
    /// `candidate_edges`, those callbacks never reach this filter — their callee
    /// is gone from the dead-candidate list before Phase 1.6 runs. So those keys
    /// were RETIRED from the regex below.
    ///
    /// **What remains, and why (no dead-code coverage lost):**
    /// - `skip_serializing` / `skip_deserializing` — NOT in the parser's
    ///   `SERDE_CALLBACK_RE`, so neither the confident pass nor the candidate
    ///   pass emits anything for them. This filter is their only keep-alive.
    /// - `with` — retained as a deliberate same-name keep: a `with = "module"`
    ///   linkage whose terminal segment (`module`) happens to also be a real
    ///   function name in the corpus stays live here even on a path/index state
    ///   where the per-file candidate emit did not record it. The confident pass
    ///   already keeps the field-level case; this preserves the corpus-wide
    ///   backstop for the terminal-name match.
    ///
    /// The derived (de)serializer calls these at runtime, but the reference
    /// lives in an attribute string the call-graph walker never resolves into
    /// an edge. So the callbacks look uncalled. This pass scans every chunk's
    /// content for the remaining serde-shaped attribute strings, extracts the
    /// terminal path segment of each (`crate::a::b::f` → `f`), and drops any Rust
    /// function candidate whose name matches.
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

        // Matches ONLY the serde-callback keys the parser emit does not cover
        // (the rest were retired in Lane 3 — see this fn's doc comment):
        // `skip_serializing = "..."`, `skip_deserializing = "..."`, and
        // `with = "..."` (the corpus-wide same-name backstop). `default` /
        // `serialize_with` / `deserialize_with` / `skip_serializing_if` /
        // `getter` are now covered by confident `serde_callback` edges +
        // `serde_container` candidates, so they are intentionally absent.
        // `skip_serializing_if` must NOT be re-added: it shares a prefix with
        // `skip_serializing`, but the `\s*=` anchor keeps them disjoint (a
        // `skip_serializing_if = "x"` has `_if` between the key and `=`, so it
        // never matches the bare `skip_serializing` alternative). `bound = "..."`
        // stays excluded (it names types/where-clauses, not fns).
        static SERDE_CALLBACK_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"(?:with|skip_serializing|skip_deserializing)\s*=\s*"([^"]+)""#)
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

            let dead_fn = DeadFunction {
                chunk,
                confidence,
                // Parent-set entry, not an overlay recompute — verdict
                // classification keeps the full ordered tiers (incl. low_conf).
                overlay_dead: false,
            };

            if is_pub && !include_pub {
                possibly_dead_pub.push(dead_fn);
            } else {
                confident.push(dead_fn);
            }
        }

        Ok((confident, possibly_dead_pub))
    }
}

/// Whether `name` has ≥1 real-caller edge (excludes `doc_reference`) in `store`.
/// A best-effort store read: a query error logs and degrades to `false` (treats
/// the function as having no real caller), the safe direction for the overlay
/// merge — it never resurrects a function on a failed read.
fn overlay_has_real_caller<M>(store: &Store<M>, name: &str) -> bool {
    match store.get_callers_full(name) {
        Ok(callers) => callers.iter().any(|c| c.edge_kind.is_real_caller()),
        Err(e) => {
            tracing::warn!(error = %e, name, "overlay get_callers_full failed; treating as no real caller");
            false
        }
    }
}

/// Resolve the definition chunk for a Direction-B dead candidate, applying the
/// Tier-1 admissibility filters `fetch_uncalled_functions` applies so an
/// overlay-added entry can never be a shape the parent path would have excluded:
/// the def must be a callable, NON-`Property`, NON-doc-path ([`is_dead_doc_path`]),
/// top-level (`parent_id` None), non-test chunk. Prefers the overlay store's def
/// (current worktree line range) when the name is defined there, else the
/// parent's. Returns `None` for a name with no admissible def (an external/std
/// call, a test, a nested chunk, a property, a doc-block fn).
fn resolve_overlay_dead_candidate_def<M>(
    store: &Store<M>,
    overlay_store: &Store<crate::store::ReadWrite>,
    name: &str,
) -> Option<ChunkSummary> {
    let admissible = |c: &ChunkSummary| -> bool {
        c.chunk_type.is_callable()
            && c.chunk_type != ChunkType::Property
            && c.parent_id.is_none()
            && !is_dead_doc_path(&c.file.to_string_lossy())
            && !crate::is_test_chunk(&c.name, &c.file.to_string_lossy())
    };
    // Prefer the overlay def (worktree line range) when present + admissible.
    if let Ok(chunks) = overlay_store.get_chunks_by_name(name) {
        if let Some(c) = chunks.into_iter().find(|c| admissible(c)) {
            return Some(c);
        }
    }
    match store.get_chunks_by_name(name) {
        Ok(chunks) => chunks.into_iter().find(|c| admissible(c)),
        Err(e) => {
            tracing::warn!(error = %e, name, "dead overlay candidate def lookup failed");
            None
        }
    }
}

/// Merge a worktree overlay into the parent dead populations, in place. The
/// single source of truth for the `cqs dead` AND `cqs ci` worktree-overlay dead
/// paths — both call this so the two surfaces cannot drift.
///
/// Recomputes the dead set over the MERGED caller graph (parent real-caller
/// edges minus delta-touched caller-origins, plus the worktree's edges), in two
/// directions:
///
/// - **parent-dead → live (removal):** a parent-dead function the worktree now
///   really-calls (the overlay store holds a real-caller edge to it) is dropped
///   from the dead set — it is live in this checkout.
/// - **parent-live → dead (addition):** a parent-live function whose every
///   real-caller edge sits in a delta-touched origin, and which the worktree no
///   longer calls, becomes dead. Candidates are exactly the callees of the delta
///   files ([`Store::distinct_callees_from_origins`]); for each, `merge_callers`
///   over (parent, overlay) is checked for zero real-caller edges. A newly-dead
///   addition is reported at `Medium` confidence — the file-activity recompute
///   `score_confidence` does for the parent set is not re-run under the overlay,
///   so `Medium` is the honest floor.
///
/// Both directions filter on [`crate::parser::CallEdgeKind::is_real_caller`] (a
/// `doc_reference` is inert), matching `fetch_uncalled_functions`'s own
/// real-caller contract.
///
/// Returns whether either direction changed the set — the participation signal
/// the daemon gates the `_meta.overlay_graph` marker on. An active overlay whose
/// delta is irrelevant returns the parent set untouched and reports `false`.
pub fn apply_dead_overlay<M>(
    store: &Store<M>,
    overlay: &crate::worktree_overlay::WorktreeOverlay,
    confident: &mut Vec<DeadFunction>,
    possibly_pub: &mut Vec<DeadFunction>,
    include_pub: bool,
    min_confidence: DeadConfidence,
) -> Result<bool, StoreError> {
    let _span = tracing::info_span!(
        "apply_dead_overlay",
        include_pub,
        masked = overlay.masked_origins.len()
    )
    .entered();
    let mut participated = false;

    // ── Direction A: parent-dead → live ──────────────────────────────────────
    // Drop any parent-dead entry the worktree now really-calls. (A worktree file
    // added a real call edge to a previously-uncalled function.)
    let before_dead = confident.len();
    let before_pub = possibly_pub.len();
    confident.retain(|d| !overlay_has_real_caller(&overlay.store, &d.chunk.name));
    possibly_pub.retain(|d| !overlay_has_real_caller(&overlay.store, &d.chunk.name));
    if confident.len() != before_dead || possibly_pub.len() != before_pub {
        participated = true;
    }

    // ── Direction B: parent-live → dead ──────────────────────────────────────
    // Candidates = functions the delta files used to call (parent-side). For each
    // not already dead, recompute the merged caller set and add it if it now has
    // zero real callers.
    let masked: Vec<String> = overlay
        .masked_origins
        .iter()
        .map(|p| crate::normalize_path(p).to_string())
        .collect();
    let candidates = store.distinct_callees_from_origins(&masked)?;

    // Names already present in the dead set (either direction) must not be
    // re-added.
    let already: std::collections::HashSet<&str> = confident
        .iter()
        .chain(possibly_pub.iter())
        .map(|d| d.chunk.name.as_str())
        .collect();

    let mut additions: Vec<DeadFunction> = Vec::new();
    for name in &candidates {
        if already.contains(name.as_str()) {
            continue;
        }
        let def = match resolve_overlay_dead_candidate_def(store, &overlay.store, name) {
            Some(c) => c,
            None => continue,
        };

        // Merged caller set: parent callers minus delta-origin call-sites, plus
        // the overlay's callers. Dead iff zero REAL callers survive.
        let parent_callers = store.get_callers_full(name)?;
        let overlay_callers = overlay.store.get_callers_full(name)?;
        let merged = overlay.merge_callers(parent_callers, overlay_callers);
        let has_real = merged.iter().any(|c| c.edge_kind.is_real_caller());
        if has_real {
            continue;
        }

        // Newly dead. `Medium` is the honest floor — the file-activity recompute
        // `score_confidence` runs for the parent set is not re-run here.
        // `overlay_dead`: computed dead over the authoritative merged caller graph
        // in this worktree, so verdict classification skips the stale parent-truth
        // `low_conf` HEURISTIC breakdown for it and instead consults the
        // overlay-merged CANDIDATE map (`build_overlay_candidate_map`) — a
        // candidate-only addition still relabels `low-confidence-live`.
        //
        // Precondition for that candidate consult: the entry has zero trusted
        // merged callers. `build_overlay_candidate_map` omits the trusted-edge
        // exclusion, so its output is only safe to read for a zero-trusted-caller
        // name — consulting it for a possibly-live function would mislabel it.
        // `has_real` is false here (zero real callers), and trusted is a subset of
        // real, so zero-trusted holds; pin it.
        debug_assert!(
            !merged.iter().any(|c| c.edge_kind.is_trusted()),
            "overlay_dead candidate {name} has a trusted merged caller — the \
             overlay candidate map must only be consulted for zero-trusted-caller entries"
        );
        let dead_fn = DeadFunction {
            chunk: def,
            confidence: DeadConfidence::Medium,
            overlay_dead: true,
        };
        if dead_fn.confidence < min_confidence {
            continue;
        }
        additions.push(dead_fn);
    }
    if !additions.is_empty() {
        participated = true;
        for dead_fn in additions {
            // Route public defs to the possibly-dead-pub list (unless
            // include_pub), matching `score_confidence`'s visibility split.
            let is_pub = dead_fn.chunk.signature.starts_with("pub ")
                || dead_fn.chunk.signature.starts_with("pub(");
            if is_pub && !include_pub {
                possibly_pub.push(dead_fn);
            } else {
                confident.push(dead_fn);
            }
        }
    }

    Ok(participated)
}

/// Build the overlay-merged candidate map keyed by callee name, mirroring
/// [`crate::worktree_overlay::WorktreeOverlay::merge_callers`]'s mask-then-union.
/// The map carries ONLY the candidate-edge fields of [`LowConfidenceLiveInfo`]
/// (`candidate_total` + `candidate_counts`); the heuristic-edge fields stay zero
/// because the candidate graph is the only population recomputed over the overlay
/// here.
///
/// A `candidate_edges` row's `file` column is the CALLER/reference origin
/// (symmetric to `function_calls.file`), so the merge is the same single-key mask
/// plus union the caller-graph merge uses:
///
/// 1. **Mask**: drop every parent candidate row whose reference origin is in
///    `masked_origins` — that file's references may have changed or vanished in
///    the worktree, so its parent rows are no longer authoritative.
/// 2. **Union**: add every overlay candidate row. Every chunk in the overlay
///    store comes from a masked origin, so every overlay candidate row's
///    reference origin is masked too — the mask above already removed any parent
///    counterpart, so the union cannot double-count.
///
/// The result is the candidate references the parent had minus the suspect
/// (masked-origin) ones, plus the worktree's fresh view of those same origins.
/// `cqs dead`'s verdict classifier consults this for a Direction-B addition so a
/// candidate-only overlay-dead entry — a function dead over the merged real
/// caller graph but still referenced by a worktree candidate edge — relabels
/// `low-confidence-live` instead of `dead`.
///
/// CALLER CONTRACT: this output must only be consulted for names with zero
/// trusted merged callers — the `overlay_dead` entries, computed dead over the
/// authoritative merged real-caller graph (zero real callers ⇒ zero trusted
/// callers, since trusted is a subset of real). Unlike
/// [`Store::find_low_confidence_live_names`], this map OMITS the trusted-edge
/// exclusion: it counts every candidate row, including rows for a function the
/// trusted call graph already proves live. Consulting it for a possibly-live
/// function (one with ≥1 trusted merged caller) would therefore relabel that
/// genuinely-live function `low-confidence-live`, mislabeling it. The consult
/// site in `apply_dead_overlay` (where `overlay_dead = true` is set, after
/// `merge_callers` confirms zero real merged callers) is the only admissible
/// reader; a `debug_assert!` there pins the zero-real-caller precondition.
pub fn build_overlay_candidate_map<M>(
    store: &Store<M>,
    overlay: &crate::worktree_overlay::WorktreeOverlay,
) -> Result<std::collections::HashMap<String, LowConfidenceLiveInfo>, StoreError> {
    let _span = tracing::info_span!(
        "build_overlay_candidate_map",
        masked = overlay.masked_origins.len()
    )
    .entered();

    // Reference-origin paths normalized the same way `masked_origins` are stored,
    // so a parent row's `file` and the mask set are comparable string-for-string.
    let masked: std::collections::HashSet<String> = overlay
        .masked_origins
        .iter()
        .map(|p| crate::normalize_path(p))
        .collect();

    // Mask parent rows whose reference origin is delta-touched, then union the
    // overlay store's rows (every one of which is from a masked origin).
    let mut merged: Vec<(String, String)> = store
        .candidate_edge_contributions()?
        .into_iter()
        .filter(|(_, _, file)| !masked.contains(&crate::normalize_path(std::path::Path::new(file))))
        .map(|(name, kind, _)| (name, kind))
        .collect();
    merged.extend(
        overlay
            .store
            .candidate_edge_contributions()?
            .into_iter()
            .map(|(name, kind, _)| (name, kind)),
    );

    // Aggregate into per-callee candidate counts (heuristic fields left zero).
    let mut out: std::collections::HashMap<String, LowConfidenceLiveInfo> =
        std::collections::HashMap::new();
    let mut per_kind: std::collections::HashMap<(String, String), u64> =
        std::collections::HashMap::new();
    for (name, kind) in merged {
        let info = out.entry(name.clone()).or_default();
        info.candidate_total += 1;
        *per_kind.entry((name, kind)).or_default() += 1;
    }
    for ((name, kind), n) in per_kind {
        if let Some(info) = out.get_mut(&name) {
            info.candidate_counts.push((kind, n));
        }
    }
    // Stable kind order for deterministic reason strings.
    for info in out.values_mut() {
        info.candidate_counts.sort();
    }
    Ok(out)
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

    /// Helper: seed a confident `serde_callback` `function_calls` edge naming
    /// `callee` (terminal segment), as the parser's `extract_serde_callback_calls`
    /// pass emits for a FIELD-level serde callback. This is the post-Lane-3
    /// keep-alive mechanism for `default` / `serialize_with` / `deserialize_with`
    /// / `skip_serializing_if` / `getter` — they were retired from
    /// `filter_serde_callbacks` because the parser now records them as edges.
    fn seed_serde_callback_edge(
        store: &Store<crate::store::ReadWrite>,
        caller_file: &str,
        callee: &str,
    ) {
        use crate::parser::{CallEdgeKind, CallSite, FunctionCalls};
        store
            .upsert_function_calls(
                std::path::Path::new(caller_file),
                &[FunctionCalls {
                    name: "__serde_derive__".to_string(),
                    line_start: 1,
                    calls: vec![CallSite {
                        callee_name: callee.to_string(),
                        line_number: 1,
                        kind: CallEdgeKind::SerdeCallback,
                    }],
                }],
            )
            .unwrap();
    }

    /// FIELD-level `#[serde(default = "name")]` is kept live by the parser's
    /// confident `serde_callback` edge (a trusted real-caller), NOT by
    /// `filter_serde_callbacks` — `default` was retired from the filter in
    /// Lane 3. Seeding that edge keeps the callback out of the dead set.
    #[test]
    fn test_serde_default_callback_kept_live_by_edge() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let cb = rust_fn_chunk(
            "src/config.rs:1",
            "src/config.rs",
            "default_ref_weight",
            "fn default_ref_weight() -> f32 { 1.0 }",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();
        // The confident pass would emit this serde_callback edge for the
        // field-level `#[serde(default = "default_ref_weight")]`.
        seed_serde_callback_edge(&store, "src/config.rs", "default_ref_weight");

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !names.contains(&"default_ref_weight"),
            "field-level serde default callback must stay live via the \
             serde_callback edge (filter coverage retired in Lane 3): {names:?}"
        );
    }

    /// FIELD-level `skip_serializing_if = "is_zero_u32"` is a predicate callback,
    /// kept live by the confident `serde_callback` edge (retired from the filter
    /// in Lane 3).
    #[test]
    fn test_serde_skip_serializing_if_callback_kept_live_by_edge() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let cb = rust_fn_chunk(
            "src/helpers.rs:1",
            "src/helpers.rs",
            "is_zero_u32",
            "fn is_zero_u32(v: &u32) -> bool { *v == 0 }",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();
        seed_serde_callback_edge(&store, "src/model.rs", "is_zero_u32");

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !names.contains(&"is_zero_u32"),
            "field-level serde skip_serializing_if callback must stay live via \
             the serde_callback edge (filter coverage retired in Lane 3): {names:?}"
        );
    }

    /// Path-qualified `serialize_with = "crate::a::b::f"` resolves to the bare
    /// terminal segment `f` — the parser's confident pass emits a `serde_callback`
    /// edge keyed by the terminal segment, keeping `f` live (retired from the
    /// filter in Lane 3).
    #[test]
    fn test_serde_path_qualified_callback_kept_live_by_terminal_edge() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let cb = rust_fn_chunk(
            "src/lib.rs:1",
            "src/lib.rs",
            "serialize_path_normalized",
            "pub fn serialize_path_normalized() {}",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();
        // The confident pass keys the edge by the terminal segment.
        seed_serde_callback_edge(&store, "src/ref.rs", "serialize_path_normalized");

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !names.contains(&"serialize_path_normalized"),
            "path-qualified serde callback must stay live via the terminal-keyed \
             serde_callback edge (filter coverage retired in Lane 3): {names:?}"
        );
    }

    /// CONTAINER-level serde callbacks are now kept live by a `serde_container`
    /// `candidate_edges` row (Lane 2 emit), relabeling the callback
    /// `low-confidence-live` instead of leaving it for the filter. Retiring the
    /// container-level keys from `filter_serde_callbacks` loses no coverage.
    #[test]
    fn test_serde_container_callback_kept_live_by_candidate() {
        use crate::parser::CandidateSite;
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let cb = rust_fn_chunk(
            "src/cfg.rs:1",
            "src/cfg.rs",
            "container_default",
            "fn container_default() {}",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();
        // The Lane-2 emit records the container-level callback as a candidate.
        store
            .upsert_candidate_edges(
                std::path::Path::new("src/cfg.rs"),
                &[CandidateSite {
                    file: std::path::PathBuf::from("src/cfg.rs"),
                    callee_name: "container_default".to_string(),
                    ref_line: 1,
                    candidate_kind: "serde_container".to_string(),
                }],
            )
            .unwrap();

        // Truly-dead set excludes it; low-confidence-live set includes it.
        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let dead_names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !dead_names.contains(&"container_default"),
            "container-level serde callback must leave the truly-dead set via the \
             serde_container candidate: {dead_names:?}"
        );
        let (low, low_pub) = store.find_low_confidence_live_functions(true).unwrap();
        let low_names: Vec<&str> = low
            .iter()
            .chain(low_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            low_names.contains(&"container_default"),
            "container-level serde callback must enter low-confidence-live: {low_names:?}"
        );
    }

    /// `skip_serializing` / `skip_deserializing` are NOT emitted by the parser
    /// (absent from its `SERDE_CALLBACK_RE`), so `filter_serde_callbacks` is
    /// still their only keep-alive — these keys were KEPT in the filter regex.
    #[test]
    fn test_serde_skip_serializing_still_filtered() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let cb = rust_fn_chunk(
            "src/h.rs:1",
            "src/h.rs",
            "skip_predicate",
            "fn skip_predicate() -> bool { true }",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();
        // No edge, no candidate — the filter's content scan is the only thing
        // that can keep it alive.
        let user = rust_fn_chunk(
            "src/m.rs:5",
            "src/m.rs",
            "M",
            "#[derive(serde::Serialize)] struct M { \
             #[serde(skip_serializing = \"skip_predicate\")] x: u32 }",
        );
        store.upsert_chunk(&user, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !names.contains(&"skip_predicate"),
            "skip_serializing callback must still be filtered — KEPT in the regex \
             because the parser does not emit it: {names:?}"
        );
    }

    /// `with = "module"` is KEPT in the filter regex as the corpus-wide same-name
    /// backstop: a function whose name matches the `with`-path terminal segment
    /// stays live via the content scan even with no edge/candidate seeded.
    #[test]
    fn test_serde_with_module_same_name_still_filtered() {
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let cb = rust_fn_chunk(
            "src/codec.rs:1",
            "src/codec.rs",
            "my_codec",
            "fn my_codec() {}",
        );
        store.upsert_chunk(&cb, &emb, Some(1)).unwrap();
        let user = rust_fn_chunk(
            "src/m.rs:5",
            "src/m.rs",
            "M",
            "#[derive(serde::Serialize)] struct M { \
             #[serde(with = \"my_codec\")] x: u32 }",
        );
        store.upsert_chunk(&user, &emb, Some(1)).unwrap();

        let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
        let names: Vec<&str> = confident
            .iter()
            .chain(possibly_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !names.contains(&"my_codec"),
            "with = \"module\" same-name keep must stay in the filter (KEPT in Lane 3): {names:?}"
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

    // ===== candidate_edges consult (Lane 3) =====

    /// A callee with ZERO `function_calls` edges but PRESENT in `candidate_edges`
    /// (a candidate-ONLY callee) must LEAVE the truly-dead set
    /// (`find_dead_code`) and ENTER the low-confidence-live set
    /// (`find_low_confidence_live_functions`). Calibration: without the candidate
    /// row the same function is truly-dead — the consult is what flips it.
    #[test]
    fn test_candidate_only_callee_flips_dead_to_low_confidence_live() {
        use crate::parser::CandidateSite;
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // The candidate-only callee — invoked by nothing in `function_calls`.
        let cand = rust_fn_chunk("src/a.rs:1", "src/a.rs", "maybe_fn", "fn maybe_fn() {}");
        store.upsert_chunk(&cand, &emb, Some(1)).unwrap();

        // CALIBRATION: before any candidate row, `maybe_fn` is truly-dead and
        // absent from the low-confidence-live set.
        let (dead0, dead_pub0) = store.find_dead_code(true).unwrap();
        let dead0_names: Vec<&str> = dead0
            .iter()
            .chain(dead_pub0.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            dead0_names.contains(&"maybe_fn"),
            "without a candidate row `maybe_fn` must be truly-dead: {dead0_names:?}"
        );
        let (low0, low_pub0) = store.find_low_confidence_live_functions(true).unwrap();
        let low0_names: Vec<&str> = low0
            .iter()
            .chain(low_pub0.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !low0_names.contains(&"maybe_fn"),
            "without a candidate row `maybe_fn` must NOT be low-confidence-live: {low0_names:?}"
        );

        // Add a `candidate_edges` row naming `maybe_fn` (a bare fn-pointer arg
        // the confident extractor declined to resolve).
        store
            .upsert_candidate_edges(
                std::path::Path::new("src/b.rs"),
                &[CandidateSite {
                    file: std::path::PathBuf::from("src/b.rs"),
                    callee_name: "maybe_fn".to_string(),
                    ref_line: 7,
                    candidate_kind: "bare_arg_unresolved".to_string(),
                }],
            )
            .unwrap();

        // FLIP: `maybe_fn` leaves truly-dead and enters low-confidence-live.
        let (dead1, dead_pub1) = store.find_dead_code(true).unwrap();
        let dead1_names: Vec<&str> = dead1
            .iter()
            .chain(dead_pub1.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !dead1_names.contains(&"maybe_fn"),
            "a candidate-only callee must LEAVE the truly-dead set: {dead1_names:?}"
        );
        let (low1, low_pub1) = store.find_low_confidence_live_functions(true).unwrap();
        let low1_names: Vec<&str> = low1
            .iter()
            .chain(low_pub1.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            low1_names.contains(&"maybe_fn"),
            "a candidate-only callee must ENTER the low-confidence-live set: {low1_names:?}"
        );

        // The breakdown names the candidate kind/count.
        let info = store.find_low_confidence_live_names().unwrap();
        let maybe = info
            .get("maybe_fn")
            .expect("maybe_fn must be in the low-confidence-live breakdown");
        assert_eq!(
            maybe.total, 0,
            "candidate-only callee has zero heuristic edges"
        );
        assert_eq!(maybe.candidate_total, 1, "one candidate reference");
        assert_eq!(
            maybe.candidate_counts,
            vec![("bare_arg_unresolved".to_string(), 1)],
            "candidate kind/count must be named"
        );
    }

    /// Disjointness invariant, EXTENDED with candidates: the truly-dead set
    /// (`find_dead_code`) and the low-confidence-live set
    /// (`find_low_confidence_live_functions`) must never share a name when both a
    /// candidate-only callee AND a genuinely-dead function are present. A
    /// candidate-only callee belongs to exactly the low set; a no-edge no-candidate
    /// function belongs to exactly the dead set; their intersection is empty.
    #[test]
    fn test_dead_and_low_confidence_live_disjoint_with_candidates() {
        use crate::parser::CandidateSite;
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        // Candidate-only callee → low set.
        let cand = rust_fn_chunk(
            "src/a.rs:1",
            "src/a.rs",
            "candidate_fn",
            "fn candidate_fn() {}",
        );
        store.upsert_chunk(&cand, &emb, Some(1)).unwrap();
        // Genuinely-dead function: no edges, no candidate → dead set.
        let dead_fn = rust_fn_chunk(
            "src/a.rs:5",
            "src/a.rs",
            "truly_dead_fn",
            "fn truly_dead_fn() {}",
        );
        store.upsert_chunk(&dead_fn, &emb, Some(1)).unwrap();

        store
            .upsert_candidate_edges(
                std::path::Path::new("src/b.rs"),
                &[CandidateSite {
                    file: std::path::PathBuf::from("src/b.rs"),
                    callee_name: "candidate_fn".to_string(),
                    ref_line: 3,
                    candidate_kind: "macro_arg_unresolved".to_string(),
                }],
            )
            .unwrap();

        let (dead, dead_pub) = store.find_dead_code(true).unwrap();
        let dead_names: std::collections::HashSet<&str> = dead
            .iter()
            .chain(dead_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        let (low, low_pub) = store.find_low_confidence_live_functions(true).unwrap();
        let low_names: std::collections::HashSet<&str> = low
            .iter()
            .chain(low_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();

        // Membership: each function is in exactly the right set.
        assert!(
            dead_names.contains("truly_dead_fn") && !low_names.contains("truly_dead_fn"),
            "no-edge no-candidate fn must be ONLY in the dead set"
        );
        assert!(
            low_names.contains("candidate_fn") && !dead_names.contains("candidate_fn"),
            "candidate-only fn must be ONLY in the low-confidence-live set"
        );
        // Disjointness: the two sets share no name.
        let overlap: Vec<&&str> = dead_names.intersection(&low_names).collect();
        assert!(
            overlap.is_empty(),
            "truly-dead and low-confidence-live must stay disjoint with candidates added: {overlap:?}"
        );
    }

    /// A callee with a TRUSTED `function_calls` edge AND a `candidate_edges` row
    /// is genuinely live: it must be in NEITHER set. Fences the candidate
    /// consult from resurrecting a function the trusted edge already proves live,
    /// and from leaking it into low-confidence-live.
    #[test]
    fn test_candidate_with_trusted_edge_is_neither_dead_nor_low_conf() {
        use crate::parser::{CallEdgeKind, CallSite, CandidateSite, FunctionCalls};
        let (store, _dir) = setup_store();
        let emb = crate::embedder::Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);

        let target = rust_fn_chunk(
            "src/a.rs:1",
            "src/a.rs",
            "live_target",
            "fn live_target() {}",
        );
        store.upsert_chunk(&target, &emb, Some(1)).unwrap();
        let caller = rust_fn_chunk(
            "src/b.rs:1",
            "src/b.rs",
            "caller_fn",
            "fn caller_fn() { live_target(); }",
        );
        store.upsert_chunk(&caller, &emb, Some(1)).unwrap();

        // A trusted `call` edge to `live_target`.
        store
            .upsert_function_calls(
                std::path::Path::new("src/b.rs"),
                &[FunctionCalls {
                    name: "caller_fn".to_string(),
                    line_start: 1,
                    calls: vec![CallSite {
                        callee_name: "live_target".to_string(),
                        line_number: 1,
                        kind: CallEdgeKind::Call,
                    }],
                }],
            )
            .unwrap();
        // AND a candidate row naming the same callee.
        store
            .upsert_candidate_edges(
                std::path::Path::new("src/b.rs"),
                &[CandidateSite {
                    file: std::path::PathBuf::from("src/b.rs"),
                    callee_name: "live_target".to_string(),
                    ref_line: 2,
                    candidate_kind: "bare_arg_unresolved".to_string(),
                }],
            )
            .unwrap();

        let (dead, dead_pub) = store.find_dead_code(true).unwrap();
        let dead_names: Vec<&str> = dead
            .iter()
            .chain(dead_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !dead_names.contains(&"live_target"),
            "a trusted-called fn must not be dead even with a candidate row: {dead_names:?}"
        );
        let (low, low_pub) = store.find_low_confidence_live_functions(true).unwrap();
        let low_names: Vec<&str> = low
            .iter()
            .chain(low_pub.iter())
            .map(|d| d.chunk.name.as_str())
            .collect();
        assert!(
            !low_names.contains(&"live_target"),
            "a trusted-called fn must not be low-confidence-live even with a candidate row: {low_names:?}"
        );
        // The breakdown must also exclude it (trusted edge gates the candidate count).
        let info = store.find_low_confidence_live_names().unwrap();
        assert!(
            !info.contains_key("live_target"),
            "trusted-called callee must be absent from the low-confidence-live breakdown"
        );
    }
}
