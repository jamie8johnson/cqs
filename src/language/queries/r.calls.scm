;; Direct function calls (foo(args))
(call
  function: (identifier) @callee)

;; Namespaced calls (pkg::func())
(call
  function: (namespace_operator
    rhs: (identifier) @callee))
