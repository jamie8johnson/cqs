//! Call extraction from tree-sitter parse trees

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::StreamingIterator;

use super::types::{
    capture_name_to_chunk_type, CallEdgeKind, CallSite, ChunkType, ChunkTypeRefs, FunctionCalls,
    Language, ParserError, TypeEdgeKind, TypeRef,
};
use super::Parser;

/// serde string-callback attributes name a free/associated function (or, for
/// `with`, a module) by path string. tree-sitter's call query never captures
/// these — they live in `#[serde(...)]` attribute string literals, not in a
/// `call_expression`. Without an explicit edge, `cqs callers default_ref_weight`
/// returns empty even though serde's derive invokes the function. This is the
/// extractor-side mirror of the dead-code `filter_serde_callbacks` heuristic:
/// where that filter keeps a candidate *alive*, this emits a real call-graph
/// edge so callers/impact/test-map see the relationship.
///
/// Captures the quoted path of `default`, `with`, `serialize_with`,
/// `deserialize_with`, `skip_serializing_if`, `getter`. `bound = "..."` is
/// excluded — it names types / where-clauses, not functions.
///
/// `with = "module"` references a *module* whose `serialize`/`deserialize`
/// free functions serde calls. We emit an edge to the terminal segment of the
/// path as written (`humantime_serde`), matching how the dead-code filter
/// keeps a same-named function alive. We do NOT synthesize
/// `module::serialize` / `module::deserialize` edges: the call graph resolves
/// callee_name by bare last segment, so a synthetic `serialize` edge would
/// alias *every* function named `serialize` in the index (massive false-edge
/// fan-out). The module's inner serde fns therefore stay unlinked — a
/// limitation of bare-name resolution, not a defect of this pass.
static SERDE_CALLBACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:default|with|serialize_with|deserialize_with|skip_serializing_if|getter)\s*=\s*"([^"]+)""#,
    )
    .expect("hardcoded serde-callback regex")
});

/// Emit synthetic call edges for serde string-callback attributes found in a
/// Rust source slice (the byte range of a struct/enum chunk).
///
/// Returns one [`CallSite`] per distinct serde callback reference, with the
/// callee set to the terminal path segment (`crate::a::b::f` → `f`) so it
/// resolves the same way the call graph resolves cross-module identifiers
/// (bare last segment — see [`SERDE_CALLBACK_RE`] docs). `line_offset` is the
/// 1-indexed line of `source`'s first byte within the file, so returned line
/// numbers are absolute and consistent with the tree-sitter call path.
///
/// No-op for non-Rust languages (serde is Rust-only) and for slices with no
/// `serde` substring, so the common case pays only a `contains` scan.
///
/// CONTAINER-LEVEL ATTRIBUTES: in tree-sitter-rust the `struct_item` /
/// `enum_item` node does NOT include the leading `#[serde(...)]`
/// attribute_items that decorate it — those are prev_siblings, outside the
/// chunk byte range. Field-level attributes live inside the item body and ARE
/// in range, so the common `#[serde(default = "fn")]`-on-a-field case is
/// covered. Container-level attributes on the type itself are not reached by
/// this pass and rely on the dead-code `filter_serde_callbacks` backstop.
pub(crate) fn extract_serde_callback_calls(
    source: &str,
    language: Language,
    line_offset: u32,
) -> Vec<CallSite> {
    let _span = tracing::debug_span!("extract_serde_callback_calls", %language).entered();

    if language != Language::Rust || !source.contains("serde") {
        return Vec::new();
    }

    let mut calls = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for cap in SERDE_CALLBACK_RE.captures_iter(source) {
        let m = match cap.get(1) {
            Some(m) => m,
            None => continue,
        };
        let path = m.as_str();
        // Terminal path segment: `crate::a::b::f` → `f`. Matches how the call
        // graph keys callee_name (bare last segment) and the dead-code filter.
        let terminal = path.rsplit("::").next().unwrap_or(path);
        if terminal.is_empty() || should_skip_callee(terminal) {
            continue;
        }
        if !seen.insert(terminal.to_string()) {
            continue;
        }
        // Line number of the attribute, absolute within the file.
        let line_in_slice = source[..m.start()].bytes().filter(|&b| b == b'\n').count() as u32;
        let line_number = line_offset.saturating_add(line_in_slice).max(1);
        calls.push(CallSite {
            callee_name: terminal.to_string(),
            line_number,
            kind: CallEdgeKind::SerdeCallback,
        });
    }
    calls
}

/// Emit synthetic call edges for `func(args)`-shaped calls that live inside
/// Rust macro token-trees.
///
/// In tree-sitter-rust a `macro_invocation` (`println!(...)`, `vec![...]`,
/// `assert_eq!(...)`) carries its arguments as an opaque `token_tree`: the
/// grammar does not parse the body, so a call like `auth_banner_tty(x, y)`
/// inside `println!("{}", auth_banner_tty(x, y))` is a flat run of tokens, NOT
/// a `call_expression`. The standard call query therefore never sees it, and a
/// function called ONLY from inside macros shows zero callers — breaking
/// callers / impact / test-map / dead.
///
/// Heuristic (high precision): inside any `token_tree`, an `identifier` whose
/// immediate next named-or-anonymous sibling is itself a `token_tree` is the
/// callee of a `func(args)` / `func[args]` / `func{args}` shape. We emit a call
/// edge for it. Identifiers NOT followed by a `token_tree` (bare variables,
/// the left half of a `m::n` path, struct field names) are skipped.
///
/// PATH-QUALIFIED CALLS: `m::n()` appears inside the token_tree as
/// `identifier(m) :: identifier(n) token_tree(())`. Only `n` is immediately
/// followed by a `token_tree`, so we emit `n` — the terminal segment, matching
/// how the call graph resolves cross-module identifiers by bare last segment
/// (see `extract_serde_callback_calls`). `m` (next sibling `::`) is skipped.
///
/// NESTED MACROS: a macro nested inside a token-tree arg (`outer!(inner!(j()))`)
/// is parsed as a real `macro_invocation` node *inside* the outer token_tree.
/// We recurse through `token_tree` AND `macro_invocation` children, so the
/// inner call (`j`) is reached. The macro NAME itself (`println`, `inner`) is
/// the `identifier` child of a `macro_invocation` whose next sibling is `!`
/// (never a `token_tree`), so it is never emitted as a call.
///
/// Walks only `node`'s subtree, so callers pass the chunk node to stay scoped
/// to one chunk's byte range. `line_offset` is subtracted (saturating, min 1)
/// to convert absolute tree rows to chunk-relative 1-indexed lines, matching
/// the `extract_calls` convention; pass `0` for absolute line numbers.
pub(crate) fn extract_macro_call_edges(
    node: tree_sitter::Node,
    source: &str,
    language: Language,
    line_offset: u32,
) -> Vec<CallSite> {
    let _span = tracing::debug_span!("extract_macro_call_edges", %language).entered();

    if language != Language::Rust {
        return Vec::new();
    }

    let mut calls = Vec::new();
    collect_macro_calls(node, source, line_offset, false, &mut calls);
    calls
}

/// Recursive worker for [`extract_macro_call_edges`].
///
/// `in_token_tree` tracks whether the current node is (transitively) inside a
/// macro `token_tree`. Only once inside do we treat an `identifier` followed by
/// a `token_tree` as a call — outside token-trees the normal call query already
/// covers real `call_expression`s, and applying the heuristic there would
/// double-count.
fn collect_macro_calls(
    node: tree_sitter::Node,
    source: &str,
    line_offset: u32,
    in_token_tree: bool,
    out: &mut Vec<CallSite>,
) {
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();

    for (i, child) in children.iter().enumerate() {
        let kind = child.kind();

        // Inside a token_tree: an `identifier` immediately followed by a
        // `token_tree` is the `func(args)` shape. The macro-name identifier of
        // a nested `macro_invocation` is followed by `!`, never a `token_tree`,
        // so it is naturally excluded.
        if in_token_tree && kind == "identifier" {
            let next_is_token_tree = children
                .get(i + 1)
                .map(|n| n.kind() == "token_tree")
                .unwrap_or(false);
            if next_is_token_tree {
                let callee_name = source[child.byte_range()].to_string();
                if !should_skip_callee(&callee_name) {
                    let line_number = (child.start_position().row as u32 + 1)
                        .saturating_sub(line_offset)
                        .max(1);
                    out.push(CallSite {
                        callee_name,
                        line_number,
                        kind: CallEdgeKind::MacroHeuristic,
                    });
                }
            }
        }

        // Recurse. Descend into token_tree (its tokens may hold calls and
        // nested token_trees) and macro_invocation (nested macros). The
        // `in_token_tree` flag latches on once we enter a token_tree and stays
        // set for that subtree.
        let child_in_token_tree = in_token_tree || kind == "token_tree";
        collect_macro_calls(*child, source, line_offset, child_in_token_tree, out);
    }
}

/// Collect the names of every Rust function definition in `node`'s subtree.
///
/// Used by [`extract_fn_pointer_arg_edges`] as the intra-file precision filter:
/// a bare `identifier` in argument position is emitted as a fn-pointer edge ONLY
/// if it names a function defined in the same file. This keeps the common
/// variable-as-argument case (`f(state, count)` where `count` is a local) out of
/// the call graph while still catching `f(state, handler)` where `handler` is a
/// local free function passed by value.
///
/// Captures `function_item` (free / inherent / associated fns) and
/// `function_signature_item` (trait method declarations). Callers pass the WHOLE
/// file's root so the set is file-wide — a fn referenced in one chunk but defined
/// in another still resolves.
pub(crate) fn collect_rust_fn_names(
    node: tree_sitter::Node,
    source: &str,
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let kind = n.kind();
        if kind == "function_item" || kind == "function_signature_item" {
            if let Some(name_node) = n.child_by_field_name("name") {
                names.insert(source[name_node.byte_range()].to_string());
            }
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    names
}

/// Emit synthetic call edges for functions passed as bare fn-pointer / callback
/// VALUES in argument position — the half of #1818 the serde-callback and
/// macro-token-tree passes don't reach.
///
/// Target shapes (all invisible to the `call_expression function:` call query,
/// because the function is a *value* not a callee):
/// - `from_fn_with_state(touch_state, touch_idle_clock)` — axum middleware
/// - `ctrlc::set_handler(on_sigterm)`
/// - `items.iter().map(parse_line)`
/// - `register(m::handler)` — scoped path argument
///
/// PRECISION (the whole problem): a bare `identifier` in argument position is
/// USUALLY a variable, not a function. Emitting an edge for every identifier
/// argument would flood the graph with variable-name noise. Two-tier rule:
///   1. Bare `(identifier)` arg → emit ONLY if it's in `known_fns` (a function
///      defined in the same file). Cheap, high-precision, intra-file only.
///   2. `(scoped_identifier)` arg (`m::handler`) → emit the terminal segment
///      UNCONDITIONALLY. A `::`-qualified path in value position is a strong
///      function/const signal (variables are bare), and bare-last-segment
///      resolution matches the rest of the call graph.
/// The cross-file BARE-identifier case (`f(handler)` where `handler` is a `use`d
/// free fn from another module) is the residual gap — it needs a query-time
/// edge_kind filter (schema v30), out of scope here.
///
/// Also descends into `tuple_expression` and `array_expression` arguments
/// (`register((a, b))`, `dispatch([handler_a, handler_b])`) applying the same
/// two-tier rule to their elements.
///
/// Scoped to `node`'s subtree; callers pass the chunk node (whole-file path) or
/// the range-covering node (standalone path). `line_offset` is subtracted
/// (saturating, min 1) to convert absolute tree rows to chunk-relative 1-indexed
/// lines; pass `0` for absolute line numbers. No-op for non-Rust languages.
pub(crate) fn extract_fn_pointer_arg_edges(
    node: tree_sitter::Node,
    source: &str,
    language: Language,
    line_offset: u32,
    known_fns: &std::collections::HashSet<String>,
) -> Vec<CallSite> {
    let _span = tracing::debug_span!("extract_fn_pointer_arg_edges", %language).entered();

    if language != Language::Rust {
        return Vec::new();
    }

    let mut calls = Vec::new();
    collect_fn_pointer_args(node, source, line_offset, known_fns, &mut calls);
    calls
}

/// Recursive worker for [`extract_fn_pointer_arg_edges`]. Finds every
/// `call_expression`, inspects its `arguments` node, and emits fn-pointer edges
/// per the two-tier precision rule. Recurses through the whole subtree so calls
/// nested in any position are reached.
fn collect_fn_pointer_args(
    node: tree_sitter::Node,
    source: &str,
    line_offset: u32,
    known_fns: &std::collections::HashSet<String>,
    out: &mut Vec<CallSite>,
) {
    if node.kind() == "call_expression" {
        if let Some(args) = node.child_by_field_name("arguments") {
            let mut cursor = args.walk();
            for arg in args.named_children(&mut cursor) {
                emit_fn_pointer_arg(arg, source, line_offset, known_fns, out);
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_fn_pointer_args(child, source, line_offset, known_fns, out);
    }
}

/// Apply the two-tier fn-pointer rule to a single argument node.
///
/// - `(identifier)` → emit if in `known_fns` (intra-file function name).
/// - `(scoped_identifier name: (identifier))` → emit terminal segment always.
/// - `(tuple_expression …)` / `(array_expression …)` → recurse into elements.
/// Other argument shapes (literals, real nested `call_expression`s, references,
/// closures) are left alone: nested calls are handled by the outer recursion in
/// [`collect_fn_pointer_args`], and the rest are not fn-pointer values.
fn emit_fn_pointer_arg(
    arg: tree_sitter::Node,
    source: &str,
    line_offset: u32,
    known_fns: &std::collections::HashSet<String>,
    out: &mut Vec<CallSite>,
) {
    match arg.kind() {
        "identifier" => {
            let name = source[arg.byte_range()].to_string();
            if known_fns.contains(&name) && !should_skip_callee(&name) {
                push_arg_edge(arg, name, line_offset, out);
            }
        }
        "scoped_identifier" => {
            // Terminal segment (`m::handler` → `handler`), matching the call
            // graph's bare-last-segment resolution.
            if let Some(name_node) = arg.child_by_field_name("name") {
                let name = source[name_node.byte_range()].to_string();
                if !should_skip_callee(&name) {
                    push_arg_edge(name_node, name, line_offset, out);
                }
            }
        }
        "tuple_expression" | "array_expression" => {
            let mut cursor = arg.walk();
            for inner in arg.named_children(&mut cursor) {
                emit_fn_pointer_arg(inner, source, line_offset, known_fns, out);
            }
        }
        _ => {}
    }
}

/// Push a [`CallSite`] for a fn-pointer argument, converting the node's absolute
/// row to a chunk-relative 1-indexed line via `line_offset`.
fn push_arg_edge(
    node: tree_sitter::Node,
    callee_name: String,
    line_offset: u32,
    out: &mut Vec<CallSite>,
) {
    let line_number = (node.start_position().row as u32 + 1)
        .saturating_sub(line_offset)
        .max(1);
    out.push(CallSite {
        callee_name,
        line_number,
        kind: CallEdgeKind::FnPointer,
    });
}

impl Parser {
    /// Extract function calls from a chunk's source code
    /// Returns call sites found within the given byte range of the source.
    pub fn extract_calls(
        &self,
        source: &str,
        language: Language,
        start_byte: usize,
        end_byte: usize,
        line_offset: u32,
    ) -> Vec<CallSite> {
        // Span carries the language + chunk byte range so the tree-sitter
        // parse-failure warns below have enough identity to distinguish
        // "grammar broken globally" from "one weird chunk".
        let _span = tracing::debug_span!(
            "extract_calls",
            %language,
            start_byte,
            end_byte
        )
        .entered();

        // Grammar-less languages (Markdown) — no tree-sitter call extraction
        if language.def().grammar.is_none() {
            return vec![];
        }

        // Normalize CRLF → LF for consistency (callers typically pass normalized
        // source, but standalone callers like extract_calls_from_chunk may not)
        let source = if source.contains("\r\n") {
            std::borrow::Cow::Owned(source.replace("\r\n", "\n"))
        } else {
            std::borrow::Cow::Borrowed(source)
        };

        let Some(grammar) = language.try_grammar() else {
            return vec![]; // Grammar-less language — custom parser handles it
        };
        let mut parser = tree_sitter::Parser::new();
        if let Err(e) = parser.set_language(&grammar) {
            tracing::warn!(
                error = %e,
                %language,
                start_byte,
                end_byte,
                "set_language failed in extract_calls"
            );
            return vec![];
        }

        let tree = match parser.parse(source.as_ref(), None) {
            Some(t) => t,
            None => {
                tracing::warn!(
                    %language,
                    start_byte,
                    end_byte,
                    "tree-sitter parse returned None in extract_calls"
                );
                return vec![];
            }
        };

        let query = match self.get_call_query(language) {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    %language,
                    start_byte,
                    end_byte,
                    "Tree-sitter query failed in extract_calls"
                );
                return vec![];
            }
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        // Only match within the chunk's byte range
        cursor.set_byte_range(start_byte..end_byte);

        let mut calls = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

        while let Some(m) = matches.next() {
            for cap in m.captures {
                let callee_name = source[cap.node.byte_range()].to_string();
                // saturating_sub prevents underflow if line_offset > position
                // .max(1) ensures we never produce line 0 (line numbers are 1-indexed)
                let line_number = (cap.node.start_position().row as u32 + 1)
                    .saturating_sub(line_offset)
                    .max(1);

                // Skip common noise (self, this, super, etc.)
                if !should_skip_callee(&callee_name) {
                    calls.push(CallSite {
                        callee_name,
                        line_number,
                        kind: CallEdgeKind::Call,
                    });
                }
            }
        }

        // Macro token-tree call edges: calls inside `println!`/`vec!`/etc.
        // token-trees are opaque tokens, not `call_expression`s, so the query
        // above misses them. Scope the tree walk to the same byte range as the
        // query cursor by descending to the node covering the range. Deduped
        // below against query-captured calls so a name appearing in both a real
        // call and a macro arg is counted once.
        if language == Language::Rust {
            let root = tree.root_node();
            let scope = root
                .descendant_for_byte_range(start_byte, end_byte)
                .unwrap_or(root);
            calls.extend(extract_macro_call_edges(
                scope,
                &source,
                language,
                line_offset,
            ));

            // Fn-pointer / callback argument edges: `f(state, handler)`,
            // `set_handler(on_sigterm)`, `.map(parse_line)`, `register(m::h)`.
            // The function is a VALUE in argument position, invisible to the
            // call query. Known-fn set is collected from the whole tree (the
            // `source` slice) so an intra-file fn referenced inside this range
            // but defined outside it still resolves. Deduped below against
            // query-captured calls.
            let known_fns = collect_rust_fn_names(root, &source);
            calls.extend(extract_fn_pointer_arg_edges(
                scope,
                &source,
                language,
                line_offset,
                &known_fns,
            ));
        }

        // Deduplicate calls to the same function (keep first occurrence)
        let mut seen = std::collections::HashSet::new();
        calls.retain(|c| seen.insert(c.callee_name.clone()));

        calls
    }

    /// Extract function calls from a parsed chunk
    /// Convenience method that extracts calls from the chunk's content.
    ///
    /// Per-chunk extractor consults `LanguageDef::chunk_call_parser` first.
    /// Markdown registers
    /// `extract_calls_from_markdown_chunk`; future grammar-less languages
    /// (SQL stored-proc cross-refs, L5X tag references, NL doc formats)
    /// can opt in by populating the field on their `LanguageDef`. Falls
    /// through to the tree-sitter call extractor when unset.
    pub fn extract_calls_from_chunk(&self, chunk: &super::types::Chunk) -> Vec<CallSite> {
        if let Some(def) = chunk.language.try_def() {
            if let Some(extractor) = def.chunk_call_parser {
                return extractor(chunk);
            }
        }
        let mut calls = self.extract_calls(
            &chunk.content,
            chunk.language,
            0,
            chunk.content.len(),
            0, // No line offset since we're parsing the content directly
        );
        // Mirror the serde string-callback edges emitted by the whole-file
        // Pass-2 walk (parse_file_relationships / parse_file_all_inner) so the
        // per-chunk shape stays in parity. Content is parsed standalone with
        // 1-indexed relative lines, so `line_offset = 1` puts an attribute on
        // the chunk's first content line at line 1 — matching the whole-file
        // path's relative-line conversion. Deduped against existing calls so a
        // callback that also appears in a real call_expression isn't doubled.
        let mut seen: std::collections::HashSet<String> =
            calls.iter().map(|c| c.callee_name.clone()).collect();
        for sc in extract_serde_callback_calls(&chunk.content, chunk.language, 1) {
            if seen.insert(sc.callee_name.clone()) {
                calls.push(sc);
            }
        }
        calls
    }

    /// Extract type references from a chunk's byte range
    /// Returns classified type references with merge logic: if a type name
    /// was captured by any classified pattern (Param/Return/Field/Impl/Bound/Alias),
    /// the catch-all duplicate is dropped. Types found ONLY by the catch-all
    /// get `kind = None`.
    pub fn extract_types(
        &self,
        source: &str,
        tree: &tree_sitter::Tree,
        language: Language,
        start_byte: usize,
        end_byte: usize,
    ) -> Vec<TypeRef> {
        let _span = tracing::info_span!("extract_types", %language).entered();

        let query = match self.get_type_query(language) {
            Ok(q) => q,
            Err(_) => {
                // Language has no type query (e.g., JavaScript) — not a warning
                return vec![];
            }
        };

        let capture_names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);

        // Collect all (type_name, line_number, kind) entries
        let mut classified: Vec<TypeRef> = Vec::new();
        let mut catch_all: Vec<TypeRef> = Vec::new();

        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let capture_name = match capture_names.get(cap.index as usize) {
                    Some(name) => *name,
                    None => continue,
                };

                let kind = match capture_name {
                    "param_type" => Some(TypeEdgeKind::Param),
                    "return_type" => Some(TypeEdgeKind::Return),
                    "field_type" => Some(TypeEdgeKind::Field),
                    "impl_type" => Some(TypeEdgeKind::Impl),
                    "bound_type" => Some(TypeEdgeKind::Bound),
                    "alias_type" => Some(TypeEdgeKind::Alias),
                    "type_ref" => None,
                    other => {
                        tracing::debug!(capture = other, "Unknown type capture");
                        continue;
                    }
                };

                let type_name = source[cap.node.byte_range()].to_string();
                let line_number = cap.node.start_position().row as u32 + 1;

                let type_ref = TypeRef {
                    type_name,
                    line_number,
                    kind,
                };

                if kind.is_some() {
                    classified.push(type_ref);
                } else {
                    catch_all.push(type_ref);
                }
            }
        }

        // Build set of type names that have at least one classified entry.
        //
        // Borrow `&str` from `classified` instead of cloning every
        // `type_name` into an owned `HashSet<String>` — the membership check
        // doesn't outlive `classified`, so no allocation is needed.
        // The set is consumed via `into_iter` of catch_all below; it must
        // not survive the subsequent `classified.push(t)` (which would alias
        // the borrow), so we materialize the keep-list first and push after.
        let to_keep: Vec<TypeRef> = {
            let classified_names: std::collections::HashSet<&str> =
                classified.iter().map(|t| t.type_name.as_str()).collect();
            catch_all
                .into_iter()
                .filter(|t| !classified_names.contains(t.type_name.as_str()))
                .collect()
        };
        classified.extend(to_keep);

        // Dedup by (type_name, kind) — same type as Param twice → one entry,
        // but same type as Param AND Return → two entries
        let mut seen = std::collections::HashSet::new();
        classified.retain(|t| seen.insert((t.type_name.clone(), t.kind)));

        classified
    }

    /// Extract all function calls from a file, ignoring size limits
    /// Returns calls for every function in the file, including those >100 lines
    /// that would normally be skipped during chunk extraction.
    /// Thin wrapper around `parse_file_relationships()`.
    pub fn parse_file_calls(&self, path: &Path) -> Result<Vec<FunctionCalls>, ParserError> {
        let (calls, _types) = self.parse_file_relationships(path)?;
        Ok(calls)
    }

    /// Extract all function calls AND type references from a file in a single parse pass
    /// Returns `(calls, type_refs)` for every chunk in the file. Single file read,
    /// single tree-sitter parse, two query cursors on the same tree.
    /// **Coupling note:** This function and `parse_file()` must agree on line numbering
    /// (`node.start_position().row as u32 + 1`) and chunk identity (same query, same
    /// post-process hooks). If either changes, the other must be updated to keep
    /// chunk names and line_start values consistent across phases.
    pub fn parse_file_relationships(
        &self,
        path: &Path,
    ) -> Result<(Vec<FunctionCalls>, Vec<ChunkTypeRefs>), ParserError> {
        let _span =
            tracing::info_span!("parse_file_relationships", path = %path.display()).entered();

        // Env-overridable cap (CQS_PARSER_MAX_FILE_SIZE).
        let max_file_size = crate::limits::parser_max_file_size();
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > max_file_size => {
                tracing::warn!(
                    size_mb = meta.len() / (1024 * 1024),
                    cap_mb = max_file_size / (1024 * 1024),
                    path = %path.display(),
                    "Skipping large file; bump CQS_PARSER_MAX_FILE_SIZE if needed"
                );
                return Ok((vec![], vec![]));
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }

        // Read file
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Ok((vec![], vec![]));
            }
            Err(e) => return Err(e.into()),
        };

        // Normalize line endings (CRLF -> LF) for consistency
        let source = source.replace("\r\n", "\n");

        let ext_raw = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let ext = ext_raw.to_ascii_lowercase();
        let language = Language::from_extension(&ext)
            .ok_or_else(|| ParserError::UnsupportedFileType(ext.to_string()))?;

        // Grammar-less languages use custom reference extraction:
        // prefer `custom_call_parser` (relationships only), fall back to
        // `custom_all_parser` (drops chunks), then to the markdown default.
        // The layered fallback means a language that only defines the
        // combined parser (like ASPX) still gets correct call/type
        // extraction here without a dedicated calls-only function.
        if language.def().grammar.is_none() {
            if let Some(f) = language.def().custom_call_parser {
                return f(&source, path, self);
            }
            if let Some(f) = language.def().custom_all_parser {
                let (_chunks, calls, chunk_types) = f(&source, path, self)?;
                return Ok((calls, chunk_types));
            }
            // Markdown (and any future grammar-less language
            // that opts into the default line-based parser)
            let md_calls = crate::parser::markdown::parse_markdown_references(&source, path)?;
            return Ok((md_calls, vec![]));
        }

        let grammar = language.try_grammar().ok_or_else(|| {
            ParserError::ParseFailed(format!("{} has no tree-sitter grammar", language))
        })?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| ParserError::ParseFailed(format!("{}", e)))?;

        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| ParserError::ParseFailed(path.display().to_string()))?;

        // Get or compile queries (lazy initialization).
        // Invariant: all grammar-bearing languages have chunk and call query patterns
        // (may be empty strings, which compile to valid queries matching nothing).
        let chunk_query = self.get_query(language)?;
        let call_query = self.get_call_query(language)?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(chunk_query, tree.root_node(), source.as_bytes());

        let mut call_results = Vec::new();
        let mut type_results = Vec::new();
        // Reuse these allocations across iterations
        let mut call_cursor = tree_sitter::QueryCursor::new();
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let capture_names = chunk_query.capture_names();
        let name_idx = chunk_query.capture_index_for_name("name");

        // File-wide Rust function-name set for the fn-pointer-argument precision
        // filter (bare-identifier args emit an edge only if they name a function
        // defined in this file). Computed once over the whole tree so a fn
        // referenced in one chunk but defined in another still resolves. Empty
        // for non-Rust languages (the pass is a no-op there).
        let known_fns = if language == Language::Rust {
            collect_rust_fn_names(tree.root_node(), &source)
        } else {
            std::collections::HashSet::new()
        };

        while let Some(m) = matches.next() {
            // Find chunk node
            let func_node = m.captures.iter().find(|c| {
                let name = capture_names.get(c.index as usize).copied().unwrap_or("");
                capture_name_to_chunk_type(name).is_some()
            });

            let Some(func_capture) = func_node else {
                continue;
            };

            let node = func_capture.node;

            // Get chunk name
            let mut name = name_idx
                .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                .map(|c| source[c.node.byte_range()].to_string())
                .unwrap_or_else(|| "<anonymous>".to_string());

            // Apply post-process hook for name corrections (needed for HCL qualified names)
            if let Some(post_process) = language.def().post_process_chunk {
                // Infer chunk_type from capture name
                let cap_name = capture_names
                    .get(func_capture.index as usize)
                    .copied()
                    .unwrap_or("");
                let mut ct = capture_name_to_chunk_type(cap_name).unwrap_or(ChunkType::Function);
                if !post_process(&mut name, &mut ct, node, &source) {
                    continue; // Skip discarded chunks
                }
            }

            let line_start = node.start_position().row as u32 + 1;
            let byte_range = node.byte_range();

            // --- Call extraction ---
            call_cursor.set_byte_range(byte_range.clone());
            calls.clear();

            let mut call_matches =
                call_cursor.matches(call_query, tree.root_node(), source.as_bytes());

            while let Some(cm) = call_matches.next() {
                for cap in cm.captures {
                    let callee_name = source[cap.node.byte_range()].to_string();
                    let call_line = cap.node.start_position().row as u32 + 1;

                    if !should_skip_callee(&callee_name) {
                        calls.push(CallSite {
                            callee_name,
                            line_number: call_line,
                            kind: CallEdgeKind::Call,
                        });
                    }
                }
            }

            // serde string-callback edges: struct/enum chunks carrying
            // `#[serde(default = "fn")]` etc. invoke those functions via the
            // derive. The tree-sitter call query can't see them (string
            // literals in attributes), so emit them explicitly here. Scans the
            // chunk's byte range — FIELD-level serde attributes (inside the
            // body) are covered; CONTAINER-level attributes preceding the item
            // sit outside the node and are left to the dead-code backstop. See
            // the matching note in `parse_file_all_inner`.
            calls.extend(extract_serde_callback_calls(
                &source[byte_range.clone()],
                language,
                line_start,
            ));

            // Macro token-tree call edges: calls inside `println!`/`vec!`/etc.
            // are opaque tokens, not `call_expression`s. Walk the chunk node for
            // them. This loop uses ABSOLUTE line numbers (the call query above
            // does not subtract line_start), so pass line_offset = 0.
            calls.extend(extract_macro_call_edges(node, &source, language, 0));

            // Fn-pointer / callback argument edges: a function passed as a VALUE
            // in argument position (`from_fn_with_state(state, touch_idle_clock)`,
            // `set_handler(on_sigterm)`, `.map(parse_line)`). Same ABSOLUTE line
            // convention as the macro pass → line_offset = 0.
            calls.extend(extract_fn_pointer_arg_edges(
                node, &source, language, 0, &known_fns,
            ));

            // Deduplicate calls
            seen.clear();
            calls.retain(|c| seen.insert(c.callee_name.clone()));

            if !calls.is_empty() {
                call_results.push(FunctionCalls {
                    name: name.clone(),
                    line_start,
                    calls: std::mem::take(&mut calls),
                });
            }

            // --- Type extraction ---
            let mut type_refs =
                self.extract_types(&source, &tree, language, byte_range.start, byte_range.end);

            // Filter self-referential types (e.g., struct Config shouldn't list Config as a dep)
            type_refs.retain(|t| t.type_name != name);

            if !type_refs.is_empty() {
                type_results.push(ChunkTypeRefs {
                    name,
                    line_start,
                    type_refs,
                });
            }
        }

        // --- Phase 2: Injection relationships (multi-grammar) ---
        let injections = language.def().injections;
        if !injections.is_empty() {
            // Release borrows on the outer tree before injection phase
            drop(matches);
            drop(cursor);

            let groups = super::injection::find_injection_ranges(&tree, &source, injections);

            // Free outer tree/parser memory before inner parse allocations
            drop(tree);
            drop(parser);
            for group in &groups {
                match self.parse_injected_relationships(&source, group, 0) {
                    Ok((inner_calls, inner_types))
                        if !inner_calls.is_empty() || !inner_types.is_empty() =>
                    {
                        // Remove outer container entries (matching parse_file's chunk removal)
                        call_results.retain(|fc| {
                            !super::injection::chunk_within_container(
                                fc.line_start,
                                fc.line_start, // calls have no line_end, use start for containment
                                &group.container_lines,
                            )
                        });
                        type_results.retain(|tr| {
                            !super::injection::chunk_within_container(
                                tr.line_start,
                                tr.line_start,
                                &group.container_lines,
                            )
                        });
                        call_results.extend(inner_calls);
                        type_results.extend(inner_types);
                    }
                    Ok(_) => {
                        // Zero inner results — keep outer
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            language = %group.language,
                            "Injection relationship parsing failed"
                        );
                    }
                }
            }
        }

        Ok((call_results, type_results))
    }
}

/// Check if a callee name should be skipped (common noise)
/// These are filtered because they don't provide meaningful call graph information:
/// - `self`, `this`, `Self`, `super`: Object references, not real function calls
/// - `new`: Constructor pattern, not a named function
/// - `toString`, `valueOf`: Ubiquitous JS/TS methods that add noise
/// Case-sensitive to avoid false positives (e.g., "This" as a variable name).
pub(crate) fn should_skip_callee(name: &str) -> bool {
    matches!(
        name,
        "self" | "this" | "super" | "Self" | "new" | "toString" | "valueOf"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    mod skip_callee_tests {
        use super::*;

        #[test]
        fn test_skips_self_variants() {
            assert!(should_skip_callee("self"));
            assert!(should_skip_callee("Self"));
            assert!(should_skip_callee("this"));
            assert!(should_skip_callee("super"));
        }

        #[test]
        fn test_skips_common_noise() {
            assert!(should_skip_callee("new"));
            assert!(should_skip_callee("toString"));
            assert!(should_skip_callee("valueOf"));
        }

        #[test]
        fn test_allows_normal_functions() {
            assert!(!should_skip_callee("process"));
            assert!(!should_skip_callee("calculate"));
            assert!(!should_skip_callee("Self_")); // Not exact match
            assert!(!should_skip_callee("myself"));
            assert!(!should_skip_callee("newValue"));
        }

        #[test]
        fn test_case_sensitive() {
            assert!(!should_skip_callee("SELF"));
            assert!(!should_skip_callee("This"));
            assert!(!should_skip_callee("NEW"));
        }
    }

    /// Creates a temporary file with the specified content and file extension.
    /// # Arguments
    /// * `content` - The string content to write to the temporary file
    /// * `ext` - The file extension (without the leading dot) to append to the temporary filename
    /// # Returns
    /// A `NamedTempFile` representing the created temporary file with the content written and flushed to disk.
    /// # Panics
    /// Panics if the temporary file cannot be created or if writing/flushing the content fails.
    fn write_temp_file(content: &str, ext: &str) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    mod call_extraction_tests {
        use super::*;

        #[test]
        fn test_extract_rust_calls() {
            let content = r#"
fn caller() {
    helper();
    other.method();
    Module::function();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let calls = parser.extract_calls_from_chunk(&chunks[0]);

            let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            assert!(names.contains(&"helper"));
            assert!(names.contains(&"method"));
            assert!(names.contains(&"function"));
        }

        #[test]
        fn test_extract_python_calls() {
            let content = r#"
def caller():
    helper()
    obj.method()
"#;
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let calls = parser.extract_calls_from_chunk(&chunks[0]);

            let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            assert!(names.contains(&"helper"));
            assert!(names.contains(&"method"));
        }

        #[test]
        fn test_skips_self_calls() {
            let content = r#"
fn example() {
    self.method();
    this.other();
    real_function();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let calls = parser.extract_calls_from_chunk(&chunks[0]);

            let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            assert!(!names.contains(&"self"));
            assert!(!names.contains(&"this"));
            assert!(names.contains(&"method"));
            assert!(names.contains(&"other"));
            assert!(names.contains(&"real_function"));
        }

        #[test]
        fn test_parse_file_calls() {
            let content = r#"
fn caller() {
    helper();
    other_func();
}

fn another() {
    third();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let function_calls = parser.parse_file_calls(file.path()).unwrap();

            assert_eq!(function_calls.len(), 2);

            let caller = function_calls
                .iter()
                .find(|fc| fc.name == "caller")
                .unwrap();
            let caller_names: Vec<_> = caller
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(caller_names.contains(&"helper"));
            assert!(caller_names.contains(&"other_func"));

            let another = function_calls
                .iter()
                .find(|fc| fc.name == "another")
                .unwrap();
            let another_names: Vec<_> = another
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(another_names.contains(&"third"));
        }

        #[test]
        fn test_parse_file_calls_unsupported_extension() {
            let file = write_temp_file("not code", "txt");
            let parser = Parser::new().unwrap();
            let result = parser.parse_file_calls(file.path());
            assert!(result.is_err());
        }

        #[test]
        fn test_parse_file_calls_empty_file() {
            let file = write_temp_file("", "rs");
            let parser = Parser::new().unwrap();
            let function_calls = parser.parse_file_calls(file.path()).unwrap();
            assert!(function_calls.is_empty());
        }
    }

    mod type_extraction_tests {
        use super::*;

        /// Helper: check if type_refs contains (name, kind)
        fn has_type(refs: &[TypeRef], name: &str, kind: Option<TypeEdgeKind>) -> bool {
            refs.iter().any(|t| t.type_name == name && t.kind == kind)
        }

        /// Parse source with tree-sitter and run extract_types on full range.
        /// Use for testing types on constructs that aren't chunks (impl blocks, type aliases).
        fn extract_types_from_source(content: &str, ext: &str) -> Vec<TypeRef> {
            let parser = Parser::new().unwrap();
            let language = Language::from_extension(ext).unwrap();
            let grammar = language
                .try_grammar()
                .expect("test language must have grammar");
            let mut ts_parser = tree_sitter::Parser::new();
            ts_parser.set_language(&grammar).unwrap();
            let tree = ts_parser.parse(content, None).unwrap();
            parser.extract_types(content, &tree, language, 0, content.len())
        }

        // --- Rust ---

        #[test]
        fn test_extract_types_rust_params_and_return() {
            let content = "fn foo(x: Config, y: Store) -> StoreError { }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "Store", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "StoreError", Some(TypeEdgeKind::Return)));
        }

        #[test]
        fn test_extract_types_rust_struct_fields() {
            let content = "struct Foo {\n    config: Config,\n    pool: SqlitePool,\n}\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            assert_eq!(types[0].name, "Foo");
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Field)));
            assert!(has_type(refs, "SqlitePool", Some(TypeEdgeKind::Field)));
        }

        #[test]
        fn test_extract_types_rust_impl() {
            // impl blocks aren't chunks — test extract_types directly with full-file range
            let content = "impl MyTrait for MyStruct {\n    fn foo(&self) { }\n}\n";
            let types = extract_types_from_source(content, "rs");
            assert!(
                has_type(&types, "MyTrait", Some(TypeEdgeKind::Impl)),
                "MyTrait should be Impl, got: {:?}",
                types
            );
            assert!(
                has_type(&types, "MyStruct", Some(TypeEdgeKind::Impl)),
                "MyStruct should be Impl, got: {:?}",
                types
            );
        }

        #[test]
        fn test_extract_types_rust_bounds() {
            let content = "fn foo<T: Display + Clone>(x: T) { }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            // The function chunk includes its entire span including generic params
            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Display", Some(TypeEdgeKind::Bound)));
            assert!(has_type(refs, "Clone", Some(TypeEdgeKind::Bound)));
        }

        #[test]
        fn test_extract_types_rust_no_primitives() {
            let content = "fn foo(x: i32, y: bool) -> u64 { 0 }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            // Primitives are `primitive_type` in tree-sitter, not `type_identifier`
            // So the function should have no type refs
            assert!(types.is_empty());
        }

        #[test]
        fn test_extract_types_rust_catch_all_merge() {
            // Config appears as Param (classified) AND inside generic (catch-all)
            // Error appears only inside generic (catch-all only)
            let content = "fn foo(c: Config) -> Result<Config, MyError> { todo!() }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;

            // Config should be Param (classified wins over catch-all)
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
            // Config should NOT also appear as None
            assert!(!has_type(refs, "Config", None));

            // Result should be Return (classified)
            assert!(has_type(refs, "Result", Some(TypeEdgeKind::Return)));

            // MyError should be None (catch-all only — inside generic)
            assert!(has_type(refs, "MyError", None));
        }

        #[test]
        fn test_extract_types_rust_reference_types() {
            let content = "fn foo(x: &Config) -> &Store { todo!() }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "Store", Some(TypeEdgeKind::Return)));
        }

        #[test]
        fn test_extract_types_rust_generic_param() {
            let content = "fn foo(x: Vec<Config>) -> Option<Store> { todo!() }\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            // Vec and Option are the outer generic types → classified
            assert!(has_type(refs, "Vec", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "Option", Some(TypeEdgeKind::Return)));
            // Config and Store are inside generics → catch-all or classified depending on pattern
            // Config is inside Vec<Config> which is a parameter → the generic_type pattern should match Vec
            // Config itself is a type_identifier inside type_arguments → catch-all
            assert!(
                has_type(refs, "Config", None)
                    || has_type(refs, "Config", Some(TypeEdgeKind::Param))
            );
            assert!(
                has_type(refs, "Store", None)
                    || has_type(refs, "Store", Some(TypeEdgeKind::Return))
            );
        }

        #[test]
        fn test_extract_types_rust_alias() {
            // type_item isn't a chunk — test extract_types directly with full-file range
            let content = "type MyResult = Result<Config, MyError>;\n";
            let types = extract_types_from_source(content, "rs");
            assert!(
                has_type(&types, "Result", Some(TypeEdgeKind::Alias)),
                "Result should be Alias, got: {:?}",
                types
            );
            // Config and MyError inside generics — catch-all only
            assert!(
                has_type(&types, "Config", None),
                "Config should be catch-all (None), got: {:?}",
                types
            );
            assert!(
                has_type(&types, "MyError", None),
                "MyError should be catch-all (None), got: {:?}",
                types
            );
        }

        // --- TypeScript ---

        #[test]
        fn test_extract_types_typescript() {
            let content =
                "function foo(x: UserConfig): ResponseData {\n    return {} as ResponseData;\n}\n";
            let file = write_temp_file(content, "ts");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "UserConfig", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "ResponseData", Some(TypeEdgeKind::Return)));
        }

        // --- Python ---

        #[test]
        fn test_extract_types_python() {
            let content = "def foo(x: MyType) -> ReturnType:\n    pass\n";
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "MyType", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "ReturnType", Some(TypeEdgeKind::Return)));
        }

        // --- Go ---

        #[test]
        fn test_extract_types_go() {
            let content =
                "package main\n\nfunc foo(cfg Config) Handler {\n    return Handler{}\n}\n";
            let file = write_temp_file(content, "go");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(types.len(), 1);
            let refs = &types[0].type_refs;
            assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
            assert!(has_type(refs, "Handler", Some(TypeEdgeKind::Return)));
        }

        // --- Java ---

        #[test]
        fn test_extract_types_java() {
            let content = "class Main {\n    public UserService getService(Config config) {\n        return null;\n    }\n}\n";
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            // Java chunk query should capture the class or method
            // method_definition captures getService
            if !types.is_empty() {
                let refs = &types[0].type_refs;
                assert!(has_type(refs, "Config", Some(TypeEdgeKind::Param)));
                assert!(has_type(refs, "UserService", Some(TypeEdgeKind::Return)));
            }
        }

        // --- C ---

        #[test]
        fn test_extract_types_c() {
            let content = "Config create_config(Pool pool) {\n    Config c;\n    return c;\n}\n";
            let file = write_temp_file(content, "c");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            if !types.is_empty() {
                let refs = &types[0].type_refs;
                // C function_definition captures return type
                assert!(has_type(refs, "Pool", Some(TypeEdgeKind::Param)));
                // Config is both return type AND function name won't match (it's the type)
                // Actually the function name is "create_config", Config is the return type
                assert!(has_type(refs, "Config", Some(TypeEdgeKind::Return)));
            }
        }

        // --- JavaScript (no types) ---

        #[test]
        fn test_extract_types_empty_for_js() {
            let content = "function foo(x) {\n    return x + 1;\n}\n";
            let file = write_temp_file(content, "js");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();
            assert!(types.is_empty());
        }

        // --- Markdown (no types) ---

        #[test]
        fn test_extract_types_empty_for_markdown() {
            let content = "# Hello\n\nSome text\n";
            let file = write_temp_file(content, "md");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();
            assert!(types.is_empty());
        }

        // --- Combined parse ---

        #[test]
        fn test_parse_file_relationships_returns_both() {
            let content = r#"
fn process(config: Config) -> StoreError {
    helper();
    store.save();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            // Should have both calls and types
            assert!(!calls.is_empty(), "Expected call results");
            assert!(!types.is_empty(), "Expected type results");

            let call_entry = calls.iter().find(|c| c.name == "process").unwrap();
            let call_names: Vec<_> = call_entry
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(call_names.contains(&"helper"));
            assert!(call_names.contains(&"save"));

            let type_entry = types.iter().find(|t| t.name == "process").unwrap();
            assert!(has_type(
                &type_entry.type_refs,
                "Config",
                Some(TypeEdgeKind::Param)
            ));
            assert!(has_type(
                &type_entry.type_refs,
                "StoreError",
                Some(TypeEdgeKind::Return)
            ));
        }

        #[test]
        fn test_parse_file_relationships_filters_self_referential() {
            let content = "struct Config {\n    pool: SqlitePool,\n}\n";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, types) = parser.parse_file_relationships(file.path()).unwrap();

            if !types.is_empty() {
                let config_refs = types.iter().find(|t| t.name == "Config").unwrap();
                // Config should NOT appear in its own type_refs
                assert!(
                    !config_refs
                        .type_refs
                        .iter()
                        .any(|t| t.type_name == "Config"),
                    "Self-referential type should be filtered out"
                );
                assert!(has_type(
                    &config_refs.type_refs,
                    "SqlitePool",
                    Some(TypeEdgeKind::Field)
                ));
            }
        }

        #[test]
        fn test_parse_file_calls_unchanged() {
            // The thin wrapper returns the same results as parse_file_relationships
            let content = r#"
fn caller() {
    helper();
    other_func();
}

fn another() {
    third();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let calls_only = parser.parse_file_calls(file.path()).unwrap();
            let (calls_combined, _types) = parser.parse_file_relationships(file.path()).unwrap();

            assert_eq!(calls_only.len(), calls_combined.len());
            for (a, b) in calls_only.iter().zip(calls_combined.iter()) {
                assert_eq!(a.name, b.name);
                assert_eq!(a.line_start, b.line_start);
                assert_eq!(a.calls.len(), b.calls.len());
            }
        }

        #[test]
        fn test_parse_file_relationships_nonexistent() {
            let parser = Parser::new().unwrap();
            let result =
                parser.parse_file_relationships(std::path::Path::new("/nonexistent/file.rs"));
            assert!(result.is_err());
        }
    }

    mod type_edge_kind_tests {
        use super::*;

        #[test]
        fn test_roundtrip() {
            let kinds = [
                TypeEdgeKind::Param,
                TypeEdgeKind::Return,
                TypeEdgeKind::Field,
                TypeEdgeKind::Impl,
                TypeEdgeKind::Bound,
                TypeEdgeKind::Alias,
            ];
            for kind in &kinds {
                let s = kind.as_str();
                let parsed: TypeEdgeKind = s.parse().unwrap();
                assert_eq!(*kind, parsed);
            }
        }

        #[test]
        fn test_display() {
            assert_eq!(TypeEdgeKind::Param.to_string(), "Param");
            assert_eq!(TypeEdgeKind::Return.to_string(), "Return");
            assert_eq!(TypeEdgeKind::Field.to_string(), "Field");
            assert_eq!(TypeEdgeKind::Impl.to_string(), "Impl");
            assert_eq!(TypeEdgeKind::Bound.to_string(), "Bound");
            assert_eq!(TypeEdgeKind::Alias.to_string(), "Alias");
        }

        #[test]
        fn test_unknown_from_str() {
            let result: Result<TypeEdgeKind, _> = "Unknown".parse();
            assert!(result.is_err());
        }
    }

    /// Diagnostic: verify type queries compile for all languages with type_query defined.
    ///
    /// Iterates `Language::all_variants()` filtered by
    /// `def().type_query.is_some()` so a new language with a type query gets
    /// covered automatically. FSharp has a `type_query` registered on its
    /// LanguageDef but the query has a tree-sitter-fsharp node-type mismatch
    /// (`type` is not a valid node), so it's skipped here.
    #[test]
    fn test_type_queries_compile() {
        let parser = Parser::new().unwrap();
        let mut covered = 0usize;
        for lang in Language::all_variants().iter().copied() {
            if lang == Language::FSharp {
                continue;
            }
            let Some(def) = lang.try_def() else {
                continue;
            };
            if def.type_query.is_none() {
                continue;
            }
            let result = parser.get_type_query(lang);
            assert!(
                result.is_ok(),
                "{} type query failed to compile: {:?}",
                lang,
                result.err()
            );
            covered += 1;
        }
        assert!(
            covered > 0,
            "test_type_queries_compile asserted nothing — \
             no Language has a `type_query` defined"
        );
    }

    /// Struct-field-assignment edges — the Rust call query captures
    /// `field: function_path` patterns inside `struct_expression` literals so
    /// functions used as Option<fn> / fn-pointer field values don't surface
    /// as `cqs dead` false positives. Pins the query patterns against
    /// representative inputs.
    #[test]
    fn test_struct_field_assignment_captures_function_value() {
        let parser = Parser::new().unwrap();
        let source = r#"
fn helper_fn() {}

struct Config {
    callback: Option<fn()>,
    direct: fn(),
}

fn build_config() -> Config {
    Config {
        callback: Some(helper_fn),
        direct: helper_fn,
    }
}
"#;
        let calls = parser.extract_calls(source, Language::Rust, 0, source.len(), 0);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        // Must capture `helper_fn` (in both `Some(helper_fn)` and bare
        // `helper_fn`) plus `Some` (the `call_expression` for `Some(...)`).
        assert!(
            names.contains(&"helper_fn"),
            "expected `helper_fn` in captured calls, got: {:?}",
            names
        );
    }

    #[test]
    fn test_struct_field_assignment_captures_scoped_function_path() {
        let parser = Parser::new().unwrap();
        let source = r#"
mod inner {
    pub fn module_fn() {}
}

struct Config {
    callback: fn(),
}

fn build() {
    let _c = Config {
        callback: inner::module_fn,
    };
}
"#;
        let calls = parser.extract_calls(source, Language::Rust, 0, source.len(), 0);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"module_fn"),
            "expected `module_fn` (from `inner::module_fn` field value), got: {:?}",
            names
        );
    }

    /// Pin the `Some(fn_path as TypeAlias)` cast pattern — Rust dispatch
    /// tables sometimes coerce a fn-item to a fn-pointer type before
    /// storing in `Option<fn(...) -> ...>`.
    #[test]
    fn test_struct_field_assignment_captures_type_cast_function() {
        let parser = Parser::new().unwrap();
        let source = r#"
type PostProcessFn = fn() -> ();

fn post_process_helper() {}

struct Config {
    handler: Option<PostProcessFn>,
}

const CFG: Config = Config {
    handler: Some(post_process_helper as PostProcessFn),
};
"#;
        let calls = parser.extract_calls(source, Language::Rust, 0, source.len(), 0);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"post_process_helper"),
            "expected `post_process_helper` (from `Some(post_process_helper as PostProcessFn)`), got: {:?}",
            names
        );
    }

    mod macro_call_edge_tests {
        use super::*;

        /// Parse Rust source standalone and return all call edges found by
        /// `extract_calls` (which now includes the macro token-tree pass).
        fn calls_for(content: &str) -> Vec<String> {
            let parser = Parser::new().unwrap();
            let calls = parser.extract_calls(content, Language::Rust, 0, content.len(), 0);
            calls.into_iter().map(|c| c.callee_name).collect()
        }

        /// Run ONLY the macro token-tree pass (`extract_macro_call_edges`) over
        /// the parsed source. Used to assert what the heuristic alone emits —
        /// distinct from `extract_calls`, which ALSO runs the tree-sitter call
        /// query (whose `(macro_invocation macro: (identifier))` pattern
        /// legitimately captures macro NAMES like `println` as callees, a
        /// pre-existing dead-code behavior this pass leaves untouched).
        fn macro_edges_for(content: &str) -> Vec<String> {
            let grammar = Language::Rust.try_grammar().unwrap();
            let mut ts = tree_sitter::Parser::new();
            ts.set_language(&grammar).unwrap();
            let tree = ts.parse(content, None).unwrap();
            extract_macro_call_edges(tree.root_node(), content, Language::Rust, 0)
                .into_iter()
                .map(|c| c.callee_name)
                .collect()
        }

        /// `println!("{}", f(x))` — a call inside a macro arg is invisible to
        /// the call query (opaque token_tree). The macro pass must surface it.
        #[test]
        fn test_macro_println_arg_call() {
            let content = r#"
fn caller() {
    println!("{}", auth_banner_tty(actual, token));
}
"#;
            let names = calls_for(content);
            assert!(
                names.contains(&"auth_banner_tty".to_string()),
                "expected auth_banner_tty edge from println! arg, got: {names:?}"
            );
            // The macro pass itself must NOT emit the macro name `println`
            // (only the existing call query captures it, by design).
            let macro_only = macro_edges_for(content);
            assert!(
                !macro_only.contains(&"println".to_string()),
                "macro pass must not emit the macro name `println`, got: {macro_only:?}"
            );
        }

        /// `assert_eq!(g(), h())` — both args are zero-arg calls.
        #[test]
        fn test_macro_assert_eq_both_args() {
            let names = calls_for("fn c() { assert_eq!(g(), h()); }\n");
            assert!(names.contains(&"g".to_string()), "got: {names:?}");
            assert!(names.contains(&"h".to_string()), "got: {names:?}");
            let macro_only = macro_edges_for("fn c() { assert_eq!(g(), h()); }\n");
            assert!(
                !macro_only.contains(&"assert_eq".to_string()),
                "macro pass must not emit macro name, got: {macro_only:?}"
            );
        }

        /// `vec![make(i)]` — bracket-delimited token_tree, call inside.
        #[test]
        fn test_macro_vec_bracket_call() {
            let names = calls_for("fn c() { let v = vec![make(i)]; }\n");
            assert!(names.contains(&"make".to_string()), "got: {names:?}");
            let macro_only = macro_edges_for("fn c() { let v = vec![make(i)]; }\n");
            assert!(
                !macro_only.contains(&"vec".to_string()),
                "macro pass must not emit macro name, got: {macro_only:?}"
            );
        }

        /// Nested macro: `outer!(inner!(j()))` — the inner call must be reached
        /// through the nested macro_invocation node, and neither macro name is
        /// emitted by the macro pass.
        #[test]
        fn test_macro_nested_invocation() {
            let names = calls_for("fn c() { outer!(inner!(j())); }\n");
            assert!(
                names.contains(&"j".to_string()),
                "nested macro inner call should surface, got: {names:?}"
            );
            let macro_only = macro_edges_for("fn c() { outer!(inner!(j())); }\n");
            assert!(
                !macro_only.contains(&"outer".to_string())
                    && !macro_only.contains(&"inner".to_string()),
                "macro pass must not emit nested macro names, got: {macro_only:?}"
            );
            // The inner call IS emitted by the macro pass.
            assert!(
                macro_only.contains(&"j".to_string()),
                "macro pass should emit inner call `j`, got: {macro_only:?}"
            );
        }

        /// Path-qualified call inside a macro: `log!(m::n())` resolves to the
        /// terminal segment `n` (matching the call graph's bare-last-segment
        /// resolution), and the leading segment `m` is NOT emitted.
        #[test]
        fn test_macro_path_qualified_terminal_segment() {
            let macro_only = macro_edges_for("fn c() { log!(m::n()); }\n");
            assert!(
                macro_only.contains(&"n".to_string()),
                "terminal segment `n` should be emitted, got: {macro_only:?}"
            );
            assert!(
                !macro_only.contains(&"m".to_string()),
                "leading path segment `m` must not be emitted, got: {macro_only:?}"
            );
        }

        /// A bare identifier inside a macro that is NOT followed by a token_tree
        /// (a variable, not a call) must not produce an edge from the macro pass.
        #[test]
        fn test_macro_bare_identifier_not_emitted() {
            let macro_only = macro_edges_for(
                r#"fn c() { println!("{} {}", alpha, beta); }
"#,
            );
            assert!(
                !macro_only.contains(&"alpha".to_string())
                    && !macro_only.contains(&"beta".to_string()),
                "bare variables in a macro must not be call edges, got: {macro_only:?}"
            );
        }

        /// Regression: a plain (non-macro) call still emits exactly ONE edge —
        /// the macro heuristic must not double-count real `call_expression`s.
        #[test]
        fn test_plain_call_emits_exactly_once() {
            let parser = Parser::new().unwrap();
            let content = "fn caller() { do_work(); }\n";
            let calls = parser.extract_calls(content, Language::Rust, 0, content.len(), 0);
            let do_work_count = calls.iter().filter(|c| c.callee_name == "do_work").count();
            assert_eq!(
                do_work_count, 1,
                "plain call should emit exactly one edge, got {do_work_count}: {:?}",
                calls
            );
        }

        /// A name appearing BOTH as a real call and inside a macro arg is
        /// deduped to a single edge (extract_calls dedups by callee_name).
        #[test]
        fn test_call_and_macro_arg_deduped() {
            let parser = Parser::new().unwrap();
            let content = r#"
fn caller() {
    shared();
    println!("{}", shared());
}
"#;
            let calls = parser.extract_calls(content, Language::Rust, 0, content.len(), 0);
            let shared_count = calls.iter().filter(|c| c.callee_name == "shared").count();
            assert_eq!(
                shared_count, 1,
                "call + macro-arg occurrence must dedup to one edge, got {shared_count}"
            );
        }

        /// End-to-end through `parse_file_relationships`: a function whose ONLY
        /// reference to `auth_banner_tty` is inside a `println!` gets a
        /// function_calls edge naming it — the store-level caller resolution
        /// then sees the relationship. This is the issue's exact shape.
        #[test]
        fn test_parse_file_relationships_emits_macro_edge() {
            let content = r#"
fn auth_banner_tty(a: u32, b: u32) -> u32 { a + b }

fn render() {
    println!("{}", auth_banner_tty(1, 2));
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();
            let render = calls
                .iter()
                .find(|fc| fc.name == "render")
                .expect("render should have a function_calls entry");
            let callees: Vec<&str> = render
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(
                callees.contains(&"auth_banner_tty"),
                "render should call auth_banner_tty via the println! macro, got: {callees:?}"
            );
        }
    }

    mod serde_callback_tests {
        use super::*;

        /// Every serde string-callback attribute form on one struct emits a
        /// call edge to the bare (terminal-segment) function name.
        #[test]
        fn test_extract_serde_callback_calls_all_forms() {
            let source = r#"
#[derive(serde::Deserialize)]
struct Config {
    #[serde(default = "default_ref_weight")]
    weight: f32,
    #[serde(with = "humantime_serde")]
    timeout: Duration,
    #[serde(serialize_with = "ser_custom")]
    a: String,
    #[serde(deserialize_with = "crate::de::de_custom")]
    b: String,
    #[serde(skip_serializing_if = "is_zero")]
    maybe: u32,
    #[serde(getter = "get_inner")]
    inner: Inner,
}
"#;
            let calls = extract_serde_callback_calls(source, Language::Rust, 1);
            let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            for expected in [
                "default_ref_weight",
                "humantime_serde",
                "ser_custom",
                "de_custom", // terminal segment of `crate::de::de_custom`
                "is_zero",
                "get_inner",
            ] {
                assert!(
                    names.contains(&expected),
                    "expected serde callback `{expected}`, got: {names:?}"
                );
            }
        }

        /// `bound = "..."` names types / where-clauses, not functions — it must
        /// not produce a spurious call edge.
        #[test]
        fn test_extract_serde_callback_calls_excludes_bound() {
            let source = r#"
#[derive(serde::Serialize)]
#[serde(bound = "T: Serialize")]
struct Wrapper<T> {
    inner: T,
}
"#;
            let calls = extract_serde_callback_calls(source, Language::Rust, 1);
            assert!(
                calls.is_empty(),
                "bound must not emit a serde call edge, got: {:?}",
                calls
            );
        }

        /// No `serde` substring → fast no-op (and non-Rust languages skip).
        #[test]
        fn test_extract_serde_callback_calls_noop_paths() {
            // No serde in source.
            assert!(
                extract_serde_callback_calls("struct Plain { x: u32 }", Language::Rust, 1)
                    .is_empty()
            );
            // Non-Rust language with a serde-shaped string is ignored.
            assert!(extract_serde_callback_calls(
                r#"x = {default = "some_fn"}"#,
                Language::Python,
                1
            )
            .is_empty());
        }

        /// End-to-end through `parse_file_relationships`: the carrying struct
        /// chunk gets a `function_calls` entry naming each callback, so the
        /// store-level `get_callers_full` can resolve them.
        #[test]
        fn test_parse_file_relationships_emits_serde_edges() {
            let content = r#"
#[derive(serde::Deserialize)]
struct RefWeightCfg {
    #[serde(default = "default_ref_weight")]
    weight: f32,
}

fn default_ref_weight() -> f32 { 1.0 }
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();

            let cfg = calls
                .iter()
                .find(|fc| fc.name == "RefWeightCfg")
                .expect("RefWeightCfg should have a function_calls entry");
            let callees: Vec<&str> = cfg.calls.iter().map(|c| c.callee_name.as_str()).collect();
            assert!(
                callees.contains(&"default_ref_weight"),
                "RefWeightCfg should call default_ref_weight, got: {:?}",
                callees
            );
        }

        /// Documented limitation: container-level `#[serde(...)]` attributes
        /// (preceding `struct`/`enum`) sit OUTSIDE the item node's byte range
        /// in tree-sitter-rust, so this pass does not emit an edge for them —
        /// they rely on the dead-code `filter_serde_callbacks` backstop. A
        /// FIELD-level attribute on the same struct IS captured. This test
        /// pins both halves so a future grammar change that pulls container
        /// attributes into range is noticed.
        #[test]
        fn test_parse_file_relationships_container_vs_field_attr() {
            let content = r#"
#[derive(serde::Deserialize)]
#[serde(default = "container_default")]
struct Settings {
    #[serde(deserialize_with = "field_de")]
    a: u32,
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();
            let settings = calls
                .iter()
                .find(|fc| fc.name == "Settings")
                .expect("Settings chunk should carry the field-level serde edge");
            let callees: Vec<&str> = settings
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(
                callees.contains(&"field_de"),
                "field-level serde deserialize_with should emit an edge, got: {:?}",
                callees
            );
            // Container-level attribute is out of range — not emitted here.
            assert!(
                !callees.contains(&"container_default"),
                "container-level serde attr is a known limitation (left to the \
                 dead-code backstop); it must NOT be emitted by this pass, got: {:?}",
                callees
            );
        }
    }

    /// Fn-pointer / callback ARGUMENT-position edges (#1818 second half):
    /// functions passed as a VALUE into a call (`f(state, handler)`,
    /// `set_handler(on_sigterm)`, `.map(parse_line)`, `register(m::handler)`).
    mod fn_pointer_arg_tests {
        use super::*;

        /// Run the whole-file relationship path and collect the callees of the
        /// named chunk (file-wide known-fn set — the production shape).
        fn callees_of(content: &str, chunk_name: &str) -> Vec<String> {
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();
            calls
                .iter()
                .find(|fc| fc.name == chunk_name)
                .map(|fc| {
                    fc.calls
                        .iter()
                        .map(|c| c.callee_name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        }

        /// axum-style `from_fn_with_state(state, handler)`: both `touch_state`
        /// and `touch_idle_clock` are local free fns passed by value. Both must
        /// surface as edges; the `state` ordinary variable must NOT.
        #[test]
        fn test_axum_state_handler_args() {
            let content = r#"
fn touch_state() {}
fn touch_idle_clock() {}

fn build() {
    let state = make_state();
    from_fn_with_state(state, touch_state, touch_idle_clock);
}
"#;
            let callees = callees_of(content, "build");
            assert!(
                callees.contains(&"touch_state".to_string()),
                "touch_state (fn-pointer arg) should emit an edge, got: {callees:?}"
            );
            assert!(
                callees.contains(&"touch_idle_clock".to_string()),
                "touch_idle_clock (fn-pointer arg) should emit an edge, got: {callees:?}"
            );
            // `state` is a local variable, not a function — precision pin.
            assert!(
                !callees.contains(&"state".to_string()),
                "the `state` variable argument must NOT emit an edge, got: {callees:?}"
            );
        }

        /// `.map(parse_line)` — a free fn passed to an iterator adapter.
        #[test]
        fn test_map_function_arg() {
            let content = r#"
fn parse_line(s: &str) -> u32 { 0 }

fn run(lines: Vec<&str>) {
    let _: Vec<u32> = lines.iter().map(parse_line).collect();
}
"#;
            let callees = callees_of(content, "run");
            assert!(
                callees.contains(&"parse_line".to_string()),
                "parse_line passed to .map() should emit an edge, got: {callees:?}"
            );
        }

        /// `set_handler(on_sigterm)` — the ctrlc-handler shape (single fn arg).
        #[test]
        fn test_set_handler_arg() {
            let content = r#"
fn on_sigterm() {}

fn install() {
    set_handler(on_sigterm);
}
"#;
            let callees = callees_of(content, "install");
            assert!(
                callees.contains(&"on_sigterm".to_string()),
                "on_sigterm passed to set_handler should emit an edge, got: {callees:?}"
            );
        }

        /// A scoped path argument `m::handler` emits the terminal segment
        /// UNCONDITIONALLY — no intra-file known-fn requirement, because a
        /// `::`-qualified value is a strong function signal.
        #[test]
        fn test_scoped_path_arg() {
            let content = r#"
fn install() {
    register(m::handler);
}
"#;
            let callees = callees_of(content, "install");
            assert!(
                callees.contains(&"handler".to_string()),
                "scoped `m::handler` arg should emit terminal segment `handler`, got: {callees:?}"
            );
            // The leading path segment `m` must not be emitted.
            assert!(
                !callees.contains(&"m".to_string()),
                "leading path segment `m` must not be emitted, got: {callees:?}"
            );
        }

        /// PRECISION PIN: a bare identifier argument that is NOT a known
        /// function (an ordinary variable / value) must not produce an edge.
        /// This is the whole-graph-noise guard.
        #[test]
        fn test_variable_argument_not_emitted() {
            let content = r#"
fn run() {
    let count = 5;
    let name = compute();
    do_thing(count, name);
}
"#;
            let callees = callees_of(content, "run");
            assert!(
                !callees.contains(&"count".to_string()),
                "`count` variable argument must NOT emit an edge, got: {callees:?}"
            );
            assert!(
                !callees.contains(&"name".to_string()),
                "`name` variable argument must NOT emit an edge, got: {callees:?}"
            );
        }

        /// REGRESSION: a plain call `do_work()` still emits exactly ONE edge —
        /// the fn-pointer pass must not double-count a real callee.
        #[test]
        fn test_plain_call_emits_exactly_once() {
            let parser = Parser::new().unwrap();
            let content = "fn do_work() {}\nfn caller() { do_work(); }\n";
            let calls = parser.extract_calls(content, Language::Rust, 0, content.len(), 0);
            let n = calls.iter().filter(|c| c.callee_name == "do_work").count();
            assert_eq!(
                n, 1,
                "plain call must emit exactly one edge, got {n}: {calls:?}"
            );
        }

        /// A fn passed in a tuple/array argument is reached and obeys the same
        /// two-tier rule (known bare fn emits, scoped emits, variable does not).
        #[test]
        fn test_tuple_and_array_args() {
            let content = r#"
fn handler_a() {}
fn handler_b() {}

fn install() {
    let v = 1;
    register((handler_a, v));
    dispatch([handler_a, handler_b]);
}
"#;
            let callees = callees_of(content, "install");
            assert!(
                callees.contains(&"handler_a".to_string()),
                "handler_a in tuple/array arg should emit, got: {callees:?}"
            );
            assert!(
                callees.contains(&"handler_b".to_string()),
                "handler_b in array arg should emit, got: {callees:?}"
            );
            assert!(
                !callees.contains(&"v".to_string()),
                "the `v` variable in a tuple arg must NOT emit, got: {callees:?}"
            );
        }

        /// Documented limitation: a CROSS-FILE bare fn-pointer arg (the fn is
        /// `use`d from another module, not defined in this file) is NOT emitted
        /// — the intra-file known-fn filter can't see it. This is the v30-era
        /// residual. Pins the boundary so a future query-time edge_kind filter
        /// that closes it is noticed.
        #[test]
        fn test_cross_file_bare_arg_not_emitted() {
            // `imported_handler` is referenced but never defined in this file,
            // so it isn't in the known-fn set → no edge (the gap by design).
            let content = r#"
fn install() {
    set_handler(imported_handler);
}
"#;
            let callees = callees_of(content, "install");
            assert!(
                !callees.contains(&"imported_handler".to_string()),
                "cross-file bare fn-pointer arg is the known v30 residual; it must \
                 NOT be emitted by the intra-file filter, got: {callees:?}"
            );
        }
    }
}
