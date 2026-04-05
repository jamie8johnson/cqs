;; Param
(parameter_declaration type: (type_identifier) @param_type)
(parameter_declaration type: (pointer_type (type_identifier) @param_type))
(parameter_declaration type: (qualified_type name: (type_identifier) @param_type))
(parameter_declaration type: (generic_type type: (type_identifier) @param_type))
(parameter_declaration type: (slice_type element: (type_identifier) @param_type))

;; Return
(function_declaration result: (type_identifier) @return_type)
(function_declaration result: (pointer_type (type_identifier) @return_type))
(function_declaration result: (qualified_type name: (type_identifier) @return_type))
(function_declaration result: (generic_type type: (type_identifier) @return_type))
(method_declaration result: (type_identifier) @return_type)
(method_declaration result: (pointer_type (type_identifier) @return_type))
(method_declaration result: (qualified_type name: (type_identifier) @return_type))
(method_declaration result: (generic_type type: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (pointer_type (type_identifier) @field_type))
(field_declaration type: (qualified_type name: (type_identifier) @field_type))
(field_declaration type: (generic_type type: (type_identifier) @field_type))
(field_declaration type: (slice_type element: (type_identifier) @field_type))

;; Impl (interface embedding — embedded types wrapped in type_elem)
(interface_type (type_elem (type_identifier) @impl_type))
(interface_type (type_elem (qualified_type name: (type_identifier) @impl_type)))

;; Alias (type definitions and type aliases)
(type_spec type: (type_identifier) @alias_type)
(type_spec type: (generic_type type: (type_identifier) @alias_type))
(type_alias type: (type_identifier) @alias_type)

;; Catch-all
(type_identifier) @type_ref
