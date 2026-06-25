//! Call extraction from tree-sitter parse trees

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::StreamingIterator;

use super::types::{
    capture_name_to_chunk_type, CallEdgeKind, CallSite, CandidateSite, ChunkType, ChunkTypeRefs,
    FunctionCalls, Language, ParserError, TypeEdgeKind, TypeRef,
};
use super::Parser;

/// Relationships extraction PLUS the file's Lane-2 candidate set:
/// `(function_calls, type_refs, candidate_edges)`. Returned by
/// [`Parser::parse_file_relationships_with_candidates`].
pub type RelationshipsWithCandidates = (Vec<FunctionCalls>, Vec<ChunkTypeRefs>, Vec<CandidateSite>);

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
/// BARE macro args (`m!(callback)`): an `identifier` inside a token_tree that is
/// NOT followed by a `token_tree` is usually a variable / a token, but it can be
/// a function or macro passed by name to a code-gen macro — e.g.
/// `for_each_logged_batch_cmd!(gen_log_query_dispatch)`, where
/// `gen_log_query_dispatch` is a `macro_rules!` the outer macro expands. Bare
/// idents are noise-prone, so this case emits an edge ONLY when the ident is in
/// `known_fns` (the same intra-file precision gate the fn-pointer-arg pass uses;
/// it includes `macro_definition` names). The leading segment of a `m::n` path
/// (next sibling `::`) is also a bare ident not followed by a `token_tree`, but
/// it is excluded by the `known_fns` gate in the normal case.
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
    known_fns: &std::collections::HashSet<String>,
) -> Vec<CallSite> {
    let _span = tracing::debug_span!("extract_macro_call_edges", %language).entered();

    if language != Language::Rust {
        return Vec::new();
    }

    let mut calls = Vec::new();
    collect_macro_calls(
        node,
        source,
        line_offset,
        false,
        known_fns,
        &mut calls,
        0,
        crate::limits::parser_max_walk_depth(),
    );
    calls
}

/// Recursive worker for [`extract_macro_call_edges`].
///
/// `in_token_tree` tracks whether the current node is (transitively) inside a
/// macro `token_tree`. Only once inside do we treat an `identifier` followed by
/// a `token_tree` as a call — outside token-trees the normal call query already
/// covers real `call_expression`s, and applying the heuristic there would
/// double-count. A bare `identifier` NOT followed by a `token_tree` emits only
/// when it is in `known_fns` (the precision gate for the noise-prone bare-arg
/// case — see [`extract_macro_call_edges`]).
#[allow(clippy::too_many_arguments)]
fn collect_macro_calls(
    node: tree_sitter::Node,
    source: &str,
    line_offset: u32,
    in_token_tree: bool,
    known_fns: &std::collections::HashSet<String>,
    out: &mut Vec<CallSite>,
    depth: usize,
    max_depth: usize,
) {
    // Depth rail: deeply-nested `token_tree`s (adversarial indexed content) would
    // recurse one stack frame per level and overflow the rayon parser-stage
    // worker stack, aborting the whole index/watch process. Stop descending past
    // the cap; see `crate::limits::PARSER_MAX_WALK_DEPTH`. Behavior-neutral for
    // legitimate code, which never nests this deep.
    if depth >= max_depth {
        return;
    }
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
            let callee_name = source[child.byte_range()].to_string();
            // `func(args)` shape: emit unconditionally (high precision — an
            // ident directly before `(...)`/`[...]`/`{...}` is a call).
            // Bare ident (no following token_tree): emit ONLY if it names a
            // same-file fn/macro (precision gate against variable noise).
            let emit = next_is_token_tree || known_fns.contains(&callee_name);
            if emit && !should_skip_callee(&callee_name) {
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

        // Recurse. Descend into token_tree (its tokens may hold calls and
        // nested token_trees) and macro_invocation (nested macros). The
        // `in_token_tree` flag latches on once we enter a token_tree and stays
        // set for that subtree.
        let child_in_token_tree = in_token_tree || kind == "token_tree";
        collect_macro_calls(
            *child,
            source,
            line_offset,
            child_in_token_tree,
            known_fns,
            out,
            depth + 1,
            max_depth,
        );
    }
}

/// Collect low-confidence MACRO-argument candidates: the bare `identifier`
/// inside a macro `token_tree` that [`collect_macro_calls`] DROPS because it is
/// not followed by a `token_tree` AND not in `known_fns` — the cross-file
/// code-gen-macro argument (`some_macro!(gen_dispatch)` where `gen_dispatch` is
/// a `macro_rules!` / fn defined in another file). The macro EDGE pass gates
/// this on `known_fns` (intra-file precision); the same drop reaching no
/// candidate is the sweep gap with the fn-pointer pass, which this closes.
///
/// PRECISION (macro token-trees are the noisiest source — a flat run of tokens,
/// no argument-position structure): a candidate is emitted ONLY for a bare ident
/// that is genuinely reference-shaped:
///   - NOT followed by a `token_tree` (that is the confident `func(args)` shape,
///     already an edge), and
///   - NOT in `known_fns` (an intra-file name is already a confident edge), and
///   - NOT a path segment — neither the prev nor next anonymous sibling is `::`
///     (`m::n` leading/trailing segments are not standalone references), and
///   - NOT a field access — the prev anonymous sibling is not `.`, and
///   - passes `should_skip_callee` (filters `self`/`Self`/keywords).
/// This still admits ordinary local-variable tokens (`vec![count]`) the macro
/// EDGE pass's `known_fns` gate would reject; candidates accept that lower bar
/// (they are hints in a side-table, never caller-graph edges), but the path /
/// field-access / keyword guards keep the obvious non-references out.
///
/// Disjoint from the macro EDGE pass by the same complement on `known_fns` the
/// fn-pointer candidate uses, so a name is never both a macro edge and a macro
/// candidate. No-op for non-Rust languages.
pub(crate) fn collect_macro_arg_candidates(
    node: tree_sitter::Node,
    source: &str,
    language: Language,
    line_offset: u32,
    known_fns: &std::collections::HashSet<String>,
    file: &Path,
    out: &mut Vec<CandidateSite>,
) {
    let _span = tracing::debug_span!("collect_macro_arg_candidates", %language).entered();

    if language != Language::Rust {
        return;
    }
    collect_macro_arg_candidates_inner(
        node,
        source,
        line_offset,
        false,
        known_fns,
        file,
        out,
        0,
        crate::limits::parser_max_walk_depth(),
    );
}

/// Recursive worker for [`collect_macro_arg_candidates`]. Mirrors
/// [`collect_macro_calls`]'s `in_token_tree`-latching traversal exactly, but
/// routes the dropped bare-ident case to a `macro_arg_unresolved`
/// [`CandidateSite`].
#[allow(clippy::too_many_arguments)]
fn collect_macro_arg_candidates_inner(
    node: tree_sitter::Node,
    source: &str,
    line_offset: u32,
    in_token_tree: bool,
    known_fns: &std::collections::HashSet<String>,
    file: &Path,
    out: &mut Vec<CandidateSite>,
    depth: usize,
    max_depth: usize,
) {
    // Depth rail against stack overflow; see `crate::limits::PARSER_MAX_WALK_DEPTH`.
    if depth >= max_depth {
        return;
    }
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();

    for (i, child) in children.iter().enumerate() {
        let kind = child.kind();

        if in_token_tree && kind == "identifier" {
            let next_kind = children.get(i + 1).map(|n| n.kind());
            let prev_kind = i
                .checked_sub(1)
                .and_then(|p| children.get(p))
                .map(|n| n.kind());
            let next_is_token_tree = next_kind == Some("token_tree");
            let name = &source[child.byte_range()];
            // Complement of the macro EDGE pass's emit: the bare-ident drop.
            // Plus the path-segment / field-access / keyword precision guards
            // (the EDGE pass leans on `known_fns` for these; the candidate has
            // no such gate, so it must exclude them explicitly).
            let is_path_segment = next_kind == Some("::") || prev_kind == Some("::");
            let is_field_access = prev_kind == Some(".");
            if !next_is_token_tree
                && !known_fns.contains(name)
                && !is_path_segment
                && !is_field_access
                && !should_skip_callee(name)
            {
                let ref_line = (child.start_position().row as u32 + 1)
                    .saturating_sub(line_offset)
                    .max(1);
                out.push(CandidateSite {
                    file: file.to_path_buf(),
                    callee_name: name.to_string(),
                    ref_line,
                    candidate_kind: CANDIDATE_MACRO_ARG_UNRESOLVED.to_string(),
                });
            }
        }

        let child_in_token_tree = in_token_tree || kind == "token_tree";
        collect_macro_arg_candidates_inner(
            *child,
            source,
            line_offset,
            child_in_token_tree,
            known_fns,
            file,
            out,
            depth + 1,
            max_depth,
        );
    }
}

/// Collect the names of every Rust function (and macro) definition in `node`'s
/// subtree.
///
/// Used as the intra-file precision filter for two bare-identifier edge passes:
/// - [`extract_fn_pointer_arg_edges`]: a bare `identifier` in CALL-argument
///   position is emitted as a fn-pointer edge ONLY if it names something defined
///   in the same file.
/// - [`collect_macro_calls`]: a bare `identifier` MACRO argument (`m!(callback)`,
///   not followed by a `token_tree`) is noise-prone, so it emits only when it
///   names a same-file definition.
/// This keeps the common variable-as-argument case (`f(state, count)` where
/// `count` is a local) out of the call graph while still catching `f(state,
/// handler)` / `m!(handler)` where `handler` is a local free function or macro
/// passed by value.
///
/// Captures `function_item` (free / inherent / associated fns),
/// `function_signature_item` (trait method declarations), and `macro_definition`
/// (`macro_rules!` — a code-gen macro like `gen_log_query_dispatch` passed as a
/// bare arg to another macro is a real reference, not a variable). Callers pass
/// the WHOLE file's root so the set is file-wide — a definition referenced in one
/// chunk but declared in another still resolves.
pub(crate) fn collect_rust_fn_names(
    node: tree_sitter::Node,
    source: &str,
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let kind = n.kind();
        if kind == "function_item"
            || kind == "function_signature_item"
            || kind == "macro_definition"
        {
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
    collect_fn_pointer_args(
        node,
        source,
        line_offset,
        known_fns,
        &mut calls,
        0,
        crate::limits::parser_max_walk_depth(),
    );
    calls
}

/// Recursive worker for [`extract_fn_pointer_arg_edges`]. Finds every
/// `call_expression`, inspects its `arguments` node, and emits fn-pointer edges
/// per the two-tier precision rule. Recurses through the whole subtree so calls
/// nested in any position are reached.
#[allow(clippy::too_many_arguments)]
fn collect_fn_pointer_args(
    node: tree_sitter::Node,
    source: &str,
    line_offset: u32,
    known_fns: &std::collections::HashSet<String>,
    out: &mut Vec<CallSite>,
    depth: usize,
    max_depth: usize,
) {
    // Depth rail against stack overflow on adversarial deep nesting; see
    // `crate::limits::PARSER_MAX_WALK_DEPTH`. Behavior-neutral for real code.
    if depth >= max_depth {
        return;
    }
    if node.kind() == "call_expression" {
        if let Some(args) = node.child_by_field_name("arguments") {
            let mut cursor = args.walk();
            for arg in args.named_children(&mut cursor) {
                emit_fn_pointer_arg(arg, source, line_offset, known_fns, out, depth, max_depth);
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_fn_pointer_args(
            child,
            source,
            line_offset,
            known_fns,
            out,
            depth + 1,
            max_depth,
        );
    }
}

/// Apply the two-tier fn-pointer rule to a single argument node.
///
/// - `(identifier)` → emit if in `known_fns` (intra-file function name).
/// - `(scoped_identifier name: (identifier))` → emit terminal segment always.
/// - `(tuple_expression …)` / `(array_expression …)` → recurse into elements.
/// - `(type_cast_expression value: …)` → unwrap the cast (`f as *const ()`)
///   and re-apply the rule to the inner value.
/// Other argument shapes (literals, real nested `call_expression`s, references,
/// closures) are left alone: nested calls are handled by the outer recursion in
/// [`collect_fn_pointer_args`], and the rest are not fn-pointer values.
#[allow(clippy::too_many_arguments)]
fn emit_fn_pointer_arg(
    arg: tree_sitter::Node,
    source: &str,
    line_offset: u32,
    known_fns: &std::collections::HashSet<String>,
    out: &mut Vec<CallSite>,
    depth: usize,
    max_depth: usize,
) {
    // Depth rail: tuple/array element recursion below can descend on adversarial
    // deep nesting; see `crate::limits::PARSER_MAX_WALK_DEPTH`.
    if depth >= max_depth {
        return;
    }
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
                emit_fn_pointer_arg(
                    inner,
                    source,
                    line_offset,
                    known_fns,
                    out,
                    depth + 1,
                    max_depth,
                );
            }
        }
        // `f as *const ()` / `on_sigterm as sighandler_t` — Rust coerces a
        // fn-item to a fn-pointer / raw-pointer type via `as`. The cast wraps
        // the fn name in a `type_cast_expression` whose `value` is the bare
        // identifier (or scoped path). Unwrap to the inner value and apply the
        // same two-tier rule: a bare ident emits only if known intra-file, a
        // scoped path emits its terminal segment. Fixes the `signal(SIG,
        // on_sigterm as *const ()...)` shape (false-DEAD `on_sigterm`).
        "type_cast_expression" => {
            if let Some(value) = arg.child_by_field_name("value") {
                emit_fn_pointer_arg(
                    value,
                    source,
                    line_offset,
                    known_fns,
                    out,
                    depth + 1,
                    max_depth,
                );
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

/// `candidate_edges.candidate_kind` for a bare `identifier` passed in
/// CALL-argument position that names something NOT defined in this file.
/// The confident fn-pointer pass drops it (the intra-file `known_fns`
/// gate guards against aliasing every same-named symbol in the index), so it
/// never becomes a `function_calls` edge — but it is a real cross-file
/// fn-pointer reference, so it lands here for a later query-time consumer
/// (Lane 3) to surface as a low-confidence candidate.
pub(crate) const CANDIDATE_BARE_ARG_UNRESOLVED: &str = "bare_arg_unresolved";

/// `candidate_edges.candidate_kind` for a bare `identifier` inside a macro
/// `token_tree` that the confident macro pass drops (not followed by a
/// `token_tree`, not in `known_fns`) — the cross-file code-gen-macro argument
/// (`some_macro!(gen_dispatch)`). Sibling of [`CANDIDATE_BARE_ARG_UNRESOLVED`]
/// on the macro side of the same precision drop.
pub(crate) const CANDIDATE_MACRO_ARG_UNRESOLVED: &str = "macro_arg_unresolved";

/// `candidate_edges.candidate_kind` for a CONTAINER-level `#[serde(...)]`
/// string callback (`#[serde(default = "fn")]` on the `struct`/`enum` itself,
/// not a field). In tree-sitter-rust the container attribute is a prev-sibling
/// OUTSIDE the item node's byte range, so the confident
/// [`extract_serde_callback_calls`] pass (which scans only the item range)
/// misses it. The dead-code `filter_serde_callbacks` backstop keeps the
/// callback live; this candidate records the reference so Lane 3 can surface
/// it without the corpus-wide content scan.
pub(crate) const CANDIDATE_SERDE_CONTAINER: &str = "serde_container";

/// `candidate_edges.candidate_kind` for the inner free functions of a
/// `#[serde(with = "module")]` linkage. serde's derive calls
/// `module::serialize` / `module::deserialize`, but the confident pass emits an
/// edge only to the terminal segment as written (`module`) to avoid aliasing
/// every `serialize`/`deserialize` in the index. The module's inner fns stay
/// unlinked in `function_calls`; this candidate records the
/// `module::serialize` / `module::deserialize` terminal segments so Lane 3 can
/// surface the linkage as low-confidence.
pub(crate) const CANDIDATE_SERDE_WITH_MODULE: &str = "serde_with_module";

/// Collect low-confidence fn-pointer-ARGUMENT candidates: bare `identifier`
/// args in CALL position that the confident [`extract_fn_pointer_arg_edges`]
/// pass DROPS because they are not in `known_fns` (the cross-file case).
///
/// Sibling of [`extract_fn_pointer_arg_edges`]; it walks the same subtree and
/// inspects the same argument shapes, but emits a [`CandidateSite`] precisely
/// where the confident pass declines to emit a [`CallSite`]. The two passes are
/// disjoint by construction: a name in `known_fns` becomes a confident edge and
/// is NOT a candidate; a name absent from `known_fns` is dropped by the
/// confident pass and becomes a candidate here. So a candidate-only callee
/// never also appears in `function_calls` — preserving the Lane-1 invariant
/// that candidates can never pollute the caller graph.
///
/// Only the bare-`identifier` arm of [`emit_fn_pointer_arg`] has a drop case:
/// `scoped_identifier` args (`m::handler`) always emit a confident edge (a
/// `::`-qualified value is a strong signal), so they yield no candidate.
///
/// `file` is the candidate's origin path (the `candidate_edges.file` column is
/// keyed by the persistence layer's normalized path; this field carries the
/// same path for descriptive parity). No-op for non-Rust languages.
pub(crate) fn collect_fn_pointer_arg_candidates(
    node: tree_sitter::Node,
    source: &str,
    language: Language,
    line_offset: u32,
    known_fns: &std::collections::HashSet<String>,
    file: &Path,
    out: &mut Vec<CandidateSite>,
) {
    let _span = tracing::debug_span!("collect_fn_pointer_arg_candidates", %language).entered();

    if language != Language::Rust {
        return;
    }
    collect_fn_pointer_arg_candidates_inner(
        node,
        source,
        line_offset,
        known_fns,
        file,
        out,
        0,
        crate::limits::parser_max_walk_depth(),
    );
}

/// Recursive worker for [`collect_fn_pointer_arg_candidates`]. Mirrors
/// [`collect_fn_pointer_args`]'s traversal (every `call_expression`'s arguments,
/// recursing the whole subtree) but routes each argument to the candidate arm
/// of [`emit_fn_pointer_arg_candidate`].
#[allow(clippy::too_many_arguments)]
fn collect_fn_pointer_arg_candidates_inner(
    node: tree_sitter::Node,
    source: &str,
    line_offset: u32,
    known_fns: &std::collections::HashSet<String>,
    file: &Path,
    out: &mut Vec<CandidateSite>,
    depth: usize,
    max_depth: usize,
) {
    // Depth rail against stack overflow; see `crate::limits::PARSER_MAX_WALK_DEPTH`.
    if depth >= max_depth {
        return;
    }
    if node.kind() == "call_expression" {
        if let Some(args) = node.child_by_field_name("arguments") {
            let mut cursor = args.walk();
            for arg in args.named_children(&mut cursor) {
                emit_fn_pointer_arg_candidate(
                    arg,
                    source,
                    line_offset,
                    known_fns,
                    file,
                    out,
                    depth,
                    max_depth,
                );
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_fn_pointer_arg_candidates_inner(
            child,
            source,
            line_offset,
            known_fns,
            file,
            out,
            depth + 1,
            max_depth,
        );
    }
}

/// Candidate-arm mirror of [`emit_fn_pointer_arg`]. Emits a
/// `bare_arg_unresolved` [`CandidateSite`] for exactly the bare-identifier
/// argument the confident pass DROPS — a name absent from `known_fns` (the
/// cross-file case). Scoped paths, tuples/arrays, and casts are descended the
/// same way so a bare ident nested in any of them is reached; none of those
/// wrapper shapes themselves produce a candidate.
#[allow(clippy::too_many_arguments)]
fn emit_fn_pointer_arg_candidate(
    arg: tree_sitter::Node,
    source: &str,
    line_offset: u32,
    known_fns: &std::collections::HashSet<String>,
    file: &Path,
    out: &mut Vec<CandidateSite>,
    depth: usize,
    max_depth: usize,
) {
    // Depth rail against stack overflow; see `crate::limits::PARSER_MAX_WALK_DEPTH`.
    if depth >= max_depth {
        return;
    }
    match arg.kind() {
        "identifier" => {
            let name = &source[arg.byte_range()];
            // The confident pass emits when `known_fns.contains(name)`. We emit
            // the candidate on the complementary case: a syntactically-valid
            // callee name NOT defined in this file. `should_skip_callee` filters
            // the same noise (self/this/super/...) so a candidate is never a
            // keyword token.
            if !known_fns.contains(name) && !should_skip_callee(name) {
                let ref_line = (arg.start_position().row as u32 + 1)
                    .saturating_sub(line_offset)
                    .max(1);
                out.push(CandidateSite {
                    file: file.to_path_buf(),
                    callee_name: name.to_string(),
                    ref_line,
                    candidate_kind: CANDIDATE_BARE_ARG_UNRESOLVED.to_string(),
                });
            }
        }
        "tuple_expression" | "array_expression" => {
            let mut cursor = arg.walk();
            for inner in arg.named_children(&mut cursor) {
                emit_fn_pointer_arg_candidate(
                    inner,
                    source,
                    line_offset,
                    known_fns,
                    file,
                    out,
                    depth + 1,
                    max_depth,
                );
            }
        }
        "type_cast_expression" => {
            if let Some(value) = arg.child_by_field_name("value") {
                emit_fn_pointer_arg_candidate(
                    value,
                    source,
                    line_offset,
                    known_fns,
                    file,
                    out,
                    depth + 1,
                    max_depth,
                );
            }
        }
        // `scoped_identifier` args always emit a confident edge (never dropped),
        // so they yield no candidate.
        _ => {}
    }
}

/// Collect low-confidence serde-callback candidates the confident
/// [`extract_serde_callback_calls`] pass does NOT reach:
///
/// - **`serde_container`** — CONTAINER-level `#[serde(...)]` string callbacks.
///   The attribute decorating a `struct`/`enum` is a prev-sibling OUTSIDE the
///   item node's byte range, so the confident pass (which scans only the item
///   range) never sees it. We walk the whole file's `attribute_item` nodes,
///   keep those whose next named sibling is a `struct_item`/`enum_item` (i.e.
///   they decorate the container, not a field — field attrs live inside the
///   body and are already covered), and emit the terminal segment of each
///   serde-shaped callback path.
/// - **`serde_with_module`** — for every `with = "module"` callback (anywhere,
///   container OR field level), emit the `module::serialize` and
///   `module::deserialize` terminal segments (`serialize` / `deserialize`)
///   that serde's derive actually calls. The confident pass emits only the
///   terminal segment of the path as written (`module`); the inner (de)ser fns
///   stay unlinked in `function_calls`, so they land here.
///
/// `file` carries the candidate's origin path. No-op for non-Rust sources and
/// for files with no `serde` substring (the common case pays only a `contains`
/// scan). Scoped to `root`'s subtree so the chunk-range path can pass a
/// chunk-covering node and still reach the container attributes preceding it
/// (callers pass the file root for whole-file paths).
pub(crate) fn collect_serde_candidates(
    root: tree_sitter::Node,
    source: &str,
    language: Language,
    file: &Path,
    out: &mut Vec<CandidateSite>,
) {
    let _span = tracing::debug_span!("collect_serde_candidates", %language).entered();

    if language != Language::Rust || !source.contains("serde") {
        return;
    }

    let mut seen: std::collections::HashSet<(String, u32, &'static str)> =
        std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "attribute_item" {
            collect_serde_attr_candidates(n, source, file, &mut seen, out);
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// Inspect one `attribute_item` node for serde container-level / `with`-module
/// candidates. Emits `serde_container` only when the attribute decorates a
/// container (`struct`/`enum`) — field-level serde attrs are already covered by
/// the confident range scan. Emits `serde_with_module` for `with = "module"`
/// regardless of attachment level (the inner (de)ser fns are unreached either
/// way). `seen` dedups within the file so the same callback on the same line is
/// emitted once per kind.
fn collect_serde_attr_candidates(
    attr: tree_sitter::Node,
    source: &str,
    file: &Path,
    seen: &mut std::collections::HashSet<(String, u32, &'static str)>,
    out: &mut Vec<CandidateSite>,
) {
    let attr_text = &source[attr.byte_range()];
    if !attr_text.contains("serde") {
        return;
    }
    // Does this attribute decorate a container (`struct`/`enum`)? The next
    // named sibling of a leading container attribute is the item it decorates
    // (other attributes chain as siblings, so skip over sibling attribute_items
    // to find the decorated node).
    let mut decorates_container = false;
    let mut sib = attr.next_named_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "attribute_item" => sib = s.next_named_sibling(),
            "struct_item" | "enum_item" | "union_item" => {
                decorates_container = true;
                break;
            }
            _ => break,
        }
    }

    let line = attr.start_position().row as u32 + 1;
    for cap in SERDE_CALLBACK_RE.captures_iter(attr_text) {
        let Some(m) = cap.get(1) else { continue };
        let path = m.as_str();
        let terminal = path.rsplit("::").next().unwrap_or(path);
        if terminal.is_empty() || should_skip_callee(terminal) {
            continue;
        }

        // `with = "module"` → the inner `module::serialize` / `module::deserialize`
        // terminal segments serde actually calls.
        let is_with = cap
            .get(0)
            .map(|whole| whole.as_str().trim_start().starts_with("with"))
            .unwrap_or(false);
        if is_with {
            for inner in ["serialize", "deserialize"] {
                if seen.insert((inner.to_string(), line, CANDIDATE_SERDE_WITH_MODULE)) {
                    out.push(CandidateSite {
                        file: file.to_path_buf(),
                        callee_name: inner.to_string(),
                        ref_line: line,
                        candidate_kind: CANDIDATE_SERDE_WITH_MODULE.to_string(),
                    });
                }
            }
        }

        // Container-level callback (the field-level case is already a confident
        // edge from the range scan, so only emit the container candidate).
        if decorates_container
            && seen.insert((terminal.to_string(), line, CANDIDATE_SERDE_CONTAINER))
        {
            out.push(CandidateSite {
                file: file.to_path_buf(),
                callee_name: terminal.to_string(),
                ref_line: line,
                candidate_kind: CANDIDATE_SERDE_CONTAINER.to_string(),
            });
        }
    }
}

impl Parser {
    /// Extract function calls from a chunk's source code
    /// Returns call sites found within the given byte range of the source.
    ///
    /// Thin wrapper over [`Self::extract_calls_with_candidates`] for callers
    /// that only need the confident `function_calls` edges; the low-confidence
    /// `candidate_edges` (Lane 2) are discarded.
    pub fn extract_calls(
        &self,
        source: &str,
        language: Language,
        start_byte: usize,
        end_byte: usize,
        line_offset: u32,
    ) -> Vec<CallSite> {
        self.extract_calls_with_candidates(source, language, start_byte, end_byte, line_offset)
            .0
    }

    /// Extract function calls AND low-confidence call-graph candidates from a
    /// chunk's byte range — the CHUNK-RANGE call-extraction path.
    ///
    /// Returns `(calls, candidates)`. `calls` are confident `function_calls`
    /// edges; `candidates` are the references the confident passes deliberately
    /// DROP (cross-file bare fn-pointer args, container-level / `with`
    /// serde callbacks) routed to the `candidate_edges` side-table so
    /// they never pollute the caller graph (Lane 1's invariant). The two sets
    /// are disjoint by construction.
    ///
    /// The returned candidates carry an EMPTY `file` (this surface has no path);
    /// the only production caller, [`Self::extract_calls_from_chunk`], fills the
    /// real origin. The whole-file path (`parse_file_all_inner`) collects
    /// candidates directly with the file in scope — both paths route candidates
    /// (the Lane-2 dual-path requirement).
    pub fn extract_calls_with_candidates(
        &self,
        source: &str,
        language: Language,
        start_byte: usize,
        end_byte: usize,
        line_offset: u32,
    ) -> (Vec<CallSite>, Vec<CandidateSite>) {
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

        let mut candidates = Vec::new();

        // Grammar-less languages (Markdown) — no tree-sitter call extraction
        if language.def().grammar.is_none() {
            return (vec![], candidates);
        }

        // Normalize CRLF → LF for consistency (callers typically pass normalized
        // source, but standalone callers like extract_calls_from_chunk may not)
        let source = if source.contains("\r\n") {
            std::borrow::Cow::Owned(source.replace("\r\n", "\n"))
        } else {
            std::borrow::Cow::Borrowed(source)
        };

        let Some(grammar) = language.try_grammar() else {
            return (vec![], candidates); // Grammar-less language — custom parser handles it
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
            return (vec![], candidates);
        }

        let tree = match crate::parser::parse_with_timeout(&mut parser, source.as_ref()) {
            Some(t) => t,
            None => {
                tracing::warn!(
                    %language,
                    start_byte,
                    end_byte,
                    "tree-sitter parse returned None in extract_calls"
                );
                return (vec![], candidates);
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
                return (vec![], candidates);
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

            // Intra-file fn/macro-name set: the precision gate for both the
            // bare-macro-arg edge (`m!(callback)`) and the fn-pointer-value
            // edge. Collected from the whole tree so a definition referenced
            // inside this range but declared outside it still resolves.
            let known_fns = collect_rust_fn_names(root, &source);

            calls.extend(extract_macro_call_edges(
                scope,
                &source,
                language,
                line_offset,
                &known_fns,
            ));

            // Fn-pointer / callback argument edges: `f(state, handler)`,
            // `set_handler(on_sigterm)`, `.map(parse_line)`, `register(m::h)`.
            // The function is a VALUE in argument position, invisible to the
            // call query. Deduped below against query-captured calls.
            calls.extend(extract_fn_pointer_arg_edges(
                scope,
                &source,
                language,
                line_offset,
                &known_fns,
            ));

            // --- Candidate edges (Lane 2): the references the confident passes
            // above DROP. Empty `file` here — `extract_calls_from_chunk` fills
            // the real origin. Cross-file bare fn-pointer args and
            // serde container / `with`-module callbacks. The serde scan
            // walks `scope`'s attribute_items, reaching container attributes
            // that precede the chunk's def node when `scope` is the file root;
            // for a tightly-scoped chunk node the leading container attr may sit
            // just outside `scope`, which the whole-file path covers.
            collect_fn_pointer_arg_candidates(
                scope,
                &source,
                language,
                line_offset,
                &known_fns,
                Path::new(""),
                &mut candidates,
            );
            collect_macro_arg_candidates(
                scope,
                &source,
                language,
                line_offset,
                &known_fns,
                Path::new(""),
                &mut candidates,
            );
            collect_serde_candidates(scope, &source, language, Path::new(""), &mut candidates);
        }

        // Deduplicate calls to the same function, keeping the MOST-TRUSTED kind
        // (lowest `trust_rank`: call < serde < macro < fn-pointer < doc-ref)
        // rather than the first occurrence. Call edges are emitted first today,
        // so first-wins happened to keep `call` — but that coupling is fragile,
        // and the consuming MIN-collapse queries already collapse on the same
        // trust rank. Picking the best kind here makes the per-chunk shape agree
        // with the cross-chunk collapse explicitly. First occurrence still wins
        // among equal-rank edges (stable line number).
        let mut best: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (i, c) in calls.iter().enumerate() {
            match best.get(&c.callee_name) {
                Some(&j) if calls[j].kind.trust_rank() <= c.kind.trust_rank() => {}
                _ => {
                    best.insert(c.callee_name.clone(), i);
                }
            }
        }
        let keep: std::collections::HashSet<usize> = best.into_values().collect();
        let mut idx = 0usize;
        calls.retain(|_| {
            let k = keep.contains(&idx);
            idx += 1;
            k
        });

        (calls, candidates)
    }

    /// Extract function calls from a parsed chunk
    /// Convenience method that extracts calls from the chunk's content.
    ///
    /// Thin wrapper over [`Self::extract_calls_and_candidates_from_chunk`] for
    /// callers that only need the confident edges; candidates are discarded.
    pub fn extract_calls_from_chunk(&self, chunk: &super::types::Chunk) -> Vec<CallSite> {
        self.extract_calls_and_candidates_from_chunk(chunk).0
    }

    /// Extract function calls AND low-confidence candidates from a parsed chunk.
    ///
    /// Per-chunk extractor consults `LanguageDef::chunk_call_parser` first.
    /// Markdown registers
    /// `extract_calls_from_markdown_chunk`; future grammar-less languages
    /// (SQL stored-proc cross-refs, L5X tag references, NL doc formats)
    /// can opt in by populating the field on their `LanguageDef`. Falls
    /// through to the tree-sitter call extractor when unset. The custom /
    /// markdown chunk-call parsers produce no candidates (the candidate kinds
    /// are Rust-only), so this surface emits candidates only for the
    /// tree-sitter path.
    ///
    /// The returned candidates carry `chunk.file` as their origin — the same
    /// path the persistence layer keys the `candidate_edges` rows by.
    pub fn extract_calls_and_candidates_from_chunk(
        &self,
        chunk: &super::types::Chunk,
    ) -> (Vec<CallSite>, Vec<CandidateSite>) {
        if let Some(def) = chunk.language.try_def() {
            if let Some(extractor) = def.chunk_call_parser {
                return (extractor(chunk), Vec::new());
            }
        }
        let (mut calls, mut candidates) = self.extract_calls_with_candidates(
            &chunk.content,
            chunk.language,
            0,
            chunk.content.len(),
            0, // No line offset since we're parsing the content directly
        );
        // Stamp the real origin on candidates (the chunk-range surface emits
        // them with an empty `file`). The persistence layer keys the
        // `candidate_edges.file` column by the normalized path, but carrying the
        // path here keeps `CandidateSite::file` descriptive and correct.
        for cand in &mut candidates {
            cand.file = chunk.file.clone();
        }
        // Mirror the serde string-callback edges emitted by the whole-file
        // Pass-2 walk (parse_file_relationships / parse_file_all_inner) so the
        // per-chunk shape stays in parity. Content is parsed standalone with
        // 1-indexed relative lines, so `line_offset = 1` puts an attribute on
        // the chunk's first content line at line 1 — matching the whole-file
        // path's relative-line conversion. Deduped against existing calls by
        // trust rank: a serde edge replaces an existing edge only when it is
        // strictly more trusted (e.g. it beats a macro/fn-pointer edge but
        // never an existing syntactic `call`).
        let mut by_name: std::collections::HashMap<String, usize> = calls
            .iter()
            .enumerate()
            .map(|(i, c)| (c.callee_name.clone(), i))
            .collect();
        for sc in extract_serde_callback_calls(&chunk.content, chunk.language, 1) {
            match by_name.get(&sc.callee_name) {
                Some(&i) => {
                    if sc.kind.trust_rank() < calls[i].kind.trust_rank() {
                        calls[i] = sc;
                    }
                }
                None => {
                    by_name.insert(sc.callee_name.clone(), calls.len());
                    calls.push(sc);
                }
            }
        }
        (calls, candidates)
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
    ///
    /// Thin wrapper over [`Self::parse_file_relationships_with_candidates`] that
    /// drops the Lane-2 candidate set.
    pub fn parse_file_relationships(
        &self,
        path: &Path,
    ) -> Result<(Vec<FunctionCalls>, Vec<ChunkTypeRefs>), ParserError> {
        let (calls, types, _candidates) = self.parse_file_relationships_with_candidates(path)?;
        Ok((calls, types))
    }

    /// Like [`Self::parse_file_relationships`] but also returns the file's
    /// low-confidence `candidate_edges` (Lane 2). Returns
    /// `(calls, type_refs, candidates)`. The candidate set holds the references
    /// the confident passes DROP (cross-file bare fn-pointer args,
    /// container-level / `with`-module serde callbacks) so they never
    /// enter `function_calls` (Lane 1's caller-graph invariant) yet are
    /// available to a query-time consumer (Lane 3). The whole-file serde scan
    /// reaches container attributes that the per-chunk range scan cannot.
    pub fn parse_file_relationships_with_candidates(
        &self,
        path: &Path,
    ) -> Result<RelationshipsWithCandidates, ParserError> {
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
                return Ok((vec![], vec![], vec![]));
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }

        // Read file
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!(path = %path.display(), "Skipping non-UTF8 file");
                return Ok((vec![], vec![], vec![]));
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
        // Grammar-less languages produce no candidates (the candidate kinds are
        // Rust-only), so they return an empty candidate set.
        if language.def().grammar.is_none() {
            if let Some(f) = language.def().custom_call_parser {
                let (calls, chunk_types) = f(&source, path, self)?;
                return Ok((calls, chunk_types, vec![]));
            }
            if let Some(f) = language.def().custom_all_parser {
                let (_chunks, calls, chunk_types) = f(&source, path, self)?;
                return Ok((calls, chunk_types, vec![]));
            }
            // Markdown (and any future grammar-less language
            // that opts into the default line-based parser)
            let md_calls = crate::parser::markdown::parse_markdown_references(&source, path)?;
            return Ok((md_calls, vec![], vec![]));
        }

        let grammar = language.try_grammar().ok_or_else(|| {
            ParserError::ParseFailed(format!("{} has no tree-sitter grammar", language))
        })?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| ParserError::ParseFailed(format!("{}", e)))?;

        let tree = crate::parser::parse_with_timeout(&mut parser, &source)
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
        let mut candidates: Vec<CandidateSite> = Vec::new();
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

        // Whole-file serde candidate pre-pass (Lane 2): CONTAINER-level
        // `#[serde(...)]` attributes sit OUTSIDE every item node's byte range
        // (prev-siblings of the struct/enum), so the per-chunk confident scan
        // never sees them — they need a file-root walk. `with = "module"` inner
        // (de)ser fns also land here. ABSOLUTE line numbers (no chunk offset).
        collect_serde_candidates(tree.root_node(), &source, language, path, &mut candidates);

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
            // does not subtract line_start), so pass line_offset = 0. `known_fns`
            // gates the bare-macro-arg case (`m!(callback)`).
            calls.extend(extract_macro_call_edges(
                node, &source, language, 0, &known_fns,
            ));

            // Fn-pointer / callback argument edges: a function passed as a VALUE
            // in argument position (`from_fn_with_state(state, touch_idle_clock)`,
            // `set_handler(on_sigterm)`, `.map(parse_line)`). Same ABSOLUTE line
            // convention as the macro pass → line_offset = 0.
            calls.extend(extract_fn_pointer_arg_edges(
                node, &source, language, 0, &known_fns,
            ));

            // Cross-file bare fn-pointer-arg + macro-arg candidates (Lane 2):
            // the bare-identifier references the confident passes above DROP
            // (not in `known_fns`). ABSOLUTE line numbers → line_offset = 0.
            collect_fn_pointer_arg_candidates(
                node,
                &source,
                language,
                0,
                &known_fns,
                path,
                &mut candidates,
            );
            collect_macro_arg_candidates(
                node,
                &source,
                language,
                0,
                &known_fns,
                path,
                &mut candidates,
            );

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
                        // Candidates inside the replaced outer container are
                        // dropped alongside its calls/types (the inner-language
                        // parse owns that range now; candidate kinds are
                        // Rust-only, so the inner parse contributes none).
                        candidates.retain(|c| {
                            !super::injection::chunk_within_container(
                                c.ref_line,
                                c.ref_line,
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

        Ok((call_results, type_results, candidates))
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
            let known_fns = collect_rust_fn_names(tree.root_node(), content);
            extract_macro_call_edges(tree.root_node(), content, Language::Rust, 0, &known_fns)
                .into_iter()
                .map(|c| c.callee_name)
                .collect()
        }

        /// Run ONLY the macro-arg candidate pass over the parsed source,
        /// returning `(callee_name, candidate_kind)` pairs.
        fn macro_candidates_for(content: &str) -> Vec<(String, String)> {
            let grammar = Language::Rust.try_grammar().unwrap();
            let mut ts = tree_sitter::Parser::new();
            ts.set_language(&grammar).unwrap();
            let tree = ts.parse(content, None).unwrap();
            let known_fns = collect_rust_fn_names(tree.root_node(), content);
            let mut out = Vec::new();
            collect_macro_arg_candidates(
                tree.root_node(),
                content,
                Language::Rust,
                0,
                &known_fns,
                std::path::Path::new(""),
                &mut out,
            );
            out.into_iter()
                .map(|c| (c.callee_name, c.candidate_kind))
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

        /// Collect, via the whole-file relationship path, every callee named by
        /// ANY `function_calls` row in the file (across all chunks). Used by the
        /// item-position macro-invocation tests below: the calls live inside a
        /// `proptest!{}` / `for_each_..!()` block that is now anchored as its own
        /// (NonCode) `MacroInvocation` chunk, so the edges show up on that chunk
        /// — not on any surrounding function. Asserting against the union of all
        /// callees in the file is the shape the dead-code resolver actually sees
        /// (it keys callee_name → chunk name across all rows).
        fn all_callees(content: &str) -> Vec<String> {
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();
            calls
                .iter()
                .flat_map(|fc| fc.calls.iter().map(|c| c.callee_name.clone()))
                .collect()
        }

        /// SLICE A — calls inside a `proptest!{}` body must reach the call graph.
        /// The whole `proptest! { ... }` block is one `macro_invocation` whose
        /// body is an opaque token-tree, so `fn prop_x` inside it is NOT a
        /// `function_item` chunk. Before the fix nothing chunked the block, so a
        /// function called ONLY from inside it (here `rewrite_all_checksums`)
        /// showed zero callers — a `cqs dead` false positive. Anchoring the block
        /// as a `MacroInvocation` chunk lets the token-tree pass attribute the
        /// inner calls. Covers BOTH the plain `f(args)` form AND the
        /// `x in generator()` strategy form.
        #[test]
        fn test_proptest_body_inner_calls_emit() {
            let content = r#"
fn rewrite_all_checksums(dir: &str, base: &str) {}
fn gen_stamp() -> u32 { 0 }

proptest! {
    #[test]
    fn prop_roundtrip(seed in gen_stamp()) {
        rewrite_all_checksums("dir", "index");
        let _ = seed;
    }
}
"#;
            let callees = all_callees(content);
            // Plain `f(args)` form inside the proptest body.
            assert!(
                callees.contains(&"rewrite_all_checksums".to_string()),
                "plain call inside proptest!{{}} body must emit an edge, got: {callees:?}"
            );
            // `x in generator()` strategy form: the generator call must emit.
            assert!(
                callees.contains(&"gen_stamp".to_string()),
                "`seed in gen_stamp()` strategy call must emit an edge, got: {callees:?}"
            );
        }

        /// SLICE A — the anchoring `MacroInvocation` chunk is NonCode, so it is
        /// never itself a dead-code candidate, and an expression-position macro
        /// (`println!` in a fn body) is NOT chunked separately — it stays inside
        /// its surrounding function chunk. This pins that the new chunk pattern
        /// is scoped to item position only.
        #[test]
        fn test_item_macro_chunk_is_noncode_and_scoped() {
            let content = r#"
fn helper() {}

fn body() {
    println!("{}", helper());
}

proptest! {
    #[test]
    fn p(x in 0u32..3) { let _ = x; helper(); }
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            // The proptest block is a MacroInvocation chunk named `proptest`.
            let mac = chunks
                .iter()
                .find(|c| c.chunk_type == ChunkType::MacroInvocation);
            assert!(
                mac.is_some(),
                "proptest!{{}} block should be a MacroInvocation chunk, got: {:?}",
                chunks
                    .iter()
                    .map(|c| (&c.name, c.chunk_type))
                    .collect::<Vec<_>>()
            );
            assert_eq!(mac.unwrap().name, "proptest");
            // It is NonCode → not callable, not code (so never a dead candidate).
            assert!(!ChunkType::MacroInvocation.is_callable());
            assert!(!ChunkType::MacroInvocation.is_code());
            // The expression-position `println!` produced NO MacroInvocation
            // chunk — `body` is the only function chunk and it carries the
            // helper() edge itself.
            let macro_chunks = chunks
                .iter()
                .filter(|c| c.chunk_type == ChunkType::MacroInvocation)
                .count();
            assert_eq!(
                macro_chunks, 1,
                "only the item-position proptest!{{}} is chunked, not the nested println!"
            );
        }

        /// SLICE B — a BARE identifier macro arg that names a same-file fn/macro
        /// emits an edge; a random bare ident does NOT. The production shape is
        /// `for_each_logged_batch_cmd!(gen_log_query_dispatch)`, where the
        /// `gen_log_query_dispatch` macro is passed by bare name (no following
        /// token_tree). The precision gate is the intra-file known-fns/macros set.
        #[test]
        fn test_bare_macro_arg_known_emits() {
            let content = r#"
macro_rules! gen_log_query_dispatch { () => {}; }
macro_rules! for_each_logged_batch_cmd { ($e:ident) => {}; }

for_each_logged_batch_cmd!(gen_log_query_dispatch);
"#;
            let callees = all_callees(content);
            assert!(
                callees.contains(&"gen_log_query_dispatch".to_string()),
                "bare known-macro arg `gen_log_query_dispatch` must emit an edge, got: {callees:?}"
            );
        }

        /// SLICE B precision pin — a bare ident macro arg that is NOT a known
        /// same-file fn/macro (an arbitrary token / variable name) must NOT emit
        /// a confident edge — but Lane 2 emits it as a `macro_arg_unresolved`
        /// CANDIDATE (the cross-file code-gen-macro arg). Edge and candidate are
        /// disjoint on `known_fns`, mirroring the fn-pointer bare-arg pair.
        #[test]
        fn test_bare_macro_arg_unknown_emitted_as_candidate_not_edge() {
            let content = r#"
macro_rules! some_macro { ($x:ident) => {}; }

some_macro!(not_a_defined_fn);
"#;
            let callees = all_callees(content);
            assert!(
                !callees.contains(&"not_a_defined_fn".to_string()),
                "an unknown bare macro arg must NOT emit a confident edge, got: {callees:?}"
            );
            let candidates = macro_candidates_for(content);
            assert!(
                candidates.contains(&(
                    "not_a_defined_fn".to_string(),
                    CANDIDATE_MACRO_ARG_UNRESOLVED.to_string()
                )),
                "an unknown bare macro arg must emit a macro_arg_unresolved candidate, got: {candidates:?}"
            );
        }

        /// Precision pins for the macro candidate: the path-segment and
        /// field-access guards must keep `m` (from `m::n`) and a `.field`
        /// receiver out of the candidate set, and a same-file known fn must be
        /// a confident edge (never a candidate).
        #[test]
        fn test_macro_arg_candidate_precision() {
            // `m::n` inside a macro: neither `m` nor `n` is a standalone
            // reference candidate (path segments).
            let path_content = r#"
macro_rules! m_macro { ($($t:tt)*) => {}; }

m_macro!(some_mod::some_item);
"#;
            let path_cands: Vec<String> = macro_candidates_for(path_content)
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            assert!(
                !path_cands.contains(&"some_mod".to_string()),
                "path leading segment `some_mod` must not be a macro candidate, got: {path_cands:?}"
            );

            // A same-file known fn passed bare is a confident edge, NOT a
            // candidate (disjointness).
            let known_content = r#"
fn local_fn() {}
macro_rules! k_macro { ($x:ident) => {}; }

k_macro!(local_fn);
"#;
            let known_cands: Vec<String> = macro_candidates_for(known_content)
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            assert!(
                !known_cands.contains(&"local_fn".to_string()),
                "a same-file known fn must be an edge, not a macro candidate, got: {known_cands:?}"
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
            // Non-Rust language whose text DOES contain the word `serde` AND a
            // callback-shaped attribute must STILL be ignored: the language
            // guard, not the cheap `contains("serde")` fast-path, is what keeps
            // the regex off non-Rust text. The case above omits `serde`, so it
            // only exercises the substring fast-path; this one pins the language
            // half of the `language != Rust || !contains("serde")` guard.
            assert!(
                extract_serde_callback_calls(
                    r#"# serde-style config
opts = {deserialize_with = "some_fn"}"#,
                    Language::Python,
                    1
                )
                .is_empty(),
                "a non-Rust source mentioning serde must not produce serde edges"
            );
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

        /// Container-level `#[serde(...)]` attributes (preceding `struct`/`enum`)
        /// sit OUTSIDE the item node's byte range in tree-sitter-rust, so they
        /// are NOT a confident `function_calls` edge — but Lane 2 emits
        /// them as a `serde_container` CANDIDATE via the whole-file attribute
        /// walk. A FIELD-level attribute on the same struct stays a confident
        /// edge (it lives inside the item body). This test pins all three halves:
        /// field edge present, container NOT an edge, container IS a candidate.
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
            let (calls, _types, candidates) = parser
                .parse_file_relationships_with_candidates(file.path())
                .unwrap();
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
            // Container-level attribute is out of the item range — NOT a
            // confident edge.
            assert!(
                !callees.contains(&"container_default"),
                "container-level serde attr must NOT be a confident edge, got: {:?}",
                callees
            );
            // Lane 2: container-level attr IS a `serde_container` candidate.
            let cand_pairs: Vec<(&str, &str)> = candidates
                .iter()
                .map(|c| (c.callee_name.as_str(), c.candidate_kind.as_str()))
                .collect();
            assert!(
                cand_pairs.contains(&("container_default", CANDIDATE_SERDE_CONTAINER)),
                "container-level serde attr must be a `serde_container` candidate, got: {cand_pairs:?}"
            );
            // The field-level callback is a confident edge, NOT a candidate —
            // candidates and edges are disjoint.
            assert!(
                !cand_pairs.iter().any(|(name, _)| *name == "field_de"),
                "field-level serde edge must NOT also be a candidate, got: {cand_pairs:?}"
            );
        }

        /// Lane 2: `#[serde(with = "module")]` links serde's derive to
        /// the module's `serialize` / `deserialize` free functions. The confident
        /// pass emits only the terminal segment as written (`module`); the inner
        /// (de)ser fns are unreachable, so they land as `serde_with_module`
        /// candidates (`serialize` + `deserialize`).
        #[test]
        fn test_serde_with_module_emits_inner_fn_candidates() {
            let content = r#"
#[derive(serde::Serialize, serde::Deserialize)]
struct Cfg {
    #[serde(with = "humantime_serde")]
    timeout: Duration,
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, _types, candidates) = parser
                .parse_file_relationships_with_candidates(file.path())
                .unwrap();
            let cand_pairs: Vec<(&str, &str)> = candidates
                .iter()
                .map(|c| (c.callee_name.as_str(), c.candidate_kind.as_str()))
                .collect();
            for inner in ["serialize", "deserialize"] {
                assert!(
                    cand_pairs.contains(&(inner, CANDIDATE_SERDE_WITH_MODULE)),
                    "`with = module` should emit `{inner}` as a serde_with_module candidate, got: {cand_pairs:?}"
                );
            }
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

        /// Run the whole-file relationship path and collect every candidate
        /// `(callee_name, candidate_kind)` pair for `content`.
        fn candidates_of(content: &str) -> Vec<(String, String)> {
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let (_calls, _types, candidates) = parser
                .parse_file_relationships_with_candidates(file.path())
                .unwrap();
            candidates
                .iter()
                .map(|c| (c.callee_name.clone(), c.candidate_kind.clone()))
                .collect()
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

        /// SLICE C — `signal(SIG, on_sigterm as *const () as sighandler_t)`:
        /// the fn name is wrapped in a `type_cast_expression` (here a DOUBLE
        /// cast). The cast arm unwraps to the inner `value` and re-applies the
        /// two-tier rule, so the known intra-file fn `on_sigterm` emits an edge.
        /// Fixes the production false-DEAD `on_sigterm` (the SIGTERM handler).
        #[test]
        fn test_fn_pointer_cast_arg() {
            let content = r#"
fn on_sigterm() {}

fn install() {
    signal(15, on_sigterm as *const () as usize);
}
"#;
            let callees = callees_of(content, "install");
            assert!(
                callees.contains(&"on_sigterm".to_string()),
                "on_sigterm in a `as`-cast arg should emit an edge, got: {callees:?}"
            );
        }

        /// SLICE C precision pin — a cast of an UNKNOWN bare ident (a variable
        /// coerced via `as`) must NOT emit; the cast arm still honours the
        /// known-fns gate for bare identifiers.
        #[test]
        fn test_fn_pointer_cast_unknown_not_emitted() {
            let content = r#"
fn install() {
    let ptr = 0usize;
    register(ptr as *const ());
}
"#;
            let callees = callees_of(content, "install");
            assert!(
                !callees.contains(&"ptr".to_string()),
                "a cast of the `ptr` variable must NOT emit an edge, got: {callees:?}"
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

        /// A CROSS-FILE bare fn-pointer arg (the fn is `use`d from another
        /// module, not defined in this file) is still NOT a confident
        /// `function_calls` edge — the intra-file known-fn filter can't see it,
        /// so emitting one would alias every same-named symbol in the index.
        /// Lane 2: it IS emitted as a `bare_arg_unresolved` CANDIDATE so
        /// a later query-time consumer (Lane 3) can surface it without it ever
        /// polluting the caller graph.
        #[test]
        fn test_cross_file_bare_arg_emitted_as_candidate_not_edge() {
            // `imported_handler` is referenced but never defined in this file,
            // so it isn't in the known-fn set → no confident edge, but a
            // candidate.
            let content = r#"
fn install() {
    set_handler(imported_handler);
}
"#;
            // Still NOT a confident edge (the precision gate holds).
            let callees = callees_of(content, "install");
            assert!(
                !callees.contains(&"imported_handler".to_string()),
                "cross-file bare fn-pointer arg must NOT be a confident edge, got: {callees:?}"
            );
            // BUT it IS present as a `bare_arg_unresolved` candidate (Lane 2).
            let candidates = candidates_of(content);
            assert!(
                candidates.contains(&(
                    "imported_handler".to_string(),
                    CANDIDATE_BARE_ARG_UNRESOLVED.to_string()
                )),
                "cross-file bare fn-pointer arg must be a `bare_arg_unresolved` \
                 candidate, got: {candidates:?}"
            );
        }
    }

    /// Lane-2 candidate emit must land on BOTH the chunk-range path
    /// (`extract_calls_with_candidates`) and the whole-file path
    /// (`parse_file_all_with_chunk_calls` / `parse_file_relationships_*`). A
    /// candidate emitted via one but not the other is an incomplete sweep — the
    /// production index pipeline uses the whole-file path while the
    /// grammar-less / injection inner-chunk routes use the chunk-range path, so
    /// both must route candidates.
    mod candidate_dual_path_tests {
        use super::*;

        const CONTENT: &str = r#"
fn install() {
    set_handler(imported_handler);
}
"#;

        /// Chunk-range path: `extract_calls_with_candidates` over the file's
        /// byte range emits the `bare_arg_unresolved` candidate.
        #[test]
        fn chunk_range_path_routes_candidate() {
            let parser = Parser::new().unwrap();
            let (_calls, candidates) =
                parser.extract_calls_with_candidates(CONTENT, Language::Rust, 0, CONTENT.len(), 0);
            assert!(
                candidates
                    .iter()
                    .any(|c| c.callee_name == "imported_handler"
                        && c.candidate_kind == CANDIDATE_BARE_ARG_UNRESOLVED),
                "chunk-range path must emit the bare_arg_unresolved candidate, got: {:?}",
                candidates
                    .iter()
                    .map(|c| (&c.callee_name, &c.candidate_kind))
                    .collect::<Vec<_>>()
            );
        }

        /// Whole-file path: the production `parse_file_all_with_chunk_calls`
        /// emits the SAME candidate.
        #[test]
        fn whole_file_path_routes_candidate() {
            let file = write_temp_file(CONTENT, "rs");
            let parser = Parser::new().unwrap();
            let (_chunks, _calls, _types, _chunk_calls, candidates) =
                parser.parse_file_all_with_chunk_calls(file.path()).unwrap();
            assert!(
                candidates
                    .iter()
                    .any(|c| c.callee_name == "imported_handler"
                        && c.candidate_kind == CANDIDATE_BARE_ARG_UNRESOLVED),
                "whole-file path must emit the bare_arg_unresolved candidate, got: {:?}",
                candidates
                    .iter()
                    .map(|c| (&c.callee_name, &c.candidate_kind))
                    .collect::<Vec<_>>()
            );
        }

        /// Both paths agree on the candidate set for the same source — the
        /// dual-path guarantee stated as an equivalence on the candidate names.
        #[test]
        fn both_paths_agree_on_candidate() {
            let parser = Parser::new().unwrap();
            let (_c, chunk_range) =
                parser.extract_calls_with_candidates(CONTENT, Language::Rust, 0, CONTENT.len(), 0);
            let file = write_temp_file(CONTENT, "rs");
            let (_ch, _ca, _t, _cc, whole_file) =
                parser.parse_file_all_with_chunk_calls(file.path()).unwrap();

            let mut a: Vec<(String, String)> = chunk_range
                .iter()
                .map(|c| (c.callee_name.clone(), c.candidate_kind.clone()))
                .collect();
            let mut b: Vec<(String, String)> = whole_file
                .iter()
                .map(|c| (c.callee_name.clone(), c.candidate_kind.clone()))
                .collect();
            a.sort();
            b.sort();
            assert_eq!(a, b, "chunk-range and whole-file candidate sets must agree");
        }
    }

    /// Both `pub` bare-parse sites in this module route their tree-sitter parse
    /// through `parse_with_timeout`, so an adversarial token stream that drives
    /// superlinear error recovery aborts on the `CQS_PARSER_TIMEOUT_MS` budget
    /// (returning the same shape an unparseable file already yields) instead of
    /// pinning the thread. These guard that wrapping: an abort yields the skip
    /// shape, and normal input is unaffected (behavior-neutral).
    mod call_parse_timeout {
        use super::*;
        use crate::parser::TIMEOUT_ENV_LOCK;

        /// Run `f` with `CQS_PARSER_TIMEOUT_MS` set/cleared, restoring the prior
        /// value. Holds the crate-shared lock so it can't race other cohorts
        /// that mutate the same process-global env var.
        fn with_timeout_env<F: FnOnce()>(value: Option<&str>, f: F) {
            let _g = TIMEOUT_ENV_LOCK.lock().unwrap();
            let prev = std::env::var("CQS_PARSER_TIMEOUT_MS").ok();
            match value {
                Some(v) => std::env::set_var("CQS_PARSER_TIMEOUT_MS", v),
                None => std::env::remove_var("CQS_PARSER_TIMEOUT_MS"),
            }
            f();
            match prev {
                Some(p) => std::env::set_var("CQS_PARSER_TIMEOUT_MS", p),
                None => std::env::remove_var("CQS_PARSER_TIMEOUT_MS"),
            }
        }

        /// ~4 MB bare-token error-recovery storm: uncapped this parses for
        /// multiple seconds. Under a tight budget the parse must abort.
        fn pathological_rust_source() -> String {
            let n = (4 * 1024 * 1024) / 4;
            format!("fn x(){{{}}}\n", "a b ".repeat(n))
        }

        /// A normal Rust source with one resolvable call — the behavior-neutral
        /// fixture (must parse under the default budget and emit the call).
        const NORMAL_SOURCE: &str = "fn hello() -> u32 { let x = compute(1, 2); x }\n";

        /// `extract_calls_with_candidates` honors the timeout: a pathological
        /// parse aborts under a tight budget and yields the skip shape
        /// (empty calls + empty candidates), the same `None`→skip the wrapper
        /// produces, rather than running the storm to completion.
        #[test]
        fn extract_calls_aborts_under_budget() {
            let src = pathological_rust_source();
            with_timeout_env(Some("1"), || {
                let parser = Parser::new().unwrap();
                let start = std::time::Instant::now();
                let (calls, candidates) =
                    parser.extract_calls_with_candidates(&src, Language::Rust, 0, src.len(), 0);
                let elapsed = start.elapsed();
                assert!(
                    calls.is_empty() && candidates.is_empty(),
                    "aborted parse must yield the skip shape (empty calls/candidates)"
                );
                assert!(
                    elapsed < std::time::Duration::from_secs(2),
                    "timeout must fire promptly; took {elapsed:?}"
                );
            });
        }

        /// Behavior-neutral: normal source parses under the default budget and
        /// `extract_calls_with_candidates` still emits the call.
        #[test]
        fn extract_calls_normal_input_unaffected() {
            with_timeout_env(None, || {
                let parser = Parser::new().unwrap();
                let (calls, _candidates) = parser.extract_calls_with_candidates(
                    NORMAL_SOURCE,
                    Language::Rust,
                    0,
                    NORMAL_SOURCE.len(),
                    0,
                );
                assert!(
                    calls.iter().any(|c| c.callee_name == "compute"),
                    "normal input must still extract the `compute` call, got: {:?}",
                    calls.iter().map(|c| &c.callee_name).collect::<Vec<_>>()
                );
            });
        }

        /// `parse_file_relationships_with_candidates` honors the timeout: a
        /// pathological file aborts under a tight budget and surfaces
        /// `ParseFailed` (the `.ok_or_else` arm), the same error an unparseable
        /// file yields, rather than running the storm to completion.
        #[test]
        fn parse_relationships_aborts_under_budget() {
            let src = pathological_rust_source();
            let file = write_temp_file(&src, "rs");
            with_timeout_env(Some("1"), || {
                let parser = Parser::new().unwrap();
                let start = std::time::Instant::now();
                let result = parser.parse_file_relationships_with_candidates(file.path());
                let elapsed = start.elapsed();
                assert!(
                    matches!(result, Err(ParserError::ParseFailed(_))),
                    "aborted parse must surface ParseFailed, got: {result:?}"
                );
                assert!(
                    elapsed < std::time::Duration::from_secs(2),
                    "timeout must fire promptly; took {elapsed:?}"
                );
            });
        }

        /// Behavior-neutral: a normal file parses under the default budget and
        /// `parse_file_relationships_with_candidates` returns its relationships.
        #[test]
        fn parse_relationships_normal_input_unaffected() {
            let file = write_temp_file(NORMAL_SOURCE, "rs");
            with_timeout_env(None, || {
                let parser = Parser::new().unwrap();
                let (function_calls, _types, _candidates) = parser
                    .parse_file_relationships_with_candidates(file.path())
                    .expect("normal file must parse within the budget");
                let callees: Vec<&str> = function_calls
                    .iter()
                    .flat_map(|fc| fc.calls.iter())
                    .map(|c| c.callee_name.as_str())
                    .collect();
                assert!(
                    callees.contains(&"compute"),
                    "normal input must still extract the `compute` call, got: {callees:?}"
                );
            });
        }
    }

    /// Regression guards for the unbounded-recursion stack-overflow DoS in the
    /// Pass-2 relationship-extraction tree-walks.
    ///
    /// `collect_macro_calls`, `collect_fn_pointer_args` (+ `emit_fn_pointer_arg`),
    /// and their candidate mirrors recurse one stack frame per tree level. An
    /// adversarial indexed file with a deeply-nested macro `token_tree` /
    /// parenthesized expression / array literal (a few KB of source, well under
    /// every size cap) drove the recursion deep enough to overflow the rayon
    /// parser-stage worker stack and SIGABRT the whole `cqs index` / `cqs watch`
    /// / daemon process. tree-sitter's own parse is iterative and tolerates the
    /// nesting cheaply; only our walks overflowed. The fix is a depth rail
    /// (`crate::limits::PARSER_MAX_WALK_DEPTH`, default 800) on each walk.
    ///
    /// Each guard parses a deeply-nested fixture and runs the walk **on a thread
    /// with a 1 MiB stack** — small enough that the UNFIXED recursive walk
    /// overflows it (aborting the test binary, a red failure), large enough that
    /// the depth-capped walk completes and the thread joins cleanly. They are the
    /// `calls.rs` siblings of `chunk.rs`'s `deep_tree_stack_safety` module
    /// (which guards the already-iterative `collect_comment_ranges` /
    /// `find_type_identifier_recursive`).
    mod deep_walk_stack_safety {
        use super::*;

        /// 1 MiB — below the unfixed walk's overflow threshold for the depths
        /// below (empirically the unfixed walk overflows a 1 MiB debug stack
        /// past ~1000 nesting levels), comfortably above the depth-800-capped
        /// walk's need.
        const SMALL_STACK_BYTES: usize = 1024 * 1024;

        /// Nesting depth for every fixture. ~20× the depth cap, so it would deep-
        /// recurse far past overflow on the unfixed code, but the fixed walk
        /// stops at the cap and returns.
        const NEST: usize = 16_000;

        fn run_on_small_stack<F: FnOnce() + Send + 'static>(f: F) {
            std::thread::Builder::new()
                .stack_size(SMALL_STACK_BYTES)
                .spawn(f)
                .expect("spawn parse thread")
                .join()
                .expect("walk thread overflowed its stack / panicked (unfixed depth guard?)");
        }

        fn parse_rust(src: &str) -> (tree_sitter::Tree, String) {
            let grammar = Language::Rust.try_grammar().expect("rust grammar");
            let mut ts = tree_sitter::Parser::new();
            ts.set_language(&grammar).expect("set language");
            let tree = ts.parse(src, None).expect("tree-sitter parse succeeds");
            (tree, src.to_string())
        }

        /// Deeply-nested parenthesized call argument: drives
        /// `collect_fn_pointer_args` / `emit_fn_pointer_arg` to tree depth.
        #[test]
        fn fn_pointer_walk_no_overflow_on_deep_nesting() {
            run_on_small_stack(|| {
                let src = format!(
                    "fn deep() {{ let _ = f({}1{}); }}\n",
                    "(".repeat(NEST),
                    ")".repeat(NEST)
                );
                let (tree, src) = parse_rust(&src);
                let known = collect_rust_fn_names(tree.root_node(), &src);
                // Must return (depth rail) rather than overflow the 1 MiB stack.
                let _edges =
                    extract_fn_pointer_arg_edges(tree.root_node(), &src, Language::Rust, 0, &known);
                let mut cands = Vec::new();
                collect_fn_pointer_arg_candidates(
                    tree.root_node(),
                    &src,
                    Language::Rust,
                    0,
                    &known,
                    std::path::Path::new("deep.rs"),
                    &mut cands,
                );
            });
        }

        /// Deeply-nested macro `token_tree`: drives `collect_macro_calls` and
        /// `collect_macro_arg_candidates_inner` to tree depth.
        #[test]
        fn macro_walk_no_overflow_on_deep_nesting() {
            run_on_small_stack(|| {
                let src = format!(
                    "fn deep() {{ my_macro!({}{}); }}\n",
                    "(".repeat(NEST),
                    ")".repeat(NEST)
                );
                let (tree, src) = parse_rust(&src);
                let known = collect_rust_fn_names(tree.root_node(), &src);
                let _edges =
                    extract_macro_call_edges(tree.root_node(), &src, Language::Rust, 0, &known);
                let mut cands = Vec::new();
                collect_macro_arg_candidates(
                    tree.root_node(),
                    &src,
                    Language::Rust,
                    0,
                    &known,
                    std::path::Path::new("deep.rs"),
                    &mut cands,
                );
            });
        }

        /// Behavior-neutrality pin: a *shallow* fixture (well under the depth
        /// cap) must still extract its fn-pointer edge after the rail was added.
        /// Guards against a future cap value (or off-by-one) that truncates real
        /// code.
        #[test]
        fn shallow_nesting_still_extracts_edge() {
            let src = "fn handler() {}\nfn build() { register(handler); }\n";
            let (tree, src) = parse_rust(src);
            let known = collect_rust_fn_names(tree.root_node(), &src);
            let edges =
                extract_fn_pointer_arg_edges(tree.root_node(), &src, Language::Rust, 0, &known);
            assert!(
                edges.iter().any(|c| c.callee_name == "handler"),
                "depth rail must not suppress the edge in shallow real code: {edges:?}"
            );
        }
    }
}
