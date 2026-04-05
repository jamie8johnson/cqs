;; Parameter types
(parameter
  (user_type
    (type_identifier) @param_type))

;; Return types (after ->)
(function_declaration
  (user_type
    (type_identifier) @return_type))

;; Property types
(property_declaration
  (user_type
    (type_identifier) @field_type))

;; Protocol conformance / inheritance
(inheritance_specifier
  (user_type
    (type_identifier) @impl_type))

;; Catch-all
(user_type
  (type_identifier) @type_ref)
