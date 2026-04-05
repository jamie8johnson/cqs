;; Param — method parameters
(parameter type: (identifier) @param_type)
(parameter type: (generic_name (identifier) @param_type))
(parameter type: (qualified_name (identifier) @param_type))
(parameter type: (nullable_type (identifier) @param_type))
(parameter type: (array_type (identifier) @param_type))

;; Return
(method_declaration returns: (identifier) @return_type)
(method_declaration returns: (generic_name (identifier) @return_type))
(method_declaration returns: (qualified_name (identifier) @return_type))
(method_declaration returns: (nullable_type (identifier) @return_type))
(local_function_statement type: (identifier) @return_type)
(local_function_statement type: (generic_name (identifier) @return_type))

;; Field — field declarations and property types
(field_declaration (variable_declaration type: (identifier) @field_type))
(field_declaration (variable_declaration type: (generic_name (identifier) @field_type)))
(property_declaration type: (identifier) @field_type)
(property_declaration type: (generic_name (identifier) @field_type))

;; Impl — base class, interface implementations
(base_list (identifier) @impl_type)
(base_list (generic_name (identifier) @impl_type))
(base_list (qualified_name (identifier) @impl_type))

;; Bound — generic constraints (where T : IFoo)
(type_parameter_constraint (type (identifier) @bound_type))
(type_parameter_constraint (type (generic_name (identifier) @bound_type)))
