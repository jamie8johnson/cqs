;; Direct function calls
(call_expression
  function: (identifier) @callee)

;; Member function calls (obj.method())
(call_expression
  function: (field_expression
    member: (identifier) @callee))
