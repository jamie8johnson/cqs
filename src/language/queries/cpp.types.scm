;; Parameter types
(parameter_declaration type: (type_identifier) @param_type)
(parameter_declaration type: (qualified_identifier name: (type_identifier) @param_type))

;; Return types
(function_definition type: (type_identifier) @return_type)
(function_definition type: (qualified_identifier name: (type_identifier) @return_type))

;; Field types
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (qualified_identifier name: (type_identifier) @field_type))

;; Base class / inheritance
(base_class_clause (type_identifier) @impl_type)
(base_class_clause (qualified_identifier name: (type_identifier) @impl_type))
(base_class_clause (template_type name: (type_identifier) @impl_type))

;; Template arguments
(template_argument_list (type_identifier) @type_ref)

;; Using alias source type (alias_declaration wraps in type_descriptor)
(alias_declaration type: (type_descriptor type: (type_identifier) @alias_type))

;; Catch-all
(type_identifier) @type_ref
