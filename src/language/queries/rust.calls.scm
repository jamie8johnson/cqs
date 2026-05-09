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
;
; AC-V1.40-4: anchored to the FIRST argument identifier via the `.`
; predicate. Pre-fix the pattern matched every `(identifier)` directly
; under `arguments`, so `Foo { handler: wrap(my_func, count, &SHARED) }`
; produced three phantom call-graph edges — the surrounding chunk's
; `→ count` edge falsely kept any global named `count` alive in
; cqs-dead. Wrapper functions in dispatch tables (`Some`, `Box::new`,
; `Rc::new`) all take a single function argument, so anchoring to the
; first child is safe for the typical case. Multi-arg wrappers that
; legitimately want every-identifier capture would be a separate
; pattern (none exist today).
(field_initializer
  value: (call_expression
    arguments: (arguments . (identifier) @callee)))

(field_initializer
  value: (call_expression
    arguments: (arguments . (scoped_identifier
      name: (identifier) @callee))))

; And `Some(fn_path as fn(T) -> R)` casts — Rust dispatch tables
; sometimes use `as` to coerce a fn-item to a fn-pointer type before
; storing in an Option<fn(...) -> ...> field. Closes the remaining
; 14 `post_process_*_*` false positives in src/language/languages.rs.
;
; AC-V1.40-4: same first-argument anchor as the un-cast variants
; above so multi-arg wrapper calls don't pollute `function_calls`.
(field_initializer
  value: (call_expression
    arguments: (arguments
      . (type_cast_expression
        value: (identifier) @callee))))

(field_initializer
  value: (call_expression
    arguments: (arguments
      . (type_cast_expression
        value: (scoped_identifier
          name: (identifier) @callee)))))
