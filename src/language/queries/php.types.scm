;; Parameter types (function foo(Type $param))
(simple_parameter
  type: (named_type (name) @param_type))

;; Return types (function foo(): Type)
(function_definition
  return_type: (named_type (name) @return_type))
(method_declaration
  return_type: (named_type (name) @return_type))

;; Property types (public Type $prop)
(property_declaration
  type: (named_type (name) @field_type))

;; Extends (class Foo extends Bar)
(base_clause
  (name) @impl_type)

;; Implements (class Foo implements Bar, Baz)
(class_interface_clause
  (name) @impl_type)

;; Catch-all for named types
(named_type (name) @type_ref)
