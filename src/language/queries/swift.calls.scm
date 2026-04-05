;; Direct function calls
(call_expression
  (simple_identifier) @callee)

;; Method calls via navigation
(call_expression
  (navigation_expression
    (navigation_suffix
      (simple_identifier) @callee)))
