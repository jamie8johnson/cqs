;; Direct function call: foo(args)
(call_expression_with_bareword
  function_name: (identifier) @callee)

;; Method call: $obj->method(args) or Package->method(args)
(method_invocation
  function_name: (identifier) @callee)
