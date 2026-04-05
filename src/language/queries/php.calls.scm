;; Regular function calls
(function_call_expression
  function: (name) @callee)

;; Method calls ($obj->method())
(member_call_expression
  name: (name) @callee)

;; Static calls (Class::method())
(scoped_call_expression
  name: (name) @callee)

;; Constructor calls (new ClassName)
(object_creation_expression
  (name) @callee)
