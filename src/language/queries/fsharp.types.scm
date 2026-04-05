;; Record field types
(record_field
  (type
    (long_identifier
      (identifier) @field_type)))

;; Parameter types in typed patterns: (x: int)
(typed_pattern
  (type
    (long_identifier
      (identifier) @param_type)))

;; Inheritance
(class_inherits_decl
  (type
    (long_identifier
      (identifier) @impl_type)))

;; Interface implementation
(interface_implementation
  (type
    (long_identifier
      (identifier) @impl_type)))

;; Constraint types
(constraint
  (type
    (long_identifier
      (identifier) @bound_type)))
