(function_definition
  name: (identifier) @name) @function

(class_definition
  name: (identifier) @name) @class

;; Module-level constant assignments (UPPER_CASE convention)
(expression_statement
  (assignment
    left: (identifier) @name
    right: (_))) @const
