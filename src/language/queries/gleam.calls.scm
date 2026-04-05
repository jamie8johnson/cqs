;; Direct function call: foo(args)
(function_call
  function: (identifier) @callee)

;; Qualified/module call: module.func(args)
(function_call
  function: (field_access
    field: (label) @callee))
