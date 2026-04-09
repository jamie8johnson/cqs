;; Function definitions (both `function foo() {}` and `foo() {}` syntaxes)
(function_definition
  name: (word) @name) @function

;; readonly FOO=bar declarations
(declaration_command
  "readonly"
  (variable_assignment
    name: (variable_name) @name)) @const

;; export FOO=bar declarations
(declaration_command
  "export"
  (variable_assignment
    name: (variable_name) @name)) @var

;; declare FOO=bar declarations
(declaration_command
  "declare"
  (variable_assignment
    name: (variable_name) @name)) @var
