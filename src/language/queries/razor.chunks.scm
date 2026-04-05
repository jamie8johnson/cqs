;; Methods (inside @code blocks)
(method_declaration name: (identifier) @name) @function
(constructor_declaration name: (identifier) @name) @function
(local_function_statement name: (identifier) @name) @function

;; Properties and fields
(property_declaration name: (identifier) @name) @property
(field_declaration
  (variable_declaration
    (variable_declarator (identifier) @name))) @property

;; Types
(class_declaration name: (identifier) @name) @class
(struct_declaration name: (identifier) @name) @struct
(record_declaration name: (identifier) @name) @struct
(interface_declaration name: (identifier) @name) @interface
(enum_declaration name: (identifier) @name) @enum

;; DI injections: @inject IService ServiceName
(razor_inject_directive
  (variable_declaration
    (variable_declarator (identifier) @name))) @property

;; @code / @functions blocks (name assigned by post-process)
(razor_block) @module

;; HTML elements (tag name extracted by post-process, noise filtered)
(element) @section
