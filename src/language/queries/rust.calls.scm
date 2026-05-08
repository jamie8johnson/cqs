(call_expression
  function: (identifier) @callee)

(call_expression
  function: (field_expression
    field: (field_identifier) @callee))

(call_expression
  function: (scoped_identifier
    name: (identifier) @callee))

(macro_invocation
  macro: (identifier) @callee)

; #1573 Tier 2a: struct-field-assignment edges. When a struct literal
; field is initialized with a function path (`field: my_func`,
; `field: Some(my_func)`, `field: module::my_func`), the function
; reference doesn't appear in a `call_expression` but IS a real use.
; Pre-fix, `cqs dead` reported 66 false positives on cqs main from
; functions assigned to LanguageDef fields (strip_go_receiver,
; extract_return_c, etc.) that the existing query missed.
;
; Capturing every field_initializer value also produces dangling
; edges for non-function values (e.g. `field: local_var`), but
; those reference no chunk in the index and are filtered out by
; the `function_calls` JOIN against the chunks table at query time.

(field_initializer
  value: (identifier) @callee)

(field_initializer
  value: (scoped_identifier
    name: (identifier) @callee))

; And inside `Some(fn_path)` / `Box::new(fn_path)` wrappers — common
; for `Option<fn>` field types in dispatch tables.
(field_initializer
  value: (call_expression
    arguments: (arguments (identifier) @callee)))

(field_initializer
  value: (call_expression
    arguments: (arguments (scoped_identifier
      name: (identifier) @callee))))
