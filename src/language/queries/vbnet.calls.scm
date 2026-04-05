;; Method calls: obj.Method(args)
(invocation
  target: (member_access
    member: (identifier) @callee))

;; Bare calls: Method(args)
(invocation
  target: (identifier) @callee)

;; Object creation: New ClassName(args) / New ClassName()
(new_expression
  type: (type (namespace_name (identifier) @callee)))
(new_expression
  type: (type (generic_type (namespace_name (identifier) @callee))))
