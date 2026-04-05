;; Parameter types
(parameter
  type: (type_identifier) @param_type)
(parameter
  type: (generic_type (type_identifier) @param_type))

;; Return types
(function_definition
  return_type: (type_identifier) @return_type)
(function_definition
  return_type: (generic_type (type_identifier) @return_type))

;; Field types — val/var type annotations
(val_definition
  type: (type_identifier) @field_type)
(val_definition
  type: (generic_type (type_identifier) @field_type))
(var_definition
  type: (type_identifier) @field_type)
(var_definition
  type: (generic_type (type_identifier) @field_type))

;; Extends clauses (class Foo extends Bar)
(extends_clause
  type: (type_identifier) @impl_type)
(extends_clause
  type: (generic_type (type_identifier) @impl_type))

;; Catch-all — generic type arguments
(type_arguments
  (type_identifier) @type_ref)
