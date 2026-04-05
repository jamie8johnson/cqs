(method_declaration
  name: (identifier) @name) @function

(constructor_declaration
  name: (identifier) @name) @function

(class_declaration
  name: (identifier) @name) @class

(interface_declaration
  name: (identifier) @name) @interface

(enum_declaration
  name: (identifier) @name) @enum

(record_declaration
  name: (identifier) @name) @struct

;; Annotation types (@interface)
(annotation_type_declaration
  name: (identifier) @name) @interface

;; Fields (class-level only — local vars are local_variable_declaration)
(field_declaration
  declarator: (variable_declarator
    name: (identifier) @name)) @property
