(function_declaration
  name: (identifier) @name) @function

(method_definition
  name: (property_identifier) @name) @function

;; Arrow function assigned to variable: const foo = () => {}
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

;; Arrow function assigned with var/let
(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

(class_declaration
  name: (identifier) @name) @class

;; Module-level const declarations (non-function values)
(lexical_declaration
  kind: "const"
  (variable_declarator
    name: (identifier) @name
    value: (_) @_val) @const)

;; Module-level let declarations → Variable
(lexical_declaration
  kind: "let"
  (variable_declarator
    name: (identifier) @name
    value: (_) @_val) @var)

;; Module-level var declarations → Variable
(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (_) @_val) @var)

;; Jest/Mocha/Vitest test blocks: describe("name", fn), it("name", fn), test("name", fn)
(expression_statement
  (call_expression
    function: (identifier) @_fn
    arguments: (arguments
      (string
        (string_fragment) @name))
    (#any-of? @_fn "describe" "it" "test"))) @test
