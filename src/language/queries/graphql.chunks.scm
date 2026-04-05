;; Object types (type User { ... })
(object_type_definition
  (name) @name) @struct

;; Interface types (interface Node { ... })
(interface_type_definition
  (name) @name) @interface

;; Enum types (enum Status { ... })
(enum_type_definition
  (name) @name) @enum

;; Union types (union SearchResult = User | Post)
(union_type_definition
  (name) @name) @typealias

;; Input types (input CreateUserInput { ... })
(input_object_type_definition
  (name) @name) @struct

;; Scalar types (scalar DateTime)
(scalar_type_definition
  (name) @name) @typealias

;; Directive definitions (@directive ...)
(directive_definition
  (name) @name) @macro

;; Operations (query GetUser { ... }, mutation CreateUser { ... })
(operation_definition
  (name) @name) @function

;; Fragments (fragment UserFields on User { ... })
(fragment_definition
  (fragment_name
    (name) @name)) @function
