;; Direct function calls
(call_expression
  (identifier) @callee)

;; Method calls (object.method())
(call_expression
  (navigation_expression
    (identifier) @callee))
