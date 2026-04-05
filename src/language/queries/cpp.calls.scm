;; Direct function call
(call_expression
  function: (identifier) @callee)

;; Qualified call (Class::method or ns::func)
(call_expression
  function: (qualified_identifier
    name: (identifier) @callee))

;; Member call (obj.method or ptr->method)
(call_expression
  function: (field_expression
    field: (field_identifier) @callee))

;; Template function call (make_shared<T>())
(call_expression
  function: (template_function
    name: (identifier) @callee))

;; Qualified template call (std::make_shared<T>())
(call_expression
  function: (qualified_identifier
    name: (template_function
      name: (identifier) @callee)))

;; new expression
(new_expression
  type: (type_identifier) @callee)
