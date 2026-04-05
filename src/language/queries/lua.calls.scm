;; Direct function calls (foo())
(function_call
  name: (identifier) @callee)

;; Method calls (obj:method())
(function_call
  name: (method_index_expression
    method: (identifier) @callee))
