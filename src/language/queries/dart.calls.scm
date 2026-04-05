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
