;; Free functions
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @function

;; Inline methods (field_identifier inside class body)
(function_definition
  declarator: (function_declarator
    declarator: (field_identifier) @name)) @function

;; Out-of-class methods (Class::method)
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (identifier) @name))) @function

;; Destructors (inline)
(function_definition
  declarator: (function_declarator
    declarator: (destructor_name) @name)) @function

;; Destructors (out-of-class, Class::~Class)
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (destructor_name) @name))) @function

;; Forward declarations with function body (rare)
(declaration
  declarator: (init_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @function

;; Classes
(class_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @class

;; Structs
(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

;; Enums (including enum class)
(enum_specifier
  name: (type_identifier) @name
  body: (enumerator_list)) @enum

;; Namespaces
(namespace_definition
  name: (namespace_identifier) @name) @module

;; Concepts (C++20)
(concept_definition
  name: (identifier) @name) @trait

;; Type aliases — using X = Y (C++11)
(alias_declaration
  name: (type_identifier) @name) @typealias

;; Typedefs (C-style)
(type_definition
  declarator: (type_identifier) @name) @typealias

;; Unions
(union_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

;; Preprocessor constants
(preproc_def
  name: (identifier) @name) @const

;; Preprocessor function macros
(preproc_function_def
  name: (identifier) @name) @macro
