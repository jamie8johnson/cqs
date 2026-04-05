;; Param
(formal_parameter type: (type_identifier) @param_type)
(formal_parameter type: (generic_type (type_identifier) @param_type))
(formal_parameter type: (scoped_type_identifier (type_identifier) @param_type))
(formal_parameter type: (array_type element: (type_identifier) @param_type))
(spread_parameter (type_identifier) @param_type)
(spread_parameter (generic_type (type_identifier) @param_type))

;; Return
(method_declaration type: (type_identifier) @return_type)
(method_declaration type: (generic_type (type_identifier) @return_type))
(method_declaration type: (scoped_type_identifier (type_identifier) @return_type))
(method_declaration type: (array_type element: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (generic_type (type_identifier) @field_type))
(field_declaration type: (scoped_type_identifier (type_identifier) @field_type))
(field_declaration type: (array_type element: (type_identifier) @field_type))

;; Impl (extends/implements)
(superclass (type_identifier) @impl_type)
(super_interfaces (type_list (type_identifier) @impl_type))

;; Bound (type parameter bounds)
(type_bound (type_identifier) @bound_type)

;; Catch-all
(type_identifier) @type_ref
