(function_item
  name: (identifier) @name) @function

(struct_item
  name: (type_identifier) @name) @struct

(enum_item
  name: (type_identifier) @name) @enum

(trait_item
  name: (type_identifier) @name) @trait

;; Impl blocks
(impl_item
  type: (type_identifier) @name) @impl

(const_item
  name: (identifier) @name) @const

(static_item
  name: (identifier) @name) @const

(macro_definition
  name: (identifier) @name) @macro

(type_item
  name: (type_identifier) @name) @typealias
