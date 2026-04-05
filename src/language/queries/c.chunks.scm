(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @function

(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

(enum_specifier
  name: (type_identifier) @name
  body: (enumerator_list)) @enum

(type_definition
  declarator: (type_identifier) @name) @typealias

(declaration
  declarator: (init_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @function

;; Union definitions
(union_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

;; Preprocessor constants (#define FOO 42)
(preproc_def
  name: (identifier) @name) @const

;; Preprocessor function macros (#define FOO(x) ...)
(preproc_function_def
  name: (identifier) @name) @macro
