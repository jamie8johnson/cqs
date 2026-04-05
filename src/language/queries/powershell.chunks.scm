;; Functions
(function_statement
  (function_name) @name) @function

;; Classes
(class_statement
  (simple_name) @name) @class

;; Class methods
(class_method_definition
  (simple_name) @name) @function

;; Class properties
(class_property_definition
  (variable) @name) @property

;; Enums
(enum_statement
  (simple_name) @name) @enum
