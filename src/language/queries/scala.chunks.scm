;; Functions
(function_definition
  name: (identifier) @name) @function

;; Classes
(class_definition
  name: (identifier) @name) @class

;; Objects (singletons)
(object_definition
  name: (identifier) @name) @object

;; Traits
(trait_definition
  name: (identifier) @name) @trait

;; Enums (Scala 3)
(enum_definition
  name: (identifier) @name) @enum

;; Val bindings
(val_definition
  pattern: (identifier) @name) @const

;; Var bindings
(var_definition
  pattern: (identifier) @name) @const

;; Type aliases — name is type_identifier, not identifier
(type_definition
  name: (type_identifier) @name) @typealias

;; Scala 3 extensions — name extracted from first parameter type
(extension_definition
  parameters: (parameters
    (parameter
      type: (type_identifier) @name))) @extension
