;; Param
(parameter type: (type_identifier) @param_type)
(parameter type: (generic_type type: (type_identifier) @param_type))
(parameter type: (reference_type type: (type_identifier) @param_type))
(parameter type: (reference_type type: (generic_type type: (type_identifier) @param_type)))
(parameter type: (scoped_type_identifier name: (type_identifier) @param_type))

;; Return
(function_item return_type: (type_identifier) @return_type)
(function_item return_type: (generic_type type: (type_identifier) @return_type))
(function_item return_type: (reference_type type: (type_identifier) @return_type))
(function_item return_type: (reference_type type: (generic_type type: (type_identifier) @return_type)))
(function_item return_type: (scoped_type_identifier name: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (generic_type type: (type_identifier) @field_type))
(field_declaration type: (reference_type type: (type_identifier) @field_type))
(field_declaration type: (reference_type type: (generic_type type: (type_identifier) @field_type)))
(field_declaration type: (scoped_type_identifier name: (type_identifier) @field_type))

;; Impl
(impl_item type: (type_identifier) @impl_type)
(impl_item type: (generic_type type: (type_identifier) @impl_type))
(impl_item trait: (type_identifier) @impl_type)
(impl_item trait: (scoped_type_identifier name: (type_identifier) @impl_type))

;; Bound
(trait_bounds (type_identifier) @bound_type)
(trait_bounds (scoped_type_identifier name: (type_identifier) @bound_type))

;; Alias
(type_item type: (type_identifier) @alias_type)
(type_item type: (generic_type type: (type_identifier) @alias_type))

;; Catch-all (captures types inside generics, type_arguments, etc.)
(type_identifier) @type_ref
