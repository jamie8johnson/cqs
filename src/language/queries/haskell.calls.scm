;; Direct function application: foo arg
(apply
  function: (variable) @callee)

;; Qualified function call: Module.func arg
(apply
  function: (qualified
    id: (variable) @callee))
