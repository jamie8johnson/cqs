;; Param
(typed_parameter type: (type (identifier) @param_type))
(typed_parameter type: (type (generic_type (identifier) @param_type)))
(typed_default_parameter type: (type (identifier) @param_type))
(typed_default_parameter type: (type (generic_type (identifier) @param_type)))

;; Return
(function_definition return_type: (type (identifier) @return_type))
(function_definition return_type: (type (generic_type (identifier) @return_type)))

;; Field
(assignment type: (type (identifier) @field_type))
(assignment type: (type (generic_type (identifier) @field_type)))

;; Impl (class inheritance)
(class_definition superclasses: (argument_list (identifier) @impl_type))

;; Alias (PEP 695)
(type_alias_statement (type (identifier) @alias_type))

;; Catch-all (scoped to type positions)
(type (identifier) @type_ref)
