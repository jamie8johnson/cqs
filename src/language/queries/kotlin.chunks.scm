;; Classes (regular, data, sealed, abstract) and interfaces
;; post_process_chunk reclassifies interfaces and enum classes
(class_declaration
  (identifier) @name) @class

;; Object declarations (singletons)
(object_declaration
  (identifier) @name) @object

;; Functions
(function_declaration
  (identifier) @name) @function

;; Secondary constructors — post_process_chunk reclassifies to Constructor
(secondary_constructor) @function

;; Init blocks — post_process_chunk reclassifies to Constructor
(anonymous_initializer) @function

;; Property declarations (val/var)
(property_declaration
  (variable_declaration
    (identifier) @name)) @property

;; Type aliases
(type_alias
  (identifier) @name) @typealias
