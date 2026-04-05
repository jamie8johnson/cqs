;; Variable declarations with basic or derived types
(var_decl_item
  (basic_data_type) @param_type)

(var_decl_item
  (derived_data_type) @param_type)

;; Array element types
(array_type
  (basic_data_type) @field_type)
(array_type
  (derived_data_type) @field_type)

;; Struct fields
(struct_field
  (basic_data_type) @field_type)
(struct_field
  (derived_data_type) @field_type)

;; FUNCTION_BLOCK EXTENDS
(function_block_definition
  base: (identifier) @impl_type)

;; Function/method return types
(function_definition
  (basic_data_type) @return_type)
(function_definition
  (derived_data_type) @return_type)
(method_definition
  (basic_data_type) @return_type)
(method_definition
  (derived_data_type) @return_type)
