;; Top-level function: function_signature as direct child of source_file
;; The signature has the name; the adjacent function_body has the implementation
(source_file
  (function_signature
    name: (identifier) @name) @function)

;; Class declarations
(class_declaration
  name: (identifier) @name) @class

;; Enum declarations
(enum_declaration
  name: (identifier) @name) @enum

;; Mixin declarations
(mixin_declaration
  name: (identifier) @name) @class

;; Extension declarations
(extension_declaration
  name: (identifier) @name) @class

;; Extension type declarations
(extension_type_declaration
  name: (identifier) @name) @class

;; Methods inside class/mixin/extension bodies
(class_member
  (method_signature
    (function_signature
      name: (identifier) @name)) @function)

;; Getter signatures (top-level or in class)
(getter_signature
  name: (identifier) @name) @property

;; Setter signatures
(setter_signature
  name: (identifier) @name) @property

;; Top-level constants (static final)
(source_file
  (static_final_declaration_list
    (static_final_declaration
      name: (identifier) @name)) @const)

;; Enum constants
(enum_constant
  name: (identifier) @name) @const
