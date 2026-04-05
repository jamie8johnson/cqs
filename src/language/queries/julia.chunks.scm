;; Function definition: function add(x, y) ... end
(function_definition
  (signature
    (call_expression . (identifier) @name))) @function

;; Struct definition: struct Point x::Float64 end
(struct_definition
  (type_head
    (identifier) @name)) @struct

;; Abstract type: abstract type Shape end
(abstract_definition
  (type_head
    (identifier) @name)) @struct

;; Module definition: module Foo ... end
(module_definition
  name: (identifier) @name) @struct

;; Macro definition: macro name(args) ... end
(macro_definition
  (signature
    (call_expression . (identifier) @name))) @function
