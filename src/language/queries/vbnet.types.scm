;; Param — method parameters (ByVal/ByRef p As Type)
(parameter
  (as_clause type: (type (namespace_name (identifier) @param_type))))
(parameter
  (as_clause type: (type (generic_type (namespace_name (identifier) @param_type)))))

;; Return — Function ... As Type (return_type field is type, not as_clause)
(method_declaration
  return_type: (type (namespace_name (identifier) @return_type)))
(method_declaration
  return_type: (type (generic_type (namespace_name (identifier) @return_type))))

;; Field — Dim/Private field As Type
(field_declaration
  (variable_declarator
    (as_clause type: (type (namespace_name (identifier) @field_type)))))
(field_declaration
  (variable_declarator
    (as_clause type: (type (generic_type (namespace_name (identifier) @field_type))))))

;; Property — Property Name As Type
(property_declaration
  (as_clause type: (type (namespace_name (identifier) @field_type))))
(property_declaration
  (as_clause type: (type (generic_type (namespace_name (identifier) @field_type)))))

;; Impl — Inherits / Implements
(inherits_clause (type (namespace_name (identifier) @impl_type)))
(inherits_clause (type (generic_type (namespace_name (identifier) @impl_type))))
(implements_clause (type (namespace_name (identifier) @impl_type)))
(implements_clause (type (generic_type (namespace_name (identifier) @impl_type))))

;; Bound — generic type constraint (Of T As IFoo)
(type_constraint (type (namespace_name (identifier) @bound_type)))
(type_constraint (type (generic_type (namespace_name (identifier) @bound_type))))

;; Imports
(imports_statement namespace: (namespace_name (identifier) @alias_type))
