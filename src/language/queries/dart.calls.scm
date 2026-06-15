;; Constructor calls: new/const Type(args)
(new_expression
  type: (type_identifier) @callee)

(const_object_expression
  type: (type_identifier) @callee)

;; Constructor invocations: Type.named(args)
(constructor_invocation
  constructor: (identifier) @callee)

;; Method calls via selectors: object.method
(unconditional_assignable_selector
  "."
  (identifier) @callee)

(conditional_assignable_selector
  "?."
  (identifier) @callee)

;; Plain function/constructor calls: an identifier whose immediately-following
;; sibling is an argument selector — `helper()`, `add(1, 2)`, `Greeter("x")`.
;; The grammar represents `f(args)` as a `_primary` (identifier) followed by a
;; `selector` containing an `argument_part`, with no enclosing call node, so the
;; callee is matched by the anchored sibling sequence.
(
  (identifier) @callee
  .
  (selector (argument_part)))
