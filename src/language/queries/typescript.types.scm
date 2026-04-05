;; Param
(required_parameter type: (type_annotation (type_identifier) @param_type))
(required_parameter type: (type_annotation (generic_type name: (type_identifier) @param_type)))
(optional_parameter type: (type_annotation (type_identifier) @param_type))
(optional_parameter type: (type_annotation (generic_type name: (type_identifier) @param_type)))

;; Return
(function_declaration return_type: (type_annotation (type_identifier) @return_type))
(function_declaration return_type: (type_annotation (generic_type name: (type_identifier) @return_type)))
(method_definition return_type: (type_annotation (type_identifier) @return_type))
(method_definition return_type: (type_annotation (generic_type name: (type_identifier) @return_type)))
(arrow_function return_type: (type_annotation (type_identifier) @return_type))
(arrow_function return_type: (type_annotation (generic_type name: (type_identifier) @return_type)))

;; Field
(public_field_definition type: (type_annotation (type_identifier) @field_type))
(public_field_definition type: (type_annotation (generic_type name: (type_identifier) @field_type)))
(property_signature type: (type_annotation (type_identifier) @field_type))
(property_signature type: (type_annotation (generic_type name: (type_identifier) @field_type)))

;; Impl (extends/implements)
(class_heritage (extends_clause value: (identifier) @impl_type))
(class_heritage (implements_clause (type_identifier) @impl_type))
(extends_type_clause (type_identifier) @impl_type)

;; Bound (type parameter constraints)
(constraint (type_identifier) @bound_type)

;; Alias
(type_alias_declaration value: (type_identifier) @alias_type)
(type_alias_declaration value: (generic_type name: (type_identifier) @alias_type))

;; Catch-all
(type_identifier) @type_ref
