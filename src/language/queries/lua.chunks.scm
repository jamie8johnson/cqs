;; Named function declarations (function foo() / function mod.foo() / function mod:bar())
(function_declaration
  name: (_) @name) @function

;; Local variable assignments (local MAX_SIZE = 100)
;; Filtered to UPPER_CASE by post_process_lua
(variable_declaration
  (assignment_statement
    (variable_list
      name: (identifier) @name))) @const

;; Global assignments (MAX_RETRIES = 3)
;; Filtered to UPPER_CASE by post_process_lua
(assignment_statement
  (variable_list
    name: (identifier) @name)) @const
