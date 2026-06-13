(function_item
  name: (identifier) @name) @function

(struct_item
  name: (type_identifier) @name) @struct

(enum_item
  name: (type_identifier) @name) @enum

(trait_item
  name: (type_identifier) @name) @trait

;; Impl blocks
(impl_item
  type: (type_identifier) @name) @impl

(const_item
  name: (identifier) @name) @const

(static_item
  name: (identifier) @name) @const

(macro_definition
  name: (identifier) @name) @macro

;; Item-position macro INVOCATIONS (`proptest! { ... }`, `define_languages! { ... }`,
;; `for_each_logged_batch_cmd!(gen_dispatch);`).
;; In tree-sitter-rust the body is an opaque `token_tree`: a `fn` / call / bare
;; fn-or-macro arg inside it is NOT a `function_item` / `call_expression`, so
;; without an anchoring chunk the relationship walk never reaches those tokens and
;; any function called ONLY from inside the block (e.g. a `proptest!` helper, or a
;; bare callback passed to a code-gen macro) shows zero callers — a `cqs dead`
;; false positive. Anchoring this block as a (NonCode) chunk lets the macro
;; token-tree call pass attribute its inner calls.
;;
;; tree-sitter-rust rejects a query that constrains the parent of an
;; `expression_statement` (Impossible pattern), so the item-vs-expression scope
;; cannot be expressed purely in the query for the statement form `m!(...);`.
;; Instead this captures EVERY `macro_invocation` and the Rust `post_process_chunk`
;; hook (`is_item_scope_macro_invocation`) discards any whose ancestor chain is
;; NOT item scope — so an expression-position `println!` inside a fn body is
;; dropped and stays inside its surrounding function chunk, while item-position
;; `proptest! { ... }` / `for_each_logged_batch_cmd!(...);` are kept.
(macro_invocation
  macro: (identifier) @name) @macro_invocation

(type_item
  name: (type_identifier) @name) @typealias
