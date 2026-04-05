;; Direct function application: `foo arg`
(apply_expression
  function: (variable_expression
    name: (identifier) @callee))

;; Qualified function application: `lib.mkDerivation arg`
(apply_expression
  function: (select_expression
    attrpath: (attrpath) @callee))
