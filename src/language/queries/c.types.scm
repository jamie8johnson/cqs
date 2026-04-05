;; Param
(parameter_declaration type: (type_identifier) @param_type)
(parameter_declaration type: (struct_specifier name: (type_identifier) @param_type))
(parameter_declaration type: (enum_specifier name: (type_identifier) @param_type))

;; Return
(function_definition type: (type_identifier) @return_type)
(function_definition type: (struct_specifier name: (type_identifier) @return_type))
(function_definition type: (enum_specifier name: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (struct_specifier name: (type_identifier) @field_type))
(field_declaration type: (enum_specifier name: (type_identifier) @field_type))

;; Alias (typedef)
(type_definition type: (type_identifier) @alias_type)
(type_definition type: (struct_specifier name: (type_identifier) @alias_type))
(type_definition type: (enum_specifier name: (type_identifier) @alias_type))

;; Catch-all
(type_identifier) @type_ref
