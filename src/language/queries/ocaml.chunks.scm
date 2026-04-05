;; Let binding (function/value): let add x y = x + y
(value_definition
  (let_binding
    pattern: (value_name) @name)) @function

;; Type definition: type color = Red | Green | Blue
(type_definition
  (type_binding
    name: (type_constructor) @name)) @struct

;; Module definition: module Foo = struct ... end
(module_definition
  (module_binding
    (module_name) @name)) @struct
