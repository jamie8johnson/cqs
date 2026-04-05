;; Parameter types
(parameter
  (user_type (identifier) @param_type))

;; Return types
(function_declaration
  (user_type (identifier) @return_type))

;; Property types
(property_declaration
  (user_type (identifier) @field_type))

;; Superclass / interface implementations
(delegation_specifier
  (user_type (identifier) @impl_type))

;; Type alias right-hand side
(type_alias
  (user_type (identifier) @alias_type))

;; Generic type arguments
(type_arguments
  (type_projection
    (user_type (identifier) @type_ref)))

;; Catch-all
(user_type (identifier) @type_ref)
