//! Chunk extraction from tree-sitter parse trees

use std::path::Path;

use super::types::{
    capture_name_to_chunk_type, Chunk, ChunkType, Language, ParserError, SignatureStyle,
};
use super::Parser;

/// Parser version stamp embedded on every emitted [`Chunk`].
///
/// Persisted via the `chunks.parser_version` SQLite column so incremental
/// UPSERT can refresh rows whose `content_hash` is unchanged but whose `doc`
/// (or other non-content field) would now be extracted differently.
///
/// Bump this when extraction logic changes the value of any non-content
/// field (`doc`, `signature`, `parent_type_name`, etc.) for a chunk whose
/// source bytes are identical to a previously-indexed version. The
/// store-side UPSERT clause must include
/// `OR chunks.parser_version != excluded.parser_version` so the bump
/// triggers a refresh. The `id` IS such a field: a change to its format for
/// any chunk class (e.g. the markdown table-chunk `:t{idx}` disambiguator)
/// alters the stored id for byte-identical source, so it requires a bump —
/// without one, a fingerprint-unchanged file is never re-parsed and keeps the
/// old-format (potentially colliding) id until its bytes next change.
///
/// 9: markdown table chunks gained a `:t{idx}` id suffix
/// (`chunk_id_suffixed`) so a top-of-file table no longer collides with the
/// whole-file/section chunk that shares its `(line_start, byte_start, hash)`.
/// 10: parser now emits low-confidence `candidate_edges` (Lane 2) — cross-file
/// bare fn-pointer args, container-level / `with`-module serde
/// callbacks — alongside the confident `function_calls`. A
/// byte-identical file re-parsed under v10 produces candidate rows it did not
/// under v9, so a refresh is required even when `content_hash` is unchanged.
/// 11: L5X/L5K chunk `byte_start` is now lifted to a file-relative offset (was
/// region-relative). Two ST regions can begin on the same file line with
/// byte-identical leading ST, which collided on (line_start, region-relative
/// byte_start, content_hash) and dropped a chunk via the id PRIMARY KEY. The
/// lift changes the stored `byte_start` and thus the `id` for every L5X/L5K
/// chunk past the first region, so a refresh is required even for unchanged
/// content.
/// 12: L5X/L5K files are now routed to their own grammar-less LanguageDef
/// (custom ST extractor) on the production index path instead of being parsed
/// as generic XML. Previously `parse_file_all_inner` resolved `.l5x`/`.l5k`
/// to `Language::Xml` and emitted XML-element chunks; now it emits
/// Structured-Text routine chunks. The chunk class, ids, and content change
/// wholesale for every indexed L5X/L5K file, so a refresh is required even for
/// byte-identical content.
/// 13: Dart call-graph wiring + body-inclusive chunk ranges. `LANG_DART` now
/// has a `call_query` (was unwired — Dart yielded zero call edges), and the
/// chunk span of a Dart function/method/getter/setter is extended to include
/// its adjacent `function_body` (the grammar inlines the top-level-definition
/// rule, so the signature node alone excluded the body). A byte-identical Dart
/// file re-parsed under v13 produces call edges and body-inclusive chunk
/// content + line_end + content_hash + id that it did not under v12, so a
/// refresh is required even when the file's bytes are unchanged.
/// 14: L5X/L5K bare statement-list ST routines now extract call/type edges. A
/// real Rockwell ST routine export is a bare statement list (the routine is the
/// scope; only Add-On-Instruction exports carry a FUNCTION_BLOCK wrapper),
/// which the ST grammar parses to a single ERROR node — yielding zero
/// call_expression / var_decl_item nodes. The L5X/L5K relationship path now
/// wrap-normalizes a bare body in a synthetic program unit before the parse and
/// lifts the wrapper offset back off extracted coordinates. A byte-identical
/// bare-list L5X/L5K file re-parsed under v14 produces call + type edges it did
/// not under v13, so a refresh is required even when the file's bytes are
/// unchanged.
/// 15: leading-comment `doc` fallback now applies to chunks of ANY length (was
/// gated on chunks spanning <= 5 lines) and uses strict adjacency — the block
/// must be glued to the definition, a blank line breaks it, and shebang /
/// encoding modelines are excluded. A byte-identical file re-parsed under v15
/// can gain a `doc` on a long definition that carries a glued leading comment
/// block, and can lose a previously-captured `doc` where a blank line separated
/// the block from the definition (that path is now handled only by the
/// sibling-walk). Either way `doc` — and thus the embedding text — can change,
/// so a refresh is required even when the file's bytes are unchanged.
/// 16: a file-top license / copyright banner is now a hard boundary in BOTH
/// `doc` paths (`comment_is_license_banner`). The sibling walk's blank tolerance
/// (`MAX_TOLERATED_BLANK_SIBLINGS`) and the fallback's strict adjacency both
/// previously let a banner glued (or, in the walk, blank-separated within
/// tolerance) directly above the file's FIRST definition become that def's
/// `doc`. The walk / scan now stops at a head-anchored license signature (SPDX,
/// `Copyright …`, `Licensed under …`, MIT/BSD/GPL/MPL body openers), keeping any
/// real doc-comment lines between the banner and the def and dropping the banner
/// itself. A byte-identical file whose first def carried a leaked banner loses
/// it from `doc` (and thus its embedding text), so a refresh is required even
/// when the file's bytes are unchanged.
pub const PARSER_VERSION: u32 = 16;

/// Build the canonical chunk id from its identifying coordinates.
///
/// Format: `{path}:{line_start}:{byte_start}:{hash8}`.
///
/// Invariant — **injectivity within a file**: two byte-distinct chunks of the
/// same file must produce distinct ids. `line_start` + `hash8` alone is NOT
/// injective: two source elements can begin on the same line (`struct A; impl
/// A {}` — both line 1) and two byte-distinct elements can share an identical
/// full `content_hash` (`m!{} m!{}` — two macro invocations on one line are
/// byte-identical, so even the 64-char hash collides). `byte_start` — the
/// element's start byte offset in the file — is the disambiguator: tree-sitter
/// nodes (and markdown sections) occupy disjoint byte ranges, so `byte_start`
/// is unique per chunk within a file, deterministic, and stable across re-runs
/// of the same source. It is reconstructed identically at the indexing
/// pipeline's re-id step (path absolute→relative rewrite) from the persisted
/// [`Chunk::byte_start`] field, so extract-time and re-id ids match exactly.
///
/// Single-sourcing this format here is load-bearing: extract-time and re-id
/// MUST agree byte-for-byte or the re-id `old_id → new_id` map silently fails
/// to remap and `parent_id`/call-edge rewrites point at stale ids.
pub fn chunk_id(
    path_display: &str,
    line_start: u32,
    byte_start: u32,
    content_hash: &str,
) -> String {
    let hash_prefix = content_hash.get(..8).unwrap_or(content_hash);
    format!("{path_display}:{line_start}:{byte_start}:{hash_prefix}")
}

/// Build a chunk id carrying a structural disambiguator suffix appended to the
/// canonical [`chunk_id`] base.
///
/// Some chunks legitimately share all four base coordinates
/// (`path`/`line_start`/`byte_start`/`hash8`) with another chunk of the same
/// file, so the base alone is NOT injective for them. Two cases:
///
/// - **Sub-chunks at a parent's start offset.** A markdown table that begins on
///   its containing section's first line shares the section's `line_start` and
///   `byte_start`; when the section is *exactly* that table with no trailing
///   newline the `content_hash` matches too — all four base fields collide.
///   The table is the `idx`-th table of its section, so a `t{idx}` suffix
///   separates it from the section and from sibling tables.
/// - **Windows of one chunk.** Row-wise table windows and token-window code
///   chunks all inherit their parent's `line_start`/`byte_start`; the window
///   ordinal is the disambiguator (`t{idx}w{widx}` for table windows, `w{idx}`
///   for code windows).
///
/// Single-sourcing the suffix here (rather than `format!("{base}:…")` at each
/// emission site) is load-bearing for re-id: the indexing pipeline's re-id step
/// reconstructs each chunk's id from its persisted coordinates, and the stored
/// `Chunk::id` carries the suffix. Reconstructing the base only (the bug this
/// closes) drops the suffix, so the re-id `old_id → new_id` map sends a
/// suffixed id to a suffix-less one — non-injective, and distinct windows
/// collapse onto one id. The pipeline calls this function with the SAME suffix
/// the emission site used, so extract-time and re-id ids match byte-for-byte.
pub fn chunk_id_suffixed(
    path_display: &str,
    line_start: u32,
    byte_start: u32,
    content_hash: &str,
    suffix: &str,
) -> String {
    let base = chunk_id(path_display, line_start, byte_start, content_hash);
    format!("{base}:{suffix}")
}

/// Collapse every run of ASCII/Unicode whitespace in `s` to a single space,
/// then trim. Pure string normalization — no tree required.
///
/// This is the fallback canonicalization used when no tree-sitter node is
/// available (window chunks, markdown sections, L5X routine fallback, etc.).
/// It catches formatting-only edits (reindent, trailing whitespace, CRLF↔LF)
/// but not comment edits.
pub fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compute the blake3 hex of the whitespace-collapsed form of `content`.
/// The no-tree fallback used by every Chunk construction site that lacks a
/// tree-sitter node (markdown sections, window chunks, L5X routine fallback).
pub fn canonical_hash_fallback(content: &str) -> String {
    blake3::hash(collapse_whitespace(content).as_bytes())
        .to_hex()
        .to_string()
}

/// Compute the canonical (comment- and whitespace-normalized) hash for a
/// chunk, used as the embedding-reuse **cache key** (never as store identity).
///
/// Strategy (tree-precise path): walk every descendant of `node` whose kind
/// name contains `"comment"` — this covers `comment`, `line_comment`,
/// `block_comment`, `doc_comment`, `block_documentation_comment`, etc. across
/// all 54 grammars — collect their byte ranges, remove those ranges from the
/// chunk's `content`, collapse whitespace, trim, then blake3 → hex.
///
/// Byte ranges from tree-sitter are absolute (into `source`); `node.start_byte()`
/// is subtracted to rebase them onto the chunk-local `content` slice.
///
/// Safety / correctness notes:
/// - This is a cache key, not identity. A **missed** comment kind merely yields
///   a cache miss later (re-embed), which is safe. The only real risk is
///   **over-matching** a non-comment node whose kind happens to contain
///   `"comment"` — acceptable per grammar conventions (no mainstream grammar
///   names a code node `*comment*`).
/// - Ranges are sorted and merged so nested/overlapping comment nodes don't
///   double-remove bytes or produce out-of-order splices.
/// - On any non-UTF-8-boundary slice (pathological injection-rooted node) the
///   affected comment range is skipped with a `warn!`, degrading to a
///   slightly-less-canonical (but still valid) hash rather than panicking.
pub(crate) fn canonical_hash(node: tree_sitter::Node, content: &str) -> String {
    canonical_hash_spanning(node, None, content)
}

/// Like [`canonical_hash`], but additionally strips comment descendants of an
/// optional `extra` node whose subtree extends `content` past `node`'s own
/// `byte_range()` (the Dart body-span case — see `extract_chunk`). The `extra`
/// node's absolute byte ranges are rebased by the SAME `node.start_byte()` base
/// because both nodes occupy one contiguous content slice that begins at `node`.
pub(crate) fn canonical_hash_spanning(
    node: tree_sitter::Node,
    extra: Option<tree_sitter::Node>,
    content: &str,
) -> String {
    let _span = tracing::debug_span!("canonical_hash", kind = node.kind()).entered();
    let base = node.start_byte();
    let content_len = content.len();

    // Collect chunk-local byte ranges of all comment descendants.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    collect_comment_ranges(node, base, content_len, &mut ranges);
    if let Some(extra) = extra {
        collect_comment_ranges(extra, base, content_len, &mut ranges);
    }

    if ranges.is_empty() {
        return canonical_hash_fallback(content);
    }

    // Sort + merge overlapping/adjacent ranges so the splice loop below can
    // walk them in one forward pass.
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }

    // Rebuild content with comment ranges removed. Replace each removed range
    // with a single space so two tokens that were only separated by a comment
    // don't fuse (`a/*c*/b` → `a b`, not `ab`).
    let mut stripped = String::with_capacity(content_len);
    let mut pos = 0usize;
    for (start, end) in merged {
        if start > content_len || end > content_len || start < pos {
            // Out-of-range or non-monotonic (shouldn't happen after merge,
            // but guard against a pathological node) — skip this range.
            continue;
        }
        if let Some(seg) = content.get(pos..start) {
            stripped.push_str(seg);
        } else {
            tracing::warn!(
                pos,
                start,
                "canonical_hash: comment range not on UTF-8 boundary, skipping segment"
            );
        }
        stripped.push(' ');
        pos = end;
    }
    if let Some(seg) = content.get(pos..) {
        stripped.push_str(seg);
    }

    blake3::hash(collapse_whitespace(&stripped).as_bytes())
        .to_hex()
        .to_string()
}

/// Collect chunk-local byte ranges of every node whose kind name contains
/// `"comment"`. Ranges are rebased by subtracting `base` (the chunk's
/// `node.start_byte()`) and clamped to `content_len`.
///
/// Iterative (explicit work stack) rather than recursive: tree depth is bounded
/// only by `CQS_MAX_FILE_SIZE` (env-raisable), so a pathological deeply-nested
/// parse tree (minified output, nested parens) could otherwise recurse deep
/// enough to overflow a rayon worker stack and SIGSEGV the whole index/watch
/// process. A heap-backed stack grows instead.
fn collect_comment_ranges<'a>(
    root: tree_sitter::Node<'a>,
    base: usize,
    content_len: usize,
    out: &mut Vec<(usize, usize)>,
) {
    let mut stack: Vec<tree_sitter::Node<'a>> = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind().contains("comment") {
            let r = node.byte_range();
            // Rebase absolute byte offsets onto the chunk-local content slice.
            let start = r.start.saturating_sub(base);
            let end = r.end.saturating_sub(base).min(content_len);
            if start < end {
                out.push((start, end));
            }
            // A comment node has no comment descendants worth descending into.
            continue;
        }
        // Push children. Sibling visit order doesn't matter — the caller sorts
        // and merges the collected ranges before splicing.
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

impl Parser {
    /// Extracts a code chunk from a source file using Tree-sitter query matches.
    /// # Arguments
    /// * `source` - The source code text to extract from
    /// * `m` - The Tree-sitter query match containing captured nodes
    /// * `query` - The Tree-sitter query that produced the match
    /// * `language` - The programming language of the source
    /// * `path` - The file path being parsed
    /// # Returns
    /// A `Chunk` containing the extracted code definition (function, struct, class, enum, trait, interface, constant, section, property, delegate, event, module, macro, object, or typealias) along with its metadata.
    /// # Errors
    /// Returns `ParserError::ParseFailed` if no definition capture is found in the match, or if required captures are missing.
    pub(crate) fn extract_chunk(
        &self,
        source: &str,
        m: &tree_sitter::QueryMatch<'_, '_>,
        query: &tree_sitter::Query,
        language: Language,
        path: &Path,
    ) -> Result<Chunk, ParserError> {
        // Find which definition capture matched and get its node.
        // Uses ChunkType::CAPTURE_NAMES (generated by define_chunk_types!) as the
        // single source of truth — no separate list to keep in sync.
        let (node, base_chunk_type) = ChunkType::CAPTURE_NAMES
            .iter()
            .find_map(|name| {
                let chunk_type = capture_name_to_chunk_type(name)?;
                query
                    .capture_index_for_name(name)
                    .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                    .map(|c| (c.node, chunk_type))
            })
            .ok_or_else(|| {
                ParserError::ParseFailed("No definition capture found in match".into())
            })?;

        // Dart body-range span: the tree-sitter-dart grammar inlines its
        // top-level-definition rule, so a `function_signature` and its
        // `function_body` are ADJACENT SIBLINGS with no node spanning both. The
        // chunk query captures the signature, whose `byte_range()` covers only
        // the signature — excluding the body, which starves per-chunk call
        // extraction (it is scoped to the captured node's byte range). When a
        // captured signature has an associated `function_body`, extend the chunk
        // span to include it. Other languages have a real spanning node, so this
        // returns `None` for them and the chunk range is unchanged.
        let body_node = dart_function_body(node, language);
        let span_end_byte = body_node.map_or_else(|| node.end_byte(), |b| b.end_byte());

        // Get name capture
        let name_idx = query.capture_index_for_name("name");
        let name_capture = name_idx.and_then(|idx| m.captures.iter().find(|c| c.index == idx));
        let mut name = name_capture
            .map(|c| {
                let raw = source[c.node.byte_range()].to_string();
                // Names should never span multiple lines — error recovery in grammars
                // (especially SQL) can extend nodes past the actual name.
                raw.lines().next().unwrap_or(&raw).trim().to_string()
            })
            .unwrap_or_else(|| "<anonymous>".to_string());

        // Extract content over the (possibly body-extended) span. For all
        // non-Dart languages `span_end_byte == node.end_byte()`, so this is the
        // captured node's range unchanged.
        let content = source[node.start_byte()..span_end_byte].to_string();

        // Validate name position: if the @name capture is far from the definition
        // start, tree-sitter error recovery likely matched the wrong node.
        // Fall back to extracting the name from the content text.
        if let Some(nc) = name_capture {
            let name_line = nc.node.start_position().row;
            let def_line = node.start_position().row;
            if name_line.saturating_sub(def_line) > 5 {
                if let Some(extracted) = extract_name_fallback(&content) {
                    name = extracted;
                }
            }
        }

        // Line numbers (1-indexed for display). `line_end` tracks the extended
        // span so a body-inclusive Dart chunk reports the body's last line.
        let line_start = node.start_position().row as u32 + 1;
        let line_end = body_node
            .map_or_else(|| node.end_position(), |b| b.end_position())
            .row as u32
            + 1;

        // Extract signature
        let signature = extract_signature(&content, language);

        // Extract doc comments. When the tree-sitter sibling walk surfaces no
        // doc-syntax comment (common for grammars whose blank-line gaps stop the
        // walk, or codebases that document with plain `//`/`--`/`#` comment runs
        // rather than doc syntax), fall back to the contiguous leading
        // ordinary-comment block glued to the definition so the embedding still
        // gets the developer's stated intent. Doc syntax always wins via
        // `or_else`; the fallback never overrides or concatenates with it.
        let doc = extract_doc_comment(node, source, language)
            .or_else(|| extract_leading_comment_doc(node, source, language, path));

        // Determine chunk type - only infer for functions (to detect methods)
        let (chunk_type, parent_type_name) = if base_chunk_type == ChunkType::Function {
            infer_chunk_type(node, language, source)
        } else {
            (base_chunk_type, None)
        };

        if let Some(ref ptn) = parent_type_name {
            tracing::debug!(parent_type = %ptn, method = %name, "Extracted parent type for method");
        }

        // Content hash for deduplication (BLAKE3 produces 64 hex chars). The id
        // is `{path}:{line_start}:{byte_start}:{hash8}`; `byte_start` makes it
        // injective within a file. `line_start` + `hash8` alone is NOT: two
        // elements can begin on one line (`struct A; impl A {}`) and two
        // byte-distinct elements can share an identical full `content_hash`
        // (`m!{} m!{}` — same-line macro twins are byte-identical), so even the
        // full hash collides. `byte_start` (the node's file-relative start
        // offset) is unique per node within a file and breaks both cases. See
        // `chunk_id`.
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let byte_start = node.byte_range().start as u32;
        let id = chunk_id(
            &path.display().to_string(),
            line_start,
            byte_start,
            &content_hash,
        );

        // Comment- and whitespace-normalized hash for embedding cache reuse.
        // Tree-precise: strips all comment descendants of `node`. For a
        // body-extended Dart chunk the body's comment descendants live under
        // `body_node`, not `node`, so they are collected from both — otherwise a
        // comment-only edit inside the body would change the canonical hash and
        // force a needless re-embed.
        let canonical = canonical_hash_spanning(node, body_node, &content);

        Ok(Chunk {
            id,
            file: path.to_path_buf(),
            language,
            chunk_type,
            name,
            signature,
            content,
            doc,
            line_start,
            line_end,
            byte_start,
            content_hash,
            canonical_hash: canonical,
            parent_id: None,
            window_idx: None,
            parent_type_name,
            parser_version: PARSER_VERSION,
        })
    }
}

/// Find the byte offset of the header-terminating `:` for a `UntilColon`
/// signature (Python `def`/`class`), scanning at bracket depth 0 only.
///
/// A naive `content.find(':')` truncates on the first annotation colon —
/// `def f(x: int, y: str) -> bool:` yields `def f(x`, discarding the parameter
/// list, defaults, and the `-> bool` return annotation (which in turn kills
/// `extract_return_python`'s `rfind("->")`). The header `:` is always the first
/// colon at depth 0: parameter annotations, dict/set defaults, and slices all
/// sit inside `()`, `[]`, or `{}`.
///
/// String-literal defaults are tracked so a quoted colon (`def f(x="a:b"):`)
/// inside brackets is ignored — though such colons are already at depth > 0.
/// A bare quoted colon at depth 0 cannot occur in a well-formed `def`/`class`
/// header before the terminating `:`; tracking strings makes the scan robust
/// to malformed input rather than relying on that invariant.
///
/// Returns `None` if no depth-0 colon exists (falls back to `content.len()`).
fn find_def_colon(content: &str) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth: i32 = 0;
    // When inside a string literal, holds the opening quote byte (b'\'' or b'"').
    let mut string_quote: Option<u8> = None;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = string_quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == q {
                string_quote = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => string_quote = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Extracts a function or method signature from source code content based on the language's signature style.
/// # Arguments
/// * `content` - The source code content to extract the signature from
/// * `language` - The programming language, which determines the signature extraction style
/// # Returns
/// A `String` containing the extracted signature. The signature is determined by the language's signature style:
/// - `UntilBrace`: text up to the first `{`
/// - `UntilColon`: text up to the first `:` at bracket depth 0 (Python headers)
/// - `FirstLine`: text up to the first newline
/// - `UntilAs`: text up to a word boundary followed by the keyword "as" (case-insensitive)
/// - `Bread`: returns the full content (for markdown, typically unused)
/// # Panics
/// Panics if the signature style is not one of the above variants (incomplete pattern match).
pub(crate) fn extract_signature(content: &str, language: Language) -> String {
    let sig_end = match language.def().signature_style {
        SignatureStyle::UntilBrace => content.find('{').unwrap_or(content.len()),
        SignatureStyle::UntilColon => find_def_colon(content).unwrap_or(content.len()),
        SignatureStyle::FirstLine => content.find('\n').unwrap_or(content.len()),
        SignatureStyle::UntilAs => {
            // ASCII case-insensitive search preserves byte offsets
            // (to_uppercase() can change byte lengths for non-ASCII like ß→SS)
            let bytes = content.as_bytes();
            let is_boundary = |b: u8| b == b' ' || b == b'\n' || b == b'\t' || b == b'\r';
            let mut pos = None;
            // Bound is `len - 1` so the last valid two-byte "AS" window
            // (i = len-2, i+1 = len-1) is reached. With `len - 2` the
            // `i + 2 >= len` end-of-string guard below was unreachable, so a
            // chunk ending exactly in " AS" kept its full text as the signature.
            for i in 0..bytes.len().saturating_sub(1) {
                if bytes[i].eq_ignore_ascii_case(&b'A') && bytes[i + 1].eq_ignore_ascii_case(&b'S')
                {
                    let left_ok = i == 0 || is_boundary(bytes[i - 1]);
                    let right_ok = i + 2 >= bytes.len() || is_boundary(bytes[i + 2]);
                    if left_ok && right_ok {
                        // Position at the boundary before AS (or 0 if AS starts the string)
                        pos = Some(if i > 0 { i - 1 } else { 0 });
                        break;
                    }
                }
            }
            pos.unwrap_or(content.len())
        }
        // Markdown builds its own signatures in the custom parser; this arm
        // satisfies exhaustiveness but is never reached via extract_chunk().
        SignatureStyle::Breadcrumb => content.len(),
    };
    let sig = &content[..sig_end];
    // Normalize whitespace
    sig.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extracts documentation comments associated with a syntax node.
/// Searches backwards through sibling nodes to find documentation comments
/// matching the language's doc node types. Skips over non-doc comments AND
/// whitespace-only siblings (some grammars surface blank-line gaps as explicit
/// nodes that would otherwise stop the walk before the comment block). Stops
/// at the first sibling whose text contains substantive non-whitespace,
/// non-comment content. For Python, also checks for a docstring as the first
/// statement in the node's body if no preceding comments are found.
fn extract_doc_comment(
    node: tree_sitter::Node,
    source: &str,
    language: Language,
) -> Option<String> {
    let doc_nodes = language.def().doc_nodes;

    // Walk backwards through siblings looking for comments
    let mut comments = Vec::new();
    let mut current = node.prev_sibling();
    let mut tolerated_blanks = 0usize;

    while let Some(sibling) = current {
        let kind = sibling.kind();

        if doc_nodes.contains(&kind) {
            let text = &source[sibling.byte_range()];
            // A shebang / encoding modeline is a doc-node comment in several
            // grammars (tree-sitter parses `#!...` as a `comment`), but it
            // documents the file's interpreter/encoding, not the definition —
            // skip it so it never pollutes `doc`, and keep walking past it.
            if is_shebang_or_modeline(text) {
                current = sibling.prev_sibling();
                continue;
            }
            // A file-top license / copyright banner documents the file's legal
            // terms, not the definition below it. Stop the walk here (a hard
            // boundary, like reaching code): any real doc-comment lines already
            // collected between the banner and the def are kept, but the banner
            // — and everything above it — never becomes the def's `doc`.
            if comment_is_license_banner(text) {
                break;
            }
            comments.push(text.to_string());
            current = sibling.prev_sibling();
        } else if kind.contains("comment") {
            // A non-doc comment: keep looking past it — unless it is a license
            // banner, which bounds the walk exactly as in the doc-node branch.
            if comment_is_license_banner(&source[sibling.byte_range()]) {
                break;
            }
            current = sibling.prev_sibling();
        } else {
            // Tolerate whitespace-only siblings — some grammars (notably the
            // SQL grammar's handling of blank lines between file-level `--`
            // comments and the next CREATE TABLE) emit gap nodes that would
            // otherwise prematurely stop the walk before the doc block.
            // Cap at MAX_TOLERATED_BLANK_SIBLINGS so a malformed file with
            // thousands of empty siblings can't make us walk to the start of
            // the file.
            let text = &source[sibling.byte_range()];
            if text.chars().all(char::is_whitespace)
                && tolerated_blanks < MAX_TOLERATED_BLANK_SIBLINGS
            {
                tolerated_blanks += 1;
                tracing::trace!(
                    skipped_kind = kind,
                    blanks = tolerated_blanks,
                    "extract_doc_comment: tolerated whitespace sibling"
                );
                current = sibling.prev_sibling();
            } else {
                break;
            }
        }
    }

    if comments.is_empty() {
        // For Python, also check for docstring as first statement in body
        if language == Language::Python {
            if let Some(body) = node.child_by_field_name("body") {
                if let Some(first) = body.named_child(0) {
                    if first.kind() == "expression_statement" {
                        if let Some(string) = first.named_child(0) {
                            if string.kind() == "string" {
                                return Some(source[string.byte_range()].to_string());
                            }
                        }
                    }
                }
            }
        }
        return None;
    }

    comments.reverse();
    Some(comments.join("\n"))
}

/// Maximum number of preceding source lines to capture as a fallback `doc`.
/// Bounded so a runaway leading-comment block can't inflate the `doc` field
/// (and, with the reverse tail read, so per-chunk work stays O(lines captured)
/// regardless of file size).
const FALLBACK_DOC_MAX_LINES: usize = 8;

/// Maximum whitespace-only siblings that `extract_doc_comment` will skip past
/// while walking backwards in search of a doc-comment block. Bounds the walk
/// so a malformed file with thousands of empty siblings can't make us walk
/// to the start of the file, while still tolerating the blank-line gap nodes
/// some grammars (notably the SQL grammar) emit between file-level comments
/// and the next CREATE TABLE.
const MAX_TOLERATED_BLANK_SIBLINGS: usize = 4;

/// Returns true if a single source line looks like the start of a
/// comment in the given language.
///
/// Consults `lang.def().line_comment_prefixes` first (per-language precision
/// for languages with non-default comment syntax — Erlang `%`, Lua `--`, etc.).
/// If the language declares no prefixes (default `&[]`), falls back to the
/// conservative global union of common comment sigils so unknown / grammar-less
/// languages still get reasonable behaviour.
///
/// Trims the leading whitespace before checking; `*` alone is also accepted
/// for the inner lines of a `/** ... */` block.
///
/// The bare-`#` and bare-`*` arms require a separator so `#[derive]` (Rust
/// attribute), `#include`/`#define`/`#pragma` (C preprocessor), `#region`
/// (C# pragma), `*ptr = 0` (C deref), and `*x = y` are NOT classified as
/// comment-like. This separator rule applies to ambiguous single-char
/// prefixes regardless of whether they came from the per-language list or the
/// global fallback.
fn line_looks_comment_like(line: &str, lang: Language) -> bool {
    let t = line.trim_start();
    if t.is_empty() {
        return true;
    }

    // Per-language prefixes take precedence. Empty `&[]` means "no
    // language-specific list registered" — fall through to the global fallback.
    let prefixes = lang.def().line_comment_prefixes;
    if !prefixes.is_empty() {
        for prefix in prefixes {
            if !t.starts_with(prefix) {
                continue;
            }
            // Apply separator rule for ambiguous single-char prefixes so
            // attribute / preprocessor / pointer-deref lines stay rejected.
            if matches!(*prefix, "#" | "*" | "'") {
                let rest = &t[prefix.len()..];
                if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                    return true;
                }
                continue;
            }
            return true;
        }
        // Block-comment continuation markers — accept if any per-language
        // prefix included `/*`. Inner-`*` lines (` * doc`) and the closing
        // `*/` line are part of a `/** ... */` block.
        if prefixes.contains(&"/*") {
            if t.starts_with("*/") {
                return true;
            }
            if let Some(rest) = t.strip_prefix('*') {
                if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                    return true;
                }
            }
        }
        return false;
    }

    // Global fallback: conservative union for unknown languages and the
    // grammar-less default routing path.
    if t.starts_with("//")
        || t.starts_with("--")
        || t.starts_with("/*")
        || t.starts_with("*/")
        || t.starts_with("<!--")
        || t.starts_with("<%--")
        || t.starts_with("(*")
    {
        return true;
    }
    if let Some(rest) = t.strip_prefix('#') {
        return rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t');
    }
    if let Some(rest) = t.strip_prefix('*') {
        return rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t');
    }
    false
}

/// Returns true if a comment's first line is a shebang (`#!...`) or an
/// editor/encoding modeline (`# -*- coding: utf-8 -*-`, `# vim: ...`,
/// `# encoding: utf-8`, etc.). These lead a file but never document a
/// definition, so both the sibling walk and the leading-comment fallback drop
/// them: they are neither captured as `doc` nor allowed to sit in a captured
/// block. Without this, a Python file opening with `#!/usr/bin/env python` +
/// `# -*- coding: utf-8 -*-` would attach those lines to its first function.
///
/// Only the first physical line is inspected. A modeline is matched by
/// stripping the line-comment marker (`#` or `//`) and requiring the modeline
/// signature at the HEAD of the payload (marker, optional whitespace, then the
/// signature). Head-anchoring — NOT substring containment — is load-bearing: an
/// ordinary comment such as `// Fix encoding: normalize to NFC`, or a doc line
/// that merely mentions `-*-` in its prose, must survive as `doc`. `--`-comment
/// languages (SQL) never enter the modeline branch at all.
fn is_shebang_or_modeline(text: &str) -> bool {
    let first = text.lines().next().unwrap_or("").trim_start();
    // Shebang: `#!` at the head of the comment.
    if first.starts_with("#!") {
        return true;
    }
    // Strip the line-comment marker; a modeline signature must sit at the head
    // of what remains (after optional whitespace).
    let payload = match first.strip_prefix("//").or_else(|| first.strip_prefix('#')) {
        Some(rest) => rest.trim_start(),
        None => return false,
    };
    payload.starts_with("-*-")
        || payload.starts_with("vim:")
        || payload.starts_with("coding:")
        || payload.starts_with("coding=")
        || payload.starts_with("encoding:")
        || payload.starts_with("encoding=")
}

/// Returns true if a comment reads as a file-top license / copyright banner —
/// text that documents the FILE's legal terms, not the definition it happens to
/// sit above. Both `doc` paths treat such a comment as a hard boundary: the
/// sibling walk ([`extract_doc_comment`]) and the leading-comment fallback
/// ([`extract_leading_comment_doc`]) STOP at it, so a banner glued (or, in the
/// sibling walk, blank-separated within tolerance) directly above the file's
/// first definition never leaks into that def's `doc` and embedding text.
///
/// The decision is **head-anchored** on the first line carrying real content:
/// blank and purely-decorative lines (comment openers like `/*`, `//-`, and
/// separator rules of `-`/`=`/`*`/`#`) are skipped, then the FIRST content line
/// is stripped of its comment marker and matched against a small set of license
/// signatures at its HEAD. Only that first content line is the oracle — a later
/// line is never consulted — so a legitimate definition doc that merely MENTIONS
/// "license" or "copyright" mid-prose (or on a second sentence) survives. This
/// mirrors the [`is_shebang_or_modeline`] discipline: match the signature at the
/// head, never by substring containment.
///
/// The signatures are drawn from the classic banner shapes observed in real
/// corpora (SPDX tags, `Copyright …`, `Licensed under …`, the MIT/BSD/GPL/MPL
/// body openers, `All rights reserved`). A prose-intro header that defers its
/// license to a later line (`// Translated from C to Rust …\n// Copyright …`) is
/// deliberately NOT matched: such provenance notes are borderline doc-like and
/// rejecting them would cost head-anchoring's precision.
fn comment_is_license_banner(text: &str) -> bool {
    // Chars that a comment marker or a decorative rule is built from. Stripping
    // them from the head normalizes `// Copyright`, ` * SPDX-…`, `//-`, and
    // `# ====` alike. `(`, `@`, `\`, and letters are preserved so `(c)`,
    // `@license`, and real words remain matchable.
    const DECOR: &[char] = &[' ', '\t', '/', '*', '#', ';', '%', '!', '~', '=', '+', '-'];

    // Scan a bounded number of leading lines for the first with real content,
    // skipping blank/decorative openers (a `/*` on its own line, a `//-`, a
    // separator rule). Bounded so a giant comment can't drive an unbounded scan.
    for raw in text.lines().take(6) {
        let payload = raw.trim_matches(DECOR);
        if payload.is_empty() {
            continue;
        }
        // First real content line: this line — and only this line — decides.
        let low = payload.to_ascii_lowercase();
        return low.starts_with("copyright")
            || low.starts_with("(c)")
            || low.starts_with("spdx-")
            || low.starts_with("licensed under")
            || low.starts_with("licensed to")
            || low.starts_with("license:")
            || low.starts_with("all rights reserved")
            || low.starts_with("permission is hereby granted")
            || low.starts_with("redistribution and use")
            || low.starts_with("this file is part of")
            || low.starts_with("this program is free software")
            || low.starts_with("this source code form is subject")
            || low.starts_with("@license");
    }
    false
}

/// Fallback `doc` enrichment for a chunk whose tree-sitter sibling walk
/// ([`extract_doc_comment`]) surfaced no doc-syntax comment. Captures the
/// contiguous block of ordinary (non-doc-syntax) comment lines immediately
/// preceding the definition and returns it as the chunk's `doc`, so codebases
/// and grammars that document with plain `//` / `--` / `#` runs — or where the
/// sibling walk misses the block behind grammar-emitted gap nodes — still get
/// the developer's stated intent into the embedding text.
///
/// Applies to chunks of ANY length. (It formerly gated on chunks spanning <= 5
/// lines, so a long procedure with a leading `--` header block lost its doc;
/// that gate is removed.)
///
/// Rules:
/// - **Strict adjacency.** The block must be glued to the definition: the line
///   immediately preceding the def must be comment-like and non-blank. A blank
///   line terminates the block (it is NOT crossed) and any non-comment code
///   line terminates it. A distant comment (one for an earlier construct, or a
///   file banner set off by a blank line) therefore never leaks onto the
///   following definition.
/// - **Shebang / modeline exclusion.** A leading `#!` shebang or an
///   editor/encoding modeline is a hard stop, never part of the block
///   ([`is_shebang_or_modeline`]).
/// - **License-banner exclusion.** A file-top license / copyright banner is a
///   hard stop ([`comment_is_license_banner`]): the scan keeps any real leading
///   comment captured below it and drops the banner, so a banner glued directly
///   above the file's FIRST definition no longer leaks onto it.
/// - **Precedence.** Only reached via `extract_doc_comment(...).or_else(...)`,
///   so a real doc-syntax comment always wins; the fallback never overrides or
///   concatenates with it.
///
/// Comment markers (`//`, `--`, `#`, ...) are kept verbatim, matching the
/// raw-with-markers convention of the primary `extract_doc_comment` path so the
/// `doc` field has one consistent shape regardless of which path filled it.
///
/// **Performance:** only the last `FALLBACK_DOC_MAX_LINES + 2` lines of the
/// prefix are materialized. The tail is located by walking prefix bytes in
/// reverse counting newlines, bounding per-chunk work to O(lines captured)
/// regardless of file size — splitting the whole prefix per call would be
/// O(N^2) on files with many chunks.
fn extract_leading_comment_doc(
    node: tree_sitter::Node,
    source: &str,
    language: Language,
    path: &Path,
) -> Option<String> {
    let _span = tracing::trace_span!(
        "extract_leading_comment_doc",
        path = %path.display()
    )
    .entered();

    // tree-sitter occasionally surfaces non-codepoint-boundary `start_byte`
    // values on injection-rooted nodes. Emit a warn (rather than silently
    // dropping via `?`) so a pathological grammar change or malformed input is
    // visible in the journal.
    let start_byte = node.start_byte();
    let prefix = match source.get(..start_byte) {
        Some(p) => p,
        None => {
            tracing::warn!(
                start_byte,
                path = %path.display(),
                "extract_leading_comment_doc: start_byte not on UTF-8 boundary — \
                 skipping fallback for this chunk"
            );
            return None;
        }
    };
    if prefix.is_empty() {
        return None;
    }

    // Read only the tail we could possibly capture: FALLBACK_DOC_MAX_LINES
    // comment lines + the def's own-line fragment + a small margin. Locate it by
    // reverse newline-counting so a file with many chunks stays O(lines) per
    // call rather than O(prefix).
    let bytes = prefix.as_bytes();
    let max_newlines = FALLBACK_DOC_MAX_LINES + 2;
    let mut newline_count = 0usize;
    let mut tail_start = 0usize;
    for i in (0..bytes.len()).rev() {
        if bytes[i] == b'\n' {
            newline_count += 1;
            if newline_count >= max_newlines {
                tail_start = i + 1;
                break;
            }
        }
    }
    let tail = &prefix[tail_start..];
    // `str::lines()` already strips trailing `\r` from `\r\n` line terminators.
    let lines: Vec<&str> = tail.lines().collect();
    if lines.is_empty() {
        return None;
    }

    // The def's own line contributes a trailing fragment (its indentation) when
    // the prefix does not end at a newline. If that fragment carries
    // non-whitespace, code sits on the def's line before it (`struct A; fn b`):
    // there is no leading comment block, so bail. Otherwise drop the fragment
    // and scan the lines above it.
    let mut scan_end = lines.len();
    if !prefix.ends_with('\n') {
        if let Some(last) = lines.last() {
            if !last.trim().is_empty() {
                return None;
            }
        }
        scan_end -= 1;
    }

    // Strict-adjacency backward scan: a blank line, a non-comment line, or a
    // shebang/modeline terminates the block. No blank-gap tolerance — the block
    // must be glued to the definition.
    let mut start = scan_end;
    let mut taken = 0usize;
    while start > 0 && taken < FALLBACK_DOC_MAX_LINES {
        let candidate = lines[start - 1];
        if candidate.trim().is_empty() {
            break;
        }
        if is_shebang_or_modeline(candidate) {
            break;
        }
        // A file-top license / copyright banner bounds the block: it documents
        // the file, not the definition. Stopping here keeps any real leading
        // comment already captured below the banner and drops the banner itself,
        // so a banner glued directly above the file's first def never leaks.
        if comment_is_license_banner(candidate) {
            break;
        }
        if !line_looks_comment_like(candidate, language) {
            break;
        }
        start -= 1;
        taken += 1;
    }
    if taken == 0 {
        return None;
    }

    let captured: String = lines[start..scan_end].join("\n");
    if captured.trim().is_empty() {
        return None;
    }
    tracing::debug!(
        captured_lines = taken,
        path = %path.display(),
        "extract_leading_comment_doc: filled empty doc from leading comment block"
    );
    Some(captured)
}

/// Extract a name from chunk content when tree-sitter's @name capture is wrong.
/// Looks for `PROCEDURE name`, `FUNCTION name`, `VIEW name`, or `TRIGGER name`
/// patterns in the first few lines.
fn extract_name_fallback(content: &str) -> Option<String> {
    let bytes = content.as_bytes();
    for keyword in &[b"PROCEDURE" as &[u8], b"FUNCTION", b"VIEW", b"TRIGGER"] {
        // ASCII case-insensitive search preserves byte offsets
        // (to_uppercase() can change byte lengths for non-ASCII like ß→SS)
        if let Some(pos) = bytes.windows(keyword.len()).position(|w| {
            w.iter()
                .zip(keyword.iter())
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        }) {
            let after_keyword = pos + keyword.len();
            if after_keyword >= content.len() {
                continue;
            }
            let rest = content[after_keyword..].trim_start();
            // Name ends at whitespace, '(', '@', or newline
            let name_end = rest
                .find(|c: char| c.is_whitespace() || c == '(' || c == '@')
                .unwrap_or(rest.len());
            let name = rest[..name_end].trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Locate the `function_body` node associated with a captured Dart
/// signature/member node, so [`Parser::extract_chunk`] can extend the chunk
/// span to include the body.
///
/// The tree-sitter-dart grammar inlines `_top_level_definition`, so a top-level
/// `function_signature` / `getter_signature` / `setter_signature` and its
/// `function_body` are adjacent siblings of `source_file` with no spanning
/// node; in-class members nest as `class_member > method_signature >
/// {function,getter,setter}_signature`, with `function_body` a sibling of
/// `method_signature`. Two cases cover both shapes:
///   1. the captured node's immediate next sibling is `function_body`
///      (top-level signature, or an in-class method captured at
///      `method_signature`), and
///   2. the captured node's parent is `method_signature` whose next sibling is
///      `function_body` (an in-class getter/setter captured at the inner
///      `getter_signature`/`setter_signature`).
/// Abstract members and signature-only declarations have no `function_body` and
/// yield `None` (the chunk keeps its signature-only range). Returns `None` for
/// every non-Dart language.
fn dart_function_body(node: tree_sitter::Node, language: Language) -> Option<tree_sitter::Node> {
    if language != Language::Dart {
        return None;
    }
    if let Some(sib) = node.next_sibling() {
        if sib.kind() == "function_body" {
            return Some(sib);
        }
    }
    let parent = node.parent()?;
    if parent.kind() == "method_signature" {
        if let Some(sib) = parent.next_sibling() {
            if sib.kind() == "function_body" {
                return Some(sib);
            }
        }
    }
    None
}

/// Infers whether a syntax tree node represents a function or method, and determines the parent type for methods.
/// # Arguments
/// * `node` - The syntax tree node to analyze
/// * `language` - The programming language definition for the node
/// * `source` - The source code text for extracting type information
/// # Returns
/// A tuple containing:
/// * `ChunkType` - Either `Function` or `Method`
/// * `Option<String>` - The parent type name if the node is a method, `None` otherwise
fn infer_chunk_type(
    node: tree_sitter::Node,
    language: Language,
    source: &str,
) -> (ChunkType, Option<String>) {
    let def = language.def();

    // Check if the node itself is a method kind (e.g., Go's "method_declaration")
    if def.method_node_kinds.contains(&node.kind()) {
        let parent_type = extract_method_receiver_type(node, language, source);
        return (ChunkType::Method, parent_type);
    }

    // Check if the function has a qualified name (e.g., C++ out-of-class methods)
    if let Some(extractor) = def.extract_qualified_method {
        if let Some(class_name) = extractor(node, source) {
            tracing::debug!(class = %class_name, "Qualified method inference");
            return (ChunkType::Method, Some(class_name));
        }
    }

    // Walk parents looking for method containers (e.g., impl blocks, class bodies)
    let mut current = node.parent();
    while let Some(parent) = current {
        if def.method_containers.contains(&parent.kind()) {
            let parent_type = extract_container_type_name(parent, language, source);
            return (ChunkType::Method, parent_type);
        }
        current = parent.parent();
    }

    (ChunkType::Function, None)
}

/// Extract type name from a method container node.
/// Uses LanguageDef fields: `container_body_kinds` and `extract_container_name`.
fn extract_container_type_name(
    container: tree_sitter::Node,
    language: Language,
    source: &str,
) -> Option<String> {
    let def = language.def();

    // If language provides a custom extractor, use it
    if let Some(extractor) = def.extract_container_name {
        return extractor(container, source);
    }

    // Default algorithm:
    // 1. If container is a body node (e.g., class_body, declaration_list), walk up
    // 2. Read the "name" field from the resulting node
    let type_node = if def.container_body_kinds.contains(&container.kind()) {
        container.parent()
    } else {
        Some(container)
    };

    type_node.and_then(|n| {
        n.child_by_field_name("name")
            .map(|name| source[name.byte_range()].to_string())
    })
}

/// Extract receiver type from a Go method_declaration.
/// Go methods: `func (r *Server) Handle()` → "Server"
fn extract_method_receiver_type(
    node: tree_sitter::Node,
    language: Language,
    source: &str,
) -> Option<String> {
    if language != Language::Go {
        return None;
    }
    // method_declaration → receiver (parameter_list) → parameter_declaration → type
    let receiver = node.child_by_field_name("receiver")?;
    let first_param = receiver.named_child(0)?;
    // type_identifier may be nested in pointer_type
    find_type_identifier_recursive(first_param, source)
}

/// Find the first `type_identifier` node (pre-order) and return its text.
/// Used for Go where the type may be wrapped in `pointer_type`.
///
/// Iterative (explicit work stack) rather than recursive for the same
/// stack-safety reason as [`collect_comment_ranges`]: a deeply-nested parse
/// tree must not overflow a rayon worker stack. Children are pushed in reverse
/// so the first child is popped first, preserving the original pre-order
/// (depth-first, leftmost) search that returns the shallowest-leftmost match.
fn find_type_identifier_recursive(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut stack: Vec<tree_sitter::Node> = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" {
            return Some(source[n.byte_range()].to_string());
        }
        let mut cursor = n.walk();
        let children: Vec<tree_sitter::Node> = n.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Regression guard for the SQL positional-cutoff defect: every
    /// `CREATE TABLE` / `CREATE VIRTUAL TABLE` in the project's own
    /// `src/schema.sql` must surface as a chunk, not a positional prefix or
    /// suffix of them. The grammar (tree-sitter-sequel-tsql) emits ERROR
    /// nodes on valid SQLite DDL it doesn't model — `IF NOT EXISTS`,
    /// `AUTOINCREMENT`, `USING fts5(...)`, `CREATE TRIGGER` — so this pins
    /// that the chunk query keeps matching `create_table` siblings across
    /// those recovery points instead of resuming at the first clean statement.
    #[test]
    fn schema_sql_chunks_every_create_table() {
        let source =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/schema.sql"))
                .unwrap();

        // Ground truth from the file text: names declared by CREATE [VIRTUAL]
        // TABLE [IF NOT EXISTS] <name>.
        let declared = sql_declared_table_names(&source);
        assert!(
            declared.len() >= 11,
            "fixture drift: expected schema.sql to declare >=11 tables, found {}: {declared:?}",
            declared.len()
        );

        let parser = Parser::new().unwrap();
        let chunks = parser
            .parse_source(
                &source,
                Language::Sql,
                std::path::Path::new("src/schema.sql"),
            )
            .unwrap();
        let chunked: std::collections::HashSet<&str> =
            chunks.iter().map(|c| c.name.as_str()).collect();

        let missing: Vec<&String> = declared
            .iter()
            .filter(|n| !chunked.contains(n.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "schema.sql tables not chunked (positional-cutoff regression): {missing:?}; \
             chunked names = {:?}",
            chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    /// Self-contained early-error-recovery shape: a table, then a
    /// parse-stressing construct the SQL grammar can't model (a
    /// `CREATE VIRTUAL TABLE ... USING fts5(...)` that yields ERROR nodes),
    /// then more tables. All three tables must chunk — an ERROR node between
    /// `create_table` siblings must not truncate the match stream. Guards the
    /// recovery contract independently of `src/schema.sql`'s content, which
    /// drifts as the real schema evolves.
    #[test]
    fn sql_tables_around_error_recovery_all_chunk() {
        let content = "\
CREATE TABLE IF NOT EXISTS early (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    val TEXT NOT NULL
);

-- fts5 virtual table: the grammar emits ERROR nodes for USING fts5(...)
CREATE VIRTUAL TABLE IF NOT EXISTS early_fts USING fts5(
    id UNINDEXED,
    body,
    tokenize='unicode61'
);

CREATE TABLE IF NOT EXISTS late (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL
);
";
        let file = write_temp_file(content, "sql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: std::collections::HashSet<&str> =
            chunks.iter().map(|c| c.name.as_str()).collect();
        for expected in ["early", "early_fts", "late"] {
            assert!(
                names.contains(expected),
                "table {expected:?} dropped across an error-recovery boundary; got {:?}",
                chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
            );
        }
    }

    /// Extract table names from `CREATE [VIRTUAL] TABLE [IF NOT EXISTS] <name>`
    /// statements via line scan — the file-text ground truth the chunk query
    /// is checked against. Case-insensitive on keywords; the trailing name is
    /// read off the original-case text so the chunk-name comparison is exact.
    fn sql_declared_table_names(source: &str) -> Vec<String> {
        // Strip a case-insensitive keyword prefix, returning the remainder of
        // the ORIGINAL-case string (so the table name keeps its real casing).
        fn strip_kw<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
            let s = s.trim_start();
            if s.len() >= kw.len() && s[..kw.len()].eq_ignore_ascii_case(kw) {
                Some(&s[kw.len()..])
            } else {
                None
            }
        }

        let mut names = Vec::new();
        for line in source.lines() {
            let after_create =
                strip_kw(line, "CREATE VIRTUAL TABLE ").or_else(|| strip_kw(line, "CREATE TABLE "));
            let Some(rest) = after_create else { continue };
            let rest = strip_kw(rest, "IF NOT EXISTS ").unwrap_or_else(|| rest.trim_start());
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                .collect();
            if !name.is_empty() {
                names.push(name);
            }
        }
        names
    }

    /// Tests for the comment-/whitespace-canonical embedding-cache hash
    /// (`canonical_hash`). Verifies that comment-only and formatting-only
    /// edits produce the SAME canonical hash (so the embedding is reused),
    /// while a real code change produces a DIFFERENT one.
    mod canonical_hash_tests {
        use super::*;

        /// Parse `content` with the given extension and return the
        /// canonical_hash of the chunk named `name`.
        fn canon_of(content: &str, ext: &str, name: &str) -> String {
            let mut file = tempfile::Builder::new()
                .suffix(&format!(".{ext}"))
                .tempfile()
                .unwrap();
            file.write_all(content.as_bytes()).unwrap();
            file.flush().unwrap();
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("chunk {name} not found in {content:?}"));
            assert!(
                !chunk.canonical_hash.is_empty(),
                "freshly-parsed chunk must have a non-empty canonical_hash"
            );
            chunk.canonical_hash.clone()
        }

        #[test]
        fn collapse_whitespace_basics() {
            assert_eq!(collapse_whitespace("a   b\n\tc"), "a b c");
            assert_eq!(
                collapse_whitespace("  leading trailing  "),
                "leading trailing"
            );
            assert_eq!(collapse_whitespace(""), "");
        }

        #[test]
        fn fallback_ignores_whitespace_only_diffs() {
            // Collapse normalizes runs of whitespace (newlines, tabs, multiple
            // spaces) to a single space + trim — so reindent / CRLF / trailing
            // whitespace edits hash identically.
            assert_eq!(
                canonical_hash_fallback("fn a() {\n\tx\n}"),
                canonical_hash_fallback("fn a()   {  x  }  ")
            );
            assert_ne!(
                canonical_hash_fallback("fn a() {}"),
                canonical_hash_fallback("fn b() {}")
            );
        }

        #[test]
        fn rust_line_comment_does_not_change_canonical_hash() {
            let a = canon_of("fn add(a: i32, b: i32) -> i32 { a + b }\n", "rs", "add");
            let b = canon_of(
                "fn add(a: i32, b: i32) -> i32 {\n    // sum the two args\n    a + b\n}\n",
                "rs",
                "add",
            );
            assert_eq!(
                a, b,
                "a //-comment-only edit must not change canonical_hash"
            );
        }

        #[test]
        fn rust_block_comment_does_not_change_canonical_hash() {
            let a = canon_of("fn mul(a: i32, b: i32) -> i32 { a * b }\n", "rs", "mul");
            let b = canon_of(
                "fn mul(a: i32, b: i32) -> i32 { /* product */ a * b }\n",
                "rs",
                "mul",
            );
            assert_eq!(
                a, b,
                "a /* */-comment-only edit must not change canonical_hash"
            );
        }

        #[test]
        fn rust_doc_comment_does_not_change_canonical_hash() {
            // The doc comment sits inside the function-item byte range only
            // when attached; here we vary an *inner* comment to keep the same
            // chunk node. Outer doc comments are a sibling of the node so they
            // don't affect the chunk content either way — this asserts the
            // inner case.
            let a = canon_of("fn f() -> u8 {\n    1\n}\n", "rs", "f");
            let b = canon_of("fn f() -> u8 {\n    //! inner\n    1\n}\n", "rs", "f");
            assert_eq!(a, b);
        }

        #[test]
        fn rust_formatting_only_edit_does_not_change_canonical_hash() {
            let a = canon_of("fn t() -> i32 { 1 + 2 }\n", "rs", "t");
            let b = canon_of("fn   t()   ->   i32   {\n\n    1  +  2\n\n}\n", "rs", "t");
            assert_eq!(
                a, b,
                "reindent / whitespace-only edit must not change canonical_hash"
            );
        }

        #[test]
        fn rust_code_change_does_change_canonical_hash() {
            let a = canon_of("fn add(a: i32, b: i32) -> i32 { a + b }\n", "rs", "add");
            let b = canon_of("fn add(a: i32, b: i32) -> i32 { a - b }\n", "rs", "add");
            assert_ne!(a, b, "a real code change MUST change canonical_hash");
        }

        #[test]
        fn python_hash_comment_does_not_change_canonical_hash() {
            let a = canon_of("def f(x):\n    return x + 1\n", "py", "f");
            let b = canon_of("def f(x):\n    # add one\n    return x + 1\n", "py", "f");
            assert_eq!(a, b, "a #-comment-only edit must not change canonical_hash");
        }

        #[test]
        fn python_code_change_does_change_canonical_hash() {
            let a = canon_of("def f(x):\n    return x + 1\n", "py", "f");
            let b = canon_of("def f(x):\n    return x + 2\n", "py", "f");
            assert_ne!(a, b);
        }

        #[test]
        fn dart_body_comment_does_not_change_canonical_hash() {
            // Exercises the Dart body-inclusive path: the tree-sitter-dart
            // grammar inlines the top-level-definition rule, so a Dart function's
            // `function_signature` (the captured chunk node) and its
            // `function_body` are ADJACENT SIBLINGS. `extract_chunk` extends the
            // chunk span over the body and passes the body as the `extra` arg to
            // `canonical_hash_spanning`, so a comment living INSIDE the body must
            // be stripped from the canonical hash. Parsing through `Parser`
            // (rather than calling the helper directly) is what feeds the real
            // body node into that `extra` argument.
            let a = canon_of(
                "int square(int x) {\n  return x * x;\n}\n",
                "dart",
                "square",
            );
            // `//` line comment inside the body.
            let b = canon_of(
                "int square(int x) {\n  // square the argument\n  return x * x;\n}\n",
                "dart",
                "square",
            );
            assert_eq!(
                a, b,
                "a //-comment-only edit inside a Dart fn body must not change canonical_hash"
            );
            // `/* */` block comment inside the body.
            let c = canon_of(
                "int square(int x) {\n  /* square the argument */\n  return x * x;\n}\n",
                "dart",
                "square",
            );
            assert_eq!(
                a, c,
                "a /* */-comment-only edit inside a Dart fn body must not change canonical_hash"
            );
        }

        #[test]
        fn dart_method_body_comment_does_not_change_canonical_hash() {
            // The method case routes through `dart_function_body`'s
            // `method_signature` branch: the body node is a sibling of the
            // signature's parent. A comment inside a Dart class method body must
            // still be stripped from the canonical hash.
            let a = canon_of(
                "class Calc {\n  int add(int a, int b) {\n    return a + b;\n  }\n}\n",
                "dart",
                "add",
            );
            let b = canon_of(
                "class Calc {\n  int add(int a, int b) {\n    // sum the two args\n    return a + b;\n  }\n}\n",
                "dart",
                "add",
            );
            assert_eq!(
                a, b,
                "a //-comment-only edit inside a Dart method body must not change canonical_hash"
            );
        }

        #[test]
        fn dart_body_code_change_does_change_canonical_hash() {
            // The dual of the comment tests: a REAL change inside the
            // body-spanned region (which lives under the `extra` body node, not
            // the captured signature node) MUST change canonical_hash. This pins
            // that the body-inclusive path is hashing body content, not just the
            // signature.
            let fn_a = canon_of(
                "int square(int x) {\n  return x * x;\n}\n",
                "dart",
                "square",
            );
            let fn_b = canon_of(
                "int square(int x) {\n  return x + x;\n}\n",
                "dart",
                "square",
            );
            assert_ne!(
                fn_a, fn_b,
                "a real Dart fn body change MUST change canonical_hash"
            );
            let m_a = canon_of(
                "class Calc {\n  int add(int a, int b) {\n    return a + b;\n  }\n}\n",
                "dart",
                "add",
            );
            let m_b = canon_of(
                "class Calc {\n  int add(int a, int b) {\n    return a - b;\n  }\n}\n",
                "dart",
                "add",
            );
            assert_ne!(
                m_a, m_b,
                "a real Dart method body change MUST change canonical_hash"
            );
        }

        #[test]
        fn comment_strip_does_not_fuse_adjacent_tokens() {
            // `a/* */b` must canonicalize differently from `ab`: the removed
            // comment is replaced with a space so the two identifiers stay
            // distinct. Use the pure helper with a synthetic split.
            assert_ne!(
                canonical_hash_fallback("a b"),
                canonical_hash_fallback("ab")
            );
        }

        // ====== Property-based: idempotence + metamorphic canonicalization ======
        //
        // The hand-written tests above each pin one fixed (base, edited) pair
        // and assert their canonical hashes match. The properties below assert
        // the underlying ALGEBRA over a generated input space:
        //   1. `collapse_whitespace` is idempotent: f(f(x)) == f(x).
        //   2. `canonical_hash_fallback` is whitespace-invariant: any edit that
        //      changes only inter-token whitespace preserves the hash.
        //   3. (metamorphic, through the real parser) inserting a `//` comment
        //      line or reindenting a function body preserves the chunk's
        //      `canonical_hash` — for EVERY generated body, not one fixture.
        mod canon_proptest {
            use super::*;
            use proptest::prelude::*;

            /// Whitespace-rich string strategy. Coverage claim: interleaves
            /// "word" tokens with runs of mixed whitespace drawn from a set
            /// that spans ASCII (` `, `\t`, `\n`, `\r`) AND Unicode whitespace
            /// (`\u{00A0}` NBSP, `\u{2003}` EM SPACE, `\u{3000}` IDEOGRAPHIC
            /// SPACE) — the chars `split_whitespace` treats as separators but
            /// that a naive `.replace(' ', "")` would miss. Includes the
            /// all-whitespace and empty cases via the `0..` lower bounds.
            ///
            /// DISTRUST: a generator that only emitted ASCII spaces would make
            /// idempotence trivial (one pass already canonical). The Unicode
            /// whitespace forces the first pass to actually transform, so the
            /// second-pass-is-identity claim is non-vacuous.
            fn whitespacey_string() -> impl Strategy<Value = String> {
                let ws = prop::sample::select(vec![
                    " ", "\t", "\n", "\r", "\r\n", "\u{00A0}", "\u{2003}", "\u{3000}", "  \t",
                ]);
                let word = "[a-zA-Z0-9_]{0,4}";
                // Alternate up to 8 (word, whitespace-run) pairs, then a
                // trailing word. Lower bound 0 covers empty and all-ws inputs.
                (
                    prop::collection::vec((word, ws.clone()), 0..8),
                    "[a-zA-Z0-9_]{0,4}",
                )
                    .prop_map(|(pairs, tail)| {
                        let mut s = String::new();
                        for (w, sep) in pairs {
                            s.push_str(&w);
                            s.push_str(sep);
                        }
                        s.push_str(&tail);
                        s
                    })
            }

            proptest! {
                #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

                /// Idempotence: collapse_whitespace(collapse_whitespace(x))
                ///            == collapse_whitespace(x), for all x.
                /// A second application of a normalizer must be the identity —
                /// example tests apply it once and never probe the fixed point.
                #[test]
                fn prop_collapse_whitespace_idempotent(s in whitespacey_string()) {
                    let once = collapse_whitespace(&s);
                    let twice = collapse_whitespace(&once);
                    prop_assert_eq!(&once, &twice, "collapse_whitespace not idempotent on {:?}", s);
                }

                /// collapse_whitespace output is in canonical form: no leading
                /// or trailing space, and no run of two consecutive ASCII
                /// spaces. A cheap oracle that pins WHY idempotence holds.
                #[test]
                fn prop_collapse_whitespace_canonical_form(s in whitespacey_string()) {
                    let c = collapse_whitespace(&s);
                    prop_assert!(!c.starts_with(' '), "leading space in {:?}", c);
                    prop_assert!(!c.ends_with(' '), "trailing space in {:?}", c);
                    prop_assert!(!c.contains("  "), "double space in {:?}", c);
                }

                /// Whitespace invariance of the no-tree fallback hash: re-spacing
                /// the inter-token whitespace (any run -> any other non-empty
                /// run) must not change canonical_hash_fallback. We build two
                /// strings with the SAME word sequence but DIFFERENT whitespace
                /// runs and assert equal hashes.
                #[test]
                fn prop_fallback_hash_whitespace_invariant(
                    words in prop::collection::vec("[a-zA-Z0-9_]{1,5}", 1..8),
                    sep_a in prop::sample::select(vec![" ", "\t", "\n", "  ", "\r\n", "\u{00A0}"]),
                    sep_b in prop::sample::select(vec![" ", "\t\t", "\n  ", "   ", " \t "]),
                ) {
                    let a = words.join(sep_a);
                    let b = words.join(sep_b);
                    prop_assert_eq!(
                        canonical_hash_fallback(&a),
                        canonical_hash_fallback(&b),
                        "fallback hash changed under a whitespace-only re-spacing: {:?} vs {:?}",
                        a, b
                    );
                }

                /// Metamorphic, through the REAL tree-sitter parser: inserting
                /// `//` line comments between the statements of a Rust function
                /// body, and/or reindenting it, must NOT change the chunk's
                /// `canonical_hash` (the embedding-reuse cache key). This is the
                /// property the example tests sample at single fixtures
                /// (`rust_line_comment_does_not_change_canonical_hash`) lifted
                /// to a generated family of bodies and comment payloads.
                #[test]
                fn prop_comment_and_reindent_preserve_canonical_hash(
                    // 1..4 simple statements that bind locals and a final expr.
                    stmt_vals in prop::collection::vec(0i32..1000, 1..4),
                    // Comment payloads. The class is restricted to printable
                    // ASCII plus the Latin-1 + general-punctuation Unicode
                    // ranges, EXCLUDING all control characters. This matters:
                    // tree-sitter-rust terminates a `//` line_comment at a NUL
                    // (or other C0 control) byte and emits the control as a
                    // separate ERROR node, which `collect_comment_ranges` does
                    // NOT strip — so a control byte in the payload would leak
                    // into the canonicalized text and (correctly) change the
                    // hash. That divergence is a grammar fact, not a codec bug:
                    // the control char is genuinely not part of the comment, so
                    // including it would make the "comment-only edit" premise
                    // false and the property too strong — a `\0` in a `//`
                    // payload parses as an ERROR node, not comment text.
                    // We therefore generate only payloads that the grammar
                    // accepts as comment continuation.
                    comments in prop::collection::vec(
                        "[ -~\u{00A1}-\u{024F}\u{2010}-\u{2027}]{0,20}",
                        1..4,
                    ),
                    extra_indent in 0usize..6,
                ) {
                    // Build the base body: `let a0 = V0; let a1 = V1; ... a0`
                    let mut base = String::from("fn f() -> i32 {\n");
                    for (i, v) in stmt_vals.iter().enumerate() {
                        base.push_str(&format!("    let a{i} = {v};\n"));
                    }
                    base.push_str("    a0\n}\n");

                    // Build the edited body: the SAME statement text as base
                    // (byte-identical `let a{i} = {v};`), but each preceded by a
                    // `//` comment line and given `extra_indent` additional
                    // leading spaces. The ONLY differences vs base are (a)
                    // inserted comment lines and (b) leading-whitespace runs —
                    // both of which `canonical_hash` must erase. Inter-token
                    // spacing within a statement is left untouched on purpose:
                    // changing it (e.g. `0;` -> `0 ;`) is a real textual edit
                    // that `canonical_hash` is *not* contracted to collapse —
                    // `collapse_whitespace` merges whitespace runs but never
                    // inserts a token boundary, so `0;` and `0 ;` hash
                    // differently. Introducing that here would make the property
                    // too strong (a generator artifact, not a codec bug).
                    let pad = " ".repeat(extra_indent);
                    // Signature with WIDENED whitespace RUNS only (`()  ->  i32`)
                    // — collapsing runs yields the same `fn f() -> i32` as base,
                    // so this stays a whitespace-only edit (no new token
                    // boundary introduced, unlike a space before `;`).
                    let mut edited = String::from("fn f()  ->  i32 {\n");
                    for (i, v) in stmt_vals.iter().enumerate() {
                        let c = &comments[i % comments.len()];
                        edited.push_str(&format!("{pad}    // {c}\n"));
                        edited.push_str(&format!("{pad}    let a{i} = {v};\n"));
                    }
                    edited.push_str("    // trailing note\n    a0\n}\n");

                    let base_hash = canon_of(&base, "rs", "f");
                    let edited_hash = canon_of(&edited, "rs", "f");
                    prop_assert_eq!(
                        &base_hash, &edited_hash,
                        "comment-insertion + reindent changed canonical_hash:\nBASE:\n{}\nEDITED:\n{}",
                        base, edited
                    );
                }
            }
        }
    }

    mod signature_tests {
        use super::*;

        #[test]
        fn test_rust_signature_stops_at_brace() {
            let content = "fn process(x: i32) -> Result<(), Error> {\n    body\n}";
            let sig = extract_signature(content, Language::Rust);
            assert_eq!(sig, "fn process(x: i32) -> Result<(), Error>");
        }

        #[test]
        fn test_rust_signature_normalizes_whitespace() {
            let content = "fn   process(  x: i32  )   -> i32 {";
            let sig = extract_signature(content, Language::Rust);
            assert_eq!(sig, "fn process( x: i32 ) -> i32");
        }

        #[test]
        fn test_python_signature_stops_at_colon() {
            let content = "def calculate(x, y):\n    return x + y";
            let sig = extract_signature(content, Language::Python);
            assert_eq!(sig, "def calculate(x, y)");
        }

        #[test]
        fn test_python_signature_annotated_full() {
            // The first annotation colon (`x: int`) must NOT truncate the
            // signature — depth-0 scan keeps the whole header including the
            // return annotation.
            let content = "def f(x: int, y: str) -> bool:\n    return True";
            let sig = extract_signature(content, Language::Python);
            assert_eq!(sig, "def f(x: int, y: str) -> bool");
            // And the return-type NL extraction can now see the `->` that the
            // old first-colon truncation would have removed.
            let nl = (Language::Python.def().extract_return_nl)(&sig);
            assert_eq!(nl.as_deref(), Some("Returns bool"));
        }

        #[test]
        fn test_python_signature_dict_default_with_colon() {
            // A dict default contains a colon inside braces (depth > 0); the
            // header colon is the one after the closing paren.
            let content = "def g(opts={\"a\": 1, \"b\": 2}) -> int:\n    return 0";
            let sig = extract_signature(content, Language::Python);
            assert_eq!(sig, "def g(opts={\"a\": 1, \"b\": 2}) -> int");
        }

        #[test]
        fn test_python_signature_lambda_default() {
            // A lambda default has a colon inside the parameter parens (depth 1).
            let content = "def h(key=lambda x: x) -> None:\n    pass";
            let sig = extract_signature(content, Language::Python);
            assert_eq!(sig, "def h(key=lambda x: x) -> None");
        }

        #[test]
        fn test_python_signature_string_default_with_colon() {
            // A colon inside a string default must be ignored even though the
            // depth tracking already places it inside parens — this exercises
            // the string-literal skip directly.
            let content = "def j(sep=\"a:b\") -> str:\n    return sep";
            let sig = extract_signature(content, Language::Python);
            assert_eq!(sig, "def j(sep=\"a:b\") -> str");
        }

        #[test]
        fn test_go_signature_stops_at_brace() {
            let content = "func (s *Server) Handle(r Request) error {\n\treturn nil\n}";
            let sig = extract_signature(content, Language::Go);
            assert_eq!(sig, "func (s *Server) Handle(r Request) error");
        }

        #[test]
        fn test_typescript_signature_stops_at_brace() {
            let content = "function processData(input: string): Promise<Result> {\n  return ok;\n}";
            let sig = extract_signature(content, Language::TypeScript);
            assert_eq!(sig, "function processData(input: string): Promise<Result>");
        }

        #[test]
        fn test_ruby_signature_stops_at_newline() {
            let content = "def calculate(x, y)\n  x + y\nend";
            let sig = extract_signature(content, Language::Ruby);
            assert_eq!(sig, "def calculate(x, y)");
        }

        #[test]
        fn test_signature_without_terminator() {
            let content = "fn abstract_decl()";
            let sig = extract_signature(content, Language::Rust);
            assert_eq!(sig, "fn abstract_decl()");
        }
        /// Verifies that extract_signature correctly preserves Unicode characters in SQL VIEW names and stops before the AS keyword.
        /// # Arguments
        /// This is a test function with no parameters.
        /// # Returns
        /// Returns nothing; this is a test assertion function.
        /// # Panics
        /// Panics if the extracted signature does not contain the Unicode character "ß" in "straße" or if it incorrectly includes the "AS" keyword.

        #[test]
        fn extract_signature_until_as_preserves_unicode() {
            let content = "CREATE VIEW stra\u{00DF}e AS SELECT 1";
            let sig = extract_signature(content, Language::Sql);
            assert!(sig.contains("stra\u{00DF}e"));
            assert!(!sig.contains("AS"));
        }

        #[test]
        fn extract_signature_until_as_trailing_as() {
            // A chunk ending exactly in " AS" must stop before the keyword.
            // The old loop bound (`len - 2`) made the `i + 2 >= len` end guard
            // unreachable, so the trailing " AS" was kept as part of the signature.
            let content = "CREATE VIEW v AS";
            let sig = extract_signature(content, Language::Sql);
            assert_eq!(sig, "CREATE VIEW v");
            assert!(!sig.contains("AS"));
        }
    }

    fn write_temp_file(content: &str, ext: &str) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    mod parse_tests {
        use super::*;

        #[test]
        fn test_parse_rust_function() {
            let content = r#"
/// Adds two numbers
fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "add");
            assert_eq!(chunks[0].chunk_type, ChunkType::Function);
            assert!(chunks[0].doc.as_ref().unwrap().contains("Adds two numbers"));
        }

        #[test]
        fn test_parse_rust_method_in_impl() {
            let content = r#"
struct Counter { value: i32 }

impl Counter {
    fn increment(&mut self) {
        self.value += 1;
    }
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let method = chunks.iter().find(|c| c.name == "increment").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        fn test_parse_python_class_method() {
            let content = r#"
class Calculator:
    """A simple calculator."""

    def add(self, a, b):
        """Add two numbers."""
        return a + b
"#;
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
            assert_eq!(class.chunk_type, ChunkType::Class);

            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        fn test_parse_go_method_vs_function() {
            let content = r#"
package main

func standalone() {
    println("standalone")
}

func (s *Server) method() {
    println("method")
}
"#;
            let file = write_temp_file(content, "go");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let standalone = chunks.iter().find(|c| c.name == "standalone").unwrap();
            assert_eq!(standalone.chunk_type, ChunkType::Function);

            let method = chunks.iter().find(|c| c.name == "method").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        fn test_parse_typescript_interface() {
            let content = r#"
interface User {
    name: string;
    age: number;
}
"#;
            let file = write_temp_file(content, "ts");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "User");
            assert_eq!(chunks[0].chunk_type, ChunkType::Interface);
        }

        #[test]
        fn test_parse_c_function() {
            let content = r#"
/* Adds two integers */
int add(int a, int b) {
    return a + b;
}
"#;
            let file = write_temp_file(content, "c");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "add");
            assert_eq!(chunks[0].chunk_type, ChunkType::Function);
            assert!(chunks[0]
                .doc
                .as_ref()
                .unwrap()
                .contains("Adds two integers"));
        }

        #[test]
        fn test_parse_c_struct_and_enum() {
            let content = r#"
struct Point {
    int x;
    int y;
};

enum Color {
    RED,
    GREEN,
    BLUE
};
"#;
            let file = write_temp_file(content, "c");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let point = chunks.iter().find(|c| c.name == "Point").unwrap();
            assert_eq!(point.chunk_type, ChunkType::Struct);

            let color = chunks.iter().find(|c| c.name == "Color").unwrap();
            assert_eq!(color.chunk_type, ChunkType::Enum);
        }

        #[test]
        fn test_parse_java_class_with_method() {
            let content = r#"
public class Calculator {
    /**
     * Adds two numbers
     */
    public int add(int a, int b) {
        return a + b;
    }
}
"#;
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
            assert_eq!(class.chunk_type, ChunkType::Class);

            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert!(method.doc.as_ref().unwrap().contains("Adds two numbers"));
        }

        #[test]
        fn test_parse_java_interface_and_enum() {
            let content = r#"
interface Printable {
    void print();
}

enum Direction {
    NORTH,
    SOUTH,
    EAST,
    WEST
}
"#;
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let iface = chunks.iter().find(|c| c.name == "Printable").unwrap();
            assert_eq!(iface.chunk_type, ChunkType::Interface);

            let dir = chunks.iter().find(|c| c.name == "Direction").unwrap();
            assert_eq!(dir.chunk_type, ChunkType::Enum);
        }

        #[test]
        #[cfg(feature = "lang-csharp")]
        fn test_parse_csharp_class_and_method() {
            let content = r#"
public class Calculator {
    public int Add(int a, int b) {
        return a + b;
    }
}
"#;
            let file = write_temp_file(content, "cs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
            assert_eq!(class.chunk_type, ChunkType::Class);

            let method = chunks.iter().find(|c| c.name == "Add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        #[cfg(feature = "lang-csharp")]
        fn test_parse_csharp_property() {
            let content = r#"
public class Foo {
    public int Value { get; set; }
}
"#;
            let file = write_temp_file(content, "cs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert!(chunks
                .iter()
                .any(|c| c.name == "Value" && c.chunk_type == ChunkType::Property));
        }

        #[test]
        #[cfg(feature = "lang-csharp")]
        fn test_parse_csharp_delegate() {
            let content = "public delegate void OnComplete(int result);";
            let file = write_temp_file(content, "cs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert!(chunks
                .iter()
                .any(|c| c.name == "OnComplete" && c.chunk_type == ChunkType::Delegate));
        }

        #[test]
        #[cfg(feature = "lang-csharp")]
        fn test_parse_csharp_event() {
            let content = r#"
public class Foo {
    public event EventHandler Changed;
}
"#;
            let file = write_temp_file(content, "cs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert!(chunks
                .iter()
                .any(|c| c.name == "Changed" && c.chunk_type == ChunkType::Event));
        }

        #[test]
        #[cfg(feature = "lang-csharp")]
        fn test_parse_csharp_interface_and_enum() {
            let content = r#"
public interface ICalculator {
    int Add(int a, int b);
}

public enum Color { Red, Green, Blue }
"#;
            let file = write_temp_file(content, "cs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert!(chunks
                .iter()
                .any(|c| c.name == "ICalculator" && c.chunk_type == ChunkType::Interface));
            assert!(chunks
                .iter()
                .any(|c| c.name == "Color" && c.chunk_type == ChunkType::Enum));
        }

        #[test]
        #[cfg(feature = "lang-csharp")]
        fn test_parse_csharp_record_maps_to_struct() {
            let content = "public record Person(string Name, int Age);";
            let file = write_temp_file(content, "cs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert!(chunks
                .iter()
                .any(|c| c.name == "Person" && c.chunk_type == ChunkType::Struct));
        }

        #[test]
        #[cfg(feature = "lang-csharp")]
        fn test_parse_csharp_constructor_inferred_constructor() {
            let content = r#"
public class Foo {
    public Foo(int x) { }
}
"#;
            let file = write_temp_file(content, "cs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            // Constructor → Function → inferred to Method → post_process to Constructor
            let ctors: Vec<_> = chunks
                .iter()
                .filter(|c| c.name == "Foo" && c.chunk_type == ChunkType::Constructor)
                .collect();
            assert!(
                !ctors.is_empty(),
                "Constructor should be classified as Constructor"
            );
        }

        #[test]
        #[cfg(feature = "lang-csharp")]
        fn test_parse_csharp_local_function() {
            let content = r#"
public class Foo {
    public void Bar() {
        int Helper(int x) { return x + 1; }
        Helper(5);
    }
}
"#;
            let file = write_temp_file(content, "cs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert!(chunks.iter().any(|c| c.name == "Helper"));
        }

        #[test]
        fn test_parse_rust_macro() {
            let content = r#"
macro_rules! my_macro {
    ($x:expr) => {
        println!("{}", $x);
    };
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "my_macro");
            assert_eq!(chunks[0].chunk_type, ChunkType::Macro);
            assert!(chunks[0].signature.contains("macro_rules! my_macro"));
        }
    }

    mod parent_type_tests {
        use super::*;

        #[test]
        fn test_rust_method_has_parent_type_name() {
            let content = r#"
struct Counter { value: i32 }
impl Counter {
    fn increment(&mut self) { self.value += 1; }
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "increment").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Counter"));
        }

        #[test]
        fn test_rust_trait_method_has_parent_type_name() {
            let content = r#"
trait Drawable {
    fn draw(&self) {}
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "draw").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Drawable"));
        }

        #[test]
        fn test_rust_impl_trait_for_type() {
            let content = r#"
struct Foo;
trait Display { fn fmt(&self) -> String; }
impl Display for Foo {
    fn fmt(&self) -> String { String::new() }
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks
                .iter()
                .find(|c| c.name == "fmt" && c.chunk_type == ChunkType::Method)
                .unwrap();
            // Should extract the target type (Foo), not the trait (Display)
            assert_eq!(method.parent_type_name.as_deref(), Some("Foo"));
        }

        #[test]
        fn test_rust_generic_impl() {
            let content = r#"
struct Container<T> { items: Vec<T> }
impl<T> Container<T> {
    fn push(&mut self, item: T) {}
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "push").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            // Should extract the base type name, not the full generic
            assert_eq!(method.parent_type_name.as_deref(), Some("Container"));
        }

        #[test]
        fn test_python_method_has_parent_type_name() {
            let content = r#"
class Calculator:
    def add(self, a, b):
        return a + b
"#;
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
        }

        #[test]
        fn test_go_method_pointer_receiver() {
            let content = r#"
package main
type Server struct{}
func (s *Server) Handle() {}
"#;
            let file = write_temp_file(content, "go");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "Handle").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Server"));
        }

        #[test]
        fn test_go_method_value_receiver() {
            let content = r#"
package main
type Point struct{ x, y int }
func (p Point) Distance() float64 { return 0.0 }
"#;
            let file = write_temp_file(content, "go");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "Distance").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Point"));
        }

        #[test]
        fn test_js_method_has_parent_type_name() {
            let content = r#"
class Cache {
    get(key) { return this.data[key]; }
}
"#;
            let file = write_temp_file(content, "js");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "get").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Cache"));
        }

        #[test]
        fn test_ts_method_has_parent_type_name() {
            let content = r#"
class TypedCache {
    get(key: string): string { return ""; }
}
"#;
            let file = write_temp_file(content, "ts");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "get").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("TypedCache"));
        }

        #[test]
        fn test_java_method_has_parent_type_name() {
            let content = r#"
public class Calculator {
    public int add(int a, int b) { return a + b; }
}
"#;
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
        }

        #[test]
        fn test_standalone_function_no_parent() {
            let content = "fn standalone() {}";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            assert_eq!(chunks[0].chunk_type, ChunkType::Function);
            assert!(chunks[0].parent_type_name.is_none());
        }
    }
    /// Validates that `extract_name_fallback` correctly identifies function names containing Unicode characters that appear before SQL keywords.
    /// # Arguments
    /// This is a test function with no parameters.
    /// # Returns
    /// Returns nothing (`()`). The function validates behavior through assertions.
    /// # Panics
    /// Panics if the assertion fails, indicating that `extract_name_fallback` did not correctly extract "café_func" from a CREATE FUNCTION statement containing Unicode characters.

    #[test]
    fn extract_name_fallback_with_unicode_before_keyword() {
        let content = "CREATE FUNCTION caf\u{00e9}_func() RETURNS void";
        let name = extract_name_fallback(content);
        assert_eq!(name, Some("caf\u{00e9}_func".to_string()));
    }

    /// Tests for the leading-comment fallback for short chunks plus
    /// blank-line tolerance in the sibling walk. Targets the `truncated_gold`
    /// failure mode catalogued in `evals/audit_r5_failure_modes.py`.
    mod doc_fallback_tests {
        use super::*;

        // ─── line_looks_comment_like (pure unit) ────────────────────────────

        #[test]
        fn comment_like_accepts_common_prefixes() {
            // Each sigil tested under a language that legitimately uses it
            // (per `lang.def().line_comment_prefixes`). Per-language precision
            // means `--` belongs to SQL/Lua/Haskell, `#` to Python/Bash, etc.
            for (line, lang) in [
                ("// rust", Language::Rust),
                ("/// outer doc", Language::Rust),
                ("//! inner doc", Language::Rust),
                ("-- sql", Language::Sql),
                ("# python", Language::Python),
                ("/* block", Language::Rust),
                (" * inner of /** */ block", Language::Rust),
                ("*/ block end", Language::Rust),
                ("<!-- html -->", Language::Html),
                ("<%-- jsp --%>", Language::Aspx),
                ("(* fsharp", Language::FSharp),
            ] {
                assert!(
                    line_looks_comment_like(line, lang),
                    "expected comment-like: {line:?} (lang={lang})"
                );
            }
        }

        #[test]
        fn comment_like_accepts_blank_and_whitespace() {
            assert!(line_looks_comment_like("", Language::Rust));
            assert!(line_looks_comment_like("   ", Language::Rust));
            assert!(line_looks_comment_like("\t  ", Language::Rust));
        }

        #[test]
        fn comment_like_rejects_code() {
            for line in [
                "fn main() {",
                "let x = 5;",
                "CREATE TABLE foo (",
                "def add(a, b):",
                "package main",
            ] {
                assert!(
                    !line_looks_comment_like(line, Language::Rust),
                    "expected NOT comment-like: {line:?}"
                );
            }
        }

        // ─── extract_leading_comment_doc via Parser end-to-end ─────────────

        /// Happy path 1: SQL `CREATE TABLE` with a glued leading `--` comment
        /// block (the canonical `truncated_gold` shape) — the fallback must
        /// surface the comment as `doc`.
        #[test]
        fn sql_table_with_glued_comment_gets_doc() {
            let content = "\
-- Stores per-row metadata for cqs chunks.
-- Indexed by chunk id; bumped on every reindex.
CREATE TABLE metadata (
    chunk_id TEXT PRIMARY KEY,
    value TEXT
);
";
            let file = write_temp_file(content, "sql");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "metadata")
                .expect("metadata chunk should exist");
            let doc = chunk.doc.as_deref().unwrap_or("");
            assert!(
                doc.contains("per-row metadata"),
                "expected leading comment to be captured, got: {doc:?}"
            );
        }

        /// Strict adjacency: a `--` header block separated from the definition
        /// by a blank line is NOT captured by the fallback (blank breaks the
        /// block). The sibling-walk does not surface it either for this SQL
        /// shape, so `doc` is `None`. Pins the deliberate v15 behavior change.
        #[test]
        fn sql_table_with_blank_separated_comment_keeps_doc_none() {
            let content = "\
-- Stores per-row metadata for cqs chunks.

CREATE TABLE metadata (
    chunk_id TEXT PRIMARY KEY
);
";
            let file = write_temp_file(content, "sql");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "metadata")
                .expect("metadata chunk should exist");
            assert!(
                chunk.doc.is_none(),
                "blank-separated header must not be captured under strict adjacency, got: {:?}",
                chunk.doc
            );
        }

        /// Happy path 2: short Rust type alias with adjacent leading
        /// comment — handled by the normal sibling walk; pin it so the
        /// fallback doesn't regress the happy path.
        #[test]
        fn rust_short_type_alias_with_adjacent_comment_gets_doc() {
            let content = "\
// Marker for things resolved at query time.
type ResolvedAt = u32;
";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "ResolvedAt")
                .expect("ResolvedAt chunk should exist");
            assert!(chunk.doc.is_some(), "expected doc for short type alias");
        }

        /// Sad path 1: short chunk follows arbitrary code (no leading
        /// comment). The fallback MUST NOT pull in random preceding code.
        #[test]
        fn short_chunk_following_code_keeps_doc_none() {
            let content = "\
fn unrelated() {
    let x = 5;
    println!(\"{x}\");
}
type Tiny = u8;
";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "Tiny")
                .expect("Tiny chunk should exist");
            assert!(
                chunk.doc.is_none(),
                "fallback must not capture non-comment-like preceding lines, got: {:?}",
                chunk.doc
            );
        }

        /// Sad path 2: short chunk at the very top of a file (no preceding
        /// source at all). Must not panic and must not invent a doc.
        #[test]
        fn short_chunk_at_top_of_file_keeps_doc_none() {
            let content = "type AtTop = u8;\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "AtTop")
                .expect("AtTop chunk should exist");
            assert!(chunk.doc.is_none());
        }

        /// A LONG Rust chunk with a glued leading plain-comment block gets a
        /// `doc` (here via the sibling walk's generic `comment` doc-node). The
        /// SQL-specific fallback path for long chunks is pinned separately by
        /// `long_sql_chunk_gets_leading_comment_via_fallback`.
        #[test]
        fn long_rust_chunk_with_leading_comment_gets_doc() {
            let content = "\
// Recomputes the running balance, applying reversals and holds.
// Idempotent: safe to call twice for the same account.
fn recompute_balance() {
    let a = 1;
    let b = 2;
    let c = 3;
    let d = 4;
    let e = 5;
    let _ = a + b + c + d + e;
}
";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "recompute_balance")
                .expect("recompute_balance chunk should exist");
            // Well past the old 5-line short-chunk gate.
            assert!(
                chunk.line_end - chunk.line_start >= 5,
                "chunk must be a long chunk to pin the long-chunk fix"
            );
            let doc = chunk.doc.as_deref().unwrap_or("");
            assert!(
                doc.contains("running balance"),
                "long chunk with glued leading comment must get fallback doc, got: {doc:?}"
            );
        }

        /// Sad path 3: long chunk with NO leading comment must still get no
        /// fallback `doc` — the fallback only fires on a real comment block.
        #[test]
        fn long_chunk_without_comment_keeps_doc_none() {
            let content = "\
fn longish() {
    let a = 1;
    let b = 2;
    let c = 3;
    let d = 4;
    let e = 5;
    let _ = a + b + c + d + e;
}
";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "longish")
                .expect("longish chunk should exist");
            assert!(
                chunk.doc.is_none(),
                "long chunk without a leading comment must have no fallback doc, got: {:?}",
                chunk.doc
            );
        }

        /// Sad path 4: UTF-8 multi-byte character in the leading comment
        /// must not crash the byte-prefix slice.
        #[test]
        fn fallback_handles_utf8_in_leading_comment() {
            let content = "\
-- caf\u{00e9} table — s\u{00e9}rieux
CREATE TABLE notes (id TEXT);
";
            let file = write_temp_file(content, "sql");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "notes")
                .expect("notes chunk should exist");
            let doc = chunk.doc.as_deref().unwrap_or("");
            assert!(
                doc.contains("caf\u{00e9}"),
                "UTF-8 should round-trip through the fallback, got: {doc:?}"
            );
        }

        /// Sad path 5: chunk with explicit doc comment from tree-sitter must
        /// surface that doc verbatim. The fallback only runs when
        /// `extract_doc_comment` returns None (`or_else` short-circuits), so
        /// confirming the explicit `///` content reaches the chunk
        /// demonstrates the fallback didn't replace it.
        #[test]
        fn explicit_doc_comment_takes_precedence_over_fallback() {
            let content = "\
/// real doc for the alias
type WithDoc = u8;
";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "WithDoc")
                .expect("WithDoc chunk should exist");
            let doc = chunk.doc.as_deref().unwrap_or("");
            assert!(
                doc.contains("real doc"),
                "expected explicit /// doc to reach chunk, got: {doc:?}"
            );
        }

        // ─── line_looks_comment_like (attribute / preprocessor) ───────────

        #[test]
        fn line_looks_comment_like_rejects_attribute_and_preprocessor_lines() {
            for line in [
                "#[derive(Debug, Clone)]",
                "#[serde(rename = \"foo\")]",
                "#[cfg(test)]",
                "#[allow(dead_code)]",
                "#include <stdio.h>",
                "#define MAX 100",
                "#pragma once",
                "#region Helpers",
                "#endregion",
                "#![feature(let_chains)]",
                "*ptr = 0;",
                "*x = y * z;",
            ] {
                assert!(
                    !line_looks_comment_like(line, Language::Rust),
                    "expected NOT comment-like: {line:?}"
                );
            }
        }

        #[test]
        fn line_looks_comment_like_still_accepts_real_comment_prefixes() {
            // Rust-specific real comment prefixes — per-language list is
            // `&["//", "/*"]` plus inner-`*` of `/** */` blocks.
            for line in [
                "// rust",
                "/// outer doc",
                "/* block",
                " * inner of /** */ block",
                "* ",
                "*",
            ] {
                assert!(
                    line_looks_comment_like(line, Language::Rust),
                    "expected comment-like: {line:?}"
                );
            }
            // `#` belongs to Python/Bash/Ruby, not Rust.
            for line in ["# python comment", "# ", "#"] {
                assert!(
                    line_looks_comment_like(line, Language::Python),
                    "expected comment-like in Python: {line:?}"
                );
            }
        }

        #[test]
        fn rust_short_struct_after_derive_keeps_doc_none() {
            let content = "#[derive(Debug)]\nstruct Tiny { a: u32 }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "Tiny")
                .expect("Tiny chunk");
            assert!(
                chunk.doc.is_none(),
                "fallback must not capture #[derive] line as doc, got: {:?}",
                chunk.doc
            );
        }

        #[test]
        fn c_short_function_after_include_keeps_doc_none() {
            let content = "#include <stdio.h>\n#define X 1\nint main(void) { return 0; }\n";
            let file = write_temp_file(content, "c");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            if let Some(chunk) = chunks.iter().find(|c| c.name == "main") {
                assert!(
                    chunk.doc.is_none(),
                    "fallback must not capture #include/#define as doc, got: {:?}",
                    chunk.doc
                );
            }
        }

        // ─── per-language line_comment_prefixes ────────────────────────────

        /// Pin that the registry-driven prefix list is populated for the
        /// languages most likely to surface in eval data. A language landing
        /// without prefixes silently falls back to the global heuristic;
        /// common languages get per-language precision.
        #[test]
        fn line_comment_prefixes_populated_for_common_languages() {
            for lang in [
                Language::Rust,
                Language::Python,
                Language::Sql,
                Language::Java,
                Language::Go,
                Language::Bash,
                Language::Yaml,
            ] {
                assert!(
                    !lang.def().line_comment_prefixes.is_empty(),
                    "expected {lang} to populate line_comment_prefixes",
                );
            }
        }

        /// Pinned decision: `#` is NOT comment-like in Rust. Per-language
        /// precision means `#` belongs to Python/Bash/Ruby/Yaml; in Rust it is
        /// an attribute (`#[derive]`) or shebang prefix.
        #[test]
        fn python_line_comment_does_not_match_in_rust_context() {
            assert!(
                !line_looks_comment_like("# python comment", Language::Rust),
                "`#` must not classify as comment-like in Rust",
            );
            assert!(
                line_looks_comment_like("# python comment", Language::Python),
                "`#` must classify as comment-like in Python",
            );
        }

        // ─── strict adjacency: internal blank breaks the block ─────────────

        /// A blank line WITHIN the leading comment run terminates the block:
        /// only the glued run (`comment 3`) is captured; the pre-blank comments
        /// (`comment 1`/`2`) are NOT. Inverts the old blank-tolerance behavior
        /// removed in v15.
        #[test]
        fn fallback_stops_at_internal_blank_gap() {
            let content = "\
-- header comment 1
-- header comment 2

-- header comment 3
CREATE TABLE x (id TEXT);
";
            let file = write_temp_file(content, "sql");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks.iter().find(|c| c.name == "x").expect("x chunk");
            let doc = chunk.doc.as_deref().unwrap_or("");
            assert!(
                doc.contains("header comment 3"),
                "glued comment 3 must be captured, got: {doc:?}"
            );
            assert!(
                !doc.contains("header comment 1") && !doc.contains("header comment 2"),
                "pre-blank comments must NOT survive the internal blank gap, got: {doc:?}"
            );
        }

        // ─── bound the bounded-walk-back's correctness ─────────────────────

        /// FALLBACK_DOC_MAX_LINES cap: feed a 12-line `--` comment block
        /// before a short SQL chunk. Only the last 8 lines must end up in
        /// the captured doc — earlier lines are out of budget.
        ///
        /// Uses unique tokens (`mark-NN-tail`) per line so substring matching
        /// can't false-positive (`"line 1"` would otherwise match `"line 11"`).
        #[test]
        #[allow(non_snake_case)]
        fn fallback_caps_at_FALLBACK_DOC_MAX_LINES() {
            let mut content = String::new();
            for i in 0..12 {
                content.push_str(&format!("-- mark-{i:02}-tail\n"));
            }
            content.push_str("CREATE TABLE capped (id TEXT);\n");
            let file = write_temp_file(&content, "sql");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "capped")
                .expect("capped chunk");
            let doc = chunk.doc.as_deref().unwrap_or("");
            // The eight most-recent comment lines must be present.
            for i in 4..12 {
                assert!(
                    doc.contains(&format!("mark-{i:02}-tail")),
                    "expected mark-{i:02}-tail in doc, got: {doc:?}"
                );
            }
            // Earlier lines must NOT be present (over budget).
            for i in 0..4 {
                assert!(
                    !doc.contains(&format!("mark-{i:02}-tail")),
                    "mark-{i:02}-tail must NOT be in doc (cap = {FALLBACK_DOC_MAX_LINES}), got: {doc:?}"
                );
            }
        }

        /// A 5-line CREATE TABLE with a glued `--` header still gets the
        /// fallback. Formerly a boundary case for the removed short-chunk gate;
        /// now just a length-agnostic happy path.
        #[test]
        fn fallback_runs_for_chunk_spanning_exactly_5_lines() {
            // 5-line body so `line_end - line_start == 4`. Three columns +
            // CREATE line + closing `)` line = 5 lines total.
            let content = "\
-- doc for 5-line table
CREATE TABLE five_line (
    id TEXT,
    name TEXT,
    note TEXT
);
";
            let file = write_temp_file(content, "sql");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "five_line")
                .expect("five_line chunk");
            assert_eq!(
                chunk.line_end - chunk.line_start,
                4,
                "chunk must span exactly 5 lines (line_end - line_start == 4) to pin the boundary"
            );
            assert!(
                chunk
                    .doc
                    .as_deref()
                    .unwrap_or("")
                    .contains("doc for 5-line table"),
                "5-line chunk must trigger the fallback, got doc: {:?}",
                chunk.doc
            );
        }

        /// Empty-only-comment-marker-lines case: a leading block of three
        /// bare `//` lines (each a valid comment marker but with no content)
        /// must result in the captured fallback being either `None` or
        /// containing only the bare markers — never an empty/whitespace
        /// `doc`. The `captured.trim().is_empty()` early-return guards this
        /// for the fallback path; the sibling-walk may surface markers
        /// themselves, which is also acceptable.
        #[test]
        fn fallback_skips_when_only_comment_marker_lines_precede() {
            let content = "//\n//\n//\ntype X = u8;\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks.iter().find(|c| c.name == "X").expect("X chunk");
            if let Some(doc) = chunk.doc.as_deref() {
                assert!(
                    doc.trim().is_empty() || doc.contains("//"),
                    "doc should be empty/None or contain only the bare markers, got: {doc:?}"
                );
            }
        }

        /// Mid-UTF-8 start_byte safety: a leading comment containing
        /// multi-byte chars must round-trip the byte slice without panic
        /// and without mid-codepoint truncation. Pins the
        /// `source.get(..start_byte)?` boundary contract end-to-end and
        /// covers the new reverse-walk-by-bytes tail computation.
        #[test]
        fn fallback_handles_mid_utf8_in_preceding_comment() {
            // Multi-byte chars in the leading comment exercise byte offsets
            // that don't align with character boundaries; `prefix.lines()`
            // must still split correctly and the captured doc must contain
            // the multi-byte codepoint intact.
            let content = "// caf\u{00e9} \u{1f389} short\ntype WithEmoji = u8;\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "WithEmoji")
                .expect("WithEmoji chunk");
            let doc = chunk.doc.as_deref().unwrap_or("");
            assert!(
                doc.contains("caf\u{00e9}"),
                "multi-byte char must round-trip, got: {doc:?}"
            );
            assert!(
                doc.contains("\u{1f389}"),
                "multi-byte emoji must round-trip, got: {doc:?}"
            );
        }

        /// Mixed-prefix walk: `line_looks_comment_like` consults
        /// `lang.def().line_comment_prefixes` rather than a global union, so a
        /// Rust chunk preceded by SQL-style `--` comments does not walk past
        /// the `--` line. Pin this so a permissive rewrite surfaces as a loud
        /// diff.
        #[test]
        fn fallback_does_not_mix_comment_styles() {
            // Under Rust (`line_comment_prefixes: &["//", "/*"]`), the `--`
            // line is NOT comment-like — the walk stops there. The `//`
            // sibling gap between the `//` header and `type` is not surfaced
            // because the walk's contiguous comment run is broken.
            let content = "// rust-style header\n-- sql-style header\ntype Mixed = u8;\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "Mixed")
                .expect("Mixed chunk");
            let doc = chunk.doc.as_deref().unwrap_or("");
            assert!(
                !doc.contains("sql-style header"),
                "per-language walk must NOT capture `--` line in Rust context, got: {doc:?}"
            );
        }

        // ─── smoke test that walk-back stays linear in chunk count ─────────

        /// Smoke test for the bounded walk-back: parsing 200 short CREATE
        /// TABLE chunks in one file must not split the entire prefix per call.
        /// We don't assert wall-clock here (CI flakiness), but we do assert we
        /// get the expected chunk count without panic and without OOM. An
        /// unbounded walk-back would be ~200 vec allocations of growing size
        /// (chunk N walked N×~3 lines of prefix); the bounded walk caps each
        /// at the FALLBACK_DOC_MAX_LINES tail.
        #[test]
        fn parses_200_short_chunks_in_one_file_without_quadratic_blowup() {
            let mut content = String::new();
            for i in 0..200 {
                // Each chunk preceded by a leading comment so the fallback
                // path actually fires.
                content.push_str(&format!("-- doc for table_{i}\n"));
                content.push_str(&format!("CREATE TABLE table_{i} (id TEXT);\n"));
            }
            let file = write_temp_file(&content, "sql");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let table_chunks: Vec<_> = chunks
                .iter()
                .filter(|c| c.name.starts_with("table_"))
                .collect();
            assert_eq!(
                table_chunks.len(),
                200,
                "expected 200 table chunks, got {}",
                table_chunks.len()
            );
            // Verify the fallback delivered a doc to a representative late
            // chunk — confirms the bounded walk-back still finds the
            // immediate leading comment.
            let last = table_chunks
                .iter()
                .find(|c| c.name == "table_199")
                .expect("table_199 chunk");
            assert!(
                last.doc
                    .as_deref()
                    .unwrap_or("")
                    .contains("doc for table_199"),
                "bounded walk-back must still find immediate leading comment, got: {:?}",
                last.doc
            );
        }

        // ─── #2126: leading-comment fallback across languages & guards ─────

        /// The fallback fires for a LONG SQL chunk (past the old 5-line gate)
        /// whose leading `--` header the sibling walk misses. This is the core
        /// long-chunk fix: RED before v15 (the fallback was short-only, so the
        /// header was dropped and the embedding lost the stated intent).
        #[test]
        fn long_sql_chunk_gets_leading_comment_via_fallback() {
            let content = "\
-- Ledger of every posted transaction, one row per entry.
CREATE TABLE ledger (
    id TEXT PRIMARY KEY,
    account TEXT,
    amount INTEGER,
    balance INTEGER,
    posted_at TEXT,
    memo TEXT
);
";
            let file = write_temp_file(content, "sql");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "ledger")
                .expect("ledger chunk should exist");
            assert!(
                chunk.line_end - chunk.line_start >= 5,
                "ledger must be a long chunk (span {})",
                chunk.line_end - chunk.line_start
            );
            assert!(
                chunk
                    .doc
                    .as_deref()
                    .unwrap_or("")
                    .contains("Ledger of every posted transaction"),
                "long SQL chunk must get its glued header as doc, got: {:?}",
                chunk.doc
            );
        }

        /// Per-language leading plain-comment block populates `doc`. `#` for
        /// Python/Ruby/Bash, `//` and `/* */` for the C family. (Most of these
        /// go through the sibling walk's generic `comment` doc-node; pinned here
        /// so a change to either the walk or the fallback surfaces loudly.)
        #[test]
        fn leading_plain_comment_populates_doc_per_language() {
            let cases: &[(&str, &str, &str, &str)] = &[
                (
                    "py",
                    "# Frobnicates the widget thoroughly.\ndef frob():\n    return 1\n",
                    "frob",
                    "Frobnicates the widget",
                ),
                (
                    "rb",
                    "# Computes the tax owed on the order.\ndef tax\n  0\nend\n",
                    "tax",
                    "Computes the tax owed",
                ),
                (
                    "sh",
                    "# Deploys the release artifact to prod.\ndeploy() {\n  echo hi\n}\n",
                    "deploy",
                    "Deploys the release artifact",
                ),
                (
                    "c",
                    "// Computes the CRC over the frame.\nint crc(void) { return 0; }\n",
                    "crc",
                    "Computes the CRC",
                ),
                (
                    "c",
                    "/* Validates the incoming request body. */\nint validate(void) { return 0; }\n",
                    "validate",
                    "Validates the incoming request",
                ),
            ];
            for (ext, content, name, needle) in cases {
                let file = write_temp_file(content, ext);
                let parser = Parser::new().unwrap();
                let chunks = parser.parse_file(file.path()).unwrap();
                let chunk = chunks
                    .iter()
                    .find(|c| c.name == *name)
                    .unwrap_or_else(|| panic!("{name} chunk should exist for .{ext}"));
                assert!(
                    chunk.doc.as_deref().unwrap_or("").contains(needle),
                    "{ext} {name}: expected doc to contain {needle:?}, got: {:?}",
                    chunk.doc
                );
            }
        }

        /// Doc-syntax precedence: a Python docstring (no leading comment)
        /// populates `doc` via the body-docstring path; the leading-comment
        /// fallback never runs to override it.
        #[test]
        fn python_docstring_populates_doc_over_fallback() {
            let content = "\
def documented():
    \"\"\"The real docstring for documented.\"\"\"
    return 1
";
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "documented")
                .expect("documented chunk");
            let doc = chunk.doc.as_deref().unwrap_or("");
            assert!(
                doc.contains("The real docstring for documented"),
                "docstring must populate doc, got: {doc:?}"
            );
        }

        /// A shebang alone yields no `doc`; a shebang followed by a real
        /// description yields only the description; a shebang + encoding
        /// modeline yields no `doc`. Covers both the sibling walk and fallback.
        #[test]
        fn shebang_and_encoding_lines_never_become_doc() {
            let shebang_only = "#!/usr/bin/env python\ndef main():\n    return 1\n";
            let shebang_desc =
                "#!/usr/bin/env python\n# Runs the batch job end to end.\ndef main():\n    return 1\n";
            let shebang_encoding =
                "#!/usr/bin/env python\n# -*- coding: utf-8 -*-\ndef main():\n    return 1\n";

            let doc_of = |content: &str| -> Option<String> {
                let file = write_temp_file(content, "py");
                let parser = Parser::new().unwrap();
                let chunks = parser.parse_file(file.path()).unwrap();
                chunks
                    .iter()
                    .find(|c| c.name == "main")
                    .and_then(|c| c.doc.clone())
            };

            assert!(
                doc_of(shebang_only).is_none(),
                "shebang alone must not be doc, got: {:?}",
                doc_of(shebang_only)
            );
            let desc = doc_of(shebang_desc).unwrap_or_default();
            assert!(
                desc.contains("Runs the batch job") && !desc.contains("#!"),
                "shebang must be dropped, description kept, got: {desc:?}"
            );
            assert!(
                doc_of(shebang_encoding).is_none(),
                "shebang + encoding modeline must not be doc, got: {:?}",
                doc_of(shebang_encoding)
            );
        }

        /// Negative space for the shebang/modeline filter: an ordinary line
        /// comment that merely MENTIONS a modeline word (`encoding:`, `vim:`,
        /// `-*-`) in its prose — not head-anchored as a real modeline —
        /// SURVIVES as `doc`. RED against a substring-containment matcher
        /// (every line comment starts with a marker, so containment drops these
        /// as false modelines, and would even drop a doc line that happens to
        /// contain `-*-`).
        #[test]
        fn line_comment_with_modeline_words_in_prose_survives() {
            let doc_of = |content: &str, ext: &str, name: &str| -> Option<String> {
                let file = write_temp_file(content, ext);
                let parser = Parser::new().unwrap();
                let chunks = parser.parse_file(file.path()).unwrap();
                chunks
                    .iter()
                    .find(|c| c.name == name)
                    .and_then(|c| c.doc.clone())
            };

            // Rust `//` glued above a def: prose mentioning `encoding:`.
            let rust = "// Fix encoding: normalize input to NFC before hashing.\nfn normalize() -> u32 {\n    0\n}\n";
            let d = doc_of(rust, "rs", "normalize").unwrap_or_default();
            assert!(
                d.contains("normalize input to NFC"),
                "prose mentioning `encoding:` must not be dropped as a modeline, got: {d:?}"
            );

            // Python `#` glued above a def: prose mentioning `vim:`.
            let py = "# Configure the vim: plugin loader for editor integration.\ndef setup():\n    return 1\n";
            let d = doc_of(py, "py", "setup").unwrap_or_default();
            assert!(
                d.contains("plugin loader"),
                "prose mentioning `vim:` must not be dropped as a modeline, got: {d:?}"
            );

            // Rust `//` prose containing `-*-` as a drawn separator.
            let sep = "// Draws a separator -*-*-*- across the report header.\nfn banner() -> u32 {\n    0\n}\n";
            let d = doc_of(sep, "rs", "banner").unwrap_or_default();
            assert!(
                d.contains("Draws a separator"),
                "prose containing `-*-` must not be dropped as a modeline, got: {d:?}"
            );
        }

        /// A comment block between two definitions attaches to the FOLLOWING
        /// definition only, never the preceding one.
        #[test]
        fn comment_between_defs_attaches_to_following_only() {
            let content = "\
fn earlier() {
    let x = 1;
}
// doc for later only
fn later() {
    let y = 2;
}
";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let earlier = chunks
                .iter()
                .find(|c| c.name == "earlier")
                .expect("earlier");
            let later = chunks.iter().find(|c| c.name == "later").expect("later");
            assert!(
                earlier.doc.as_deref().unwrap_or("").is_empty()
                    || !earlier
                        .doc
                        .as_deref()
                        .unwrap_or("")
                        .contains("doc for later"),
                "comment must not attach to the preceding def, got: {:?}",
                earlier.doc
            );
            assert!(
                later.doc.as_deref().unwrap_or("").contains("doc for later"),
                "comment must attach to the following def, got: {:?}",
                later.doc
            );
        }

        /// A realistic file-top license header (banner + blank + imports +
        /// blank + first def) does NOT become the first def's `doc`: the
        /// intervening `use` stops the sibling walk and the blank stops the
        /// fallback. Pins the banner-safety intent for the common license shape.
        #[test]
        fn file_top_license_header_does_not_become_first_def_doc() {
            let content = "\
// Copyright 2026 The cqs Authors.
// Licensed under the MIT License.

use std::fmt;

pub fn first_function() -> u32 {
    0
}
";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let chunk = chunks
                .iter()
                .find(|c| c.name == "first_function")
                .expect("first_function chunk");
            let doc = chunk.doc.as_deref().unwrap_or("");
            assert!(
                !doc.contains("Copyright") && !doc.contains("Licensed"),
                "license banner must not leak onto the first def, got: {doc:?}"
            );
        }

        /// Unit-level oracle for [`comment_is_license_banner`]: the classic
        /// banner shapes observed in real corpora (SPDX tags, `Copyright …`,
        /// `Licensed under …`, MIT/BSD body openers, `All rights reserved`,
        /// block-comment `/* * SPDX */`, decorative `//-` / `====` leaders) are
        /// detected; ordinary docs — including ones that MENTION "license" or
        /// "copyright" mid-prose or on a later line — are not.
        #[test]
        fn license_banner_classifier_head_anchored() {
            // Positive: real file-top banners.
            for banner in [
                "// Copyright 2018 Ulf Adams",
                "// Copyright (c) The nextest Contributors",
                "// Copyright © 2016-2018 winapi-rs developers",
                "// SPDX-License-Identifier: MIT OR Apache-2.0",
                "/*\n * SPDX-FileCopyrightText: Copyright (c) 2024, NVIDIA CORPORATION.\n * SPDX-License-Identifier: Apache-2.0\n */",
                "// Licensed under the Apache License, Version 2.0",
                "// Redistribution and use in source and binary forms",
                "// Permission is hereby granted, free of charge",
                "// This file is part of ICU4X. For terms of use, see LICENSE",
                "// All rights reserved.",
                "//-\n// Copyright 2017, 2018 The proptest developers", // decorative leader
                "// =====================\n// Copyright 2020 Foo",       // separator rule
                "-- Copyright 2020 The SQL Authors",                     // `--` comment lang
                "# Copyright 2020 The Python Authors",                   // `#` comment lang
            ] {
                assert!(
                    comment_is_license_banner(banner),
                    "expected banner, got miss: {banner:?}"
                );
            }

            // Negative: legitimate definition docs. A mid-prose mention of
            // "license"/"copyright", or a banner word on a SECOND line after a
            // real first content line, must NOT be classified as a banner.
            for doc in [
                "/// Validates the license key format before activation.",
                "/// Returns the copyright holder recorded in the manifest.",
                "// Parses the input file.\n// Copyright info is preserved in the output.",
                "/// License-compatible: this is a permissive re-implementation.",
                "-- Users table. See LICENSE at repo root for terms.",
                "/// Computes the SPDX identifier for a detected license.",
                "// Fix encoding: normalize input to NFC before hashing.",
                "/// The registration copyright year, e.g. 2026.",
            ] {
                assert!(
                    !comment_is_license_banner(doc),
                    "expected NOT banner (legit doc), got hit: {doc:?}"
                );
            }
        }

        /// The #2205 leak: a license banner glued DIRECTLY above the file's
        /// first definition (no intervening `use`, no blank) must NOT become
        /// that def's `doc`. RED before the banner-boundary fix — the sibling
        /// walk collected the `//` banner lines (Rust `//` is a doc-node) and
        /// returned them as `first_glued`'s doc. Covers the glued case and the
        /// blank-separated-within-tolerance case the walk's blank tolerance
        /// (`MAX_TOLERATED_BLANK_SIBLINGS`) admits.
        #[test]
        fn banner_glued_to_first_def_does_not_leak() {
            let doc_of = |content: &str, ext: &str, name: &str| -> Option<String> {
                let file = write_temp_file(content, ext);
                let parser = Parser::new().unwrap();
                let chunks = parser.parse_file(file.path()).unwrap();
                chunks
                    .iter()
                    .find(|c| c.name == name)
                    .and_then(|c| c.doc.clone())
            };

            // Glued: banner immediately above the first def.
            let glued = "\
// Copyright 2026 The cqs Authors.
// SPDX-License-Identifier: MIT
pub fn first_glued() -> u32 {
    0
}
";
            let d = doc_of(glued, "rs", "first_glued").unwrap_or_default();
            assert!(
                !d.contains("Copyright") && !d.contains("SPDX"),
                "glued banner must not leak onto first def, got: {d:?}"
            );

            // Blank-separated within the sibling walk's blank tolerance.
            let spaced = "\
// Copyright 2026 The cqs Authors.
// Licensed under the Apache License, Version 2.0

pub fn first_spaced() -> u32 {
    0
}
";
            let d = doc_of(spaced, "rs", "first_spaced").unwrap_or_default();
            assert!(
                !d.contains("Copyright") && !d.contains("Licensed"),
                "blank-separated banner must not leak onto first def, got: {d:?}"
            );

            // C block/line-comment banner glued to the first def.
            let c = "\
// Copyright (c) 2015, Google Inc.
// SPDX-License-Identifier: ISC
int first_c(void) {
    return 0;
}
";
            let d = doc_of(c, "c", "first_c").unwrap_or_default();
            assert!(
                !d.contains("Copyright") && !d.contains("SPDX"),
                "C banner must not leak onto first def, got: {d:?}"
            );
        }

        /// Composite (the `nextest` / aws-lc shape): a license banner followed
        /// by a REAL doc that belongs to the first def. The banner is stripped
        /// but the real doc is KEPT — the walk breaks at the banner, retaining
        /// the doc-comment lines already collected between it and the def.
        #[test]
        fn banner_above_real_doc_keeps_only_the_real_doc() {
            let doc_of = |content: &str, ext: &str, name: &str| -> Option<String> {
                let file = write_temp_file(content, ext);
                let parser = Parser::new().unwrap();
                let chunks = parser.parse_file(file.path()).unwrap();
                chunks
                    .iter()
                    .find(|c| c.name == name)
                    .and_then(|c| c.doc.clone())
            };

            // Rust: `///` doc below a `//` banner.
            let rust = "\
// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
/// Documented exit codes for the runner.
pub enum ExitCode {
    Ok,
}
";
            let d = doc_of(rust, "rs", "ExitCode").unwrap_or_default();
            assert!(
                d.contains("Documented exit codes for the runner"),
                "real doc must survive below the banner, got: {d:?}"
            );
            assert!(
                !d.contains("Copyright") && !d.contains("SPDX"),
                "banner must be stripped from the kept doc, got: {d:?}"
            );

            // C: descriptive `//` line below a `//` banner (aws-lc shape).
            let c = "\
// Copyright (c) 2015, Google Inc.
// SPDX-License-Identifier: ISC
// Ensure we can't call OPENSSL_malloc circularly.
int first_c(void) {
    return 0;
}
";
            let d = doc_of(c, "c", "first_c").unwrap_or_default();
            assert!(
                d.contains("Ensure we can't call OPENSSL_malloc"),
                "real description must survive below the banner, got: {d:?}"
            );
            assert!(
                !d.contains("Copyright") && !d.contains("SPDX"),
                "banner must be stripped from the kept description, got: {d:?}"
            );
        }

        /// Negative space for the banner filter: a legitimate first-def doc that
        /// merely MENTIONS "License" mid-prose (not head-anchored as a banner)
        /// SURVIVES intact. RED against a substring-containment banner matcher.
        #[test]
        fn first_def_doc_mentioning_license_in_prose_survives() {
            let doc_of = |content: &str, ext: &str, name: &str| -> Option<String> {
                let file = write_temp_file(content, ext);
                let parser = Parser::new().unwrap();
                let chunks = parser.parse_file(file.path()).unwrap();
                chunks
                    .iter()
                    .find(|c| c.name == name)
                    .and_then(|c| c.doc.clone())
            };

            let rust = "\
/// Validates a license key against the vendor allowlist.
/// The copyright holder is recorded alongside for audit.
pub fn validate_license() -> bool {
    true
}
";
            let d = doc_of(rust, "rs", "validate_license").unwrap_or_default();
            assert!(
                d.contains("Validates a license key") && d.contains("copyright holder is recorded"),
                "legit doc mentioning license/copyright must survive, got: {d:?}"
            );

            // SQL `--` header (the #2126 glued-header shape, the common case)
            // that MENTIONS "license" in prose: the banner filter must not drop
            // it. Glued with no blank, so v15's fallback captures it — the only
            // way it can go missing is a wrong banner classification.
            let sql = "\
-- Accounts table. See the LICENSE file for redistribution terms.
CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
    name TEXT
);
";
            let d = doc_of(sql, "sql", "accounts").unwrap_or_default();
            assert!(
                d.contains("Accounts table") && d.contains("LICENSE file"),
                "glued non-banner SQL header mentioning license must survive, got: {d:?}"
            );
        }
    }

    /// Stack-safety: the tree walkers must be iterative so a pathologically
    /// deep parse tree (minified code, thousands of nested parens) doesn't
    /// overflow a (2 MiB) rayon worker stack and SIGSEGV the indexer.
    mod deep_tree_stack_safety {
        use super::*;

        /// Parse `source` with `lang`'s grammar and return its tree, keeping
        /// the source alive alongside so the caller can borrow node text.
        fn parse_raw(source: &str, lang: Language) -> tree_sitter::Tree {
            let grammar = lang.try_grammar().expect("grammar available");
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&grammar).expect("set language");
            parser.parse(source, None).expect("parse succeeds")
        }

        #[test]
        fn collect_comment_ranges_no_overflow_on_deep_tree() {
            // 50k nested parens around a literal, plus a trailing comment so the
            // walker has at least one range to collect. Well within the 1 MiB
            // default size cap; generation is a cheap string build.
            let depth = 50_000;
            let mut src = String::with_capacity(depth * 2 + 64);
            src.push_str("x = ");
            for _ in 0..depth {
                src.push('(');
            }
            src.push('1');
            for _ in 0..depth {
                src.push(')');
            }
            src.push_str("  # trailing comment\n");

            let tree = parse_raw(&src, Language::Python);
            let root = tree.root_node();
            let mut ranges: Vec<(usize, usize)> = Vec::new();
            // Must not stack-overflow. A recursive walk at this depth would.
            collect_comment_ranges(root, root.start_byte(), src.len(), &mut ranges);
            assert!(
                !ranges.is_empty(),
                "the trailing comment range should be collected"
            );
        }

        #[test]
        fn find_type_identifier_no_overflow_on_deep_tree() {
            // Same deep-paren tree, no type_identifier present: the walker must
            // exhaust the whole tree iteratively and return None without
            // overflowing.
            let depth = 50_000;
            let mut src = String::with_capacity(depth * 2 + 16);
            src.push_str("x = ");
            for _ in 0..depth {
                src.push('(');
            }
            src.push('1');
            for _ in 0..depth {
                src.push(')');
            }
            src.push('\n');

            let tree = parse_raw(&src, Language::Python);
            let root = tree.root_node();
            // No type_identifier in a Python expression tree → exhaustive walk.
            let found = find_type_identifier_recursive(root, &src);
            assert!(found.is_none());
        }

        #[test]
        fn find_type_identifier_returns_shallowest_leftmost() {
            // Pre-order semantics preserved after the iterative rewrite: a Go
            // pointer receiver `(r *Server)` yields the inner type_identifier.
            let src = "package p\nfunc (r *Server) Handle() {}\n";
            let tree = parse_raw(src, Language::Go);
            // Walk down to the receiver parameter type and confirm extraction.
            let name = find_type_identifier_recursive(tree.root_node(), src);
            assert_eq!(name.as_deref(), Some("Server"));
        }
    }
}
